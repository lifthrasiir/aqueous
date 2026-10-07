//! The token model: the DEFLATE stream's own LZ77 tokens, coded against the image.
//!
//! The parse is kept as it is (so rebuilding the stream needs no match search), but every
//! symbol is coded with static per-file tables chosen by image context:
//! - whether the token is a match, by the previous two token kinds and the local activity;
//! - literals as the residual of the raw (unfiltered) pixel against a MED prediction with
//!   inter-channel decorrelation, by channel and activity. The PNG filter of the row is
//!   reapplied to give the literal byte;
//! - match lengths by activity, distances by length.
//!
//! The same function runs both ways (`Io::sym` records a symbol when encoding and returns
//! the decoded one when decoding), so the two sides cannot drift apart.
//!
//! Buffers: the filtered data has `Geometry::pad` zero bytes in front, so that the bytes
//! one row up and one pixel left are always in bounds. The raw (unfiltered) image is kept
//! in its own layout: every pass has a zero row on top and every row `bpp` zero bytes on
//! the left, which is exactly what the PNG filters assume outside the image.

use crate::filter;
use crate::rans::{self, BitR, BitW, Decoder, Tables};
use flate_recons::deflate::Token;
use flate_recons::fast::inflate::{self, Layout, Sink};
use flate_recons::huffman::{DIST_BASE, DIST_EXTRA, LEN_BASE, LEN_EXTRA};

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
const ACT: usize = 12;
const T_LIT: usize = 0;
/// Literal tables by (previous token a match, channel, activity).
const T_FILTER: usize = T_LIT + 8 * ACT;
const T_ESC: usize = T_FILTER + 1;
const T_LEN: usize = T_ESC + 1;
const T_DIST: usize = T_LEN + ACT + 1;
pub const TABLES: usize = T_DIST + 6;
const ESC: u32 = 255;

fn alphabet(t: usize) -> usize {
    if t < T_ESC {
        256
    } else if t == T_ESC {
        2
    } else if t < T_DIST {
        29
    } else {
        30
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

pub struct Recorder {
    pub syms: Vec<(u16, u16)>,
    /// Per table, 256 counts.
    pub counts: Vec<u32>,
    pub raw: BitW,
}

impl Recorder {
    pub fn new(cap: usize) -> Recorder {
        Recorder { syms: Vec::with_capacity(cap), counts: vec![0; TABLES * 256], raw: BitW::default() }
    }

    /// Tables, then the rANS stream, then the raw bits.
    pub fn finish(self, out: &mut Vec<u8>) {
        let mut w = BitW::default();
        // A bitmap of the tables in use, then their levels.
        let counts: Vec<&[u32]> = (0..TABLES).map(|t| &self.counts[t * 256..t * 256 + alphabet(t)]).collect();
        let used: Vec<bool> = counts.iter().map(|c| c.iter().any(|&x| x > 0)).collect();
        for &u in &used {
            w.bits(1, u as u32);
        }
        let freqs: Vec<Vec<u32>> = (0..TABLES)
            .map(|t| if used[t] { rans::write_table(&mut w, counts[t]) } else { vec![rans::PROB_SCALE] })
            .collect();
        let tb = w.finish();
        let st = rans::encode(&self.syms, &freqs);
        let raw = self.raw.finish();
        for part in [&tb, &st, &raw] {
            crate::container::put_varint(out, part.len() as u64);
        }
        out.extend_from_slice(&tb);
        out.extend_from_slice(&st);
        out.extend_from_slice(&raw);
    }
}

impl Io for Recorder {
    #[inline(always)]
    fn sym(&mut self, t: usize, v: u32) -> u32 {
        self.syms.push((t as u16, v as u16));
        self.counts[t * 256 + v as usize] += 1;
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
    pub fn new(src: &'a [u8], at: &mut usize) -> crate::Result<Player<'a>> {
        let mut lens = [0usize; 3];
        for l in &mut lens {
            *l = crate::container::get_varint(src, at)? as usize;
        }
        let mut part = |n: usize| -> crate::Result<&'a [u8]> {
            let s = src.get(*at..at.checked_add(n).ok_or_else(crate::corrupt)?).ok_or_else(crate::corrupt)?;
            *at += n;
            Ok(s)
        };
        let (tb, st, raw) = (part(lens[0])?, part(lens[1])?, part(lens[2])?);
        let mut r = BitR::new(tb);
        let used: Vec<bool> = (0..TABLES).map(|_| r.bits(1) == 1).collect();
        let mut freqs = Vec::with_capacity(TABLES);
        for (t, &u) in used.iter().enumerate() {
            freqs.push(if u { rans::read_table(&mut r, alphabet(t))? } else { vec![rans::PROB_SCALE] });
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

#[inline(always)]
fn bucket(v: u32) -> usize {
    // 0, 1, 2-3, 4-7, ..., >= 1024 -> 11
    ((32 - v.leading_zeros()) as usize).min(ACT - 1)
}

#[inline(always)]
fn mag(x: u8) -> u32 {
    (x as i8).unsigned_abs() as u32
}

#[inline(always)]
fn zigzag(e: u8) -> u32 {
    let e = e as i8;
    ((e << 1) ^ (e >> 7)) as u8 as u32
}

#[inline(always)]
fn unzigzag(z: u32) -> u8 {
    ((z >> 1) as u8) ^ ((z & 1) as u8).wrapping_neg()
}

/// LOCO-I's median edge detector: the median of `a`, `b` and `a + b - c`, branch-free.
#[inline(always)]
fn med(a: i32, b: i32, c: i32) -> i32 {
    (a + b - c).max(a.min(b)).min(a.max(b))
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
}

impl Cursor {
    fn new(g: &Geometry) -> Cursor {
        let mut c = Cursor { pass: usize::MAX, pend: 0, rstart: 0, rend: 0, rs: 0, q0: 0, rs2: 0 };
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
                self.q0 = ps.base2 + ps.rs2 + g.bpp;
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
fn unfilter_part<const BPP: usize>(ft: u8, d: &[u8], raw: &mut [u8], q: usize, rs2: usize, x0: usize, x1: usize, bpp: usize) {
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
    while x < x1 && x % BPP != 0 {
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

/// Unfilters the `len` bytes at `p`, where the cursor is.
#[inline(always)]
fn unfilter_span<const BPP: usize>(g: &Geometry, cur: &Cursor, buf: &[u8], raw: &mut [u8], p: usize, len: usize) {
    let end = (p + len).min(g.len);
    let x = p - cur.rstart;
    if x > 0 && end <= cur.rend {
        // Within the row (the usual case).
        let d = &buf[g.pad + cur.rstart + 1..g.pad + cur.rend];
        unfilter_part::<BPP>(buf[g.pad + cur.rstart], d, raw, cur.q0, cur.rs2, x - 1, end - cur.rstart - 1, g.bpp);
    } else if p < end {
        let mut c = cur.clone();
        let mut i = p;
        while i < end {
            c.seek(g, i);
            if i == c.rstart {
                i += 1;
                continue;
            }
            let x1 = end.min(c.rend) - c.rstart - 1;
            let d = &buf[g.pad + c.rstart + 1..g.pad + c.rend];
            unfilter_part::<BPP>(buf[g.pad + c.rstart], d, raw, c.q0, c.rs2, i - c.rstart - 1, x1, g.bpp);
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
}

impl<'g> St<'g> {
    fn new(g: &'g Geometry) -> St<'g> {
        St { g, cur: Cursor::new(g), p: 0, after: 0, tokens: 0 }
    }

    #[inline(always)]
    fn prep<const BPP: usize, const IMAGE: bool>(&mut self, raw: &[u8], buf: &[u8], token: bool) -> Lit {
        self.cur.seek(self.g, self.p);
        let bpp = if IMAGE { BPP } else { self.g.bpp };
        let mut l = prepare::<BPP, IMAGE>(self.g, &self.cur, raw, buf, self.p, bpp);
        if token && l.t < T_FILTER {
            l.t += self.after;
        }
        l
    }

    /// Copies a match in the data (and unfilters it).
    #[inline(always)]
    fn copy<const BPP: usize, const IMAGE: bool>(&mut self, raw: &mut [u8], buf: &mut [u8], len: usize, dist: usize) {
        copy_match(buf, self.g.pad + self.p, dist, len);
        if IMAGE {
            unfilter_span::<BPP>(self.g, &self.cur, buf, raw, self.p, len);
        }
        self.p += len;
        self.after = 4 * ACT;
        self.tokens += 1;
    }
}

/// Writes the `len` bytes `dist` back at `d` (an LZ77 copy, overlapping if `dist < len`).
#[inline(always)]
fn copy_match(buf: &mut [u8], d: usize, dist: usize, len: usize) {
    let s = d - dist;
    if dist >= len {
        if len <= 16 {
            for i in 0..len {
                buf[d + i] = buf[s + i];
            }
        } else {
            buf.copy_within(s..s + len, d);
        }
    } else if dist == 1 {
        let b = buf[s];
        buf[d..d + len].fill(b);
    } else {
        // A pattern of period `dist`: copy it, then double what is written (always a
        // multiple of the period).
        buf.copy_within(s..d, d);
        let mut done = dist;
        while done < len {
            let k = done.min(len - done);
            buf.copy_within(d..d + k, d + done);
            done += k;
        }
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
}

impl<const BPP: usize, const IMAGE: bool> Enc<'_, BPP, IMAGE> {
    /// Room for `k` bytes past the current position.
    #[inline(always)]
    fn room(&mut self, k: usize) {
        let need = self.st.g.pad + self.st.p + k;
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
        let s = zigzag(v.wrapping_sub(l.pred));
        self.io.sym(l.t, s);
        if token && s == ESC {
            self.io.bit(T_ESC, 0);
        }
        if l.q != usize::MAX {
            self.raw[l.q] = v;
        }
        self.buf[self.st.g.pad + self.st.p] = f;
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
        // Three literals or a match.
        self.room(MAX_MATCH);
    }

    #[inline(always)]
    fn lit(&mut self, b: u8) {
        Enc::lit(self, b, true);
    }

    #[inline(always)]
    fn mat(&mut self, len: u16, dist: u16, alt258: bool) {
        let l = self.st.prep::<BPP, IMAGE>(&self.raw, &self.buf, true);
        self.io.sym(l.t, ESC);
        self.io.bit(T_ESC, 1);
        code_match(&mut self.io, l.len_ctx(), len as u32, dist as u32);
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

/// What the compressor gets: the symbols, the stream's structure, the data size and the
/// exceptions.
pub struct Encoded {
    pub rec: Recorder,
    pub layout: Layout,
    pub len: usize,
    pub alts: Vec<usize>,
}

const MAX_MATCH: usize = 258;

/// Parses and models the raw DEFLATE stream `deflate` of an image with geometry `g`.
pub fn encode(kind: Kind, g: &Geometry, deflate: &[u8]) -> crate::Result<Encoded> {
    match (kind, g.bpp) {
        (Kind::Image, 1) => encode_bpp::<1, true>(g, deflate),
        (Kind::Image, 2) => encode_bpp::<2, true>(g, deflate),
        (Kind::Image, 3) => encode_bpp::<3, true>(g, deflate),
        (Kind::Image, 4) => encode_bpp::<4, true>(g, deflate),
        (Kind::Image, _) => Err(crate::Error::Unsupported("pixel size")),
        (Kind::Bytes, _) => encode_bpp::<0, false>(g, deflate),
    }
}

fn encode_bpp<const BPP: usize, const IMAGE: bool>(g: &Geometry, deflate: &[u8]) -> crate::Result<Encoded> {
    let mut e = Enc::<BPP, IMAGE> {
        io: Recorder::new(g.len / 2 + 1024),
        st: St::new(g),
        buf: vec![0u8; g.pad + g.len + MAX_MATCH],
        raw: if IMAGE { vec![0u8; g.raw_len] } else { vec![] },
        alts: vec![],
    };
    let layout = inflate::parse_into(deflate, usize::MAX, &mut e)?;
    Ok(Encoded { rec: e.io, layout, len: e.st.p, alts: e.alts })
}

/// Rebuilds the data and tokens of `spans` (the decompressor's side); `buf` is the padded
/// data, `raw` `g.raw_len` zeros (for `Kind::Image`).
/// Symbol frequencies of a Huffman block as `flate_recons::fast::header` counts them.
pub type Freqs = ([u32; 288], [u32; 32]);

/// Rebuilds the data and tokens of `spans`, and per span the symbol frequencies (zeros for
/// stored blocks; matches of length 258 counted as symbol 285).
#[allow(clippy::too_many_arguments)]
pub fn decode(
    player: &Player,
    kind: Kind,
    g: &Geometry,
    spans: &[Span],
    tokens: &mut [Token],
    freqs: &mut Vec<Freqs>,
    buf: &mut [u8],
    raw: &mut [u8],
) -> crate::Result<()> {
    match (kind, g.bpp) {
        (Kind::Image, 1) => decode_bpp::<1, true>(player, g, spans, tokens, freqs, buf, raw),
        (Kind::Image, 2) => decode_bpp::<2, true>(player, g, spans, tokens, freqs, buf, raw),
        (Kind::Image, 3) => decode_bpp::<3, true>(player, g, spans, tokens, freqs, buf, raw),
        (Kind::Image, 4) => decode_bpp::<4, true>(player, g, spans, tokens, freqs, buf, raw),
        (Kind::Image, _) => Err(crate::corrupt()),
        (Kind::Bytes, _) => decode_bpp::<0, false>(player, g, spans, tokens, freqs, buf, raw),
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

fn decode_bpp<const BPP: usize, const IMAGE: bool>(
    player: &Player,
    g: &Geometry,
    spans: &[Span],
    tokens: &mut [Token],
    freqs: &mut Vec<Freqs>,
    buf: &mut [u8],
    raw: &mut [u8],
) -> crate::Result<()> {
    let mut live = Live { tables: &player.tables, dec: player.dec, raw: player.raw };
    decode_loop::<Live, BPP, IMAGE>(&mut live, g, spans, tokens, freqs, buf, raw)?;
    if !live.dec.finished() || live.raw.overrun() {
        return Err(crate::corrupt());
    }
    Ok(())
}

#[inline(always)]
fn decode_loop<I: Io, const BPP: usize, const IMAGE: bool>(
    io: &mut I,
    g: &Geometry,
    spans: &[Span],
    tokens: &mut [Token],
    freqs: &mut Vec<Freqs>,
    buf: &mut [u8],
    raw: &mut [u8],
) -> crate::Result<()> {
    let n = buf.len() - g.pad;
    let mut st = St::new(g);
    for &span in spans {
        match span {
            Span::Stored(len) => {
                freqs.push(([0; 288], [0; 32]));
                if len > n - st.p {
                    return Err(crate::corrupt());
                }
                for _ in 0..len {
                    let l = st.prep::<BPP, IMAGE>(raw, buf, false);
                    let s = io.sym(l.t, 0);
                    l.finish::<true>(s, raw, buf, g.pad + st.p);
                    st.p += 1;
                }
            }
            Span::Tokens(count) => {
                let (mut lf, mut df) = ([0u32; 288], [0u32; 32]);
                lf[256] = 1;
                for _ in 0..count {
                    if st.p >= n {
                        return Err(crate::corrupt());
                    }
                    let l = st.prep::<BPP, IMAGE>(raw, buf, true);
                    let s = io.sym(l.t, 0);
                    if s != ESC || io.bit(T_ESC, 0) == 0 {
                        let v = l.finish::<true>(s, raw, buf, g.pad + st.p);
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
                        *tokens.get_mut(st.tokens).ok_or_else(crate::corrupt)? = Token::Match { len: len as u16, dist: dist as u16, alt258: false };
                        lf[257 + len_code(len as u32) as usize] += 1;
                        df[dist_code(dist as u32) as usize] += 1;
                        st.copy::<BPP, IMAGE>(raw, buf, len, dist);
                    }
                }
                freqs.push((lf, df));
            }
        }
    }
    if st.p != n {
        return Err(crate::corrupt());
    }
    Ok(())
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

/// `s[i]` without a bounds check, for the offsets the geometry keeps in bounds (see the
/// callers): the data has `Geometry::pad` bytes in front, the raw buffer its margin, zero
/// row and padding.
#[inline(always)]
fn get(s: &[u8], i: usize) -> u8 {
    debug_assert!(i < s.len());
    // SAFETY: see above; checked in debug builds.
    unsafe { *s.get_unchecked(i) }
}

#[inline(always)]
fn put(s: &mut [u8], i: usize, v: u8) {
    debug_assert!(i < s.len());
    // SAFETY: as in `get`.
    unsafe { *s.get_unchecked_mut(i) = v }
}

impl Lit {
    /// The bucket of match lengths' context.
    #[inline(always)]
    fn len_ctx(&self) -> usize {
        if self.act == u32::MAX { ACT } else { bucket(self.act) }
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

/// The coding of position `p` (below the data's length), where the cursor is.
#[inline(always)]
fn prepare<const BPP: usize, const IMAGE: bool>(g: &Geometry, cur: &Cursor, raw: &[u8], buf: &[u8], p: usize, bpp: usize) -> Lit {
    let x = p - cur.rstart;
    if x == 0 || cur.rs == usize::MAX {
        return Lit { t: T_FILTER, act: u32::MAX, pred: 0, fp: 0, q: usize::MAX };
    }
    // A pixel byte: the bytes a row up and a pixel left are within the data's padding,
    // and the raw neighbours within the raw buffer.
    let x = x - 1;
    let at = g.pad + p;
    let fl = mag(get(buf, at - bpp));
    let fu = mag(get(buf, at - cur.rs));
    if IMAGE {
        let q = cur.q0 + x;
        let up = q - cur.rs2;
        let (a, b, c) = (get(raw, q - BPP) as i32, get(raw, up) as i32, get(raw, up - BPP) as i32);
        let ch = x % BPP;
        let mut pred = med(a, b, c);
        if BPP >= 3 && (ch == 1 || ch == 2) {
            // Green and blue add the previous channel's residual.
            let (a1, b1, c1) = (get(raw, q - 1 - BPP) as i32, get(raw, up - 1) as i32, get(raw, up - 1 - BPP) as i32);
            pred = (pred + get(raw, q - 1) as i32 - med(a1, b1, c1)).clamp(0, 255);
        }
        let act = ((a - c).abs() + (b - c).abs()) as u32;
        let ft = get(buf, g.pad + cur.rstart);
        Lit {
            t: T_LIT + ch.min(3) * ACT + bucket(act + fl + fu),
            act,
            pred: pred as u8,
            fp: filter::predict(ft, a as u8, b as u8, c as u8),
            q,
        }
    } else {
        Lit { t: T_LIT + (x % bpp & 3) * ACT + bucket(fl + fu), act: fl + fu, pred: 0, fp: 0, q: usize::MAX }
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
            let ih = crate::png::Ihdr { width: 37, height: 9, depth: 8, color, compression: 0, filter: 0, interlace: 0 };
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
                        1 => unfilter_span::<1>(&g, &cur, &buf, &mut raw, p, len),
                        2 => unfilter_span::<2>(&g, &cur, &buf, &mut raw, p, len),
                        3 => unfilter_span::<3>(&g, &cur, &buf, &mut raw, p, len),
                        _ => unfilter_span::<4>(&g, &cur, &buf, &mut raw, p, len),
                    }
                    p += len;
                }
                for r in 0..9 {
                    let q = g.passes[0].base2 + (r + 1) * g.passes[0].rs2 + ch;
                    assert_eq!(&raw[q..q + rs - 1], &want[r * (rs - 1)..(r + 1) * (rs - 1)], "ch {ch} piece {piece} row {r}");
                }
                let mut whole = vec![0u8; g.raw_len];
                unfilter_all(&g, &buf, &mut whole);
                assert_eq!(whole, raw);
            }
        }
    }

    #[test]
    fn copies() {
        for dist in 1..40 {
            for len in [3, 4, 15, 16, 17, 31, 100, 258] {
                let mut a: Vec<u8> = (0..400u32).map(|i| (i * 37 % 251) as u8).collect();
                let mut b = a.clone();
                copy_match(&mut a, 50, dist, len);
                for i in 50..50 + len {
                    b[i] = b[i - dist];
                }
                assert_eq!(a, b, "dist {dist} len {len}");
            }
        }
    }

    #[test]
    fn code_tables() {
        for l in 3..=258u16 {
            assert_eq!(super::len_code(l as u32) as usize, flate_recons::huffman::len_code(l));
        }
        for d in 1..=32768u16 {
            assert_eq!(super::dist_code(d as u32) as usize, flate_recons::huffman::dist_code(d));
        }
    }
}
