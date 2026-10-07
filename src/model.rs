//! The pixel model: 8-bit planes coded with static per-file tables and rANS.
//!
//! Planes come in *groups*: a layer's color planes with its transparency, or a single plane.
//! Within a group the alpha plane (if any) is coded first, then the color planes. Every
//! sample is predicted by MED from its neighbors; for a visible pixel, neighbors that are
//! fully transparent are replaced by a visible one, since their color is arbitrary. Color
//! planes after the first code their residual relative to the first plane's. The table is
//! chosen by plane kind, transparency of the pixel, local activity and (after the first
//! color plane) the first plane's residual.
//!
//! Encoder and decoder share [`walk`], which predicts each sample and asks a [`Coder`] for
//! its residual symbol: the encoder records the symbol of the known sample, the decoder
//! decodes one and writes the sample.

use png_recons::rans::{self, BitR, BitW, Decoder, Tables};

use crate::psd::{Error, Result};

const ACT: usize = 8;
const CROSS: usize = 4;
const CTX: usize = ACT * CROSS;
/// Plane kinds: first color plane, other color planes, alpha, lone plane.
const KINDS: usize = 4;
pub const TABLES: usize = KINDS * 2 * CTX;

fn corrupt() -> Error {
    Error("corrupt pixel data".into())
}

/// A group of planes of one size, alpha (if any) last.
pub struct Group {
    pub w: usize,
    pub h: usize,
    pub planes: Vec<Vec<u8>>,
    pub has_alpha: bool,
}

pub trait Coder {
    /// The residual symbol of the current sample in table `t`; `sym` is the encoder's.
    fn code(&mut self, t: usize, sym: u8) -> u8;
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

/// Codes plane `k` of `g`, whose residuals go to `res` (and `first` holds the first color
/// plane's, for planes after it).
fn walk_plane<C: Coder>(
    c: &mut C,
    g: &mut Group,
    k: usize,
    kind: usize,
    res: &mut [u8],
    first: Option<&[u8]>,
) {
    let (w, h) = (g.w, g.h);
    let (planes, alpha) = if g.has_alpha && kind != 2 {
        let last = g.planes.len() - 1;
        let (p, a) = g.planes.split_at_mut(last);
        (p, Some(&a[0][..]))
    } else {
        (&mut g.planes[..], None)
    };
    let p = &mut planes[k];
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let mut a = if x > 0 { p[i - 1] } else if y > 0 { p[i - w] } else { 0 };
            let mut b = if y > 0 { p[i - w] } else { a };
            let mut cc = if x > 0 && y > 0 { p[i - w - 1] } else { b };
            let d = if y > 0 && x + 1 < w { p[i - w + 1] } else { b };
            let mut transparent = false;
            if let Some(al) = alpha {
                if al[i] == 0 {
                    transparent = true;
                } else {
                    let vis = |j: usize| al[j] > 0;
                    let jw = (x > 0 && vis(i - 1)).then(|| p[i - 1]);
                    let jn = (y > 0 && vis(i - w)).then(|| p[i - w]);
                    let jnw = (x > 0 && y > 0 && vis(i - w - 1)).then(|| p[i - w - 1]);
                    if let Some(fb) = jw.or(jn).or(jnw) {
                        a = jw.unwrap_or(fb);
                        b = jn.unwrap_or(fb);
                        cc = jnw.unwrap_or(fb);
                    }
                }
            }
            let pred = med(a, b, cc);
            let rw = if x > 0 { mag(res[i - 1]) } else { 0 };
            let rn = if y > 0 { mag(res[i - w]) } else { 0 };
            let act = a.abs_diff(cc) as u32 + b.abs_diff(cc) as u32 + b.abs_diff(d) as u32;
            let (off, cross) = match first {
                Some(f) => (f[i], bucket(mag(f[i]), CROSS)),
                None => (0, 0),
            };
            let ctx = bucket(act + 2 * (rw + rn), ACT) * CROSS + cross;
            let t = (kind * 2 + transparent as usize) * CTX + ctx;
            let r = p[i].wrapping_sub(pred);
            let s = c.code(t, r.wrapping_sub(off));
            let r = s.wrapping_add(off);
            res[i] = r;
            p[i] = pred.wrapping_add(r);
        }
    }
}

/// Codes all planes of `g` in model order.
pub fn walk<C: Coder>(c: &mut C, g: &mut Group) {
    let n = g.w * g.h;
    let colors = g.planes.len() - g.has_alpha as usize;
    let mut res = vec![0u8; n];
    if g.has_alpha {
        walk_plane(c, g, colors, 2, &mut res, None);
    }
    if colors == 1 && !g.has_alpha {
        walk_plane(c, g, 0, 3, &mut res, None);
        return;
    }
    let mut first = vec![0u8; n];
    for k in 0..colors {
        if k == 0 {
            walk_plane(c, g, 0, 0, &mut first, None);
        } else {
            walk_plane(c, g, k, 1, &mut res, Some(&first));
        }
    }
}

/// Records symbols for encoding.
#[derive(Default)]
pub struct Recorder {
    pub syms: Vec<(u16, u16)>,
}

impl Coder for Recorder {
    #[inline(always)]
    fn code(&mut self, t: usize, sym: u8) -> u8 {
        self.syms.push((t as u16, sym as u16));
        sym
    }
}

impl Recorder {
    /// Tables and rANS stream of the recorded symbols.
    pub fn finish(self) -> Vec<u8> {
        let mut counts = vec![vec![0u32; 256]; TABLES];
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
        let mut out = Vec::with_capacity(8 + head.len() + body.len());
        crate::container::put_varint(&mut out, head.len() as u64);
        out.extend_from_slice(&head);
        out.extend_from_slice(&body);
        out
    }
}

fn unused() -> Vec<u32> {
    let mut f = vec![0u32; 256];
    f[0] = rans::PROB_SCALE;
    f
}

/// Decodes symbols.
pub struct Player<'a> {
    tables: Tables,
    dec: Decoder<'a>,
}

impl<'a> Player<'a> {
    pub fn new(src: &'a [u8]) -> Result<Player<'a>> {
        let mut at = 0;
        let hl = crate::container::get_varint(src, &mut at)? as usize;
        let head = src.get(at..at + hl).ok_or_else(corrupt)?;
        let mut r = BitR::new(head);
        let mut freqs = Vec::with_capacity(TABLES);
        for _ in 0..TABLES {
            freqs.push(if r.bits(1) == 1 { rans::read_table(&mut r, 256).map_err(|_| corrupt())? } else { unused() });
        }
        if r.overrun() {
            return Err(corrupt());
        }
        Ok(Player { tables: Tables::new(&freqs), dec: Decoder::new(&src[at + hl..]) })
    }

    pub fn finished(&self) -> bool {
        self.dec.finished()
    }
}

impl Coder for Player<'_> {
    #[inline(always)]
    fn code(&mut self, t: usize, _sym: u8) -> u8 {
        self.dec.get(&self.tables, t) as u8
    }
}
