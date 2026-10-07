//! Compression and decompression of whole files.
//!
//! The coefficients of decodable granules go through [`model::code_granule`] into one rANS
//! stream and one raw bit stream (signs, escape mantissas). Everything else (file structure,
//! side information, scalefactors, gaps) goes into small byte streams compressed with zstd.

use crate::bits::{BitReader, BitWriter, FastWriter};
use crate::frame::{Header, SideInfo, crc16, scf_layout};
use crate::huff;
use crate::model::{self, Coder, State};
use crate::rans::{self, Tables};
use crate::rc;
use crate::side::SideModel;
use crate::stream::{R_PAD, place, segment};
use std::sync::OnceLock;

/// The built-in frequency tables (see `train`), or uniform ones if none are built in.
static BUILTIN: &[u8] = include_bytes!("../model/builtin.bin");

/// Identifies the context layout the built-in tables were trained for.
pub fn layout_id() -> u32 {
    let mut h = 0x811c9dc5u32;
    for c in 0..model::N_CTX {
        h = (h ^ model::alphabet(c) as u32).wrapping_mul(0x01000193);
    }
    h
}

pub fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| Tables::new(&builtin_freqs().unwrap_or_else(uniform_freqs)))
}

fn uniform_freqs() -> Vec<Vec<u16>> {
    (0..model::N_CTX)
        .map(|c| {
            let mut f = vec![0u16; model::alphabet(c)];
            rans::normalize(&vec![0; f.len()], &mut f);
            f
        })
        .collect()
}

/// Serialises frequency tables (for the built-in model file).
pub fn freqs_to_bytes(f: &[Vec<u16>]) -> Vec<u8> {
    let mut raw = layout_id().to_le_bytes().to_vec();
    for t in f {
        for &x in t {
            raw.extend_from_slice(&x.to_le_bytes());
        }
    }
    zstd::bulk::compress(&raw, 19).unwrap()
}

fn builtin_freqs() -> Option<Vec<Vec<u16>>> {
    if BUILTIN.is_empty() {
        return None;
    }
    let raw = zstd::stream::decode_all(BUILTIN).ok()?;
    if raw.len() < 4 || u32::from_le_bytes(raw[0..4].try_into().unwrap()) != layout_id() {
        return None;
    }
    let mut p = 4;
    let mut out = Vec::with_capacity(model::N_CTX);
    for c in 0..model::N_CTX {
        let n = model::alphabet(c);
        let t: Vec<u16> = raw.get(p..p + 2 * n)?.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
        p += 2 * n;
        out.push(t);
    }
    Some(out)
}

struct Enc<'a> {
    t: &'a Tables,
    rans: rans::Encoder,
    raw: FastWriter,
}

impl Coder for Enc<'_> {
    #[inline(always)]
    fn sym(&mut self, ctx: usize, s: u32) -> u32 {
        self.rans.put(self.t, ctx, s);
        s
    }
    #[inline(always)]
    fn raw(&mut self, n: u32, v: u32) -> u32 {
        let v = if n == 0 { 0 } else { v & (u32::MAX >> (32 - n)) };
        self.raw.put(v as u64, n);
        v
    }
    #[inline(always)]
    fn reserve(&mut self, syms: usize, raw_bits: usize) {
        self.rans.reserve(syms);
        self.raw.reserve(raw_bits / 8 + 8);
    }
    const ROLLBACK: bool = true;
    fn save(&self) -> [u64; 4] {
        let (p, a, n) = self.raw.mark();
        [self.rans.len() as u64, p as u64, a, n as u64]
    }
    fn restore(&mut self, s: [u64; 4]) {
        self.rans.truncate(s[0] as usize);
        self.raw.rollback((s[1] as usize, s[2], s[3] as u32));
    }
}

struct Dec<'a, 'b> {
    rans: rans::Decoder<'a>,
    raw: huff::BitBuf<'b>,
}

impl Coder for Dec<'_, '_> {
    #[inline(always)]
    fn sym(&mut self, ctx: usize, _s: u32) -> u32 {
        self.rans.get(ctx)
    }
    #[inline(always)]
    fn raw(&mut self, n: u32, _v: u32) -> u32 {
        // At most 32 bits per call (in practice 13).
        self.raw.refill();
        let v = ((self.raw.acc >> 1) >> (63 - n)) as u32;
        self.raw.consume(n);
        v
    }
}

// Byte streams.
const S_SEGS: usize = 0;
const S_JUNK: usize = 1;
const S_HDR: usize = 2;
const S_CRC: usize = 3;
const S_TAILBITS: usize = 4;
const S_RAWGR: usize = 5;
const S_GAPS: usize = 6;
/// Range coded side information and scalefactors.
const S_SIDE: usize = 7;
const N_BYTE_STREAMS: usize = 8;
pub const STREAM_NAMES: [&str; N_BYTE_STREAMS + 2] =
    ["segs", "junk", "headers", "crc", "tail bits", "raw gr", "gaps", "side", "rans", "raw bits"];

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

#[derive(Debug)]
pub struct Error(pub &'static str);

/// Sequential reader of a byte stream.
struct Rd<'a> {
    d: &'a [u8],
    p: usize,
}

impl<'a> Rd<'a> {
    fn u8(&mut self) -> Result<u8, Error> {
        let v = *self.d.get(self.p).ok_or(Error("stream exhausted"))?;
        self.p += 1;
        Ok(v)
    }
    fn bytes(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let s = self.d.get(self.p..self.p + n).ok_or(Error("stream exhausted"))?;
        self.p += n;
        Ok(s)
    }
    fn varint(&mut self) -> Result<u64, Error> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.u8()?;
            v |= ((b & 0x7f) as u64) << shift;
            if b < 0x80 {
                return Ok(v);
            }
        }
        Err(Error("bad varint"))
    }
}

/// Compressed representation: byte streams (before zstd), rANS words and raw bits.
pub struct Coded {
    pub streams: Vec<Vec<u8>>,
    pub words: Vec<u32>,
    pub raw: Vec<u8>,
    pub raw_bits: u64,
    pub bit_lens: [u64; 3],
}

pub fn encode(d: &[u8]) -> Coded {
    let t = tables();
    let mut c = Enc { t, rans: rans::Encoder::with_capacity(d.len() * 5 / 4 + model::MAX_GRANULE_SYMS), raw: FastWriter::with_capacity(d.len() / 4) };
    let (streams, bit_lens) = encode_with(d, &mut c);
    let words = c.rans.finish(t);
    let raw_bits = c.raw.len() as u64;
    Coded { streams, words, raw: c.raw.finish(), raw_bits, bit_lens }
}

/// Runs the model over `d` with a counting coder (for training).
pub fn count<C: Coder>(d: &[u8], c: &mut C) {
    encode_with(d, c);
}

fn encode_with<C: Coder>(d: &[u8], c: &mut C) -> (Vec<Vec<u8>>, [u64; 3]) {
    let (segs, offsets) = segment(d);
    let mut b: Vec<Vec<u8>> = vec![Vec::new(); N_BYTE_STREAMS];
    for &n in &segs {
        put_varint(&mut b[S_SEGS], n as u64);
    }
    {
        let (mut p, mut fi) = (0usize, 0usize);
        for (i, &n) in segs.iter().enumerate() {
            if i % 2 == 0 {
                b[S_JUNK].extend_from_slice(&d[p..p + n as usize]);
                p += n as usize;
            } else if n > 0 {
                fi += n as usize;
                let last = offsets[fi - 1];
                p = last + Header::from_bytes(&d[last..]).frame_len();
            }
        }
    }

    // Headers, side information, CRCs, reservoir.
    let mut r_buf = Vec::with_capacity(d.len() + R_PAD);
    let mut frames: Vec<(Header, SideInfo, usize, usize)> = Vec::with_capacity(offsets.len());
    let mut prev_h = 0u32;
    for &p in &offsets {
        let h = Header::from_bytes(&d[p..]);
        b[S_HDR].extend_from_slice(&(h.0 ^ prev_h).to_be_bytes());
        prev_h = h.0;
        let si_at = p + 4 + 2 * h.protected() as usize;
        let sil = h.side_info_len();
        let mut tmp = [0u8; 40];
        tmp[..sil].copy_from_slice(&d[si_at..si_at + sil]);
        let si = SideInfo::read(h, &mut BitReader::new(&tmp, 0));
        if h.protected() {
            let crc = u16::from_be_bytes([d[p + 4], d[p + 5]]);
            if crc == crc16(&d[p..p + 4], &d[si_at..si_at + sil]) {
                b[S_CRC].push(1);
            } else {
                b[S_CRC].push(0);
                b[S_CRC].extend_from_slice(&crc.to_be_bytes());
            }
        }
        let len = h.frame_len() - h.prefix_len();
        frames.push((h, si, r_buf.len(), len));
        r_buf.extend_from_slice(&d[p + h.prefix_len()..p + h.frame_len()]);
    }
    let r_len = r_buf.len();
    r_buf.resize(r_len + R_PAD, 0);

    let mut rc = rc::Encoder::default();
    let mut side = SideModel::new();
    let mut tail = BitWriter::new();
    let mut raw_gr = BitWriter::new();
    let mut gaps = BitWriter::new();
    let mut st = State::new();
    let mut coefs = [0i32; 576];
    let mut scf = [0u8; 39];
    let mut end = 0usize;
    for (h, si, pay, len) in &frames {
        let (h, pay, len) = (*h, *pay, *len);
        let start = place(si, h, pay, len, end);
        let mut placed = start.is_some();
        let pred = pay as i64 - end.div_ceil(8) as i64;
        let mut resid = (si.main_data_begin as i64 - pred) as i32;
        let mut si_c = *si;
        side.frame(&mut rc, h, &mut si_c, &mut placed, &mut resid);
        let Some(start) = start else {
            for gr in 0..h.granules() {
                for ch in 0..h.channels() {
                    let mut raw = true;
                    side.granule(&mut rc, h, &mut si_c.gr[gr][ch], ch, false, &mut raw);
                }
            }
            continue;
        };
        gaps.copy(&mut BitReader::new(&r_buf, end), start - end);
        let mut r = BitReader::new(&r_buf, start);
        for gr in 0..h.granules() {
            for ch in 0..h.channels() {
                let g = &si.gr[gr][ch];
                let gr_start = r.pos;
                let gr_end = gr_start + g.part2_3_length as usize;
                let lay = scf_layout(h, si, gr, ch);
                let mut nscf = 0;
                let mut part3 = None;
                if lay.bits() as usize <= g.part2_3_length as usize {
                    for k in 0..4 {
                        if !lay.copy[k] {
                            for _ in 0..lay.count[k] {
                                scf[nscf] = r.get(lay.size[k] as u32) as u8;
                                nscf += 1;
                            }
                        }
                    }
                    if C::ROLLBACK {
                        let saved = (c.save(), st.scf_prev[ch]);
                        c.reserve(39, 0);
                        model::code_scalefactors(c, &mut st, &lay, ch, &mut scf[..nscf]);
                        part3 = model::encode_granule(c, &mut st, h, si, gr, ch, &scf[..nscf], &mut r, gr_end);
                        if part3.is_none() {
                            c.restore(saved.0);
                            st.scf_prev[ch] = saved.1;
                        }
                    } else {
                        part3 = huff::decode_coefs(h, g, &mut r, gr_end, &mut coefs);
                        if let Some(p3) = &part3 {
                            model::code_scalefactors(c, &mut st, &lay, ch, &mut scf[..nscf]);
                            model::code_granule(c, &mut st, h, si, gr, ch, &scf[..nscf], p3.quads, &mut coefs);
                        }
                    }
                }
                let mut raw = part3.is_none();
                side.granule(&mut rc, h, &mut si_c.gr[gr][ch], ch, true, &mut raw);
                match part3 {
                    Some(p3) => {
                        let (mut nq, mut tl) = (p3.quads, p3.tail);
                        side.part3(&mut rc, ch, &mut nq, &mut tl);
                        tail.copy(&mut r, p3.tail);
                    }
                    None => {
                        raw_gr.copy(&mut BitReader::new(&r_buf, gr_start), g.part2_3_length as usize);
                    }
                }
                r.pos = gr_end;
            }
        }
        end = r.pos;
    }
    gaps.copy(&mut BitReader::new(&r_buf, end), r_len * 8 - end);
    let bit_lens = [tail.len() as u64, raw_gr.len() as u64, gaps.len() as u64];
    b[S_TAILBITS] = tail.finish();
    b[S_RAWGR] = raw_gr.finish();
    b[S_GAPS] = gaps.finish();
    b[S_SIDE] = rc.finish();
    (b, bit_lens)
}

pub fn decode(c: &Coded) -> Result<Vec<u8>, Error> {
    let t = tables();
    let raw_buf = {
        let mut p = c.raw.to_vec();
        p.resize(c.raw.len() + R_PAD, 0);
        p
    };
    let mut dc = Dec { rans: rans::Decoder::new(t, &c.words), raw: huff::BitBuf::new(&BitReader::new(&raw_buf, 0)) };
    let out = decode_with(c, &mut dc)?;
    if !dc.rans.in_bounds() {
        return Err(Error("rans stream exhausted"));
    }
    Ok(out)
}

/// [`decode`] with the model's symbols and raw bits taken from `dc`.
pub fn decode_with<C: Coder>(c: &Coded, dc: &mut C) -> Result<Vec<u8>, Error> {
    if c.streams.len() != N_BYTE_STREAMS {
        return Err(Error("stream count"));
    }
    let mut r: Vec<Rd> = c.streams.iter().map(|s| Rd { d: s, p: 0 }).collect();
    let mut segs = Vec::new();
    while r[S_SEGS].p < r[S_SEGS].d.len() {
        segs.push(r[S_SEGS].varint()? as usize);
    }
    let nframes: usize = segs.iter().skip(1).step_by(2).sum();

    let pad = |v: &[u8]| {
        let mut p = v.to_vec();
        p.resize(v.len() + R_PAD, 0);
        p
    };
    let tail_buf = pad(&c.streams[S_TAILBITS]);
    let raw_gr_buf = pad(&c.streams[S_RAWGR]);
    let gaps_buf = pad(&c.streams[S_GAPS]);
    let (mut tail, mut raw_gr, mut gaps) =
        (BitReader::new(&tail_buf, 0), BitReader::new(&raw_gr_buf, 0), BitReader::new(&gaps_buf, 0));
    let mut rc = rc::Decoder::new(&c.streams[S_SIDE]);
    let mut side = SideModel::new();

    let mut frames: Vec<(Header, SideInfo, usize, usize)> = Vec::with_capacity(nframes);
    let mut r_len = 0usize;
    let mut prev_h = 0u32;
    for _ in 0..nframes {
        let x = r[S_HDR].bytes(4)?;
        let h = Header(u32::from_be_bytes([x[0], x[1], x[2], x[3]]) ^ prev_h);
        prev_h = h.0;
        if !h.valid() {
            return Err(Error("bad header"));
        }
        let len = h.frame_len() - h.prefix_len();
        frames.push((h, SideInfo::default(), r_len, len));
        r_len += len;
    }

    let mut w = FastWriter::with_capacity(r_len + 8);
    let mut st = State::new();
    let mut coefs = [0i32; 576];
    let mut end = 0usize;
    let mut scf = [0u8; 39];
    for (h, si, pay, len) in frames.iter_mut() {
        let (h, pay, len) = (*h, *pay, *len);
        let mut placed = false;
        let mut resid = 0;
        side.frame(&mut rc, h, si, &mut placed, &mut resid);
        if !placed {
            for gr in 0..h.granules() {
                for ch in 0..h.channels() {
                    let mut raw = true;
                    side.granule(&mut rc, h, &mut si.gr[gr][ch], ch, false, &mut raw);
                }
            }
            if place(si, h, pay, len, end).is_some() {
                return Err(Error("placement"));
            }
            continue;
        }
        let pred = pay as i64 - end.div_ceil(8) as i64;
        let mdb = pred + resid as i64;
        let max_mdb = if h.lsf() { 255 } else { 511 };
        if !(0..=max_mdb).contains(&mdb) || mdb > pay as i64 || (pay - mdb as usize) * 8 < end {
            return Err(Error("main_data_begin"));
        }
        si.main_data_begin = mdb as u16;
        let start = (pay - mdb as usize) * 8;
        w.copy(&mut gaps, start - end);
        for gr in 0..h.granules() {
            for ch in 0..h.channels() {
                let mut raw = false;
                side.granule(&mut rc, h, &mut si.gr[gr][ch], ch, true, &mut raw);
                let g = si.gr[gr][ch];
                if raw {
                    w.copy(&mut raw_gr, g.part2_3_length as usize);
                    continue;
                }
                let gr_start = w.len();
                let lay = scf_layout(h, si, gr, ch);
                let nscf: usize = (0..4).filter(|&k| !lay.copy[k]).map(|k| lay.count[k] as usize).sum();
                let (mut nq, mut tl) = (0, 0);
                side.part3(&mut rc, ch, &mut nq, &mut tl);
                model::code_scalefactors(dc, &mut st, &lay, ch, &mut scf[..nscf]);
                let mut k = 0;
                w.reserve(64);
                for g4 in 0..4 {
                    if !lay.copy[g4] {
                        for _ in 0..lay.count[g4] {
                            w.put(scf[k] as u64 & ((1 << lay.size[g4]) - 1), lay.size[g4] as u32);
                            k += 1;
                        }
                    }
                }
                if g.big_values > 288 || g.big_values as usize * 2 + nq * 4 > 576 {
                    return Err(Error("granule size"));
                }
                if g.table_select.iter().any(|&t| t == 4 || t == 14) {
                    return Err(Error("table"));
                }
                model::code_granule(dc, &mut st, h, si, gr, ch, &scf[..nscf], nq, &mut coefs);
                huff::encode_coefs(h, &g, &coefs, nq, &mut w);
                if tl > 4095 {
                    return Err(Error("tail"));
                }
                w.copy(&mut tail, tl);
                let p23 = w.len() - gr_start;
                if p23 > 4095 {
                    return Err(Error("part2_3_length"));
                }
                si.gr[gr][ch].part2_3_length = p23 as u16;
            }
        }
        end = w.len();
        if end > (pay + len) * 8 {
            return Err(Error("main data overflow"));
        }
    }
    if !rc.in_bounds() {
        return Err(Error("side stream exhausted"));
    }
    w.copy(&mut gaps, r_len * 8 - end);
    let r_out = w.finish();

    let total = c.streams[S_JUNK].len() + frames.iter().map(|f| f.0.frame_len()).sum::<usize>();
    let mut out = Vec::with_capacity(total);
    let (mut fi, mut jp) = (0usize, 0usize);
    for (k, &n) in segs.iter().enumerate() {
        if k % 2 == 0 {
            out.extend_from_slice(c.streams[S_JUNK].get(jp..jp + n).ok_or(Error("junk"))?);
            jp += n;
            continue;
        }
        for _ in 0..n {
            let (h, si, pay, len) = &frames[fi];
            let hb = h.0.to_be_bytes();
            out.extend_from_slice(&hb);
            let mut sw = BitWriter::new();
            si.write(*h, &mut sw);
            let side = sw.finish();
            if h.protected() {
                if r[S_CRC].u8()? != 0 {
                    out.extend_from_slice(&crc16(&hb, &side).to_be_bytes());
                } else {
                    out.extend_from_slice(r[S_CRC].bytes(2)?);
                }
            }
            out.extend_from_slice(&side);
            out.extend_from_slice(&r_out[*pay..pay + len]);
            fi += 1;
        }
    }
    Ok(out)
}
