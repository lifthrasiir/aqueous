//! The token model: the DEFLATE stream's own LZ77 tokens, coded against the image.
//!
//! The parse is kept as it is (so rebuilding the stream needs no match search), but every
//! symbol is coded with static per-file tables chosen by image context:
//! - a literal is the residual of the raw (unfiltered) byte against aqr-raw's pixel model
//!   ([`aqr_raw::image::pixel`]: MED, with the previous channel's residual added for green
//!   and blue); its table is chosen by channel, local activity (raw gradients and the
//!   filtered neighbours' magnitudes) and whether a match came just before. The row's PNG filter is reapplied to give the
//!   literal byte. Filter bytes have a table of their own;
//! - a match starts as the literal escape symbol (255, then a binary decision against a
//!   literal residual of 255), and has its length coded by activity and its distance by
//!   length, with the extra bits raw.
//!
//! Decoding runs the model byte by byte. Encoding sees the whole data up front: images
//! whose stream expands little (mostly literals) get every pixel byte's symbol computed
//! row by row (with NEON where available), then the tokens walked; the others are modelled
//! as the parser hands over the tokens.
//!
//! Buffers: the filtered data has `Geometry::pad` zero bytes in front, so that the bytes
//! one row up and one pixel left are always in bounds. The raw (unfiltered) image is kept
//! in its own layout: every pass has a zero row on top and every row `bpp` zero bytes on
//! the left, which is exactly what the PNG filters assume outside the image.

use crate::filter;
use crate::rans::{self, BitR, BitW, Decoder, Tables};
use aqr_flate::deflate::Token;
use aqr_flate::huffman::{DIST_BASE, DIST_EXTRA, LEN_BASE, LEN_EXTRA};
use aqr_flate::inflate::{self, Sink};
use aqr_raw::image::pixel::{self, ACT, bucket, get, mag, med, put, unzigzag, zigzag};

#[derive(Debug, Clone, Copy)]
pub struct Pass {
    /// Offset of the first row's filter byte in the data.
    pub start: usize,
    pub rows: usize,
    /// Row size in the data, the filter byte included.
    pub rs: usize,
    /// Offset of the zero row in the raw buffer, and the raw row size (`bpp + rs - 1`).
    pub base2: usize,
    pub rs2: usize,
}

/// How the decompressed data is laid out: contiguous passes of rows.
#[derive(Debug, Clone)]
pub struct Geometry {
    pub passes: Vec<Pass>,
    /// Bytes per pixel for the filters (at least 1).
    pub bpp: usize,
    /// Total bytes the passes cover.
    pub len: usize,
    /// Size of the raw buffer.
    pub raw_len: usize,
    /// Zero bytes in front of the data.
    pub pad: usize,
}

impl Geometry {
    pub fn new(ih: &crate::png::Ihdr) -> Option<Geometry> {
        let bits = ih.channels() * ih.depth as usize;
        if bits == 0 || ih.width == 0 || ih.height == 0 {
            return None;
        }
        let bpp = bits.div_ceil(8);
        let (w, h) = (ih.width as u64, ih.height as u64);
        let list: &[(u64, u64, u64, u64)] = if ih.interlace == 1 {
            &[(0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8), (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2)]
        } else {
            &[(0, 0, 1, 1)]
        };
        let mut passes = vec![];
        // The raw buffer starts with a margin: the channel decorrelation reads the byte
        // before the first pixel's neighbours.
        let (mut at, mut at2, mut pad) = (0usize, 8usize, 0usize);
        for &(x0, y0, dx, dy) in list {
            let pw = if w > x0 { (w - x0).div_ceil(dx) } else { 0 };
            let ph = if h > y0 { (h - y0).div_ceil(dy) } else { 0 };
            if pw == 0 || ph == 0 {
                continue;
            }
            let rs = (pw as usize).checked_mul(bits)?.div_ceil(8) + 1;
            let rs2 = bpp + rs - 1;
            let rows = ph as usize;
            passes.push(Pass { start: at, rows, rs, base2: at2, rs2 });
            at = at.checked_add(rs.checked_mul(rows)?)?;
            at2 = at2.checked_add(rs2.checked_mul(rows + 1)?)?;
            pad = pad.max(rs + bpp);
        }
        // Far beyond any PNG a decoder would accept, and keeps the arithmetic in range.
        if at > 1 << 40 {
            return None;
        }
        Some(Geometry { passes, bpp, len: at, raw_len: at2, pad: pad + 8 })
    }
}

/// What literals are coded against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// 8-bit samples (gray, gray+alpha, RGB, RGBA): raw-domain prediction.
    Image,
    /// Anything else: the filtered bytes themselves.
    Bytes,
}

// Table layout. Literals and the start of matches share an alphabet: the symbol 255 is an
// escape, followed by a binary decision (match, or literal residual 255).
const T_LIT: usize = 0;
/// Literal tables by (previous token a match, channel, activity).
const T_FILTER: usize = T_LIT + 8 * ACT;
const T_ESC: usize = T_FILTER + 1;
const T_LEN: usize = T_ESC + 1;
const T_DIST: usize = T_LEN + ACT + 1;
pub const TABLES: usize = T_DIST + 6;
/// Records coded against a reference ([`Reference`]) have more tables: whether a token is the
/// reference's at the same place, by whether the last such decision was yes and whether the
/// reference's token is a match.
const T_SAME: usize = TABLES;
pub const TABLES_REF: usize = T_SAME + 4;
// Symbols are kept as (table, value) bytes.
const _: () = assert!(TABLES_REF <= 256);
const ESC: u32 = 255;

fn alphabet(t: usize) -> usize {
    if t < T_ESC {
        256
    } else if t == T_ESC {
        2
    } else if t < T_DIST {
        29
    } else if t < T_SAME {
        30
    } else {
        2
    }
}

/// The tokens of a similar image's data (the *reference*: of the same geometry, such as a
/// variant of the picture with a layer shown or hidden), by where they start in the data.
/// Where both images have the same data, encoders (of the same writer) mostly make the same
/// parse, so that a token is often the reference's at the same place.
pub struct Reference {
    /// Positions, increasing, and the tokens starting there (matches without `alt258`).
    pos: Vec<u32>,
    tokens: Vec<Token>,
}

impl Reference {
    /// The reference of the image of payload `payload`: its zlib streams as [`encode`] finds
    /// them. Fails where it would.
    pub fn parse(payload: &[u8]) -> crate::Result<Reference> {
        if payload.len() < 2 {
            return Err(crate::Error::Unsupported("no zlib stream"));
        }
        let mut sink = RefSink { p: 0, r: Reference { pos: vec![], tokens: vec![] } };
        let mut o = 0;
        loop {
            let l = inflate::parse_head(&payload[o + 2..], usize::MAX, &mut sink)?;
            match next_stream(payload, o + 2 + l.end).filter(|&next| parses(&payload[next + 2..])) {
                Some(next) => o = next,
                None => break,
            }
        }
        if sink.p > u32::MAX as usize {
            return Err(crate::Error::Unsupported("image data of 4 GiB or more"));
        }
        Ok(sink.r)
    }
}

/// The parser's sink for a [`Reference`]: the position, and the tokens so far.
struct RefSink {
    p: usize,
    r: Reference,
}

impl Sink for RefSink {
    fn lit(&mut self, b: u8) {
        self.r.pos.push(self.p as u32);
        self.r.tokens.push(Token::Lit(b));
        self.p += 1;
    }
    fn mat(&mut self, len: u16, dist: u16, _: bool) {
        self.r.pos.push(self.p as u32);
        self.r.tokens.push(Token::Match { len, dist, alt258: false });
        self.p += len as usize;
    }
    fn stored(&mut self, b: &[u8]) {
        self.p += b.len();
    }
}

/// Where coding against a reference is: its next token, and whether the last decision was
/// that a token is the reference's.
struct Follow<'r> {
    r: &'r Reference,
    i: usize,
    same: usize,
}

impl<'r> Follow<'r> {
    fn new(r: &'r Reference) -> Follow<'r> {
        Follow { r, i: 0, same: 0 }
    }

    /// The reference's token starting at position `p` (positions only grow), if one does,
    /// and the table of the decision whether the token there is the same.
    #[inline(always)]
    fn at(&mut self, p: usize) -> Option<(Token, usize)> {
        let pos = &self.r.pos;
        while self.i < pos.len() && (pos[self.i] as usize) < p {
            self.i += 1;
        }
        let t = *self.r.tokens.get(self.i).filter(|_| pos[self.i] as usize == p)?;
        Some((t, T_SAME + 2 * self.same + matches!(t, Token::Match { .. }) as usize))
    }
}

/// One side of the coder.
pub trait Io {
    /// Codes symbol `v` with table `t` (encoder), or decodes one (decoder).
    fn sym(&mut self, t: usize, v: u32) -> u32;
    /// The same for a binary table.
    fn bit(&mut self, t: usize, v: u32) -> u32;
    /// Codes `k` raw bits.
    fn raw(&mut self, k: u32, v: u32) -> u32;
}

/// Symbol lists of at least this length are counted by several threads.
const PAR_HIST_MIN: usize = 1 << 20;

pub struct Recorder {
    pub syms: Vec<(u8, u8)>,

    pub raw: BitW,
    /// The tables there are ([`TABLES`], or [`TABLES_REF`] against a reference).
    pub tables: usize,
}

impl Recorder {
    pub fn new(cap: usize) -> Recorder {
        Recorder { syms: Vec::with_capacity(cap), raw: BitW::default(), tables: TABLES }
    }

    /// Tables, then the rANS stream, then the raw bits.
    pub fn finish(self, out: &mut Vec<u8>) {
        self.finish_threads(out, 1)
    }

    /// [`Recorder::finish`] counting the symbols on up to `threads` threads (the counts
    /// are sums, so the output is the same).
    pub fn finish_threads(self, out: &mut Vec<u8>, threads: usize) {
        let mut w = BitW::default();
        let tables = self.tables;
        // A bitmap of the tables in use, then their levels.
        let mut h = vec![0u32; tables * 256];
        let count = |syms: &[(u8, u8)], h: &mut [u32]| {
            for &(t, v) in syms {
                h[t as usize * 256 + v as usize] += 1;
            }
        };
        if threads > 1 && self.syms.len() >= PAR_HIST_MIN {
            let chunk = self.syms.len().div_ceil(threads);
            let parts: Vec<Vec<u32>> = std::thread::scope(|sc| {
                let hs: Vec<_> = self
                    .syms
                    .chunks(chunk)
                    .map(|c| {
                        sc.spawn(move || {
                            let mut h = vec![0u32; tables * 256];
                            for &(t, v) in c {
                                h[t as usize * 256 + v as usize] += 1;
                            }
                            h
                        })
                    })
                    .collect();
                hs.into_iter().map(|h| h.join().unwrap_or_else(|e| std::panic::resume_unwind(e))).collect()
            });
            for p in &parts {
                for (a, b) in h.iter_mut().zip(p) {
                    *a += b;
                }
            }
        } else {
            count(&self.syms, &mut h);
        }
        let counts: Vec<&[u32]> = (0..tables).map(|t| &h[t * 256..t * 256 + alphabet(t)]).collect();
        let used: Vec<bool> = counts.iter().map(|c| c.iter().any(|&x| x > 0)).collect();
        for &u in &used {
            w.bits(1, u as u32);
        }
        let freqs: Vec<Vec<u32>> = (0..tables)
            .map(|t| if used[t] { rans::write_table(&mut w, counts[t]) } else { vec![rans::PROB_SCALE] })
            .collect();
        let tb = w.finish();
        let Recorder { syms, raw, .. } = self;
        let st = rans::encode(&syms, &freqs);
        // The symbols are the largest part by far: gone before the record is copied.
        drop(syms);
        let raw = raw.finish();
        out.reserve(tb.len() + st.len() + raw.len() + 30);
        for part in [&tb, &st, &raw] {
            aqr_common::varint::put(out, part.len() as u64);
        }
        out.extend_from_slice(&tb);
        out.extend_from_slice(&st);
        out.extend_from_slice(&raw);
    }
}

impl Io for Recorder {
    #[inline(always)]
    fn sym(&mut self, t: usize, v: u32) -> u32 {
        self.syms.push((t as u8, v as u8));
        v
    }
    #[inline(always)]
    fn bit(&mut self, t: usize, v: u32) -> u32 {
        self.sym(t, v)
    }
    #[inline(always)]
    fn raw(&mut self, k: u32, v: u32) -> u32 {
        self.raw.bits(k, v);
        v
    }
}

pub struct Player<'a> {
    tables: Tables,
    dec: Decoder<'a>,
    raw: BitR<'a>,
}

impl<'a> Player<'a> {
    /// The model payload at `at` of `src`, with `tables` tables ([`TABLES`] or
    /// [`TABLES_REF`]).
    pub fn new(src: &'a [u8], at: &mut usize, tables: usize) -> crate::Result<Player<'a>> {
        let mut lens = [0usize; 3];
        for l in &mut lens {
            *l = crate::codec::get_varint(src, at)? as usize;
        }
        let mut part = |n: usize| -> crate::Result<&'a [u8]> {
            let s = src.get(*at..at.checked_add(n).ok_or_else(crate::corrupt)?).ok_or_else(crate::corrupt)?;
            *at += n;
            Ok(s)
        };
        let (tb, st, raw) = (part(lens[0])?, part(lens[1])?, part(lens[2])?);
        let mut r = BitR::new(tb);
        let used: Vec<bool> = (0..tables).map(|_| r.bits(1) == 1).collect();
        let mut freqs = Vec::with_capacity(tables);
        for (t, &u) in used.iter().enumerate() {
            freqs.push(if u { rans::read_table(&mut r, alphabet(t))? } else { vec![] });
        }
        if r.overrun() {
            return Err(crate::corrupt());
        }
        Ok(Player { tables: Tables::new(&freqs), dec: Decoder::new(st), raw: BitR::new(raw) })
    }
}

/// The blocks as the token coder sees them: stored bytes or a number of tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Span {
    Stored(usize),
    Tokens(usize),
}

/// The row containing the current position, followed as positions grow.
#[derive(Clone)]
struct Cursor {
    pass: usize,
    /// End of the pass in the data.
    pend: usize,
    /// The row's filter byte and end in the data.
    rstart: usize,
    rend: usize,
    rs: usize,
    /// Raw offset of the row's first pixel byte, and the raw row size.
    q0: usize,
    rs2: usize,
    /// The raw offset the raw buffer starts at (see [`St::slide`]).
    rbase: usize,
}

impl Cursor {
    fn new(g: &Geometry) -> Cursor {
        let mut c = Cursor { pass: usize::MAX, pend: 0, rstart: 0, rend: 0, rs: 0, q0: 0, rs2: 0, rbase: 0 };
        c.enter(g, 0);
        c
    }

    /// Starts pass `i` at the current `rend`.
    fn enter(&mut self, g: &Geometry, i: usize) {
        self.pass = i;
        self.rstart = self.rend;
        match g.passes.get(i) {
            Some(ps) => {
                self.pend = ps.start + ps.rows * ps.rs;
                self.rs = ps.rs;
                self.rs2 = ps.rs2;
                self.q0 = ps.base2 + ps.rs2 + g.bpp - self.rbase;
                self.rend = self.rstart + ps.rs;
            }
            None => {
                // Past the image: one endless "row" whose bytes all count as filter bytes.
                self.pend = usize::MAX;
                self.rs = usize::MAX;
                self.rend = usize::MAX;
            }
        }
    }

    #[inline(always)]
    fn seek(&mut self, g: &Geometry, p: usize) {
        while p >= self.rend {
            if self.rend >= self.pend {
                self.enter(g, self.pass + 1);
            } else {
                self.rstart = self.rend;
                self.rend += self.rs;
                self.q0 += self.rs2;
            }
        }
    }
}

/// Unfilters data positions `from..to` (both within the image) into `raw`.
fn unfilter_range(g: &Geometry, buf: &[u8], raw: &mut [u8], from: usize, to: usize) {
    let bpp = g.bpp;
    let mut p = from;
    while p < to {
        // Passes are contiguous from 0.
        let ps = *g.passes.iter().rev().find(|ps| ps.start <= p).unwrap();
        let row = (p - ps.start) / ps.rs;
        let rstart = ps.start + row * ps.rs;
        if p == rstart {
            p += 1;
            continue;
        }
        let x0 = p - rstart - 1;
        let x1 = to.min(rstart + ps.rs) - rstart - 1;
        let q = ps.base2 + (row + 1) * ps.rs2 + bpp;
        let d = &buf[g.pad + rstart + 1..g.pad + rstart + ps.rs];
        unfilter_part::<0>(buf[g.pad + rstart], d, raw, q, ps.rs2, x0, x1, bpp);
        p = rstart + 1 + x1;
    }
}

/// Unfilters bytes `x0..x1` of a row (filter `ft`, filtered bytes `d`) into `raw` at `q`.
/// `BPP` is the pixel size, or 0 to take `bpp`.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn unfilter_part<const BPP: usize>(
    ft: u8,
    d: &[u8],
    raw: &mut [u8],
    q: usize,
    rs2: usize,
    x0: usize,
    x1: usize,
    bpp: usize,
) {
    if BPP == 0 {
        return unfilter_bytes(ft, d, raw, q, rs2, x0, x1, bpp);
    }
    if x0 >= x1 {
        return;
    }
    // The row with its left padding, and the row above with its own (adjacent in `raw`):
    // byte `x` is at `BPP + x` in both.
    let (head, tail) = raw.split_at_mut(q - BPP);
    let w = rs2 - BPP;
    let up = &head[q - rs2 - BPP..q - BPP];
    let row = &mut tail[..BPP + w];
    match ft {
        2 => {
            for ((o, &f), &u) in row[BPP + x0..BPP + x1].iter_mut().zip(&d[x0..x1]).zip(&up[BPP + x0..BPP + x1]) {
                *o = f.wrapping_add(u);
            }
            return;
        }
        1 | 3 | 4 => {}
        _ => {
            row[BPP + x0..BPP + x1].copy_from_slice(&d[x0..x1]);
            return;
        }
    }
    // Byte by byte up to a pixel boundary, then pixel by pixel with the left (and above
    // left) pixel kept in registers, then the bytes left.
    let byte = |row: &mut [u8], x: usize| {
        let (a, b, c) = (row[x], up[BPP + x], up[x]);
        row[BPP + x] = d[x].wrapping_add(filter::predict(ft, a, b, c));
    };
    let mut x = x0;
    while x < x1 && !x.is_multiple_of(BPP) {
        byte(row, x);
        x += 1;
    }
    let px = (x1 - x) / BPP;
    if px > 0 {
        let mut a: [u8; BPP] = row[x..x + BPP].try_into().unwrap();
        let mut c: [u8; BPP] = up[x..x + BPP].try_into().unwrap();
        let rows = row[BPP + x..BPP + x + px * BPP].chunks_exact_mut(BPP);
        let ds = d[x..x + px * BPP].chunks_exact(BPP);
        let ups = up[BPP + x..BPP + x + px * BPP].chunks_exact(BPP);
        match ft {
            1 => {
                for (o, f) in rows.zip(ds) {
                    for k in 0..BPP {
                        a[k] = f[k].wrapping_add(a[k]);
                        o[k] = a[k];
                    }
                }
            }
            3 => {
                for ((o, f), u) in rows.zip(ds).zip(ups) {
                    for k in 0..BPP {
                        a[k] = f[k].wrapping_add(((a[k] as u16 + u[k] as u16) >> 1) as u8);
                        o[k] = a[k];
                    }
                }
            }
            _ => {
                for ((o, f), u) in rows.zip(ds).zip(ups) {
                    for k in 0..BPP {
                        a[k] = f[k].wrapping_add(filter::paeth(a[k], u[k], c[k]));
                        c[k] = u[k];
                        o[k] = a[k];
                    }
                }
            }
        }
        x += px * BPP;
    }
    while x < x1 {
        byte(row, x);
        x += 1;
    }
}

/// [`unfilter_part`] for any pixel size.
#[allow(clippy::too_many_arguments)]
fn unfilter_bytes(ft: u8, d: &[u8], raw: &mut [u8], q: usize, rs2: usize, x0: usize, x1: usize, bpp: usize) {
    for x in x0..x1 {
        let (a, b, c) = (raw[q + x - bpp], raw[q + x - rs2], raw[q + x - rs2 - bpp]);
        raw[q + x] = d[x].wrapping_add(filter::predict(ft, a, b, c));
    }
}

/// Unfilters the `len` bytes at `p`, where the cursor is (position `i` of the data at
/// `off + i` in `buf`).
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn unfilter_span<const BPP: usize>(
    g: &Geometry,
    cur: &Cursor,
    buf: &[u8],
    off: usize,
    raw: &mut [u8],
    p: usize,
    len: usize,
) {
    // Past the image there is nothing to unfilter; the data may end before the image.
    let end = (p + len).min(g.len);
    if p >= end {
        return;
    }
    let x = p - cur.rstart;
    if x > 0 && end <= cur.rend {
        // Within the row (the usual case).
        let at = off.wrapping_add(cur.rstart);
        let d = &buf[at + 1..at + end - cur.rstart];
        unfilter_part::<BPP>(buf[at], d, raw, cur.q0, cur.rs2, x - 1, end - cur.rstart - 1, g.bpp);
    } else {
        let mut c = cur.clone();
        let mut i = p;
        while i < end {
            c.seek(g, i);
            if i == c.rstart {
                i += 1;
                continue;
            }
            let x1 = end.min(c.rend) - c.rstart - 1;
            let at = off.wrapping_add(c.rstart);
            let d = &buf[at + 1..at + 1 + x1];
            unfilter_part::<BPP>(buf[at], d, raw, c.q0, c.rs2, i - c.rstart - 1, x1, g.bpp);
            i = c.rstart + 1 + x1;
        }
    }
}

/// Unfilters the whole image (`buf` is the padded data, `raw` `g.raw_len` zeros).
pub fn unfilter_all(g: &Geometry, buf: &[u8], raw: &mut [u8]) {
    unfilter_range(g, buf, raw, 0, g.len);
}

/// The coding position and what follows it.
struct St<'g> {
    g: &'g Geometry,
    cur: Cursor,
    /// Position in the data.
    p: usize,
    /// Offset of the literal tables after a match.
    after: usize,
    /// Tokens coded so far.
    tokens: usize,
    /// Where position `i` of the data is in the buffer: at `off + i` (wrapping), from
    /// position `base` on, after `Geometry::pad` bytes.
    off: usize,
    base: usize,
    win: Win,
}

/// The buffers' sizes, and when they are moved on (see [`St::slide`]).
#[derive(Clone, Copy)]
struct Win {
    /// Data bytes kept before the position: the farthest match and a row and a pixel.
    keep: usize,
    /// The data buffer's offset, and the raw buffer's, beyond which to move on: with room
    /// for a stored block, a row or a match, and the raw rows of as many bytes.
    blim: usize,
    rlim: usize,
    buf: usize,
    raw: usize,
}

/// Data bytes per move of the windows.
const WINDOW: usize = 1 << 20;

impl Win {
    /// The buffers of `g` for `n` bytes of data: whole, or windows of them for large data.
    fn new(g: &Geometry, n: usize, image: bool) -> Win {
        let raw_len = if image { g.raw_len } else { 0 };
        // The data, and the overshoot of a match's copy.
        let whole = Win { keep: 0, blim: usize::MAX, rlim: usize::MAX, buf: g.pad + n + 16, raw: raw_len };
        let (min, step) = window_sizes();
        if n <= min {
            return whole;
        }
        let rs = g.passes.iter().map(|ps| ps.rs).max().unwrap_or(0);
        let rs2 = g.passes.iter().map(|ps| ps.rs2).max().unwrap_or(0);
        let keep = 32768 + rs + 16;
        let room = rs.max(65536) + ROOM;
        let buf = g.pad + keep + step + room;
        // Raw offsets advance at most twice as fast as positions (`rs2 < 2 * rs`), and by a
        // zero row (and a pixel) at each pass.
        let rroom = 2 * room + 10 * (rs2 + 8) + 256;
        let rkeep = 2 * rs2 + 16;
        let raw = if image { rkeep + 2 * (keep + step) + rroom } else { 0 };
        if buf >= whole.buf || (image && raw >= raw_len) {
            return whole;
        }
        Win { keep, blim: buf - room, rlim: raw.saturating_sub(rroom), buf, raw }
    }
}

/// Data sizes above which the buffers are windows, and the windows' steps: `WINDOW`, or
/// as `PNGR_WINDOW` sets it (a step, for any size; for tests).
fn window_sizes() -> (usize, usize) {
    static SIZES: std::sync::OnceLock<(usize, usize)> = std::sync::OnceLock::new();
    *SIZES.get_or_init(|| match std::env::var("PNGR_WINDOW").ok().and_then(|s| s.parse().ok()) {
        Some(step) => (0, step),
        None => (4 * WINDOW, WINDOW),
    })
}

impl<'g> St<'g> {
    fn new(g: &'g Geometry, win: Win) -> St<'g> {
        St { g, cur: Cursor::new(g), p: 0, after: 0, tokens: 0, off: g.pad, base: 0, win }
    }

    /// Where position `p` is in the buffer.
    #[inline(always)]
    fn at(&self, p: usize) -> usize {
        self.off.wrapping_add(p)
    }

    /// Moves the windows on if the next token (a stored block, a row's literals or a
    /// match) might not fit; the cursor is at the position.
    #[inline(always)]
    fn room(&mut self, buf: &mut [u8], raw: &mut [u8]) {
        if self.at(self.p) > self.win.blim || self.cur.q0 > self.win.rlim && self.cur.rs != usize::MAX {
            self.slide(buf, raw);
        }
    }

    /// Moves the data so that what the model may still read (the farthest match and the
    /// row above) starts the buffer, after its padding; and the raw rows from the one above
    /// the cursor's on, after a margin. The rest of the raw buffer is zeroed (the zero row
    /// and left margins the rows to come assume).
    #[cold]
    #[inline(never)]
    fn slide(&mut self, buf: &mut [u8], raw: &mut [u8]) {
        let pad = self.g.pad;
        let from = self.p.saturating_sub(self.win.keep).max(self.base);
        let src = self.at(from) - pad;
        buf.copy_within(src..self.at(self.p), 0);
        self.base = from;
        self.off = pad.wrapping_sub(from);
        let c = &mut self.cur;
        if c.rs != usize::MAX && !raw.is_empty() {
            // The row above (and the byte before it), and the row so far.
            let src = c.q0 - self.g.bpp - c.rs2 - 8;
            let end = c.q0 + c.rs2;
            raw.copy_within(src..end, 0);
            raw[end - src..].fill(0);
            c.q0 -= src;
            c.rbase += src;
        }
    }

    #[inline(always)]
    fn prep<const BPP: usize, const IMAGE: bool>(&mut self, raw: &[u8], buf: &[u8], token: bool) -> Lit {
        self.cur.seek(self.g, self.p);
        let bpp = if IMAGE { BPP } else { self.g.bpp };
        let mut l = prepare::<BPP, IMAGE>(&self.cur, raw, buf, self.off, self.p, bpp);
        if token && l.t < T_FILTER {
            l.t += self.after;
        }
        l
    }

    /// Copies a match in the data (and unfilters it).
    #[inline(always)]
    fn copy<const BPP: usize, const IMAGE: bool>(&mut self, raw: &mut [u8], buf: &mut [u8], len: usize, dist: usize) {
        let at = self.at(self.p);
        debug_assert!(dist <= at && at + len + 16 <= buf.len());
        // SAFETY: `1 <= dist <=` the position (checked by the callers), and at most 32768,
        // which the buffer keeps before it; room for the match and the overshoot of its
        // copy past it (`Win`: `room`, or 16 bytes past the data).
        unsafe { inflate::copy_match(buf.as_mut_ptr(), at, dist, len) };
        if IMAGE {
            unfilter_span::<BPP>(self.g, &self.cur, buf, self.off, raw, self.p, len);
        }
        self.p += len;
        self.after = 4 * ACT;
        self.tokens += 1;
    }
}

/// Codes a match's length and distance (`len`, `dist` ignored when decoding); returns
/// them, or `None` if they are invalid.
#[inline(always)]
fn code_match<I: Io>(io: &mut I, act: usize, len: u32, dist: u32) -> Option<(usize, usize)> {
    let lc = io.sym(T_LEN + act, len_code(len)) as usize;
    if lc >= 29 {
        return None;
    }
    let len = LEN_BASE[lc] as u32 + io.raw(LEN_EXTRA[lc] as u32, len.wrapping_sub(LEN_BASE[lc] as u32));
    let lb = match len {
        3 => 0,
        4 => 1,
        5 => 2,
        6..=8 => 3,
        9..=32 => 4,
        _ => 5,
    };
    let dc = io.sym(T_DIST + lb, dist_code(dist)) as usize;
    if dc >= 30 {
        return None;
    }
    let dist = DIST_BASE[dc] as u32 + io.raw(DIST_EXTRA[dc] as u32, dist.wrapping_sub(DIST_BASE[dc] as u32));
    Some((len as usize, dist as usize))
}

/// The compressor's side: parses the DEFLATE stream and records its model symbols.
struct Enc<'g, const BPP: usize, const IMAGE: bool> {
    io: Recorder,
    st: St<'g>,
    buf: Vec<u8>,
    raw: Vec<u8>,
    /// Token indices of matches of length 258 sent as 284 + 31.
    alts: Vec<usize>,
    follow: Option<Follow<'g>>,
}

impl<const BPP: usize, const IMAGE: bool> Enc<'_, BPP, IMAGE> {
    /// Against a reference, codes whether token `t` (a match without `alt258`) is the
    /// reference's where it starts, if one starts there; returns whether it is.
    #[inline(always)]
    fn same(&mut self, t: Token) -> bool {
        let Some(fo) = &mut self.follow else { return false };
        let Some((r, table)) = fo.at(self.st.p) else { return false };
        let same = r == t;
        self.io.bit(table, same as u32);
        fo.same = same as usize;
        same
    }

    /// Room for `k` bytes past the current position (at most a stored block).
    #[inline(always)]
    fn room(&mut self, k: usize) {
        self.st.cur.seek(self.st.g, self.st.p);
        self.st.room(&mut self.buf, &mut self.raw);
        let need = self.st.at(self.st.p) + k;
        if self.buf.len() < need {
            self.grow(need);
        }
    }

    #[cold]
    #[inline(never)]
    fn grow(&mut self, need: usize) {
        self.buf.resize(need.max(self.buf.len() * 2), 0);
    }

    #[inline(always)]
    fn lit(&mut self, f: u8, token: bool) {
        let l = self.st.prep::<BPP, IMAGE>(&self.raw, &self.buf, token);
        let v = f.wrapping_add(l.fp);
        if !(token && self.same(Token::Lit(f))) {
            let s = zigzag(v.wrapping_sub(l.pred));
            self.io.sym(l.t, s);
            if token && s == ESC {
                self.io.bit(T_ESC, 0);
            }
        }
        if l.q != usize::MAX {
            self.raw[l.q] = v;
        }
        let at = self.st.at(self.st.p);
        self.buf[at] = f;
        self.st.p += 1;
        if token {
            self.st.after = 0;
            self.st.tokens += 1;
        }
    }
}

impl<const BPP: usize, const IMAGE: bool> Sink for Enc<'_, BPP, IMAGE> {
    #[inline(always)]
    fn reserve(&mut self) {
        // Three literals or a match (and the overshoot of its copy).
        self.room(ROOM);
    }

    #[inline(always)]
    fn lit(&mut self, b: u8) {
        Enc::lit(self, b, true);
    }

    #[inline(always)]
    fn mat(&mut self, len: u16, dist: u16, alt258: bool) {
        if !self.same(Token::Match { len, dist, alt258: false }) {
            let l = self.st.prep::<BPP, IMAGE>(&self.raw, &self.buf, true);
            self.io.sym(l.t, ESC);
            self.io.bit(T_ESC, 1);
            code_match(&mut self.io, l.len_ctx(), len as u32, dist as u32);
        }
        if alt258 {
            self.alts.push(self.st.tokens);
        }
        self.st.copy::<BPP, IMAGE>(&mut self.raw, &mut self.buf, len as usize, dist as usize);
    }

    fn stored(&mut self, bytes: &[u8]) {
        self.room(bytes.len());
        for &b in bytes {
            self.lit(b, false);
        }
    }
}

/// One zlib stream of an image's payload, as parsed: its header, its blocks (without their
/// tokens) and trailing bytes (the Adler-32 checksum, and anything after the last stream),
/// per block its first token (counted over the whole payload) then the end, and per block
/// its symbol frequencies.
pub struct Coded {
    pub zhdr: [u8; 2],
    pub stream: aqr_flate::deflate::Stream,
    pub starts: Vec<usize>,
    pub freqs: Vec<Freqs>,
}

/// What the compressor gets: the symbols, the streams' structure, the data size and the
/// exceptions.
pub struct Encoded {
    pub rec: Recorder,
    pub streams: Vec<Coded>,
    pub len: usize,
    /// Token indices of matches of length 258 sent as 284 + 31.
    pub alts: Vec<usize>,
}

const MAX_MATCH: usize = 258;

/// Whether `b` starts with a zlib header (deflate, no preset dictionary).
fn zlib_header(b: &[u8]) -> bool {
    b.len() >= 2
        && b[0] & 0x0f == 8
        && b[0] >> 4 <= 7
        && b[1] & 0x20 == 0
        && (b[0] as u16 * 256 + b[1] as u16).is_multiple_of(31)
}

/// Where the next zlib stream would start after a stream ending at `end` (past its
/// checksum), if one plausibly does.
fn next_stream(payload: &[u8], end: usize) -> Option<usize> {
    let next = end + 4;
    (next + 2 < payload.len() && zlib_header(&payload[next..])).then_some(next)
}

/// Parses and models the payload `payload` (one or more zlib streams, as in Apple's
/// split IDAT) of an image with geometry `g`.
pub fn encode(kind: Kind, g: &Geometry, payload: &[u8]) -> crate::Result<Encoded> {
    encode_threads(kind, g, payload, 1, None)
}

/// [`encode`] with up to `threads` threads for the symbols of the rows of large images, and
/// against `reference` if given (of an image of the same geometry); the result is the same
/// whatever `threads` is.
pub fn encode_threads(
    kind: Kind,
    g: &Geometry,
    payload: &[u8],
    threads: usize,
    reference: Option<&Reference>,
) -> crate::Result<Encoded> {
    if payload.len() < 2 {
        return Err(crate::Error::Unsupported("no zlib stream"));
    }
    // Streams that expand a lot are mostly long matches: modelling only the literals as
    // they come costs less than computing every pixel's symbol up front.
    let sparse = g.len > ROWS_MAX_EXPANSION * payload.len();
    let r = reference;
    match (kind, g.bpp, sparse) {
        (Kind::Image, 1, false) => encode_image::<1>(g, payload, threads, r),
        (Kind::Image, 2, false) => encode_image::<2>(g, payload, threads, r),
        (Kind::Image, 3, false) => encode_image::<3>(g, payload, threads, r),
        (Kind::Image, 4, false) => encode_image::<4>(g, payload, threads, r),
        (Kind::Image, 1, true) => encode_bpp::<1, true>(g, payload, r),
        (Kind::Image, 2, true) => encode_bpp::<2, true>(g, payload, r),
        (Kind::Image, 3, true) => encode_bpp::<3, true>(g, payload, r),
        (Kind::Image, 4, true) => encode_bpp::<4, true>(g, payload, r),
        (Kind::Image, ..) => Err(crate::Error::Unsupported("pixel size")),
        (Kind::Bytes, ..) => encode_bpp::<0, false>(g, payload, r),
    }
}

/// The reference of none, never followed.
static NO_REFERENCE: Reference = Reference { pos: Vec::new(), tokens: Vec::new() };

/// Above this ratio of image data to DEFLATE stream, literals are modelled as they come
/// (see [`encode`]).
const ROWS_MAX_EXPANSION: usize = 4;

/// The compressor's side for images: every literal's context depends on the data only,
/// which the compressor has before it codes the tokens. So as the stream is parsed, its
/// rows are unfiltered and the symbols of their pixel bytes computed a window of rows at a
/// time (once the rows are complete; no dependency between them), and the tokens in the
/// window walked (against `reference` if given).
fn encode_image<'g, const BPP: usize>(
    g: &'g Geometry,
    payload: &[u8],
    threads: usize,
    reference: Option<&'g Reference>,
) -> crate::Result<Encoded> {
    let est = g.len.min(MAX_EXPANSION * payload.len());
    let threads = if est >= PAR_ROWS_MIN { threads.max(1) } else { 1 };
    let mut sink = Rows::<BPP>::new(g, est, threads, reference);
    let mut streams = vec![];
    let mut o = 0;
    loop {
        let base = sink.tokens();
        let l = inflate::parse_head(&payload[o + 2..], usize::MAX, &mut sink)?;
        let end = o + 2 + l.end;
        let next = next_stream(payload, end).filter(|&next| parses(&payload[next + 2..]));
        let mut stream = l.stream;
        stream.trailing = payload[end..next.map_or(payload.len(), |_| end + 4)].to_vec();
        streams.push(Coded {
            zhdr: [payload[o], payload[o + 1]],
            starts: l.starts.iter().map(|&s| s + base).collect(),
            freqs: l.stats.freqs,
            stream,
        });
        match next {
            Some(next) => o = next,
            None => break,
        }
    }
    let n = sink.p;
    // Token indices are kept in 32 bits (as the layout codes the data length).
    if n > u32::MAX as usize {
        return Err(crate::Error::Unsupported("image data of 4 GiB or more"));
    }
    sink.advance(true);
    let tokens = sink.tokens();
    let Rows { rec, alts, w, events, .. } = sink;
    if w.ti != tokens || w.ei != events.len() || w.p != n {
        return Err(crate::corrupt());
    }
    Ok(Encoded { rec, streams, len: n, alts })
}

/// DEFLATE's largest expansion (a stored byte aside): 258 bytes from a 2-bit match code, at
/// most 1032 bytes per byte of stream.
const MAX_EXPANSION: usize = 1032;

/// Whether `raw` starts with a DEFLATE stream that parses.
fn parses(raw: &[u8]) -> bool {
    inflate::parse_head(raw, usize::MAX, &mut Discard).is_ok()
}

/// A sink keeping nothing (the parser checks the distances itself).
struct Discard;

impl Sink for Discard {
    fn lit(&mut self, _: u8) {}
    fn mat(&mut self, _: u16, _: u16, _: bool) {}
    fn stored(&mut self, _: &[u8]) {}
}

/// A match (`dist > 0`) or a stored block (`dist == 0`, of `len` bytes) of the parse, at a
/// token index (a stored block comes before that token).
#[derive(Clone, Copy)]
struct Event {
    at: u32,
    len: u16,
    dist: u16,
}

/// The compressor's sink for images: a window of the data (after `Geometry::pad` bytes, as
/// the model reads it), and of the tokens not walked yet the matches and stored blocks (the
/// literals are the bytes between). As rows are complete, their symbols are computed and
/// their tokens walked into the recorder.
struct Rows<'g, const BPP: usize> {
    g: &'g Geometry,
    buf: Vec<u8>,
    /// The position, and where position `i` is in `buf`: at `off + i` (wrapping), from
    /// position `base` on.
    p: usize,
    off: usize,
    base: usize,
    /// Below this position there is room for a match and no window to walk.
    plim: usize,
    events: Vec<Event>,
    /// Bytes not of literals so far, beyond a byte per match: the tokens so far are
    /// `p - skip`.
    skip: usize,
    /// `buf` where position 0 would be (`off` on; wrapping).
    ptr: *mut u8,
    /// Token indices of matches of length 258 sent as 284 + 31.
    alts: Vec<usize>,
    win: Window,
    w: Walk,
    rec: Recorder,
    threads: usize,
    /// The largest row size.
    rs: usize,
    follow: Option<Follow<'g>>,
}

impl<'g, const BPP: usize> Rows<'g, BPP> {
    /// For about `est` bytes of data, against `reference` if given.
    fn new(g: &'g Geometry, est: usize, threads: usize, reference: Option<&'g Reference>) -> Rows<'g, BPP> {
        let mut win = Window::new(STRIPE * threads);
        // the literals compared with the reference's
        win.keep = reference.is_some();
        let size = (g.pad + KEEP + win.size + g.rs_max() + 2 * (65536 + ROOM)).min(g.pad + est + ROOM);
        let mut rec = Recorder::new(0);
        if reference.is_some() {
            rec.tables = TABLES_REF;
        }
        // About a symbol per byte (they grow as needed).
        rec.syms.reserve(est.min(STRIPE * 64));
        let mut r = Rows {
            g,
            buf: vec![0; size],
            p: 0,
            off: g.pad,
            base: 0,
            plim: 0,
            events: vec![],
            skip: 0,
            ptr: std::ptr::null_mut(),
            alts: vec![],
            win,
            w: Walk { p: 0, ti: 0, ei: 0, left: 0, after: 0 },
            rec,
            threads,
            rs: g.rs_max(),
            follow: reference.map(Follow::new),
        };
        r.limit();
        r
    }

    #[inline(always)]
    fn at(&self, p: usize) -> usize {
        self.off.wrapping_add(p)
    }

    #[inline(always)]
    fn tokens(&self) -> usize {
        self.p - self.skip
    }

    fn limit(&mut self) {
        self.ptr = self.buf.as_mut_ptr().wrapping_add(self.off);
        let room = self.base + (self.buf.len() - self.g.pad).saturating_sub(ROOM);
        self.plim = room.min(self.due());
    }

    /// Where the next window is due: a window's worth (or a row's) on.
    fn due(&self) -> usize {
        self.win.s1 + self.win.size.max(self.rs)
    }

    /// Walks the windows there are, then makes room for `k` bytes.
    #[cold]
    #[inline(never)]
    fn room(&mut self, k: usize) {
        if self.p >= self.due() {
            self.advance(false);
        }
        if self.at(self.p) + k > self.buf.len() {
            // What the parser and the rows to come still read: the farthest match, and from
            // the row above the next window on.
            let from = self.p.saturating_sub(32768).min(self.win.s1.saturating_sub(self.rs + 16)).max(self.base);
            let src = self.at(from) - self.g.pad;
            let end = self.at(self.p);
            self.buf.copy_within(src..end, 0);
            (self.base, self.off) = (from, self.g.pad.wrapping_sub(from));
            if self.at(self.p) + k > self.buf.len() {
                self.buf.resize((self.at(self.p) + k).max(2 * self.buf.len()), 0);
            }
        }
        self.limit();
    }

    /// Computes the windows whose rows are complete (all of them, `last`) and walks their
    /// tokens.
    fn advance(&mut self, last: bool) {
        let mut out = Syms::new(&mut self.rec.syms, &mut self.rec.raw);
        loop {
            if self.w.p >= self.win.s1 {
                let data = &self.buf[..];
                if !self.win.next::<BPP>(self.g, data, self.off, self.p, self.w.p, self.threads, last) {
                    break;
                }
            }
            let end = self.p - self.skip;
            let full = match &mut self.follow {
                Some(fo) => walk::<true>(&mut out, &self.win, &self.events, &mut self.w, end, fo),
                None => {
                    walk::<false>(&mut out, &self.win, &self.events, &mut self.w, end, &mut Follow::new(&NO_REFERENCE))
                }
            };
            if !full {
                break;
            }
        }
        out.done();
        self.events.drain(..self.w.ei);
        self.w.ei = 0;
    }
}

impl<const BPP: usize> Sink for Rows<'_, BPP> {
    #[inline(always)]
    fn reserve(&mut self) {
        // Three literals or a match.
        if self.p >= self.plim {
            self.room(ROOM);
        }
    }

    #[inline(always)]
    fn lit(&mut self, b: u8) {
        // SAFETY: `reserve` came before, and leaves room for three literals in `buf` (which
        // `ptr` points into from `off` on).
        unsafe { self.ptr.wrapping_add(self.p).write(b) };
        self.p += 1;
    }

    #[inline(always)]
    fn mat(&mut self, len: u16, dist: u16, alt258: bool) {
        // SAFETY: `1 <= dist <=` the bytes so far (checked by the parser), all kept before
        // the position (at least a match's distance, from `buf`'s start), and `reserve` left
        // room for the match and the overshoot of its copy.
        unsafe { inflate::copy_match(self.buf.as_mut_ptr(), self.at(self.p), dist as usize, len as usize) };
        let t = self.tokens();
        if alt258 {
            self.alts.push(t);
        }
        self.events.push(Event { at: t as u32, len, dist });
        self.p += len as usize;
        self.skip += len as usize - 1;
    }

    fn stored(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.room(bytes.len() + ROOM);
        let at = self.at(self.p);
        self.buf[at..at + bytes.len()].copy_from_slice(bytes);
        self.events.push(Event { at: self.tokens() as u32, len: bytes.len() as u16, dist: 0 });
        self.p += bytes.len();
        self.skip += bytes.len();
    }
}

/// Data bytes the image sink keeps for the parser: the farthest match.
const KEEP: usize = 32768 + 16;

/// Room kept past the position: a match and the overshoot of its copy.
const ROOM: usize = MAX_MATCH + 16;

impl Geometry {
    /// The largest row size.
    fn rs_max(&self) -> usize {
        self.passes.iter().map(|ps| ps.rs).max().unwrap_or(0)
    }
}

/// Images of at least this many data bytes get their symbols computed by several threads.
const PAR_ROWS_MIN: usize = 1 << 20;

/// Data bytes per window and thread (see [`Window`]).
const STRIPE: usize = 1 << 18;

/// Per position of a window of the data (whole rows of a pass, or a piece past the image),
/// its symbol, its table relative to `T_LIT` (before the after-match offset) and the
/// bucket of its activity; and the raw rows of the window, after the row above.
struct Window {
    s: Vec<u8>,
    t: Vec<u8>,
    act: Vec<u8>,
    /// The positions it covers.
    s0: usize,
    s1: usize,
    /// The next row to unfilter: its pass and row.
    pass: usize,
    row: usize,
    /// Bytes it covers (about).
    size: usize,
    /// The raw rows, from the raw offset `rbase` on.
    raw: Vec<u8>,
    rbase: usize,
    /// Whether to keep the data of the positions it covers, and that data.
    keep: bool,
    d: Vec<u8>,
}

impl Window {
    fn new(size: usize) -> Window {
        Window {
            s: vec![],
            t: vec![],
            act: vec![],
            s0: 0,
            s1: 0,
            pass: 0,
            row: 0,
            size,
            raw: vec![],
            rbase: 0,
            keep: false,
            d: vec![],
        }
    }

    /// Moves on to the window containing position `p` (`p <` the `n` bytes there are) of the
    /// data (position `i` at `off + i` in `buf`): unfilters the rows before it and its own,
    /// and computes its symbols, those of its rows on up to `threads` threads. Only whole
    /// rows are taken but for the `last` window; returns whether there was one.
    #[allow(clippy::too_many_arguments)]
    fn next<const BPP: usize>(
        &mut self,
        g: &Geometry,
        buf: &[u8],
        off: usize,
        n: usize,
        p: usize,
        threads: usize,
        last: bool,
    ) -> bool {
        const F: u8 = (T_FILTER - T_LIT) as u8;
        let at = |i: usize| off.wrapping_add(i);
        loop {
            let ps = g.passes.get(self.pass).filter(|ps| self.row < ps.rows && ps.start + self.row * ps.rs < n);
            let Some(ps) = ps.copied() else {
                // Past the image (or the data): pieces of filter-byte symbols.
                let s0 = self.s1.max(g.len.min(n)).max(p);
                if s0 >= n {
                    return false;
                }
                self.reset(s0, n.min(s0 + self.size));
                self.copy(buf, off);
                let k = self.s1 - s0;
                for (i, s) in self.s[..k].iter_mut().enumerate() {
                    *s = zigzag(buf[at(s0 + i)]) as u8;
                }
                self.t[..k].fill(F);
                self.act[..k].fill(ACT as u8);
                return true;
            };
            let r0 = self.row;
            let have = if last { (n - ps.start).div_ceil(ps.rs) } else { (n - ps.start) / ps.rs };
            let r1 = (r0 + (self.size / ps.rs).max(1)).min(ps.rows).min(have);
            if r1 == r0 {
                return false;
            }
            (self.row, self.pass) = if r1 == ps.rows { (0, self.pass + 1) } else { (r1, self.pass) };
            let s0 = ps.start + r0 * ps.rs;
            let s1 = (ps.start + r1 * ps.rs).min(n);
            // The raw rows: the row above (the pass's zero row for its first), with 8 bytes
            // before it (the channel decorrelation reads the byte before a pixel's
            // neighbours), then zeros for the window's rows (and their left margins).
            let above = ps.base2 + r0 * ps.rs2 - 8;
            let len = 8 + (r1 - r0 + 1) * ps.rs2 + 16;
            if self.raw.len() < len {
                self.raw.resize(len, 0);
            }
            if r0 == 0 {
                self.raw[..8 + ps.rs2].fill(0);
            } else {
                let src = above - self.rbase;
                self.raw.copy_within(src..src + 8 + ps.rs2, 0);
            }
            // The rows' bytes are all unfiltered, their left margins not.
            for r in 1..=r1 - r0 {
                self.raw[8 + r * ps.rs2..8 + r * ps.rs2 + BPP].fill(0);
            }
            self.rbase = above;
            let rbase = self.rbase;
            let row = |r: usize| {
                let rstart = ps.start + r * ps.rs;
                let w = (ps.rs - 1).min(n - rstart - 1);
                (rstart, w, ps.base2 + (r + 1) * ps.rs2 + BPP - rbase)
            };
            // The rows of this window, unfiltered in order (each needs the row above).
            for r in r0..r1 {
                let (rstart, w, q) = row(r);
                let d = &buf[at(rstart) + 1..at(rstart) + 1 + w];
                unfilter_part::<BPP>(buf[at(rstart)], d, &mut self.raw, q, ps.rs2, 0, w, BPP);
            }
            if s1 <= p {
                // A match went past all of it.
                self.s1 = s1;
                continue;
            }
            self.reset(s0, s1);
            self.copy(buf, off);
            let raw: &[u8] = &self.raw;
            let syms = |ms: &mut RowSymsMut, rows: std::ops::Range<usize>, d0: usize| {
                for r in rows {
                    let (rstart, w, q) = row(r);
                    ms.s[rstart - d0] = zigzag(buf[at(rstart)]) as u8;
                    ms.t[rstart - d0] = F;
                    ms.act[rstart - d0] = ACT as u8;
                    row_syms::<BPP>(raw, buf, q, at(rstart) + 1, w, ps.rs, ps.rs2, ms, rstart + 1 - d0);
                }
            };
            let mut ms = RowSymsMut { s: &mut self.s, t: &mut self.t, act: &mut self.act };
            let k = threads.min(r1 - r0);
            if k <= 1 {
                syms(&mut ms, r0..r1, s0);
                return true;
            }
            // Stripes of rows, each into its own part of the window.
            std::thread::scope(|sc| {
                let per = (r1 - r0).div_ceil(k);
                let mut rest = ms;
                for j in (r0..r1).step_by(per) {
                    let rows = j..(j + per).min(r1);
                    let len = (ps.start + rows.end * ps.rs).min(n) - (ps.start + j * ps.rs);
                    let (s1, s2) = std::mem::take(&mut rest.s).split_at_mut(len);
                    let (t1, t2) = std::mem::take(&mut rest.t).split_at_mut(len);
                    let (a1, a2) = std::mem::take(&mut rest.act).split_at_mut(len);
                    rest = RowSymsMut { s: s2, t: t2, act: a2 };
                    let d0 = ps.start + j * ps.rs;
                    let mut part = RowSymsMut { s: s1, t: t1, act: a1 };
                    if rows.end == r1 {
                        syms(&mut part, rows, d0);
                    } else {
                        sc.spawn(move || syms(&mut part, rows, d0));
                    }
                }
            });
            return true;
        }
    }

    /// Keeps the data it covers, if it is to (position `i` at `off + i` in `buf`).
    fn copy(&mut self, buf: &[u8], off: usize) {
        if self.keep {
            let (a, n) = (off.wrapping_add(self.s0), self.s1 - self.s0);
            self.d.clear();
            self.d.extend_from_slice(&buf[a..a + n]);
        }
    }

    /// Covers `s0..s1` (the symbols are left to be written).
    fn reset(&mut self, s0: usize, s1: usize) {
        (self.s0, self.s1) = (s0, s1);
        for v in [&mut self.s, &mut self.t, &mut self.act] {
            if v.len() < s1 - s0 {
                v.resize(s1 - s0, 0);
            }
        }
    }
}

/// Where the token walk is: position, token, next event, bytes left of a stored block, and
/// the offset of the literal tables after a match.
struct Walk {
    p: usize,
    ti: usize,
    ei: usize,
    left: usize,
    after: usize,
}

/// Records the symbols of the bytes from `w.p` within the window: of the tokens up to
/// `end` (those parsed) and the stored blocks among them (`events` from `w.ei`). Returns
/// whether it got to the window's end (else to the tokens').
///
/// `REF`: against the reference `fo` follows, each token where one of its starts first a
/// decision whether it is the same (the data of the window kept, [`Window::keep`]).
#[inline(never)]
fn walk<const REF: bool>(
    out: &mut Syms,
    win: &Window,
    events: &[Event],
    w: &mut Walk,
    end: usize,
    fo: &mut Follow,
) -> bool {
    const F: usize = T_FILTER - T_LIT;
    let (st, ss, sa) = (&win.t[..], &win.s[..], &win.act[..]);
    let lim = win.s1 - win.s0;
    let (mut p, mut ti, mut ei, mut left, mut after) = (w.p - win.s0, w.ti, w.ei, w.left, w.after);
    let full = loop {
        if p >= lim {
            break true;
        }
        let e = events.get(ei).copied().unwrap_or(Event { at: u32::MAX, len: 0, dist: 1 });
        let at = if ei < events.len() { e.at as usize } else { usize::MAX };
        if left > 0 || at == ti && e.dist == 0 {
            // A stored block: its bytes as they are, before the token.
            if left == 0 {
                left = e.len as usize;
            }
            let k = left.min(lim - p);
            for i in p..p + k {
                out.sym(T_LIT + st[i] as usize, ss[i] as u32);
            }
            p += k;
            left -= k;
            if left == 0 {
                ei += 1;
            }
            continue;
        }
        if ti == end {
            break false;
        }
        if at == ti {
            let same = REF && same(out, fo, win.s0 + p, Token::Match { len: e.len, dist: e.dist, alt258: false });
            if !same {
                let e2 = st[p] as usize;
                // The after-match offset applies to the literal tables only.
                let mask = ((e2 < F) as usize).wrapping_neg();
                out.sym(T_LIT + e2 + (after & mask), ESC);
                out.bit(T_ESC, 1);
                code_match(out, sa[p] as usize, e.len as u32, e.dist as u32);
            }
            p += e.len as usize;
            (ti, ei, after) = (ti + 1, ei + 1, 4 * ACT);
            continue;
        }
        // Literals up to the next event, the tokens' end or the window's.
        let k = (at.min(end) - ti).min(lim - p);
        for i in p..p + k {
            if !(REF && same(out, fo, win.s0 + i, Token::Lit(win.d[i]))) {
                let e = st[i] as usize;
                let mask = ((e < F) as usize).wrapping_neg();
                let s = ss[i] as u32;
                out.sym(T_LIT + e + (after & mask), s);
                if s == ESC {
                    out.bit(T_ESC, 0);
                }
            }
            after = 0;
        }
        p += k;
        ti += k;
    };
    (w.p, w.ti, w.ei, w.left, w.after) = (p + win.s0, ti, ei, left, after);
    full
}

/// Against the reference `fo` follows, codes whether token `t` at position `p` is the
/// reference's, if one of its starts there; returns whether it is.
#[inline(always)]
fn same(out: &mut Syms, fo: &mut Follow, p: usize, t: Token) -> bool {
    let Some((r, table)) = fo.at(p) else { return false };
    let same = r == t;
    out.bit(table, same as u32);
    fo.same = same as usize;
    same
}

/// Symbols written into reserved room (the compressor's token walk), which grows if it
/// runs out.
struct Syms<'a> {
    ptr: *mut (u8, u8),
    n: usize,
    cap: usize,
    v: &'a mut Vec<(u8, u8)>,
    raw: &'a mut BitW,
}

impl<'a> Syms<'a> {
    fn new(v: &'a mut Vec<(u8, u8)>, raw: &'a mut BitW) -> Syms<'a> {
        Syms { ptr: v.as_mut_ptr(), n: v.len(), cap: v.capacity(), v, raw }
    }

    #[cold]
    #[inline(never)]
    fn grow(&mut self) {
        // SAFETY: the first `n` entries are written.
        unsafe { self.v.set_len(self.n) };
        self.v.reserve(self.n.max(1 << 10));
        (self.ptr, self.cap) = (self.v.as_mut_ptr(), self.v.capacity());
    }

    /// Gives the vector its length.
    fn done(self) {
        // SAFETY: the first `n` entries are written, within the capacity.
        unsafe { self.v.set_len(self.n) };
    }
}

impl Io for Syms<'_> {
    #[inline(always)]
    fn sym(&mut self, t: usize, v: u32) -> u32 {
        if self.n == self.cap {
            self.grow();
        }
        // SAFETY: `n < cap`, made sure of just above.
        unsafe { self.ptr.add(self.n).write((t as u8, v as u8)) };
        self.n += 1;
        v
    }
    #[inline(always)]
    fn bit(&mut self, t: usize, v: u32) -> u32 {
        self.sym(t, v)
    }
    #[inline(always)]
    fn raw(&mut self, k: u32, v: u32) -> u32 {
        self.raw.bits(k, v);
        v
    }
}

/// A window's part of [`Window`]'s symbols (all of it, or the stripe of rows one thread
/// fills).
struct RowSymsMut<'a> {
    s: &'a mut [u8],
    t: &'a mut [u8],
    act: &'a mut [u8],
}

/// The symbols of a row's pixel bytes `x0..w` (raw offset `q`, data offset `at`), all
/// unfiltered, into `out` at data position `d` (byte 0's).
#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn row_syms_scalar<const BPP: usize>(
    raw: &[u8],
    buf: &[u8],
    q: usize,
    at: usize,
    x0: usize,
    w: usize,
    rs: usize,
    rs2: usize,
    out: &mut RowSymsMut,
    d: usize,
) {
    for x in x0..w {
        let m = pixel::<BPP>(raw, buf, q + x, at + x, x % BPP, rs, rs2);
        out.s[d + x] = zigzag(get(raw, q + x).wrapping_sub(m.pred)) as u8;
        out.t[d + x] = (m.t - T_LIT) as u8;
        out.act[d + x] = bucket(m.act) as u8;
    }
}

/// [`row_syms_scalar`] for a whole row, 16 bytes at a time with NEON where available.
#[allow(clippy::too_many_arguments)]
fn row_syms<const BPP: usize>(
    raw: &[u8],
    buf: &[u8],
    q: usize,
    at: usize,
    w: usize,
    rs: usize,
    rs2: usize,
    out: &mut RowSymsMut,
    d: usize,
) {
    #[cfg(target_arch = "aarch64")]
    {
        let done = neon::row_syms::<BPP>(raw, buf, q, at, w, rs, rs2, out, d);
        row_syms_scalar::<BPP>(raw, buf, q, at, done, w, rs, rs2, out, d);
    }
    #[cfg(not(target_arch = "aarch64"))]
    row_syms_scalar::<BPP>(raw, buf, q, at, 0, w, rs, rs2, out, d);
}

#[cfg(target_arch = "aarch64")]
mod neon {
    use super::{ACT, RowSymsMut};
    use aqr_raw::image::pixel::{self, neon::*};
    use std::arch::aarch64::*;

    /// Returns how many bytes it did (whole 16-byte chunks).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn row_syms<const BPP: usize>(
        raw: &[u8],
        buf: &[u8],
        q: usize,
        at: usize,
        w: usize,
        rs: usize,
        rs2: usize,
        out: &mut RowSymsMut,
        d: usize,
    ) -> usize {
        let chunks = w / 16;
        if chunks == 0 {
            return 0;
        }
        // Every read below is of the row, the row above, their left padding, the byte
        // before (the raw margin) or the data's padding: within the buffers.
        assert!(q > rs2 + BPP && q + w <= raw.len() && at >= rs && at + w <= buf.len());
        assert!(d + w <= out.s.len() && d + w <= out.t.len() && d + w <= out.act.len());
        // Per phase of the channel pattern: the lanes to decorrelate (channels 1 and 2 of
        // three or more) and the table offsets (`min(ch, 3) * ACT`).
        let mut dec = [[0u8; 16]; 4];
        let mut toff = [[0u8; 16]; 4];
        for ph in 0..BPP.min(4) {
            for i in 0..16 {
                let ch = (ph + i) % BPP;
                dec[ph][i] = if pixel::decorrelated::<BPP>(ch) { 0xff } else { 0 };
                toff[ph][i] = (ch.min(3) * ACT) as u8;
            }
        }
        unsafe {
            let r = raw.as_ptr();
            let f = buf.as_ptr();
            for k in 0..chunks {
                let x = k * 16;
                let ph = x % BPP;
                let ld = |p: *const u8| vld1q_u8(p);
                let v = ld(r.add(q + x));
                let a = ld(r.add(q + x - BPP));
                let b = ld(r.add(q + x - rs2));
                let c = ld(r.add(q + x - rs2 - BPP));
                let p = if BPP >= 3 {
                    let v1 = ld(r.add(q + x - 1));
                    let a1 = ld(r.add(q + x - 1 - BPP));
                    let b1 = ld(r.add(q + x - 1 - rs2));
                    let c1 = ld(r.add(q + x - 1 - rs2 - BPP));
                    pred(a, b, c, v1, a1, b1, c1, vld1q_u8(dec[ph].as_ptr()))
                } else {
                    pred_med(a, b, c)
                };
                vst1q_u8(out.s.as_mut_ptr().add(d + x), zigzag16(vsubq_u8(v, p)));
                // Activity |a - c| + |b - c|, and with the filtered neighbours' magnitudes.
                let (act_lo, act_hi) = act16(a, b, c);
                let (ff_lo, ff_hi) = mag_sum16(ld(f.add(at + x - BPP)), ld(f.add(at + x - rs)));
                let t_lo = bucket16(vaddq_u16(act_lo, ff_lo));
                let t_hi = bucket16(vaddq_u16(act_hi, ff_hi));
                let t = vaddq_u8(vcombine_u8(vmovn_u16(t_lo), vmovn_u16(t_hi)), vld1q_u8(toff[ph].as_ptr()));
                vst1q_u8(out.t.as_mut_ptr().add(d + x), t);
                let ab = vcombine_u8(vmovn_u16(bucket16(act_lo)), vmovn_u16(bucket16(act_hi)));
                vst1q_u8(out.act.as_mut_ptr().add(d + x), ab);
            }
        }
        chunks * 16
    }
}

fn encode_bpp<'g, const BPP: usize, const IMAGE: bool>(
    g: &'g Geometry,
    payload: &[u8],
    reference: Option<&'g Reference>,
) -> crate::Result<Encoded> {
    let win = Win::new(g, g.len.min(MAX_EXPANSION * payload.len()), IMAGE);
    let mut io = Recorder::new(payload.len().min(g.len) + 1024);
    if reference.is_some() {
        io.tables = TABLES_REF;
    }
    let mut e = Enc::<BPP, IMAGE> {
        // Room for a symbol per byte of stream to start with (the data is mostly long
        // matches); both grow as needed.
        io,
        st: St::new(g, win),
        buf: vec![0u8; win.buf + ROOM],
        raw: vec![0u8; win.raw],
        alts: vec![],
        follow: reference.map(Follow::new),
    };
    let mut streams: Vec<Coded> = vec![];
    let mut o = 0;
    loop {
        let base = e.st.tokens;
        let l = inflate::parse_into(&payload[o + 2..], usize::MAX, &mut e)?;
        let end = payload.len() - l.stream.trailing.len();
        streams.push(Coded {
            zhdr: [payload[o], payload[o + 1]],
            starts: l.starts.iter().map(|&s| s + base).collect(),
            freqs: l.stats.freqs,
            stream: l.stream,
        });
        // Another stream follows if it parses (checked apart, before it feeds the model).
        match next_stream(payload, end) {
            Some(next) if parses(&payload[next + 2..]) => {
                streams.last_mut().unwrap().stream.trailing.truncate(4);
                o = next;
            }
            _ => break,
        }
    }
    Ok(Encoded { rec: e.io, streams, len: e.st.p, alts: e.alts })
}

/// Symbol frequencies of a Huffman block as `aqr_flate::header` counts them.
pub type Freqs = ([u32; 288], [u32; 32]);

/// What [`decode`] hands over after each span: its tokens (none for stored blocks), its
/// symbol frequencies (zeros for stored blocks; matches of length 258 counted as symbol
/// 285), and the data from a position on (at least that of the span, if stored), and that
/// position.
pub type Emit<'e> = dyn FnMut(&mut [Token], &mut Freqs, &[u8], usize) -> crate::Result<()> + 'e;

/// Rebuilds the `n` bytes of data of `spans` (the decompressor's side), handing each span's
/// tokens to `emit` as they are decoded; against `reference` if given.
pub fn decode(
    player: &Player,
    kind: Kind,
    g: &Geometry,
    spans: &[Span],
    n: usize,
    reference: Option<&Reference>,
    emit: &mut Emit,
) -> crate::Result<()> {
    match (kind, g.bpp, reference) {
        (Kind::Image, 1, None) => decode_bpp::<1, true, false>(player, g, spans, n, None, emit),
        (Kind::Image, 2, None) => decode_bpp::<2, true, false>(player, g, spans, n, None, emit),
        (Kind::Image, 3, None) => decode_bpp::<3, true, false>(player, g, spans, n, None, emit),
        (Kind::Image, 4, None) => decode_bpp::<4, true, false>(player, g, spans, n, None, emit),
        (Kind::Image, 1, r) => decode_bpp::<1, true, true>(player, g, spans, n, r, emit),
        (Kind::Image, 2, r) => decode_bpp::<2, true, true>(player, g, spans, n, r, emit),
        (Kind::Image, 3, r) => decode_bpp::<3, true, true>(player, g, spans, n, r, emit),
        (Kind::Image, 4, r) => decode_bpp::<4, true, true>(player, g, spans, n, r, emit),
        (Kind::Image, ..) => Err(crate::corrupt()),
        (Kind::Bytes, _, None) => decode_bpp::<0, false, false>(player, g, spans, n, None, emit),
        (Kind::Bytes, _, r) => decode_bpp::<0, false, true>(player, g, spans, n, r, emit),
    }
}

/// The decoder's moving parts as a local value (the tables are only borrowed), so that the
/// rANS states and stream positions can live in registers.
struct Live<'a> {
    tables: &'a Tables,
    dec: Decoder<'a>,
    raw: BitR<'a>,
}

impl Io for Live<'_> {
    #[inline(always)]
    fn sym(&mut self, t: usize, _: u32) -> u32 {
        self.dec.get(self.tables, t)
    }
    #[inline(always)]
    fn bit(&mut self, t: usize, _: u32) -> u32 {
        self.dec.bit(self.tables, t)
    }
    #[inline(always)]
    fn raw(&mut self, k: u32, _: u32) -> u32 {
        self.raw.bits(k)
    }
}

/// `REF`: against `reference`, which is then given.
fn decode_bpp<const BPP: usize, const IMAGE: bool, const REF: bool>(
    player: &Player,
    g: &Geometry,
    spans: &[Span],
    n: usize,
    reference: Option<&Reference>,
    emit: &mut Emit,
) -> crate::Result<()> {
    let mut live = Live { tables: &player.tables, dec: player.dec, raw: player.raw };
    let win = Win::new(g, n, IMAGE);
    let (mut buf, mut raw) = (vec![0u8; win.buf], vec![0u8; win.raw]);
    let none = Reference { pos: vec![], tokens: vec![] };
    let mut fo = Follow::new(reference.unwrap_or(&none));
    decode_loop::<Live, BPP, IMAGE, REF>(&mut live, g, spans, n, win, &mut buf, &mut raw, &mut fo, emit)?;
    if !live.dec.finished() || live.raw.overrun() {
        return Err(crate::corrupt());
    }
    Ok(())
}

#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn decode_loop<I: Io, const BPP: usize, const IMAGE: bool, const REF: bool>(
    io: &mut I,
    g: &Geometry,
    spans: &[Span],
    n: usize,
    win: Win,
    buf: &mut [u8],
    raw: &mut [u8],
    fo: &mut Follow,
    emit: &mut Emit,
) -> crate::Result<()> {
    let mut st = St::new(g, win);
    // One block's tokens at a time (counted from 0 in `st.tokens`).
    let most = spans.iter().map(|s| if let Span::Tokens(c) = *s { c } else { 0 }).max().unwrap_or(0);
    let mut tokens = vec![Token::Lit(0); most];
    for &span in spans {
        match span {
            Span::Stored(len) => {
                if len > n - st.p {
                    return Err(crate::corrupt());
                }
                st.cur.seek(g, st.p);
                st.room(buf, raw);
                for _ in 0..len {
                    let l = st.prep::<BPP, IMAGE>(raw, buf, false);
                    let s = io.sym(l.t, 0);
                    l.finish::<true>(s, raw, buf, st.at(st.p));
                    st.p += 1;
                }
                emit(&mut [], &mut ([0; 288], [0; 32]), &buf[g.pad..st.at(st.p)], st.base)?;
            }
            Span::Tokens(count) => {
                let (mut lf, mut df) = ([0u32; 288], [0u32; 32]);
                lf[256] = 1;
                st.tokens = 0;
                let end = count;
                while st.tokens < end {
                    if st.p >= n {
                        return Err(crate::corrupt());
                    }
                    st.cur.seek(g, st.p);
                    st.room(buf, raw);
                    if REF && let Some((t, table)) = fo.at(st.p) {
                        fo.same = io.bit(table, 0) as usize;
                        if fo.same == 1 {
                            // The reference's token.
                            let slot = tokens.get_mut(st.tokens).ok_or_else(crate::corrupt)?;
                            match t {
                                Token::Lit(f) => {
                                    let l = st.prep::<BPP, IMAGE>(raw, buf, true);
                                    l.put(f, raw, buf, st.at(st.p));
                                    *slot = t;
                                    lf[f as usize] += 1;
                                    st.p += 1;
                                    st.after = 0;
                                    st.tokens += 1;
                                }
                                Token::Match { len, dist, .. } => {
                                    let (len, dist) = (len as usize, dist as usize);
                                    if dist > st.p || len > n - st.p {
                                        return Err(crate::corrupt());
                                    }
                                    *slot = t;
                                    lf[257 + len_code(len as u32) as usize] += 1;
                                    df[dist_code(dist as u32) as usize] += 1;
                                    st.copy::<BPP, IMAGE>(raw, buf, len, dist);
                                }
                            }
                            continue;
                        }
                    }
                    let x = st.p - st.cur.rstart;
                    if IMAGE && !REF && x > 0 && st.cur.rs != usize::MAX {
                        // A run of literals within the row, with the row's filter fixed.
                        let run = (st.cur.rend - st.p).min(end - st.tokens).min(n - st.p);
                        let ft = get(buf, st.at(st.cur.rstart));
                        let (done, act) = match ft {
                            0 => lit_run::<I, BPP, 0>(io, &st, x, run, raw, buf, &mut tokens, &mut lf)?,
                            1 => lit_run::<I, BPP, 1>(io, &st, x, run, raw, buf, &mut tokens, &mut lf)?,
                            2 => lit_run::<I, BPP, 2>(io, &st, x, run, raw, buf, &mut tokens, &mut lf)?,
                            3 => lit_run::<I, BPP, 3>(io, &st, x, run, raw, buf, &mut tokens, &mut lf)?,
                            4 => lit_run::<I, BPP, 4>(io, &st, x, run, raw, buf, &mut tokens, &mut lf)?,
                            _ => lit_run::<I, BPP, 5>(io, &st, x, run, raw, buf, &mut tokens, &mut lf)?,
                        };
                        st.p += done;
                        st.tokens += done;
                        if done > 0 {
                            st.after = 0;
                        }
                        if let Some(act) = act {
                            // The run stopped at a match (its escape is decoded).
                            let (len, dist) = code_match(io, bucket(act), 0, 0).ok_or_else(crate::corrupt)?;
                            if dist > st.p || len > n - st.p {
                                return Err(crate::corrupt());
                            }
                            tokens[st.tokens] = Token::Match { len: len as u16, dist: dist as u16, alt258: false };
                            lf[257 + len_code(len as u32) as usize] += 1;
                            df[dist_code(dist as u32) as usize] += 1;
                            st.copy::<BPP, IMAGE>(raw, buf, len, dist);
                        }
                        continue;
                    }
                    let l = st.prep::<BPP, IMAGE>(raw, buf, true);
                    let s = io.sym(l.t, 0);
                    if s != ESC || io.bit(T_ESC, 0) == 0 {
                        let v = l.finish::<true>(s, raw, buf, st.at(st.p));
                        *tokens.get_mut(st.tokens).ok_or_else(crate::corrupt)? = Token::Lit(v);
                        lf[v as usize] += 1;
                        st.p += 1;
                        st.after = 0;
                        st.tokens += 1;
                    } else {
                        let (len, dist) = code_match(io, l.len_ctx(), 0, 0).ok_or_else(crate::corrupt)?;
                        if dist > st.p || len > n - st.p {
                            return Err(crate::corrupt());
                        }
                        *tokens.get_mut(st.tokens).ok_or_else(crate::corrupt)? =
                            Token::Match { len: len as u16, dist: dist as u16, alt258: false };
                        lf[257 + len_code(len as u32) as usize] += 1;
                        df[dist_code(dist as u32) as usize] += 1;
                        st.copy::<BPP, IMAGE>(raw, buf, len, dist);
                    }
                }
                emit(&mut tokens[..count], &mut (lf, df), &buf[g.pad..st.at(st.p)], st.base)?;
            }
        }
    }
    if st.p != n {
        return Err(crate::corrupt());
    }
    Ok(())
}

/// The model of a pixel byte (raw offset `q`, data offset `at`, channel `ch`): its table
/// (before the after-match offset), prediction and activity, and the raw neighbours.
#[derive(Clone, Copy)]
struct Pixel {
    t: usize,
    pred: u8,
    act: u32,
    a: u8,
    b: u8,
    c: u8,
}

impl Pixel {
    /// The prediction of filter `ft`.
    #[inline(always)]
    fn fp(&self, ft: u8) -> u8 {
        match ft {
            1 => self.a,
            2 => self.b,
            3 => ((self.a as u16 + self.b as u16) >> 1) as u8,
            4 => filter::paeth(self.a, self.b, self.c),
            _ => 0,
        }
    }
}

/// The pixel model of the byte (its raw offset `q` and data offset `at`, as [`Pixel`]), with
/// the magnitudes of the filtered bytes left and above added to the activity of its table
/// (`buf` holds filtered data, `raw` the raw buffer of [`pixel`]).
#[inline(always)]
fn pixel<const BPP: usize>(raw: &[u8], buf: &[u8], q: usize, at: usize, ch: usize, rs: usize, rs2: usize) -> Pixel {
    let fl = mag(get(buf, at - BPP));
    let fu = mag(get(buf, at - rs));
    let p = pixel::predict::<BPP>(raw, q, rs2, ch);
    Pixel { t: T_LIT + ch.min(3) * ACT + bucket(p.act + fl + fu), pred: p.pred, act: p.act, a: p.a, b: p.b, c: p.c }
}

/// Decodes up to `run` literals from the position (byte `x > 0` of its row, filter `FT`),
/// stopping at a match: returns the literals decoded and, if a match follows (its escape
/// decoded), the activity there. `run` keeps within the row, the block and the data.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
#[allow(clippy::explicit_counter_loop)]
fn lit_run<I: Io, const BPP: usize, const FT: u8>(
    io: &mut I,
    st: &St,
    x: usize,
    run: usize,
    raw: &mut [u8],
    buf: &mut [u8],
    tokens: &mut [Token],
    lf: &mut [u32; 288],
) -> crate::Result<(usize, Option<u32>)> {
    let (rs, rs2) = (st.cur.rs, st.cur.rs2);
    let mut q = st.cur.q0 + x - 1;
    let mut at = st.at(st.p);
    let mut ch = (x - 1) % BPP;
    let mut after = st.after;
    let ti = st.tokens;
    if ti + run > tokens.len() {
        return Err(crate::corrupt());
    }
    // The previous byte's MED and raw value, carried along for the decorrelation.
    let (mut pmed, mut pv) = if BPP >= 3 && q > 0 {
        let (a1, b1, c1) =
            (get(raw, q - 1 - BPP) as i32, get(raw, q - 1 - rs2) as i32, get(raw, q - 1 - rs2 - BPP) as i32);
        (med(a1, b1, c1), get(raw, q - 1) as i32)
    } else {
        (0, 0)
    };
    for i in 0..run {
        let fl = mag(get(buf, at - BPP));
        let fu = mag(get(buf, at - rs));
        let up = q - rs2;
        let (a, b, c) = (get(raw, q - BPP) as i32, get(raw, up) as i32, get(raw, up - BPP) as i32);
        let md = med(a, b, c);
        let pred = if pixel::decorrelated::<BPP>(ch) { (md + pv - pmed).clamp(0, 255) } else { md };
        let act = ((a - c).abs() + (b - c).abs()) as u32;
        let t = T_LIT + ch.min(3) * ACT + bucket(act + fl + fu);
        let s = io.sym(t + after, 0);
        if s == ESC && io.bit(T_ESC, 0) == 1 {
            return Ok((i, Some(act)));
        }
        let v = (pred as u8).wrapping_add(unzigzag(s));
        let m = Pixel { t, pred: pred as u8, act, a: a as u8, b: b as u8, c: c as u8 };
        let f = v.wrapping_sub(m.fp(FT));
        pmed = md;
        pv = v as i32;
        put(raw, q, v);
        put(buf, at, f);
        // SAFETY: `ti + run <= tokens.len()`, checked above.
        unsafe { *tokens.get_unchecked_mut(ti + i) = Token::Lit(f) };
        lf[f as usize] += 1;
        q += 1;
        at += 1;
        ch = if ch + 1 == BPP { 0 } else { ch + 1 };
        after = 0;
    }
    Ok((run, None))
}

/// Length code (0-based) of every match length.
static LEN_CODE: [u8; 259] = {
    let mut t = [0u8; 259];
    let (mut c, mut l) = (0, 3);
    while l <= 258 {
        while c + 1 < 29 && LEN_BASE[c + 1] as usize <= l {
            c += 1;
        }
        t[l] = c as u8;
        l += 1;
    }
    t
};

/// Distance code of distances 1..=256, and of the larger ones by `(dist - 1) >> 7`.
static DIST_CODE: [[u8; 256]; 2] = {
    let mut t = [[0u8; 256]; 2];
    let mut i = 0;
    while i < 256 {
        let (lo, hi) = (i + 1, i * 128 + 1);
        let mut c = 0;
        while c + 1 < 30 && DIST_BASE[c + 1] as usize <= lo {
            c += 1;
        }
        t[0][i] = c as u8;
        let mut c = 0;
        while c + 1 < 30 && DIST_BASE[c + 1] as usize <= hi {
            c += 1;
        }
        t[1][i] = c as u8;
        i += 1;
    }
    t
};

#[inline(always)]
fn len_code(len: u32) -> u32 {
    LEN_CODE[len.min(258) as usize] as u32
}

#[inline(always)]
fn dist_code(d: u32) -> u32 {
    let d = d.clamp(1, 32768) as usize;
    if d <= 256 { DIST_CODE[0][d - 1] as u32 } else { DIST_CODE[1][(d - 1) >> 7] as u32 }
}

/// How the byte at a position is coded: its table, and what turns the symbol into the
/// filtered byte (and the raw one).
#[derive(Clone, Copy)]
struct Lit {
    t: usize,
    /// The local activity (`u32::MAX` at filter bytes and past the image), for match
    /// lengths.
    act: u32,
    /// The prediction of the raw byte, and the row filter's prediction of it.
    pred: u8,
    fp: u8,
    /// The raw offset (image positions with raw-domain prediction), else `usize::MAX`.
    q: usize,
}

impl Lit {
    /// The bucket of match lengths' context.
    #[inline(always)]
    fn len_ctx(&self) -> usize {
        if self.act == u32::MAX { ACT } else { bucket(self.act) }
    }

    /// Stores the filtered byte `f` and its raw value (decoder). `at` is the position's offset
    /// in the padded data (below its length).
    #[inline(always)]
    fn put(&self, f: u8, raw: &mut [u8], buf: &mut [u8], at: usize) {
        if self.q != usize::MAX {
            put(raw, self.q, f.wrapping_add(self.fp));
        }
        put(buf, at, f);
    }

    /// Applies symbol `s`: stores the bytes (decoder) and returns the filtered byte. `at`
    /// is the position's offset in the padded data (below its length).
    #[inline(always)]
    fn finish<const DEC: bool>(&self, s: u32, raw: &mut [u8], buf: &mut [u8], at: usize) -> u8 {
        let v = self.pred.wrapping_add(unzigzag(s));
        let f = v.wrapping_sub(self.fp);
        if DEC {
            if self.q != usize::MAX {
                put(raw, self.q, v);
            }
            put(buf, at, f);
        }
        f
    }
}

/// The coding of position `p` (below the data's length, at `off + p` in `buf`), where the
/// cursor is.
#[inline(always)]
fn prepare<const BPP: usize, const IMAGE: bool>(
    cur: &Cursor,
    raw: &[u8],
    buf: &[u8],
    off: usize,
    p: usize,
    bpp: usize,
) -> Lit {
    let x = p - cur.rstart;
    if x == 0 || cur.rs == usize::MAX {
        return Lit { t: T_FILTER, act: u32::MAX, pred: 0, fp: 0, q: usize::MAX };
    }
    // A pixel byte: the bytes a row up and a pixel left are within the data's padding,
    // and the raw neighbours within the raw buffer.
    let x = x - 1;
    let at = off.wrapping_add(p);
    if IMAGE {
        let q = cur.q0 + x;
        let m = pixel::<BPP>(raw, buf, q, at, x % BPP, cur.rs, cur.rs2);
        let ft = get(buf, off.wrapping_add(cur.rstart));
        Lit { t: m.t, act: m.act, pred: m.pred, fp: m.fp(ft), q }
    } else {
        let (fl, fu) = (mag(get(buf, at - bpp)), mag(get(buf, at - cur.rs)));
        Lit { t: T_LIT + ((x % bpp) & 3) * ACT + bucket(fl + fu), act: fl + fu, pred: 0, fp: 0, q: usize::MAX }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unfiltering in pieces (as matches do) and whole gives what the reference does, for
    /// every filter and pixel size.
    #[test]
    fn unfilter_pieces() {
        let mut seed = 7u32;
        let mut rnd = || {
            seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
            (seed >> 16) as u8
        };
        for (color, ch) in [(0u8, 1usize), (4, 2), (2, 3), (6, 4)] {
            let ih =
                crate::png::Ihdr { width: 37, height: 9, depth: 8, color, compression: 0, filter: 0, interlace: 0 };
            let g = Geometry::new(&ih).unwrap();
            let rs = g.passes[0].rs;
            let mut data: Vec<u8> = (0..g.len).map(|_| rnd()).collect();
            for r in 0..9 {
                data[r * rs] = (r % 5) as u8;
            }
            let mut want = vec![];
            assert!(filter::unfilter(&data, rs - 1, ch, &mut want));
            let mut buf = vec![0u8; g.pad];
            buf.extend_from_slice(&data);
            for piece in [1usize, 2, 3, 5, 7, 64, 1000] {
                let mut raw = vec![0u8; g.raw_len];
                let cur0 = Cursor::new(&g);
                let mut cur = cur0.clone();
                let mut p = 0;
                while p < g.len {
                    cur.seek(&g, p);
                    let len = piece.min(g.len - p);
                    match ch {
                        1 => unfilter_span::<1>(&g, &cur, &buf, g.pad, &mut raw, p, len),
                        2 => unfilter_span::<2>(&g, &cur, &buf, g.pad, &mut raw, p, len),
                        3 => unfilter_span::<3>(&g, &cur, &buf, g.pad, &mut raw, p, len),
                        _ => unfilter_span::<4>(&g, &cur, &buf, g.pad, &mut raw, p, len),
                    }
                    p += len;
                }
                for r in 0..9 {
                    let q = g.passes[0].base2 + (r + 1) * g.passes[0].rs2 + ch;
                    assert_eq!(
                        &raw[q..q + rs - 1],
                        &want[r * (rs - 1)..(r + 1) * (rs - 1)],
                        "ch {ch} piece {piece} row {r}"
                    );
                }
                let mut whole = vec![0u8; g.raw_len];
                unfilter_all(&g, &buf, &mut whole);
                assert_eq!(whole, raw);
            }
        }
    }

    /// A zlib payload of `data` (rows of `rs` bytes, pixels of `bpp`): a greedy parse by
    /// matches at the distances of a row, a pixel and a byte, in one fixed block.
    fn payload(data: &[u8], rs: usize, bpp: usize) -> Vec<u8> {
        use aqr_flate::deflate::{Block, Body, Stream};
        let (mut tokens, mut p) = (vec![], 0);
        while p < data.len() {
            let best = [rs, bpp, 1]
                .into_iter()
                .filter(|&d| d <= p)
                .map(|d| (d, (0..258.min(data.len() - p)).take_while(|&i| data[p + i] == data[p + i - d]).count()))
                .max_by_key(|&(_, n)| n)
                .filter(|&(_, n)| n >= 3);
            match best {
                Some((dist, len)) => {
                    tokens.push(Token::Match { len: len as u16, dist: dist as u16, alt258: false });
                    p += len;
                }
                None => {
                    tokens.push(Token::Lit(data[p]));
                    p += 1;
                }
            }
        }
        let stream = Stream { blocks: vec![Block { last: true, body: Body::Fixed { tokens } }], ..Default::default() };
        let mut z = vec![0x78, 0x01];
        z.extend(aqr_flate::deflate::write(&stream, data).unwrap());
        z.extend([0; 4]);
        z
    }

    /// Against a reference, the image encoder (by rows) and the one of tokens as they come
    /// give the same symbols; and they are fewer than alone.
    #[test]
    fn encoders_against_agree() {
        let ih =
            crate::png::Ihdr { width: 120, height: 80, depth: 8, color: 6, compression: 0, filter: 0, interlace: 0 };
        let g = Geometry::new(&ih).unwrap();
        let rs = g.passes[0].rs;
        let image = |rect: bool| -> Vec<u8> {
            let mut d = vec![];
            for y in 0..80usize {
                d.push((y % 5) as u8);
                for x in 0..480usize {
                    let inside = rect && (20..50).contains(&(x / 4)) && (30..60).contains(&y);
                    d.push(if inside {
                        ((x * 7) ^ (y * 13)) as u8
                    } else {
                        ((x / 4 * 3 + y * 5) / 7 % 251 + (x % 4) * 50) as u8
                    });
                }
            }
            d
        };
        let (base, variant) = (payload(&image(false), rs, 4), payload(&image(true), rs, 4));
        let r = Reference::parse(&base).unwrap();
        let rows = encode_image::<4>(&g, &variant, 1, Some(&r)).unwrap();
        let tokens = encode_bpp::<4, true>(&g, &variant, Some(&r)).unwrap();
        let alone = encode_image::<4>(&g, &variant, 1, None).unwrap();
        let (n, m) = (rows.rec.syms.len(), alone.rec.syms.len());
        assert!(n * 4 < m * 3, "{n} against {m}");
        let (mut a, mut b) = (vec![], vec![]);
        rows.rec.finish(&mut a);
        tokens.rec.finish(&mut b);
        assert!(a == b);
    }

    #[test]
    fn code_tables() {
        for l in 3..=258u16 {
            assert_eq!(super::len_code(l as u32) as usize, aqr_flate::huffman::len_code(l));
        }
        for d in 1..=32768u16 {
            assert_eq!(super::dist_code(d as u32) as usize, aqr_flate::huffman::dist_code(d));
        }
    }
}
