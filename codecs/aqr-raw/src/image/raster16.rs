//! Raw rasters of 16-bit samples: interleaved pixels of 1 to 4 channels, little- or big-endian,
//! each row perhaps padded (medical images, scientific data). Each sample is predicted from its
//! neighbours in its channel (MED or the mean of the left and upper ones, whichever a chunk of
//! rows takes fewer residual magnitudes with), and its residual (wrapping in 16 bits, zigzag)
//! coded as a token of its magnitude, by rANS in a table chosen by its channel and activity
//! (the neighbours' gradients plus the magnitudes of the left and upper residuals), and the
//! bits below the token's as they are. Static tables per chunk of rows; the padding is kept
//! apart, as it is.
//!
//! The model looks at the current and previous rows only.

use aqr_common::{rans32, varint};

/// The layout of a raster of 16-bit samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    pub width: usize,
    pub height: usize,
    /// Samples per pixel (1..=4).
    pub channels: usize,
    /// Bytes from a row to the next, at least `2 * width * channels`.
    pub stride: usize,
    pub big_endian: bool,
}

impl Format {
    /// Bytes of the samples of a row.
    pub fn row_bytes(&self) -> usize {
        2 * self.width * self.channels
    }

    /// Bytes of the raster, padding included.
    pub fn len(&self) -> Option<usize> {
        self.stride.checked_mul(self.height)
    }

    pub fn valid(&self) -> bool {
        (1..=4).contains(&self.channels)
            && self.width > 0
            && self.height > 0
            && self.width.checked_mul(2 * self.channels).is_some_and(|r| r <= self.stride && (r as u64) < 1 << 32)
            && self.len().is_some_and(|l| (l as u64) < 1 << 48)
    }

    /// The format as bytes: varints of the width, the height and the row padding
    /// (`stride - 2 * width * channels`), then the channels and 1 for big-endian samples or 0.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut v = Vec::new();
        varint::put(&mut v, self.width as u64);
        varint::put(&mut v, self.height as u64);
        varint::put(&mut v, (self.stride - self.row_bytes()) as u64);
        v.push(self.channels as u8);
        v.push(self.big_endian as u8);
        v
    }

    /// The format of [`Format::to_bytes`], if valid and nothing follows.
    pub fn from_bytes(b: &[u8]) -> Option<Format> {
        let mut p = 0;
        let mut num = || usize::try_from(varint::get(b, &mut p).ok()?).ok();
        let (width, height, pad) = (num()?, num()?, num()?);
        let &[channels, big @ (0 | 1)] = b.get(p..)? else { return None };
        let channels = channels as usize;
        let stride = width.checked_mul(2 * channels)?.checked_add(pad)?;
        let f = Format { width, height, channels, stride, big_endian: big == 1 };
        f.valid().then_some(f)
    }

    pub fn sample(&self, b: &[u8]) -> u16 {
        let b = [b[0], b[1]];
        if self.big_endian { u16::from_be_bytes(b) } else { u16::from_le_bytes(b) }
    }

    fn bytes(&self, v: u16) -> [u8; 2] {
        if self.big_endian { v.to_be_bytes() } else { v.to_le_bytes() }
    }
}

#[derive(Debug)]
pub struct Error(pub &'static str);

const PROB_BITS: u32 = 12;
type Tables = rans32::Tables<u8, PROB_BITS>;
type BitW = rans32::BitW<false>;
type BitR<'a> = rans32::BitR<'a, false>;

/// Activity buckets of a channel.
const ACT: usize = 32;
/// Tables: by channel and activity bucket.
const TABLES: usize = 4 * ACT;
/// Residuals below this are tokens of their own; above, a token holds the magnitude's highest
/// bit and the one below it, and the bits below those follow as they are.
const DIRECT: u32 = 16;
/// Tokens: up to a zigzag residual of 16 bits.
const TOKENS: usize = DIRECT as usize + 2 * (16 - DIRECT.ilog2() as usize);

/// Samples per chunk of rows (at least one row), each with tables of its own.
const CHUNK: usize = 1 << 21;

fn chunk_rows(f: &Format) -> usize {
    (CHUNK / (f.width * f.channels)).max(1)
}

/// The predictors a chunk may take.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pred {
    Med = 0,
    Mean = 1,
}

/// The neighbours of a sample: left, upper, upper left, upper right (the image's edges
/// extended: the first row's upper ones are its left one, the first column's left ones its
/// upper one, the last column's upper right one its upper one).
#[derive(Clone, Copy)]
struct Near {
    w: i32,
    n: i32,
    nw: i32,
    ne: i32,
}

impl Near {
    /// Of sample `x` (of a row of `rs` samples, `bpp` to a pixel), `prev` the row before, if any.
    #[inline(always)]
    fn of(cur: &[u16], prev: Option<&[u16]>, x: usize, bpp: usize) -> Near {
        let left = x >= bpp;
        let w = if left { cur[x - bpp] as i32 } else { prev.map_or(0, |p| p[x] as i32) };
        let Some(p) = prev else { return Near { w, n: w, nw: w, ne: w } };
        let n = p[x] as i32;
        let nw = if left { p[x - bpp] as i32 } else { n };
        let ne = if x + bpp < p.len() { p[x + bpp] as i32 } else { n };
        Near { w, n, nw, ne }
    }

    #[inline(always)]
    fn predict(&self, p: Pred) -> u16 {
        let (w, n, nw) = (self.w, self.n, self.nw);
        (match p {
            Pred::Med if nw >= w.max(n) => w.min(n),
            Pred::Med if nw <= w.min(n) => w.max(n),
            Pred::Med => w + n - nw,
            Pred::Mean => (w + n + 1) >> 1,
        }) as u16
    }

    #[inline(always)]
    fn activity(&self) -> u32 {
        ((self.w - self.nw).abs() + (self.n - self.nw).abs() + (self.n - self.ne).abs()) as u32
    }
}

/// The table of a sample of channel `ch`, of activity `a` and the left and upper residuals'
/// magnitudes `ew`, `en`.
#[inline(always)]
fn table(ch: usize, a: u32, ew: u16, en: u16) -> usize {
    let v = a + ew as u32 + en as u32 + 1;
    let l = v.ilog2();
    let half = if l > 0 { (v >> (l - 1)) & 1 } else { 0 };
    ch.min(3) * ACT + ((2 * l + half) as usize).min(ACT - 1)
}

fn zigzag(e: u16) -> u32 {
    let e = e as i16 as i32;
    ((e << 1) ^ (e >> 31)) as u32 & 0xffff
}

fn unzigzag(z: u32) -> u16 {
    ((z >> 1) as u16) ^ (z & 1).wrapping_neg() as u16
}

/// The token of zigzag residual `z`, and the bits below it (their count and value).
#[inline(always)]
fn token(z: u32) -> (u8, u32, u32) {
    if z < DIRECT {
        return (z as u8, 0, 0);
    }
    let l = z.ilog2();
    let t = DIRECT + 2 * (l - DIRECT.ilog2()) + ((z >> (l - 1)) & 1);
    (t as u8, l - 1, z & ((1 << (l - 1)) - 1))
}

/// The residual of token `t`, its `k` bits below read by `bits`.
#[inline(always)]
fn untoken(t: u32, bits: impl FnOnce(u32) -> u32) -> u32 {
    if t < DIRECT {
        return t;
    }
    let l = (t - DIRECT) / 2 + DIRECT.ilog2();
    let top = (2 | ((t - DIRECT) & 1)) << (l - 1);
    top | bits(l - 1)
}

/// The magnitude of a residual, for the contexts.
#[inline(always)]
fn mag(e: u16) -> u16 {
    (e as i16).unsigned_abs()
}

/// The coded samples and the row padding.
pub struct Coded {
    /// Per chunk of rows: its predictor (a byte), varints of the lengths of its tables, its rANS
    /// stream and its bits, then those.
    pub coded: Vec<u8>,
    pub pad: Vec<u8>,
}

/// Codes the raster `d` of layout `f` (`d` exactly as long as it).
pub fn encode(d: &[u8], f: Format) -> Option<Coded> {
    if !f.valid() || f.len() != Some(d.len()) {
        return None;
    }
    let (rs, rb, bpp) = (f.width * f.channels, f.row_bytes(), f.channels);
    let rows: Vec<Vec<u16>> =
        d.chunks_exact(f.stride).map(|r| r[..rb].chunks_exact(2).map(|s| f.sample(s)).collect()).collect();
    let mut c = Coded { coded: Vec::with_capacity(d.len() / 2), pad: Vec::new() };
    for r in d.chunks_exact(f.stride) {
        c.pad.extend_from_slice(&r[rb..]);
    }
    let per = chunk_rows(&f);
    let (mut res_prev, mut res_cur) = (vec![0u16; rs], vec![0u16; rs]);
    let mut syms: Vec<(u8, u8)> = Vec::with_capacity(per.min(f.height) * rs);
    for y0 in (0..f.height).step_by(per) {
        let ys = y0..(y0 + per).min(f.height);
        // the predictor of fewer residual magnitudes
        let mut cost = [0u64; 2];
        for y in ys.clone() {
            let prev = y.checked_sub(1).map(|p| &rows[p][..]);
            for x in 0..rs {
                let n = Near::of(&rows[y], prev, x, bpp);
                for (p, c) in [Pred::Med, Pred::Mean].into_iter().zip(&mut cost) {
                    *c += mag(rows[y][x].wrapping_sub(n.predict(p))) as u64;
                }
            }
        }
        let pred = if cost[1] < cost[0] { Pred::Mean } else { Pred::Med };
        let mut bits = BitW::default();
        for y in ys {
            let prev = y.checked_sub(1).map(|p| &rows[p][..]);
            for x in 0..rs {
                let n = Near::of(&rows[y], prev, x, bpp);
                let e = rows[y][x].wrapping_sub(n.predict(pred));
                let ew = if x >= bpp { res_cur[x - bpp] } else { 0 };
                let t = table(x % bpp, n.activity(), ew, res_prev[x]);
                res_cur[x] = mag(e);
                let (tok, k, v) = token(zigzag(e));
                syms.push((t as u8, tok));
                if k > 0 {
                    bits.bits(k, v);
                }
            }
            std::mem::swap(&mut res_prev, &mut res_cur);
        }
        put_chunk(pred, &syms, bits.finish(), &mut c.coded);
        syms.clear();
    }
    Some(c)
}

fn put_chunk(pred: Pred, syms: &[(u8, u8)], bits: Vec<u8>, out: &mut Vec<u8>) {
    let mut h = vec![0u32; TABLES * TOKENS];
    for &(t, v) in syms {
        h[t as usize * TOKENS + v as usize] += 1;
    }
    let mut w = BitW::default();
    let used: Vec<bool> = h.chunks_exact(TOKENS).map(|c| c.iter().any(|&x| x > 0)).collect();
    for &u in &used {
        w.bits(1, u as u32);
    }
    let freqs: Vec<Vec<u32>> = h
        .chunks_exact(TOKENS)
        .zip(&used)
        .map(|(c, &u)| if u { rans32::write_table::<PROB_BITS, false>(&mut w, c) } else { vec![1 << PROB_BITS] })
        .collect();
    let tb = w.finish();
    let st = rans32::encode::<PROB_BITS>(syms, &freqs);
    out.push(pred as u8);
    for n in [tb.len(), st.len(), bits.len()] {
        varint::put(out, n as u64);
    }
    out.extend_from_slice(&tb);
    out.extend_from_slice(&st);
    out.extend_from_slice(&bits);
}

/// Decodes the raster of layout `f` from `coded` and `pad` into `out` (appending).
pub fn decode(coded: &[u8], pad: &[u8], f: Format, out: &mut Vec<u8>) -> Result<(), Error> {
    let corrupt = Error("corrupt raster");
    if !f.valid() {
        return Err(Error("format"));
    }
    let (rs, rb, bpp) = (f.width * f.channels, f.row_bytes(), f.channels);
    let pb = f.stride - rb;
    if pad.len() != pb * f.height {
        return Err(Error("padding"));
    }
    out.reserve(f.len().unwrap().min(coded.len().saturating_mul(16) + pad.len() + (1 << 20)));
    let per = chunk_rows(&f);
    let (mut prev, mut cur) = (vec![0u16; rs], vec![0u16; rs]);
    let (mut res_prev, mut res_cur) = (vec![0u16; rs], vec![0u16; rs]);
    let mut at = 0;
    for y0 in (0..f.height).step_by(per) {
        let pred = match coded.get(at) {
            Some(0) => Pred::Med,
            Some(1) => Pred::Mean,
            _ => return Err(corrupt),
        };
        at += 1;
        let mut len =
            || varint::get(coded, &mut at).ok().and_then(|v| usize::try_from(v).ok()).ok_or(Error("corrupt raster"));
        let (nt, ns, nb) = (len()?, len()?, len()?);
        let mut part = |n: usize| -> Result<&[u8], Error> {
            let p = coded.get(at..at.checked_add(n).ok_or(Error("corrupt raster"))?).ok_or(Error("corrupt raster"))?;
            at += n;
            Ok(p)
        };
        let (tb, st, bits) = (part(nt)?, part(ns)?, part(nb)?);
        let mut r = BitR::new(tb);
        let used: Vec<bool> = (0..TABLES).map(|_| r.bits(1) == 1).collect();
        let mut freqs = Vec::with_capacity(TABLES);
        for &u in &used {
            freqs.push(if u {
                rans32::read_table::<PROB_BITS, false>(&mut r, TOKENS).map_err(|_| Error("corrupt raster tables"))?
            } else {
                vec![]
            });
        }
        if r.overrun() {
            return Err(Error("corrupt raster tables"));
        }
        let tables = Tables::new(&freqs);
        let mut dec = rans32::Decoder::new(st);
        let mut bits = BitR::new(bits);
        let mut read = 0usize;
        for y in y0..(y0 + per).min(f.height) {
            for x in 0..rs {
                let n = Near::of(&cur, (y > 0).then_some(&prev[..]), x, bpp);
                let ew = if x >= bpp { res_cur[x - bpp] } else { 0 };
                let t = table(x % bpp, n.activity(), ew, res_prev[x]);
                let tok = dec.get(&tables, t);
                if tok as usize >= TOKENS {
                    return Err(corrupt);
                }
                let z = untoken(tok, |k| {
                    read += k as usize;
                    bits.bits(k)
                });
                let e = unzigzag(z);
                res_cur[x] = mag(e);
                cur[x] = n.predict(pred).wrapping_add(e);
            }
            for &v in &cur {
                out.extend_from_slice(&f.bytes(v));
            }
            out.extend_from_slice(&pad[y * pb..(y + 1) * pb]);
            std::mem::swap(&mut prev, &mut cur);
            std::mem::swap(&mut res_prev, &mut res_cur);
        }
        if !dec.finished() || bits.overrun() || read.div_ceil(8) != nb {
            return Err(corrupt);
        }
    }
    if at != coded.len() {
        return Err(corrupt);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_bytes() {
        let f = Format { width: 300, height: 7, channels: 3, stride: 1804, big_endian: true };
        assert_eq!(Format::from_bytes(&f.to_bytes()), Some(f));
        assert_eq!(Format::from_bytes(&[f.to_bytes(), vec![0]].concat()), None);
        assert_eq!(Format::from_bytes(&[1, 1, 0, 1, 2]), None);
        assert_eq!(Format::from_bytes(&[1, 1, 0, 5, 0]), None);
    }

    #[test]
    fn tokens() {
        for z in 0..1 << 16 {
            let (t, k, v) = token(z);
            assert!((t as usize) < TOKENS);
            assert_eq!(
                untoken(t as u32, |n| {
                    assert_eq!(n, k);
                    v
                }),
                z
            );
        }
        for e in [0u16, 1, 0x7fff, 0x8000, 0xffff] {
            assert_eq!(unzigzag(zigzag(e)), e);
        }
    }

    #[test]
    fn round_trip() {
        let mut seed = 7u32;
        for f in [
            Format { width: 1, height: 1, channels: 1, stride: 2, big_endian: false },
            Format { width: 37, height: 5, channels: 3, stride: 224, big_endian: true },
            Format { width: 64, height: 40, channels: 4, stride: 512, big_endian: false },
            Format { width: 9, height: 3000, channels: 2, stride: 36, big_endian: false },
            Format { width: 1500, height: 1600, channels: 1, stride: 3000, big_endian: true },
        ] {
            let d: Vec<u8> = (0..f.len().unwrap() / 2)
                .flat_map(|i| {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    // smooth with noise, and now and then a jump
                    let v = ((i % (f.stride / 2)) as u32 * 7
                        + (seed >> 22)
                        + if seed >> 31 == 1 && i % 97 == 0 { 40000 } else { 0 }) as u16;
                    f.bytes(v)
                })
                .collect();
            let c = encode(&d, f).unwrap();
            let mut out = vec![];
            decode(&c.coded, &c.pad, f, &mut out).unwrap();
            assert!(out == d, "{f:?}");
            assert!(decode(&c.coded[..c.coded.len() - 1], &c.pad, f, &mut vec![]).is_err());
        }
    }
}
