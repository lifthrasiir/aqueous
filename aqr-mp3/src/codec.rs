//! Encoding and decoding of whole files.
//!
//! The coefficients of decodable granules go through [`model::code_granule`] into one rANS
//! stream and one raw bit stream (signs, escape mantissas). Everything else (file structure,
//! side information, scalefactors, gaps) goes into small byte streams.

use crate::bits::{BitReader, BitWriter, FastWriter, WordWriter};
use crate::frame::{Granule, Header, SideInfo, crc16, scf_layout};
use crate::huff;
use crate::model::{self, Coder, State, Sym};
use crate::rans::{self, Tables, builtin};
use crate::rc;
use crate::side::SideModel;
use crate::stream::{R_PAD, place, segment};
use aqr_common::varint::{self, put as put_varint};
use std::sync::{Arc, OnceLock};

/// The built-in frequency tables (see `train`), or uniform ones if none are built in.
static BUILTIN: &[u8] = include_bytes!("../model/builtin.bin");

/// Identifies the context layout the built-in tables were trained for.
pub fn layout_id() -> u32 {
    builtin::layout_id((0..model::N_CTX).map(|c| model::alphabet(c) as u32))
}

/// The built-in tables (what the encoder codes with).
pub fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| tables_from(builtin_raw()).expect("built-in tables"))
}

/// The built-in tables in their uncompressed form: the module's resource.
pub fn builtin_raw() -> &'static Arc<[u8]> {
    static R: OnceLock<Arc<[u8]>> = OnceLock::new();
    R.get_or_init(|| {
        builtin::decompress(BUILTIN).unwrap_or_else(|| builtin::to_raw(layout_id(), &uniform_freqs())).into()
    })
}

/// Frames per run of the rANS stream: the coder restarts before every frame whose index is a
/// positive multiple of it, so that the encoder holds the symbols of one run at a time.
pub const RUN_FRAMES: usize = 1024;

/// The tables in the uncompressed form `raw`, if they are valid for the layout.
pub fn tables_from(raw: &[u8]) -> Option<Tables> {
    Some(Tables::new(&builtin::parse(raw, layout_id(), (0..model::N_CTX).map(model::alphabet))?))
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
    builtin::to_bytes(layout_id(), f)
}

/// The built-in frequency tables, if any match the layout (for tools).
pub fn builtin_freqs_pub() -> Option<Vec<Vec<u16>>> {
    builtin_freqs()
}

fn builtin_freqs() -> Option<Vec<Vec<u16>>> {
    builtin::from_bytes(BUILTIN, layout_id(), (0..model::N_CTX).map(model::alphabet))
}

pub(crate) struct Enc<'a> {
    pub(crate) t: &'a Tables,
    pub(crate) rans: rans::Encoder,
    /// The words of the runs flushed so far, u32 LE.
    pub(crate) words: Vec<u8>,
    pub(crate) raw: WordWriter,
}

impl Enc<'_> {
    pub(crate) fn new(t: &Tables, input_len: usize) -> Enc<'_> {
        // Symbols of a run: about 1.2 per byte of input; frames are rarely beyond 1.5 KB.
        let syms = (input_len * 5 / 4).min(RUN_FRAMES * 1500) + model::MAX_GRANULE_SYMS;
        Enc {
            t,
            rans: rans::Encoder::with_capacity(syms),
            // About 0.6 bytes per byte of input; what is not used is not touched.
            words: Vec::with_capacity(input_len),
            raw: WordWriter::with_capacity(input_len / 4),
        }
    }

    /// The rANS words of all runs (u32 LE), the last one flushed.
    pub(crate) fn finish(&mut self) -> Vec<u8> {
        self.rans.flush(self.t, &mut self.words);
        std::mem::take(&mut self.words)
    }
}

impl Coder for Enc<'_> {
    type L = EncLocal;
    #[inline(always)]
    fn local(&mut self) -> EncLocal {
        EncLocal { rans: self.rans.local_tracked(self.t), raw: self.raw.local() }
    }
    #[inline(always)]
    fn put_back(&mut self, l: EncLocal) {
        self.rans.put_back(l.rans);
        self.raw.put_back(l.raw);
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
    fn restart(&mut self) {
        self.rans.flush(self.t, &mut self.words);
    }
}

pub(crate) struct EncLocal {
    rans: rans::EncLocal<true>,
    raw: crate::bits::WordBits,
}

impl Sym for EncLocal {
    #[inline(always)]
    fn sym(&mut self, ctx: usize, s: u32) -> u32 {
        self.rans.put(ctx, s);
        s
    }
    #[inline(always)]
    fn raw(&mut self, n: u32, v: u32) -> u32 {
        let v = if n == 0 { 0 } else { v & (u32::MAX >> (32 - n)) };
        self.raw.put(v as u64, n);
        v
    }
}

pub(crate) struct Dec<'a, 'b> {
    pub(crate) rans: rans::Decoder<'a>,
    pub(crate) raw: huff::BitBuf<'b>,
}

impl<'b> Coder for Dec<'_, 'b> {
    type L = DecLocal<'b>;
    #[inline(always)]
    fn local(&mut self) -> DecLocal<'b> {
        DecLocal { rans: self.rans.local(), raw: self.raw }
    }
    #[inline(always)]
    fn put_back(&mut self, l: DecLocal<'b>) {
        self.rans.put_back(l.rans);
        self.raw = l.raw;
    }
    fn restart(&mut self) {
        self.rans.restart();
    }
}

pub(crate) struct DecLocal<'b> {
    rans: rans::DecLocal,
    raw: huff::BitBuf<'b>,
}

impl Sym for DecLocal<'_> {
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
pub(crate) const S_SEGS: usize = 0;
pub(crate) const S_JUNK: usize = 1;
pub(crate) const S_HDR: usize = 2;
pub(crate) const S_CRC: usize = 3;
pub(crate) const S_TAILBITS: usize = 4;
pub(crate) const S_RAWGR: usize = 5;
pub(crate) const S_GAPS: usize = 6;
/// Range coded side information and scalefactors.
pub(crate) const S_SIDE: usize = 7;
pub(crate) const N_BYTE_STREAMS: usize = 8;
pub const STREAM_NAMES: [&str; N_BYTE_STREAMS + 2] =
    ["segs", "junk", "headers", "crc", "tail bits", "raw gr", "gaps", "side", "rans", "raw bits"];

#[derive(Debug)]
pub struct Error(pub &'static str);

/// Sequential reader of a byte stream.
pub(crate) struct Rd<'a> {
    pub(crate) d: &'a [u8],
    pub(crate) p: usize,
}

impl<'a> Rd<'a> {
    pub(crate) fn u8(&mut self) -> Result<u8, Error> {
        let v = *self.d.get(self.p).ok_or(Error("stream exhausted"))?;
        self.p += 1;
        Ok(v)
    }
    pub(crate) fn bytes(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let s = self.d.get(self.p..self.p + n).ok_or(Error("stream exhausted"))?;
        self.p += n;
        Ok(s)
    }
    fn varint(&mut self) -> Result<u64, Error> {
        varint::get(self.d, &mut self.p).map_err(|e| Error(e.0))
    }
}

/// Compressed representation: byte streams, rANS words and raw bits.
pub struct Coded {
    /// Length of the original file.
    pub len: u64,
    pub streams: Vec<Vec<u8>>,
    /// The rANS words, u32 LE.
    pub words: Vec<u8>,
    pub raw: Vec<u8>,
    pub raw_bits: u64,
    pub bit_lens: [u64; 3],
}

pub fn encode(d: &[u8]) -> Coded {
    let mut c = Enc::new(tables(), d.len());
    let (streams, bit_lens) = encode_with(d, &mut c);
    let words = c.finish();
    let raw_bits = c.raw.len() as u64;
    Coded { len: d.len() as u64, streams, words, raw: c.raw.finish(), raw_bits, bit_lens }
}

/// Runs the model over `d` with a counting coder (for training).
pub fn count<C: Coder>(d: &[u8], c: &mut C) {
    encode_with(d, c);
}

fn encode_with<C: Coder>(d: &[u8], c: &mut C) -> (Vec<Vec<u8>>, [u64; 3]) {
    model_pass(split(d), c)
}

/// A file split into frames, before the model pass: the byte streams of the file structure
/// (segments, junk, headers, CRCs) are complete.
pub(crate) struct Split {
    pub(crate) b: Vec<Vec<u8>>,
    /// Header, side information, payload offset in `r_buf` and payload length per frame.
    pub(crate) frames: Vec<(Header, SideInfo, usize, usize)>,
    /// The reservoir (payloads of all frames), padded with `R_PAD` zero bytes.
    r_buf: Vec<u8>,
    r_len: usize,
}

pub(crate) fn split(d: &[u8]) -> Split {
    let (junk, runs, offsets) = segment(d);
    let mut b: Vec<Vec<u8>> = vec![Vec::new(); N_BYTE_STREAMS];
    // Junk lengths and frame runs (count, free format length), alternating.
    for (i, &j) in junk.iter().enumerate() {
        put_varint(&mut b[S_SEGS], j as u64);
        if let Some(&(n, free)) = runs.get(i) {
            put_varint(&mut b[S_SEGS], n as u64);
            put_varint(&mut b[S_SEGS], free as u64);
        }
    }
    {
        let (mut p, mut fi) = (0usize, 0usize);
        for (i, &j) in junk.iter().enumerate() {
            b[S_JUNK].extend_from_slice(&d[p..p + j as usize]);
            p += j as usize;
            if let Some(&(n, _)) = runs.get(i) {
                fi += n as usize;
                if n > 0 {
                    let (last, len) = offsets[fi - 1];
                    p = last + len;
                }
            }
        }
    }

    // Headers, side information, CRCs, reservoir.
    let mut r_buf = Vec::with_capacity(d.len() + R_PAD);
    let mut frames: Vec<(Header, SideInfo, usize, usize)> = Vec::with_capacity(offsets.len());
    let mut prev_h = 0u32;
    for &(p, flen) in &offsets {
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
        let len = flen - h.prefix_len();
        frames.push((h, si, r_buf.len(), len));
        r_buf.extend_from_slice(&d[p + h.prefix_len()..p + flen]);
    }
    let r_len = r_buf.len();
    r_buf.resize(r_len + R_PAD, 0);
    Split { b, frames, r_buf, r_len }
}

/// Codes the frames of `s` (coefficients through `c`, the rest into the byte streams).
pub(crate) fn model_pass<C: Coder>(s: Split, c: &mut C) -> (Vec<Vec<u8>>, [u64; 3]) {
    let Split { mut b, frames, r_buf, r_len } = s;
    let mut rc = rc::Encoder::default();
    let mut side = SideModel::new();
    let mut tail = BitWriter::new();
    let mut raw_gr = BitWriter::new();
    let mut gaps = BitWriter::new();
    let mut st = State::new();
    let mut coefs = [0i32; 576];
    let mut scf = [0u8; 39];
    let mut end = 0usize;
    for (fi, (h, si, pay, len)) in frames.iter().enumerate() {
        if fi > 0 && fi % RUN_FRAMES == 0 {
            c.restart();
        }
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

pub fn decode(c: &Coded, t: &Tables) -> Result<Vec<u8>, Error> {
    let streams: Vec<&[u8]> = c.streams.iter().map(|s| &s[..]).collect();
    let streams = streams.try_into().map_err(|_| Error("stream count"))?;
    decode_parts(t, c.len, &streams, &c.words, &c.raw)
}

/// [`decode`] of the coded form in parts: the byte streams, the rANS words and the raw bits.
pub(crate) fn decode_parts(
    t: &Tables,
    len: u64,
    s: &[&[u8]; N_BYTE_STREAMS],
    words: &[u8],
    raw: &[u8],
) -> Result<Vec<u8>, Error> {
    let raw_buf = {
        let mut p = Vec::with_capacity(raw.len() + R_PAD);
        p.extend_from_slice(raw);
        p.resize(raw.len() + R_PAD, 0);
        p
    };
    let mut dc =
        Dec { rans: rans::Decoder::from_le_bytes(t, words), raw: huff::BitBuf::new(&BitReader::new(&raw_buf, 0)) };
    let mut fr = parse_frames(s[S_SEGS], s[S_HDR], len)?;
    let mut side = InlineSide { rc: rc::Decoder::new(s[S_SIDE]), m: SideModel::new() };
    let mut out = Assembler::new(&fr, s[S_JUNK], s[S_CRC]);
    let w = main_pass(&mut fr, s[S_TAILBITS], s[S_RAWGR], s[S_GAPS], &mut side, &mut dc, &mut out, WINDOW)?;
    let out = out.finish(&fr, &w)?;
    if !dc.rans.in_bounds() {
        return Err(Error("rans stream exhausted"));
    }
    Ok(out)
}

/// The file structure: junk and frame runs, and the frames.
pub(crate) struct Frames {
    /// Junk lengths and frame runs (count, free format length), alternating.
    pub(crate) junk: Vec<usize>,
    pub(crate) runs: Vec<(usize, usize)>,
    /// Header, side information, payload offset in the reservoir, payload length.
    pub(crate) frames: Vec<(Header, SideInfo, usize, usize)>,
    pub(crate) flens: Vec<usize>,
    r_len: usize,
}

/// Reads the segments and the headers; the side information is left to [`main_pass`].
pub(crate) fn parse_frames(segs: &[u8], hdr: &[u8], file_len: u64) -> Result<Frames, Error> {
    let mut rs = Rd { d: segs, p: 0 };
    let mut rh = Rd { d: hdr, p: 0 };
    let mut junk = Vec::new();
    let mut runs = Vec::new();
    loop {
        junk.push(rs.varint()? as usize);
        if rs.p == rs.d.len() {
            break;
        }
        let n = rs.varint()? as usize;
        let free = rs.varint()? as usize;
        if free > 1 << 16 {
            return Err(Error("free format length"));
        }
        runs.push((n, free));
    }
    let nframes: usize = runs.iter().map(|r| r.0).sum();
    if nframes > hdr.len() / 4 {
        return Err(Error("frame count"));
    }

    let mut frames: Vec<(Header, SideInfo, usize, usize)> = Vec::with_capacity(nframes);
    let mut flens = Vec::with_capacity(nframes);
    let mut r_len = 0usize;
    let mut prev_h = 0u32;
    for &(n, free) in &runs {
        for _ in 0..n {
            let x = rh.bytes(4)?;
            let h = Header(u32::from_be_bytes([x[0], x[1], x[2], x[3]]) ^ prev_h);
            prev_h = h.0;
            if !h.valid_any() || h.free_format() != (free > 0) {
                return Err(Error("bad header"));
            }
            let flen = h.frame_len_free(free);
            if flen < h.prefix_len() || (r_len + flen) as u64 > file_len {
                return Err(Error("frame length"));
            }
            let len = flen - h.prefix_len();
            frames.push((h, SideInfo::default(), r_len, len));
            flens.push(flen);
            r_len += len;
        }
    }
    Ok(Frames { junk, runs, frames, flens, r_len })
}

/// Where the main pass gets the side information from (the decoding order is fixed: per
/// frame `frame`, then per granule `granule` and, for coded granules, `part3`).
pub(crate) trait SideSrc {
    fn frame(&mut self, h: Header, si: &mut SideInfo, placed: &mut bool, resid: &mut i32);
    fn granule(&mut self, h: Header, g: &mut Granule, ch: usize, placed: bool, raw: &mut bool);
    fn part3(&mut self, ch: usize, nq: &mut usize, tl: &mut usize);
    /// After the last frame: whether the side stream was long enough.
    fn in_bounds(&mut self) -> bool;
}

/// Side information decoded in the main pass itself.
pub(crate) struct InlineSide<'a> {
    pub(crate) rc: rc::Decoder<'a>,
    pub(crate) m: Box<SideModel>,
}

impl SideSrc for InlineSide<'_> {
    #[inline(always)]
    fn frame(&mut self, h: Header, si: &mut SideInfo, placed: &mut bool, resid: &mut i32) {
        self.m.frame(&mut self.rc, h, si, placed, resid);
    }
    #[inline(always)]
    fn granule(&mut self, h: Header, g: &mut Granule, ch: usize, placed: bool, raw: &mut bool) {
        self.m.granule(&mut self.rc, h, g, ch, placed, raw);
    }
    #[inline(always)]
    fn part3(&mut self, ch: usize, nq: &mut usize, tl: &mut usize) {
        self.m.part3(&mut self.rc, ch, nq, tl);
    }
    fn in_bounds(&mut self) -> bool {
        self.rc.in_bounds()
    }
}

/// What the main pass hands the reservoir to as it is written.
pub(crate) trait Sink {
    /// Before frame `fi`, whose predecessors' side information is complete, and at the end
    /// (`fi` the number of frames): the reservoir written so far in `w`, of which the sink may
    /// drop what it no longer needs.
    fn ready(&mut self, fr: &Frames, fi: usize, w: &mut FastWriter);
}

/// The reservoir kept whole.
impl Sink for () {
    #[inline(always)]
    fn ready(&mut self, _fr: &Frames, _fi: usize, _w: &mut FastWriter) {}
}

/// Initial size of the reservoir's buffer for [`Assembler`] (it grows as needed).
const WINDOW: usize = 1 << 20;

/// Bytes of the reservoir dropped at a time by [`Assembler`], at least.
const DISCARD_MIN: usize = 256 << 10;

/// The output file, written as the main pass goes: junk, and per frame header, CRC, side
/// information and payload, each frame once its side information is complete and its payload
/// in the reservoir is final. The reservoir before the next frame's payload is dropped. The
/// first error is kept for the end of the main pass, whose errors come first.
pub(crate) struct Assembler<'a> {
    junk_s: &'a [u8],
    crc: Rd<'a>,
    out: Vec<u8>,
    /// The next junk to write, and its offset in the junk stream.
    k: usize,
    jp: usize,
    /// Frames left in the current run, and the next frame to write.
    left: usize,
    fi: usize,
    err: Option<Error>,
}

impl<'a> Assembler<'a> {
    pub(crate) fn new(fr: &Frames, junk_s: &'a [u8], crc_s: &'a [u8]) -> Assembler<'a> {
        let total = junk_s.len() + fr.flens.iter().sum::<usize>();
        Assembler {
            junk_s,
            crc: Rd { d: crc_s, p: 0 },
            out: Vec::with_capacity(total),
            k: 0,
            jp: 0,
            left: 0,
            fi: 0,
            err: None,
        }
    }

    /// Writes the frames before `ready` whose payloads lie within `r` (the reservoir from byte
    /// `base`), and the junk before them and after the last.
    fn write(&mut self, fr: &Frames, ready: usize, base: usize, r: &[u8]) -> Result<(), Error> {
        loop {
            while self.left == 0 {
                let Some(&j) = fr.junk.get(self.k) else { return Ok(()) };
                self.out.extend_from_slice(self.junk_s.get(self.jp..self.jp + j).ok_or(Error("junk"))?);
                self.jp += j;
                self.left = fr.runs.get(self.k).map_or(0, |r| r.0);
                self.k += 1;
            }
            let (h, si, pay, len) = &fr.frames[self.fi];
            if self.fi >= ready || pay + len > base + r.len() {
                return Ok(());
            }
            let hb = h.0.to_be_bytes();
            self.out.extend_from_slice(&hb);
            let mut sw = BitWriter::new();
            si.write(*h, &mut sw);
            let side = sw.finish();
            if h.protected() {
                if self.crc.u8()? != 0 {
                    self.out.extend_from_slice(&crc16(&hb, &side).to_be_bytes());
                } else {
                    self.out.extend_from_slice(self.crc.bytes(2)?);
                }
            }
            self.out.extend_from_slice(&side);
            self.out.extend_from_slice(&r[pay - base..pay + len - base]);
            self.fi += 1;
            self.left -= 1;
        }
    }

    /// The output, after the main pass (the whole reservoir written to `w`).
    pub(crate) fn finish(mut self, fr: &Frames, w: &FastWriter) -> Result<Vec<u8>, Error> {
        if self.err.is_none() {
            let (base, r) = w.done();
            self.write(fr, fr.frames.len(), base, r)?;
        }
        self.err.map_or(Ok(self.out), Err)
    }
}

impl Sink for Assembler<'_> {
    fn ready(&mut self, fr: &Frames, fi: usize, w: &mut FastWriter) {
        if self.err.is_some() {
            return;
        }
        let (base, r) = w.done();
        if let Err(e) = self.write(fr, fi, base, r) {
            self.err = Some(e);
            return;
        }
        let keep = fr.frames.get(self.fi).map_or(base + r.len(), |f| f.2.min(base + r.len()));
        if keep - base >= DISCARD_MIN {
            w.discard(keep);
        }
    }
}

/// Rebuilds the reservoir (the main data of all frames) into a writer of `capacity` bytes to
/// begin with, handing it to `sink` as it goes, and completes the side information of the
/// frames (main_data_begin, part2_3_length). Returns the writer, the whole reservoir written.
#[allow(clippy::too_many_arguments)]
pub(crate) fn main_pass<C: Coder, S: SideSrc, K: Sink>(
    fr: &mut Frames,
    tail: &[u8],
    raw_gr: &[u8],
    gaps: &[u8],
    side: &mut S,
    dc: &mut C,
    sink: &mut K,
    capacity: usize,
) -> Result<FastWriter, Error> {
    let pad = |v: &[u8]| {
        let mut p = v.to_vec();
        p.resize(v.len() + R_PAD, 0);
        p
    };
    let tail_buf = pad(tail);
    let raw_gr_buf = pad(raw_gr);
    let gaps_buf = pad(gaps);
    let (mut tail, mut raw_gr, mut gaps) =
        (BitReader::new(&tail_buf, 0), BitReader::new(&raw_gr_buf, 0), BitReader::new(&gaps_buf, 0));
    let r_len = fr.r_len;

    let mut w = FastWriter::with_capacity(capacity);
    let mut st = State::new();
    let mut end = 0usize;
    let mut scf = [0u8; 39];
    for fi in 0..fr.frames.len() {
        sink.ready(fr, fi, &mut w);
        if fi > 0 && fi % RUN_FRAMES == 0 {
            dc.restart();
        }
        let (h, si, pay, len) = &mut fr.frames[fi];
        let (h, pay, len) = (*h, *pay, *len);
        let mut placed = false;
        let mut resid = 0;
        side.frame(h, si, &mut placed, &mut resid);
        if !placed {
            for gr in 0..h.granules() {
                for ch in 0..h.channels() {
                    let mut raw = true;
                    side.granule(h, &mut si.gr[gr][ch], ch, false, &mut raw);
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
                side.granule(h, &mut si.gr[gr][ch], ch, true, &mut raw);
                let g = si.gr[gr][ch];
                if raw {
                    w.copy(&mut raw_gr, g.part2_3_length as usize);
                    continue;
                }
                let gr_start = w.len();
                let lay = scf_layout(h, si, gr, ch);
                let nscf: usize = (0..4).filter(|&k| !lay.copy[k]).map(|k| lay.count[k] as usize).sum();
                let (mut nq, mut tl) = (0, 0);
                side.part3(ch, &mut nq, &mut tl);
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
                model::decode_granule(dc, &mut st, h, si, gr, ch, &scf[..nscf], nq, &mut w);
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
    if !side.in_bounds() {
        return Err(Error("side stream exhausted"));
    }
    w.copy(&mut gaps, r_len * 8 - end);
    sink.ready(fr, fr.frames.len(), &mut w);
    Ok(w)
}

/// The output file: junk, and per frame header, CRC, side information and main data, from
/// the whole reservoir `r_out`.
pub(crate) fn assemble(fr: &Frames, junk_s: &[u8], crc_s: &[u8], r_out: &[u8]) -> Result<Vec<u8>, Error> {
    let mut a = Assembler::new(fr, junk_s, crc_s);
    a.write(fr, fr.frames.len(), 0, r_out)?;
    Ok(a.out)
}
