//! The token representation of a FlateDecode stream: its DEFLATE tokens (literals and
//! matches, as the encoder chose them) coded with static per-file tables chosen by context,
//! so that rebuilding the stream needs no match search, only Huffman coding.
//!
//! - Literals and length codes share one alphabet ([`LIT_ALPHA`]); the table is chosen by
//!   the byte before (per class, at the resolution that costs least for the file: every
//!   byte, 32 buckets, 8 buckets or none). Stored blocks' bytes are coded as literals.
//! - Distance codes have a table per length code (capped), and extra bits are raw.
//! - The block layout (types, token counts, stored lengths, padding) and the matches of
//!   length 258 sent as 284 + 31 go to a layout stream; dynamic Huffman headers go through
//!   flate-recons' header coder, against the block's frequencies.
//!
//! All streams of a file share the layout, header and raw bit streams and one rANS stream,
//! coded and decoded in file order.

use crate::kind::{CLASSES, Class};
use crate::rans::{self, BitR, BitW, Decoder, Tables};
use crate::{Error, Result};
use flate_recons::bits::{BitReader, BitWriter};
use flate_recons::deflate::{Block, Body, DynHeader, Stream, Token};
use flate_recons::fast::header::{self, Recipe};
use flate_recons::fast::inflate::Flat;
use flate_recons::huffman::{DIST_BASE, DIST_EXTRA, LEN_BASE, LEN_EXTRA};

/// Literals 0-255, then length codes 0-28 (symbols 257-285).
pub const LIT_ALPHA: usize = 256 + 29;
pub const DIST_ALPHA: usize = 30;
const STRIDE: usize = 288;
/// Distance tables per class (by length code, capped).
const DIST_CTX: usize = 8;
/// Literal contexts at the finest resolution: the byte before.
const LIT_CTX: usize = 256;
/// Tables per class at the finest resolution.
const PER_CLASS: usize = LIT_CTX + DIST_CTX;

/// The length code of every length 3-258.
static LEN_CODE: [u8; 259] = {
    let mut t = [0u8; 259];
    let mut c = 0;
    while c < 29 {
        let lo = LEN_BASE[c] as usize;
        let hi = if c == 28 { 259 } else { LEN_BASE[c + 1] as usize };
        let mut l = lo;
        while l < hi && l < 259 {
            t[l] = c as u8;
            l += 1;
        }
        c += 1;
    }
    t[258] = 28;
    t
};

#[inline(always)]
fn dist_code(d: u32) -> usize {
    // 32 - lz(d - 1) is the bit length; codes come in pairs per bit length.
    if d <= 4 {
        return d as usize - 1;
    }
    let x = d - 1;
    let nb = 31 - x.leading_zeros();
    (2 * nb + ((x >> (nb - 1)) & 1)) as usize
}

/// Coarse byte classes for text-like data (content streams, object streams): buckets of
/// similar bytes, grouped by fours so that `bucket >> 2` is a coarser class.
static TEXT_MAP: [u8; 256] = {
    let mut t = [27u8; 256];
    let mut b = 0;
    while b < 256 {
        let c = b as u8;
        t[b] = match c {
            b'0' | b'1' => 0,
            b'2'..=b'4' => 1,
            b'5'..=b'7' => 2,
            b'8' | b'9' => 3,
            b' ' => 4,
            b'\n' => 5,
            b'\r' => 6,
            b'\t' | 0x0c | 0 => 7,
            b'.' => 8,
            b'-' => 9,
            b'+' => 10,
            b',' => 11,
            b'a'..=b'f' => 12,
            b'g'..=b'm' => 13,
            b'n'..=b's' => 14,
            b't'..=b'z' => 15,
            b'A'..=b'F' => 16,
            b'G'..=b'M' => 17,
            b'N'..=b'S' => 18,
            b'T'..=b'Z' => 19,
            b'/' => 20,
            b'(' | b')' => 21,
            b'<' | b'>' => 22,
            b'[' | b']' | b'{' | b'}' => 23,
            b'\\' => 24,
            b'%' | b'#' => 25,
            b'*' | b'\'' | b'"' | b'_' => 26,
            0x80..=0x9f => 28,
            0xa0..=0xbf => 29,
            0xc0..=0xdf => 30,
            0xe0..=0xff => 31,
            _ => 27,
        };
        b += 1;
    }
    t
};

static BIN_MAP: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut b = 0;
    while b < 256 {
        t[b] = (b >> 3) as u8;
        b += 1;
    }
    t
};

fn bucket_map(c: Class) -> &'static [u8; 256] {
    match c {
        Class::Font | Class::Image => &BIN_MAP,
        _ => &TEXT_MAP,
    }
}

/// Literal context resolutions: every byte, 32 buckets, 8 buckets, one.
const RESOLUTIONS: usize = 4;

/// The literal table (0..) of byte `b` at resolution `res`.
#[inline(always)]
fn lit_ctx(map: &[u8; 256], res: u8, b: u8) -> usize {
    match res {
        0 => b as usize,
        1 => map[b as usize] as usize,
        2 => (map[b as usize] >> 2) as usize,
        _ => 0,
    }
}

fn res_tables(res: u8) -> usize {
    [256, 32, 8, 1][res as usize]
}

/// Copies a match of `len` bytes from `dist` back to `o`, in 8-byte chunks (the buffer has
/// room for the overshoot). Distances under 8 copy their first period byte by byte, then
/// chunks from a multiple of the distance.
#[inline(always)]
fn copy_match(d: &mut [u8], o: usize, dist: usize, len: usize) {
    assert!(dist >= 1 && dist <= o && o + len + 8 <= d.len());
    let p = d.as_mut_ptr();
    // SAFETY: by the assertion, every chunk reads from `o - dist` on and writes below
    // `o + len + 8`, within the buffer; chunks read only bytes already written.
    unsafe {
        let chunk = |from: usize, to: usize| {
            let v = std::ptr::read_unaligned(p.add(from) as *const u64);
            std::ptr::write_unaligned(p.add(to) as *mut u64, v);
        };
        if dist >= 8 {
            let mut k = 0;
            while k < len {
                chunk(o + k - dist, o + k);
                k += 8;
            }
        } else if dist == 1 {
            std::ptr::write_bytes(p.add(o), *p.add(o - 1), len);
        } else {
            let m = [0, 0, 8, 9, 8, 10, 12, 14][dist];
            let first = m.min(len);
            for k in 0..first {
                *p.add(o + k) = *p.add(o + k - dist);
            }
            let mut k = first;
            while k < len {
                chunk(o + k - m, o + k);
                k += 8;
            }
        }
    }
}

/// Room after the data for overlapping copies.
const SLACK: usize = 16;

/// The encoder of a file's token streams.
pub struct Encoder {
    syms: Vec<(u16, u16)>,
    raw: BitW,
    layout: BitW,
    headers: BitWriter,
    recipe: Recipe,
    /// Literal and distance counts per class at the finest resolution (allocated on use).
    counts: Vec<Vec<u32>>,
    /// Raw bits per class.
    raw_bits: [u64; CLASSES.len()],
}

impl Default for Encoder {
    fn default() -> Self {
        Encoder {
            syms: vec![],
            raw: BitW::default(),
            layout: BitW::default(),
            headers: BitWriter::new(),
            recipe: Recipe::default(),
            counts: vec![vec![]; CLASSES.len()],
            raw_bits: [0; CLASSES.len()],
        }
    }
}

impl Encoder {
    /// Adds a parsed stream (with no trailing bytes) of class `c`.
    pub fn add(&mut self, c: Class, f: &Flat) -> Result<()> {
        if !f.stream.trailing.is_empty() {
            return Err(Error::Corrupt);
        }
        let ci = c as usize;
        if self.counts[ci].is_empty() {
            self.counts[ci] = vec![0; PER_CLASS * STRIDE];
        }
        let counts = &mut self.counts[ci];
        let base = (ci * PER_CLASS) as u16;
        // Layout.
        let w = &mut self.layout;
        let blocks = &f.stream.blocks;
        w.gamma(blocks.len() as u32);
        let std_last = blocks.iter().enumerate().all(|(i, b)| b.last == (i + 1 == blocks.len()));
        w.bits(1, std_last as u32);
        let mut prev_count = 0usize;
        for (i, b) in blocks.iter().enumerate() {
            if !std_last {
                w.bits(1, b.last as u32);
            }
            w.bits(2, b.body.btype() as u32);
            match b.body {
                Body::Stored { pad, len } => {
                    w.bits(8, pad as u32);
                    w.bits(16, len as u32);
                }
                _ => {
                    let count = f.starts[i + 1] - f.starts[i];
                    if count == prev_count {
                        w.bits(1, 1);
                    } else {
                        w.bits(1, 0);
                        w.gamma(count as u32);
                    }
                    prev_count = count;
                }
            }
        }
        w.bits(8, f.stream.tail_pad as u32);
        let mut last = 0usize;
        let alts: Vec<usize> = f
            .tokens
            .iter()
            .enumerate()
            .filter(|(_, t)| matches!(t, Token::Match { alt258: true, .. }))
            .map(|(i, _)| i)
            .collect();
        w.gamma(alts.len() as u32);
        for &i in &alts {
            w.gamma((i - last) as u32);
            last = i;
        }
        // Headers.
        for (b, (lf, df)) in blocks.iter().zip(&f.stats.freqs) {
            if let Body::Dynamic { header: h, .. } = &b.body {
                header::write(&mut self.headers, lf, df, h, &mut self.recipe);
            }
        }
        // Tokens, with stored blocks' bytes as literals.
        let d = &f.data;
        let mut p = 0usize;
        let mut skips = f.skips.iter().peekable();
        self.syms.reserve(f.tokens.len() + 2 * f.skips.len());
        let lit = |p: usize, counts: &mut Vec<u32>, syms: &mut Vec<(u16, u16)>| {
            let prev = if p > 0 { d[p - 1] } else { 0 };
            let t = prev as usize;
            counts[t * STRIDE + d[p] as usize] += 1;
            syms.push((base + t as u16, d[p] as u16));
        };
        for (k, t) in f.tokens.iter().enumerate() {
            while let Some(&&(at, len)) = skips.peek()
                && at == k
            {
                for q in p..p + len {
                    lit(q, counts, &mut self.syms);
                }
                p += len;
                skips.next();
            }
            match *t {
                Token::Lit(_) => {
                    lit(p, counts, &mut self.syms);
                    p += 1;
                }
                Token::Match { len, dist, .. } => {
                    let prev = if p > 0 { d[p - 1] } else { 0 } as usize;
                    let lc = LEN_CODE[len as usize] as usize;
                    counts[prev * STRIDE + 256 + lc] += 1;
                    self.syms.push((base + prev as u16, (256 + lc) as u16));
                    self.raw.bits(LEN_EXTRA[lc] as u32, (len - LEN_BASE[lc]) as u32);
                    self.raw_bits[ci] += (LEN_EXTRA[lc] + DIST_EXTRA[dist_code(dist as u32)]) as u64;
                    let dc = dist_code(dist as u32);
                    let dt = LIT_CTX + lc.min(DIST_CTX - 1);
                    counts[dt * STRIDE + dc] += 1;
                    self.syms.push((base + dt as u16, dc as u16));
                    self.raw.bits(DIST_EXTRA[dc] as u32, (dist - DIST_BASE[dc]) as u32);
                    p += len as usize;
                }
            }
        }
        for &(_, len) in skips {
            for q in p..p + len {
                lit(q, counts, &mut self.syms);
            }
            p += len;
        }
        debug_assert_eq!(p, d.len());
        Ok(())
    }

    /// The coded streams: tables, rANS, raw bits, layout, headers (each with its length);
    /// and the estimated bytes of each class (tables, symbols and raw bits).
    pub fn finish(self) -> (Vec<u8>, [f64; CLASSES.len()]) {
        let mut sizes = [0f64; CLASSES.len()];
        let mut tw = BitW::default();
        // Per class: unused, or the literal resolution, then the tables in use.
        // Fine table -> table actually coded.
        let mut remap = vec![u16::MAX; CLASSES.len() * PER_CLASS];
        let mut freqs: Vec<Vec<u32>> = vec![];
        for (ci, counts) in self.counts.iter().enumerate() {
            if counts.is_empty() {
                tw.bits(1, 0);
                continue;
            }
            tw.bits(1, 1);
            let map = bucket_map(CLASSES[ci]);
            // The cheapest resolution, data plus (estimated) tables.
            let lit_counts = |res: u8| -> Vec<Vec<u32>> {
                let mut v = vec![vec![0u32; LIT_ALPHA]; res_tables(res)];
                for b in 0..LIT_CTX {
                    let t = lit_ctx(map, res, b as u8);
                    for s in 0..LIT_ALPHA {
                        v[t][s] += counts[b * STRIDE + s];
                    }
                }
                v
            };
            let est = |v: &[Vec<u32>]| -> f64 {
                v.iter()
                    .filter(|c| c.iter().any(|&x| x > 0))
                    .map(|c| rans::cost(c) + 6.0 * c.iter().filter(|&&x| x > 0).count() as f64 + 16.0)
                    .sum()
            };
            let mut best = (f64::INFINITY, 0u8, vec![]);
            for res in 0..RESOLUTIONS as u8 {
                let v = lit_counts(res);
                let e = est(&v);
                if e < best.0 {
                    best = (e, res, v);
                }
            }
            let (bits, res, lits) = best;
            sizes[ci] = (bits + self.raw_bits[ci] as f64) / 8.0;
            for k in 0..DIST_CTX {
                let t = LIT_CTX + k;
                sizes[ci] += rans::cost(&counts[t * STRIDE..t * STRIDE + DIST_ALPHA]) / 8.0;
            }
            tw.bits(2, res as u32);
            let mut code = |c: &[u32], tw: &mut BitW| -> Option<u16> {
                let used = c.iter().any(|&x| x > 0);
                tw.bits(1, used as u32);
                used.then(|| {
                    freqs.push(rans::write_table(tw, c));
                    (freqs.len() - 1) as u16
                })
            };
            let lit_ids: Vec<Option<u16>> = lits.iter().map(|c| code(c, &mut tw)).collect();
            for b in 0..LIT_CTX {
                if let Some(id) = lit_ids[lit_ctx(map, res, b as u8)] {
                    remap[ci * PER_CLASS + b] = id;
                }
            }
            for k in 0..DIST_CTX {
                let t = LIT_CTX + k;
                if let Some(id) = code(&counts[t * STRIDE..t * STRIDE + DIST_ALPHA], &mut tw) {
                    remap[ci * PER_CLASS + t] = id;
                }
            }
        }
        let syms: Vec<(u16, u16)> = self.syms.iter().map(|&(t, s)| (remap[t as usize], s)).collect();
        let st = rans::encode(&syms, &freqs, STRIDE);
        let tb = tw.finish();
        let raw = self.raw.finish();
        let layout = self.layout.finish();
        let mut hw = self.headers;
        let pad = hw.pad_len();
        hw.bits(pad, 0);
        let headers = hw.finish();
        let mut out = vec![];
        for part in [&tb, &st, &raw, &layout, &headers] {
            crate::varint::put(&mut out, part.len() as u64);
        }
        for part in [&tb, &st, &raw, &layout, &headers] {
            out.extend_from_slice(part);
        }
        (out, sizes)
    }
}

/// The decoder of a file's token streams, in the order they were added.
pub struct DecoderState<'a> {
    tables: Tables,
    /// Per class: literal table of each byte before, then the distance tables.
    ctx: Vec<[u16; PER_CLASS]>,
    dec: Decoder<'a>,
    raw: BitR<'a>,
    layout: BitR<'a>,
    headers: BitReader<'a>,
    recipe: Recipe,
}

impl<'a> DecoderState<'a> {
    pub fn new(src: &'a [u8]) -> Result<DecoderState<'a>> {
        let mut r = crate::varint::Reader::new(src);
        let mut lens = [0usize; 5];
        for l in &mut lens {
            *l = r.usize(src.len())?;
        }
        let parts: Vec<&[u8]> = lens.iter().map(|&n| r.bytes(n)).collect::<Result<_>>()?;
        let mut tr = BitR::new(parts[0]);
        let mut freqs: Vec<Vec<u32>> = vec![];
        let mut ctx = vec![[0u16; PER_CLASS]; CLASSES.len()];
        let mut read = |tr: &mut BitR, n: usize| -> Result<Option<u16>> {
            if tr.bits(1) == 0 {
                return Ok(None);
            }
            freqs.push(rans::read_table(tr, n)?);
            Ok(Some((freqs.len() - 1) as u16))
        };
        // Unused contexts point at a table of their own (decoding them is an error later).
        let mut unused = vec![];
        for (ci, cx) in ctx.iter_mut().enumerate() {
            if tr.bits(1) == 0 {
                unused.push(ci);
                continue;
            }
            let res = tr.bits(2) as u8;
            let map = bucket_map(CLASSES[ci]);
            let ids: Vec<Option<u16>> = (0..res_tables(res)).map(|_| read(&mut tr, LIT_ALPHA)).collect::<Result<_>>()?;
            for b in 0..LIT_CTX {
                cx[b] = ids[lit_ctx(map, res, b as u8)].unwrap_or(u16::MAX);
            }
            for k in 0..DIST_CTX {
                cx[LIT_CTX + k] = read(&mut tr, DIST_ALPHA)?.unwrap_or(u16::MAX);
            }
            if tr.overrun() {
                return Err(Error::Corrupt);
            }
        }
        let dummy = freqs.len() as u16;
        freqs.push(vec![rans::PROB_SCALE]);
        for cx in ctx.iter_mut() {
            for t in cx.iter_mut() {
                if *t == u16::MAX {
                    *t = dummy;
                }
            }
        }
        for ci in unused {
            ctx[ci] = [dummy; PER_CLASS];
        }
        Ok(DecoderState {
            tables: Tables::new(&freqs, STRIDE),
            ctx,
            dec: Decoder::new(parts[1]),
            raw: BitR::new(parts[2]),
            layout: BitR::new(parts[3]),
            headers: BitReader::new(parts[4]),
            recipe: Recipe::default(),
        })
    }

    /// Rebuilds the next stream (class `c`, `plain` bytes of data): returns the DEFLATE
    /// bytes and the data.
    pub fn next(&mut self, c: Class, plain: usize) -> Result<(Vec<u8>, Vec<u8>)> {
        let l = &mut self.layout;
        let nblocks = l.gamma()? as usize;
        if nblocks > plain + 1 {
            return Err(Error::Corrupt);
        }
        let std_last = l.bits(1) == 1;
        // Per block: stored length, or token count.
        let mut blocks = Vec::with_capacity(nblocks);
        let mut spans = Vec::with_capacity(nblocks);
        let mut prev_count = 0usize;
        let mut ntok = 0usize;
        for i in 0..nblocks {
            let last = if std_last { i + 1 == nblocks } else { l.bits(1) == 1 };
            let body = match l.bits(2) {
                0 => {
                    let pad = l.bits(8) as u8;
                    let len = l.bits(16) as u16;
                    spans.push((true, len as usize));
                    Body::Stored { pad, len }
                }
                t @ (1 | 2) => {
                    let count = if l.bits(1) == 1 { prev_count } else { l.gamma()? as usize };
                    if count > plain {
                        return Err(Error::Corrupt);
                    }
                    prev_count = count;
                    ntok += count;
                    spans.push((false, count));
                    if t == 1 {
                        Body::Fixed { tokens: vec![] }
                    } else {
                        Body::Dynamic {
                            header: DynHeader::default(),
                            tokens: vec![],
                        }
                    }
                }
                _ => return Err(Error::Corrupt),
            };
            blocks.push(Block { last, body });
        }
        let tail_pad = l.bits(8) as u8;
        let nalts = l.gamma()? as usize;
        if nalts > ntok {
            return Err(Error::Corrupt);
        }
        let mut alts = Vec::with_capacity(nalts);
        let mut last = 0usize;
        for _ in 0..nalts {
            last += l.gamma()? as usize;
            alts.push(last);
        }
        if l.overrun() || ntok > plain {
            return Err(Error::Corrupt);
        }

        let cx = &self.ctx[c as usize];
        let tb = &self.tables;
        let mut dec = self.dec;
        let raw = &mut self.raw;
        let mut data = vec![0u8; plain + SLACK];
        let mut tokens: Vec<Token> = Vec::with_capacity(ntok);
        let mut freqs = Vec::with_capacity(nblocks);
        let mut o = 0usize;
        for &(stored, n) in &spans {
            if stored {
                if o + n > plain {
                    return Err(Error::Corrupt);
                }
                for _ in 0..n {
                    let prev = if o > 0 { data[o - 1] } else { 0 };
                    let s = dec.get(tb, cx[prev as usize] as usize);
                    if s >= 256 {
                        return Err(Error::Corrupt);
                    }
                    data[o] = s as u8;
                    o += 1;
                }
                freqs.push(([0u32; 288], [0u32; 32]));
                continue;
            }
            let mut lf = [0u32; 288];
            let mut df = [0u32; 32];
            lf[256] = 1;
            // The tokens go straight into the buffer (counted against `ntok` above).
            let tp = tokens.as_mut_ptr();
            let mut ti = tokens.len();
            let mut prev = if o > 0 { data[o - 1] } else { 0 };
            for _ in 0..n {
                let s = dec.get(tb, cx[prev as usize] as usize) as usize;
                let tok = if s < 256 {
                    if o >= plain {
                        return Err(Error::Corrupt);
                    }
                    data[o] = s as u8;
                    o += 1;
                    lf[s] += 1;
                    prev = s as u8;
                    Token::Lit(s as u8)
                } else {
                    let lc = s - 256;
                    let len = LEN_BASE[lc] as usize + raw.bits(LEN_EXTRA[lc] as u32) as usize;
                    let dc = dec.get(tb, cx[LIT_CTX + lc.min(DIST_CTX - 1)] as usize) as usize;
                    if dc >= DIST_ALPHA || len > 258 {
                        return Err(Error::Corrupt);
                    }
                    let dist = DIST_BASE[dc] as usize + raw.bits(DIST_EXTRA[dc] as u32) as usize;
                    if dist > o || o + len > plain {
                        return Err(Error::Corrupt);
                    }
                    copy_match(&mut data, o, dist, len);
                    o += len;
                    prev = data[o - 1];
                    lf[257 + lc] += 1;
                    df[dc] += 1;
                    Token::Match {
                        len: len as u16,
                        dist: dist as u16,
                        alt258: false,
                    }
                };
                // SAFETY: the blocks' token counts sum to `ntok`, the capacity.
                unsafe { tp.add(ti).write(tok) };
                ti += 1;
            }
            // SAFETY: the first `ti` tokens were written.
            unsafe { tokens.set_len(ti) };
            freqs.push((lf, df));
        }
        self.dec = dec;
        if o != plain || raw.overrun() {
            return Err(Error::Corrupt);
        }
        data.truncate(plain);
        let mut starts = Vec::with_capacity(nblocks + 1);
        let mut ti = 0;
        for &(stored, n) in &spans {
            starts.push(ti);
            if !stored {
                ti += n;
            }
        }
        starts.push(ti);
        for &i in &alts {
            match tokens.get_mut(i) {
                Some(Token::Match { len: 258, alt258, .. }) => *alt258 = true,
                _ => return Err(Error::Corrupt),
            }
            let b = starts.partition_point(|&s| s <= i) - 1;
            freqs[b].0[285] -= 1;
            freqs[b].0[284] += 1;
        }
        for (b, (lf, df)) in blocks.iter_mut().zip(&freqs) {
            if let Body::Dynamic { header, .. } = &mut b.body {
                *header = header::read_freqs(&mut self.headers, lf, df, &mut self.recipe).map_err(|_| Error::Corrupt)?;
            }
        }
        let stream = Stream {
            blocks,
            tail_pad,
            trailing: vec![],
        };
        let out = flate_recons::fast::write::write(&stream, &starts, &tokens, &data).map_err(|_| Error::Corrupt)?;
        Ok((out, data))
    }

    /// Whether every stream was consumed exactly.
    pub fn finished(&self) -> bool {
        self.dec.finished()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes() {
        for d in 1..=32768u32 {
            let c = dist_code(d);
            assert!(DIST_BASE[c] as u32 <= d && (c == 29 || d < DIST_BASE[c + 1] as u32), "{d}");
        }
        for l in 3..=258u16 {
            let c = LEN_CODE[l as usize] as usize;
            assert!(LEN_BASE[c] <= l && (c == 28 || l < LEN_BASE[c + 1]) || l == 258 && c == 28, "{l}");
        }
    }

    #[test]
    fn roundtrip() {
        let mut text = vec![];
        for i in 0..20000 {
            text.extend_from_slice(format!("{} {} Td ({}) Tj\n", i % 97, i * 7 % 13, i * i % 1000).as_bytes());
        }
        let z = zstd::bulk::compress(&text, 1).unwrap();
        let _ = z;
        let raw = flate_zoo_like(&text);
        let f = flate_recons::fast::inflate::parse(&raw).unwrap();
        let mut e = Encoder::default();
        e.add(Class::Content, &f).unwrap();
        e.add(Class::Font, &f).unwrap();
        let out = e.finish().0;
        let mut d = DecoderState::new(&out).unwrap();
        assert_eq!(d.next(Class::Content, text.len()).unwrap().0, raw);
        assert_eq!(d.next(Class::Font, text.len()).unwrap().0, raw);
        assert!(d.finished());
    }

    /// A DEFLATE stream with fixed and stored blocks, built by hand from greedy matches.
    fn flate_zoo_like(data: &[u8]) -> Vec<u8> {
        let mut st = Stream::default();
        let mut tokens = vec![];
        let mut i = 0;
        let mut last: std::collections::HashMap<&[u8], usize> = Default::default();
        while i < data.len() {
            let key = &data[i..(i + 4).min(data.len())];
            let m = last.get(key).copied().filter(|&j| i - j <= 32768 && key.len() == 4);
            last.insert(key, i);
            if let Some(j) = m {
                let mut l = 0;
                while i + l < data.len() && l < 258 && data[j + l] == data[i + l] {
                    l += 1;
                }
                tokens.push(Token::Match {
                    len: l as u16,
                    dist: (i - j) as u16,
                    alt258: false,
                });
                i += l;
            } else {
                tokens.push(Token::Lit(data[i]));
                i += 1;
            }
        }
        st.blocks.push(Block {
            last: false,
            body: Body::Stored { pad: 0, len: 0 },
        });
        st.blocks.push(Block {
            last: true,
            body: Body::Fixed { tokens },
        });
        flate_recons::deflate::write(&st, data).unwrap()
    }
}
