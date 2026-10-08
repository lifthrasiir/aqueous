//! The pixel model: 8-bit planes coded row by row with static tables and rANS.
//!
//! Planes come in *groups*: a layer's color planes with its transparency, or a single plane.
//! Rows are coded in order, and within a row the alpha plane (if any) first, then the color
//! planes; the model only looks at the current and previous rows, so memory does not depend
//! on the image size. Where the left neighbor equals a long stretch of the row above, a run
//! of samples equal to it is coded by its length (JPEG-LS style). Otherwise a sample is
//! predicted by MED from its neighbors; for a visible pixel, neighbors that are fully
//! transparent are replaced by a visible one, since their color is arbitrary. Color planes
//! after the first code their residual relative to the first plane's. The table is chosen by
//! plane kind, transparency of the pixel, local activity and (after the first color plane) the
//! first plane's residual.
//!
//! A group may come with a *hint*, a prediction of its planes (the composite image rendered
//! from the layers): where the left and upper samples equal the hint's, the difference to the
//! hint is coded instead, in tables of its own.
//!
//! Symbols go to a [`Coder`]: the encoder ([`Recorder`]) records the symbol of the known
//! sample, the decoder ([`Player`]) decodes one and the sample is rebuilt. The tables are
//! static per chunk of [`CHUNK`] symbols: counted, written, then the chunk coded with rANS,
//! which bounds the encoder's buffer.

use png_recons::rans::{self, BitR, BitW, Decoder, Tables};

use crate::psd::{Error, Result};

const ACT: usize = 8;
const CROSS: usize = 4;
const CTX: usize = ACT * CROSS;
/// Plane kinds: first color plane, other color planes, alpha, lone plane.
const KINDS: usize = 4;
/// Run mode needs more than this many equal samples above, from the upper left on (shorter
/// runs cost more than they save).
const RUN_LOOK: u32 = 32;
/// Run length tables, by plane kind and transparency.
const RUN_BASE: usize = KINDS * 2 * CTX;
/// Tables of the difference to the hint where the left and upper samples matched it, by
/// plane kind and whether the upper diagonal ones did.
const MATCH_BASE: usize = RUN_BASE + KINDS * 2;
pub const TABLES: usize = MATCH_BASE + KINDS * 4;
/// Symbols per chunk of the entropy coded stream.
pub const CHUNK: usize = 1 << 22;

fn corrupt() -> Error {
    Error("corrupt pixel data".into())
}

pub trait Coder {
    /// The residual symbol of the current sample in table `t`; `sym` is the encoder's.
    fn code(&mut self, t: usize, sym: u8) -> u8;
}

/// Gives back the encoder's symbols: walks planes that are not coded, for their residuals.
struct Passthrough;

impl Coder for Passthrough {
    #[inline(always)]
    fn code(&mut self, _t: usize, sym: u8) -> u8 {
        sym
    }
}

#[inline(always)]
fn med(a: u8, b: u8, c: u8) -> u8 {
    let (lo, hi) = (a.min(b), a.max(b));
    if c >= hi {
        lo
    } else if c <= lo {
        hi
    } else {
        a.wrapping_add(b).wrapping_sub(c)
    }
}

#[inline(always)]
fn bucket(v: u32, n: usize) -> usize {
    ((32 - v.leading_zeros()) as usize).min(n - 1)
}

#[inline(always)]
fn mag(r: u8) -> u32 {
    (r as i8).unsigned_abs() as u32
}

/// The rows a plane row is coded with.
struct RowCtx<'a> {
    kind: usize,
    /// Whether this is the first row (no row above).
    top: bool,
    prev: &'a [u8],
    /// The alpha plane's current and previous rows, for a color plane of a group with alpha.
    alpha: Option<(&'a [u8], &'a [u8])>,
    res_prev: &'a [u8],
    /// The first color plane's residuals of this row, for the planes after it.
    first: Option<&'a [u8]>,
    /// The hint's current and previous rows.
    hint: Option<(&'a [u8], &'a [u8])>,
}

/// Codes one row `p` of a plane, writing its residuals to `res`.
fn code_row<C: Coder>(c: &mut C, r: &RowCtx, p: &mut [u8], res: &mut [u8], up_run: &mut [u32]) {
    let w = p.len();
    let (kind, top, up) = (r.kind, r.top, r.prev);
    let run_t = RUN_BASE + kind * 2;
    // For the row above: how many equal samples follow each position (itself included).
    if !top {
        up_run[w - 1] = 1;
        for x in (0..w - 1).rev() {
            up_run[x] = if up[x + 1] == up[x] { up_run[x + 1] + 1 } else { 1 };
        }
    }
    let mut x = 0;
    let mut after_run = false;
    while x < w {
        // Run mode: in a flat neighborhood, the number of following samples equal to the left
        // one, in chunks of up to 254 (255 continues).
        if !after_run && !top && x > 0 && up_run[x - 1] > RUN_LOOK {
            let v = p[x - 1];
            if up[x - 1] == v {
                let t = run_t + r.alpha.is_some_and(|(al, _)| al[x] == 0) as usize;
                let mut n = 0;
                loop {
                    let avail = p[x + n..].iter().take(255).take_while(|&&b| b == v).count();
                    let s = (c.code(t, avail as u8) as usize).min(w - x - n);
                    p[x + n..x + n + s].fill(v);
                    n += s;
                    if s < 255 {
                        break;
                    }
                }
                res[x..x + n].fill(0);
                x += n;
                after_run = true;
                continue;
            }
        }
        after_run = false;
        let mut a = if x > 0 { p[x - 1] } else if !top { up[x] } else { 0 };
        let mut b = if !top { up[x] } else { a };
        let mut cc = if x > 0 && !top { up[x - 1] } else { b };
        let d = if !top && x + 1 < w { up[x + 1] } else { b };
        let mut transparent = false;
        if let Some((al, alp)) = r.alpha {
            if al[x] == 0 {
                transparent = true;
            } else {
                let jw = (x > 0 && al[x - 1] > 0).then(|| p[x - 1]);
                let jn = (!top && alp[x] > 0).then(|| up[x]);
                let jnw = (x > 0 && !top && alp[x - 1] > 0).then(|| up[x - 1]);
                if let Some(fb) = jw.or(jn).or(jnw) {
                    a = jw.unwrap_or(fb);
                    b = jn.unwrap_or(fb);
                    cc = jnw.unwrap_or(fb);
                }
            }
        }
        let pred = med(a, b, cc);
        if let Some((hc, hp)) = r.hint {
            if x > 0 && !top && p[x - 1] == hc[x - 1] && up[x] == hp[x] {
                let m = (up[x - 1] == hp[x - 1]) as usize + 2 * (x + 1 < w && up[x + 1] == hp[x + 1]) as usize;
                let s = c.code(MATCH_BASE + kind * 4 + m, p[x].wrapping_sub(hc[x]));
                p[x] = hc[x].wrapping_add(s);
                res[x] = p[x].wrapping_sub(pred);
                x += 1;
                continue;
            }
        }
        let rw = if x > 0 { mag(res[x - 1]) } else { 0 };
        let rn = if !top { mag(r.res_prev[x]) } else { 0 };
        let act = a.abs_diff(cc) as u32 + b.abs_diff(cc) as u32 + b.abs_diff(d) as u32;
        let (off, cross) = match r.first {
            Some(f) => (f[x], bucket(mag(f[x]), CROSS)),
            None => (0, 0),
        };
        let ctx = bucket(act + 2 * (rw + rn), ACT) * CROSS + cross;
        let t = (kind * 2 + transparent as usize) * CTX + ctx;
        let rr = p[x].wrapping_sub(pred);
        let s = c.code(t, rr.wrapping_sub(off));
        let rr = s.wrapping_add(off);
        res[x] = rr;
        p[x] = pred.wrapping_add(rr);
        x += 1;
    }
}

/// The state of a group being coded row by row: planes of `w` samples, alpha (if any) last.
pub struct GroupCoder {
    w: usize,
    has_alpha: bool,
    /// Planes not coded: their rows are given (copies of planes coded before).
    known: Vec<bool>,
    /// Planes equal to another plane coded before them in a row; not coded.
    same_as: Vec<Option<usize>>,
    top: bool,
    prev: Vec<Vec<u8>>,
    res_prev: Vec<Vec<u8>>,
    res: Vec<Vec<u8>>,
    hint_prev: Vec<Vec<u8>>,
    up_run: Vec<u32>,
}

impl GroupCoder {
    pub fn new(w: usize, planes: usize, has_alpha: bool, known: Vec<bool>, same_as: Vec<Option<usize>>) -> GroupCoder {
        GroupCoder {
            w,
            has_alpha,
            known,
            same_as,
            top: true,
            prev: vec![vec![0; w]; planes],
            res_prev: vec![vec![0; w]; planes],
            res: vec![vec![0; w]; planes],
            hint_prev: vec![vec![0; w]; planes],
            up_run: vec![0; w],
        }
    }

    /// Planes in coding order within a row: alpha (if any), then the color planes.
    pub fn order(planes: usize, has_alpha: bool) -> Vec<usize> {
        let colors = planes - has_alpha as usize;
        let mut o = Vec::with_capacity(planes);
        if has_alpha {
            o.push(colors);
        }
        o.extend(0..colors);
        o
    }

    /// Whether plane `k` is coded (rather than known or a copy of another).
    pub fn coded(&self, k: usize) -> bool {
        !self.known[k] && self.same_as[k].is_none()
    }

    /// Codes the next row: `rows` holds one row per plane (the encoder's samples, or for the
    /// decoder the known planes' and room for the others), `hint` the hint's rows.
    pub fn code_row<C: Coder>(&mut self, c: &mut C, rows: &mut [Vec<u8>], hint: Option<&[Vec<u8>]>) {
        let n = rows.len();
        let colors = n - self.has_alpha as usize;
        let lone = colors == 1 && !self.has_alpha;
        for k in Self::order(n, self.has_alpha) {
            if let Some(j) = self.same_as[k] {
                let (a, b) = if j < k { let (a, b) = rows.split_at_mut(k); (&a[j], &mut b[0]) } else { let (a, b) = rows.split_at_mut(j); (&b[0], &mut a[k]) };
                b.copy_from_slice(a);
            }
            let is_alpha = self.has_alpha && k == colors;
            let kind = if lone { 3 } else if is_alpha { 2 } else if k == 0 { 0 } else { 1 };
            let coded = self.coded(k);
            // Planes not coded are walked only for the first color plane's residuals.
            if !coded && !(kind == 0 && colors > 1) {
                continue;
            }
            let (alpha, rest) = if self.has_alpha && !is_alpha {
                let (r, a) = rows.split_at_mut(colors);
                (Some((&a[0][..], &self.prev[colors][..])), r)
            } else {
                (None, &mut rows[..])
            };
            let (res_k, first) = if kind == 1 {
                let (f, r) = self.res.split_at_mut(k);
                (&mut r[0], Some(&f[0][..]))
            } else {
                (&mut self.res[k], None)
            };
            let ctx = RowCtx {
                kind,
                top: self.top,
                prev: &self.prev[k],
                alpha,
                res_prev: &self.res_prev[k],
                first,
                hint: hint.map(|h| (&h[k][..], &self.hint_prev[k][..])),
            };
            let p = &mut rest[k][..self.w];
            if coded {
                code_row(c, &ctx, p, res_k, &mut self.up_run);
            } else {
                code_row(&mut Passthrough, &ctx, p, res_k, &mut self.up_run);
            }
        }
        for k in 0..n {
            self.prev[k].copy_from_slice(&rows[k][..self.w]);
            std::mem::swap(&mut self.res_prev[k], &mut self.res[k]);
            if let Some(h) = hint {
                self.hint_prev[k].copy_from_slice(&h[k][..self.w]);
            }
        }
        self.top = false;
    }
}

/// Records symbols, and codes them a chunk at a time into `out`.
pub struct Recorder<'o> {
    syms: Vec<(u16, u16)>,
    out: &'o mut Vec<u8>,
    pub chunks: usize,
}

impl<'o> Recorder<'o> {
    pub fn new(out: &'o mut Vec<u8>) -> Self {
        Recorder { syms: Vec::with_capacity(CHUNK), out, chunks: 0 }
    }

    /// Writes the recorded symbols as a chunk: symbol count, tables, rANS stream.
    fn flush(&mut self) {
        if self.syms.is_empty() {
            return;
        }
        let mut counts = vec![[0u32; 256]; TABLES];
        for &(t, s) in &self.syms {
            counts[t as usize][s as usize] += 1;
        }
        let mut w = BitW::default();
        let mut freqs = Vec::with_capacity(TABLES);
        for c in &counts {
            let used = c.iter().any(|&n| n > 0);
            w.bits(1, used as u32);
            freqs.push(if used { rans::write_table(&mut w, c) } else { unused() });
        }
        let head = w.finish();
        let body = rans::encode(&self.syms, &freqs);
        let o = &mut *self.out;
        crate::container::put_varint(o, self.syms.len() as u64);
        crate::container::put_varint(o, head.len() as u64);
        o.extend_from_slice(&head);
        crate::container::put_varint(o, body.len() as u64);
        o.extend_from_slice(&body);
        self.syms.clear();
        self.chunks += 1;
    }

    /// Writes what is left; the stream ends with a chunk of no symbols.
    pub fn finish(mut self) {
        self.flush();
        crate::container::put_varint(self.out, 0);
    }
}

impl Coder for Recorder<'_> {
    #[inline(always)]
    fn code(&mut self, t: usize, sym: u8) -> u8 {
        if self.syms.len() == CHUNK {
            self.flush();
        }
        self.syms.push((t as u16, sym as u16));
        sym
    }
}

fn unused() -> Vec<u32> {
    let mut f = vec![0u32; 256];
    f[0] = rans::PROB_SCALE;
    f
}

/// Decodes symbols chunk by chunk.
pub struct Player<'a> {
    src: &'a [u8],
    at: usize,
    tables: Tables,
    dec: Decoder<'a>,
    left: usize,
    /// Set when the stream turns out corrupt; decoding then yields zeros.
    pub failed: bool,
    ended: bool,
}

impl<'a> Player<'a> {
    pub fn new(src: &'a [u8]) -> Player<'a> {
        let mut p = Player { src, at: 0, tables: Tables::new(&[]), dec: Decoder::new(&[]), left: 0, failed: false, ended: false };
        p.next_chunk();
        p
    }

    fn next_chunk(&mut self) {
        let r = (|| -> Result<()> {
            let src = self.src;
            let n = crate::container::get_varint(src, &mut self.at)? as usize;
            if n == 0 {
                self.ended = true;
                return Ok(());
            }
            let hl = crate::container::get_varint(src, &mut self.at)? as usize;
            let head = src.get(self.at..self.at.checked_add(hl).ok_or_else(corrupt)?).ok_or_else(corrupt)?;
            self.at += hl;
            let mut r = BitR::new(head);
            let mut freqs = Vec::with_capacity(TABLES);
            for _ in 0..TABLES {
                freqs.push(if r.bits(1) == 1 { rans::read_table(&mut r, 256).map_err(|_| corrupt())? } else { unused() });
            }
            if r.overrun() {
                return Err(corrupt());
            }
            let bl = crate::container::get_varint(src, &mut self.at)? as usize;
            let body = src.get(self.at..self.at.checked_add(bl).ok_or_else(corrupt)?).ok_or_else(corrupt)?;
            self.at += bl;
            self.tables = Tables::new(&freqs);
            self.dec = Decoder::new(body);
            self.left = n;
            Ok(())
        })();
        if r.is_err() {
            self.failed = true;
        }
    }

    /// Whether every chunk was consumed exactly; the stream's length in `src`.
    pub fn finish(mut self) -> Result<usize> {
        if self.failed || self.left != 0 || !self.dec.finished() {
            return Err(corrupt());
        }
        if !self.ended {
            self.next_chunk();
        }
        if self.failed || !self.ended {
            return Err(corrupt());
        }
        Ok(self.at)
    }
}

impl Coder for Player<'_> {
    #[inline(always)]
    fn code(&mut self, t: usize, _sym: u8) -> u8 {
        if self.left == 0 {
            if self.ended || self.failed || !self.dec.finished() {
                self.failed = true;
                return 0;
            }
            self.next_chunk();
            if self.left == 0 {
                self.failed = true;
                return 0;
            }
        }
        self.left -= 1;
        self.dec.get(&self.tables, t) as u8
    }
}
