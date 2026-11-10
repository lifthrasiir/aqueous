//! The entropy model of lossless JPEG's samples (see [`crate::lossless`]): each sample is coded
//! as its difference from a prediction, by adaptive binary models in the context of how large
//! the errors of its neighbours were.
//!
//! The prediction is a blend of several predictors (four of the standard, a gradient with the
//! sample to the north-east, and the same change as the previous component of the MCU had), each
//! weighted by the inverse square of the error it made at the neighbours. The difference is a
//! Golomb code whose parameter follows the errors around, its quotient and top bits by adaptive
//! bits. Everything is integers, so that the Wasm module and the native code agree.
//!
//! A frame may be coded in bands of rows ([`encode_bands`]), each alone, so that they run at
//! once: the cost is the first row of each (0.05% in 16 bands).
//!
//! The samples are walked in the order of the stream, an MCU at a time, so that a component can
//! look at the ones before it in the MCU. [`walk`] is written once for the encoder and the
//! decoder ([`Coder`]).

use aqr_common::arith::{ArithDecoder, ArithEncoder, Bit, Coder};

use crate::jpeg::{Error, err};
use crate::lossless::{Header, Samples};

/// The predictors: a, b, a+(b-c)/2, b+(a-c)/2, a+d-b, and with a component before it in the MCU,
/// a+q-qa and b+q-qb.
const SAME: usize = 5;
const K: usize = 7;
/// Contexts of the size of errors.
const ENERGY: usize = 32;
/// Contexts of the size of the error of the component before.
const EQ: usize = 8;
/// Adaptive bits of the unary quotient, beyond which an escape follows.
const QMAX: usize = 12;
/// The Golomb parameter is the bit length of the mean error around, less this.
const KSHIFT: usize = 0;

/// The adaptive bits of a model: what frames coded one after the other share, as the tiles of
/// a DNG, for each to learn from the ones before it.
pub struct Model {
    /// the quotient in unary, by context and position
    quo: Vec<Bit>,
    /// the quotients beyond `QMAX`, as a gamma code
    esc: Vec<Bit>,
    /// the two bits of the remainder next to the quotient, by the remainder's length, the
    /// quotient (0, 1 or more) and the bit before
    low: Vec<Bit>,
}

impl Model {
    /// A model for frames of `comps` components.
    pub fn new(comps: usize) -> Model {
        Model {
            quo: vec![Bit::default(); comps * ENERGY * EQ * QMAX],
            esc: vec![Bit::default(); 20],
            low: vec![Bit::default(); comps * 18 * 3 * 4],
        }
    }

    /// Bytes of the model's three tables of bits (the blocks of `new`'s vectors, before they are
    /// rounded up).
    pub fn bytes(comps: usize) -> [u64; 3] {
        let bit = size_of::<Bit>() as u64;
        let n = comps as u64;
        [n * (ENERGY * EQ * QMAX) as u64 * bit, 20 * bit, n * 18 * 3 * 4 * bit]
    }
}

/// Bytes of the working memory of [`code_frame`] for a frame of `h`: the rows of errors of each
/// component (one block of them), the most it takes beyond the model and the frame's samples.
pub fn frame_bytes(h: &Header) -> Vec<u64> {
    (0..h.comps.len()).map(|i| ((h.comps[i].v + 2) * (h.plane(i).0 + 2) * size_of::<[u16; LANES]>()) as u64).collect()
}

/// Lanes of the errors at a position: those of the predictors, then the blend's, padded.
const LANES: usize = 16;
const FIN: usize = K;

/// The state of a component: rows of the errors at each position, `w + 2` of them to a row (a
/// zero at each end, so that the edges need no tests), as many rows as the MCU needs and two.
struct Comp {
    w: usize,
    rows: usize,
    err: Vec<[u16; LANES]>,
}
/// 2^40 / (i² + 8), the weight of a predictor whose errors around summed to `i`.
static WEIGHT: [u64; 4096] = {
    let mut t = [0; 4096];
    let mut i = 0;
    while i < 4096 {
        t[i] = (1u64 << 40) / (i as u64 * i as u64 + 8);
        i += 1;
    }
    t
};

/// [`WEIGHT`] of `e`, beyond the table by its high 12 bits.
#[inline]
fn weight(e: u32) -> u64 {
    if e < 4096 {
        return WEIGHT[e as usize];
    }
    let k = bitlen(e) - 12;
    (WEIGHT[(e >> k) as usize] >> (2 * k)).max(1)
}

fn bitlen(x: u32) -> usize {
    32 - x.leading_zeros() as usize
}

/// The context of the energy `e` of the errors around: about 2 per octave.
fn energy(e: u32) -> usize {
    if e < 2 {
        return e as usize;
    }
    let n = bitlen(e);
    (2 * n - 2 + ((e >> (n - 2)) & 1) as usize).min(ENERGY - 1)
}

/// Codes `r` (the difference of a sample from its prediction) in context `ctx` (of the energy,
/// within the component), `eq` (of the error of the component before),
/// as a Golomb code of parameter `k`: the quotient in unary by adaptive bits, the two high bits
/// of the remainder modelled, the rest raw in one step. Returns what was coded.
fn residual<C: Coder>(
    c: &mut C,
    m: &mut Model,
    comp: usize,
    ctx: usize,
    eq: usize,
    k: u32,
    r: i32,
) -> Result<i32, Error> {
    let i = (comp * ENERGY + ctx) * EQ + eq;
    let v = ((r << 1) ^ (r >> 31)) as u32;
    let q = v >> k;
    let mut t = 0;
    while t < QMAX && c.bit(&mut m.quo[i * QMAX + t], (t as u32) < q) {
        t += 1;
    }
    let q = if t == QMAX {
        let e = c.gamma32(&mut m.esc, q.wrapping_sub(QMAX as u32), || Error("corrupt model data".into()))?;
        (QMAX as u32).wrapping_add(e)
    } else {
        t as u32
    };
    let base = ((comp * 18 + k as usize) * 3 + q.min(2) as usize) * 4;
    let high = k.min(2);
    let mut low = 0u32;
    for s in 0..high {
        let want = v >> (k - 1 - s) & 1 != 0;
        let at = base + if s == 0 { 0 } else { 1 + (low & 1) as usize };
        low = low << 1 | c.bit(&mut m.low[at], want) as u32;
    }
    if k > high {
        let rest = k - high;
        low = low << rest | c.raw_run(rest, v & ((1 << rest) - 1));
    }
    let x = q << k | low;
    Ok((x >> 1) as i32 ^ -((x & 1) as i32))
}

/// Walks the samples of the frame of `h` in the stream's order, coding each by `c`, with `m`. Decoding, `planes`
/// are zeros that get filled.
pub fn code_frame<C: Coder>(c: &mut C, m: &mut Model, h: &Header, planes: &mut [&mut [u16]]) -> Result<(), Error> {
    let (mx, my) = h.mcus();
    let default = 1i32 << (h.precision - h.point - 1);
    let mut st: Vec<Comp> = h
        .comps
        .iter()
        .enumerate()
        .map(|(i, comp)| {
            let w = h.plane(i).0;
            let rows = comp.v + 2;
            Comp { w, rows, err: vec![[0; LANES]; rows * (w + 2)] }
        })
        .collect();
    // the component before each one of the scan, if it has its shape
    let before_of: Vec<Option<usize>> = (0..h.scan.len())
        .map(|pos| pos.checked_sub(1).map(|p| h.scan[p].0).filter(|&p| h.plane(p) == h.plane(h.scan[pos].0)))
        .collect();
    let mut cands = [0i32; K];
    for r in 0..my {
        for col in 0..mx {
            for (pos, &(ci, _)) in h.scan.iter().enumerate() {
                let comp = h.comps[ci];
                let w = st[ci].w;
                let (rows, stride) = (st[ci].rows, w + 2);
                let before = before_of[pos];
                for v in 0..comp.v {
                    let y = r * comp.v + v;
                    let (ry, rp) = ((y % rows) * stride + 1, ((y + rows - 1) % rows) * stride + 1);
                    for hh in 0..comp.h {
                        let x = col * comp.h + hh;
                        let at = y * w + x;
                        let p = &planes[ci];
                        let a = if x > 0 {
                            p[at - 1] as i32
                        } else if y > 0 {
                            p[at - w] as i32
                        } else {
                            default
                        };
                        let b = if y > 0 { p[at - w] as i32 } else { a };
                        let cc = if x > 0 && y > 0 { p[at - w - 1] as i32 } else { b };
                        // (the north-east sample is there where its MCU is: above this MCU row, or
                        // to the left of this one)
                        let ne = y > 0 && x + 1 < w && (v == 0 || (x + 1) / comp.h <= col);
                        let d = if ne { p[at - w + 1] as i32 } else { b };
                        cands[0] = a;
                        cands[1] = b;
                        cands[2] = a + ((b - cc) >> 1);
                        cands[3] = b + ((a - cc) >> 1);
                        cands[4] = a + d - b;
                        let mut n = SAME;
                        if let Some(q) = before {
                            let pq = &planes[q];
                            let qv = pq[at] as i32;
                            let qa = if x > 0 {
                                pq[at - 1] as i32
                            } else if y > 0 {
                                pq[at - w] as i32
                            } else {
                                qv
                            };
                            let qb = if y > 0 { pq[at - w] as i32 } else { qa };
                            cands[5] = a + qv - qa;
                            cands[6] = b + qv - qb;
                            n = K;
                        }
                        // the errors around (west, north-west, north, north-east), by lane
                        let s = &st[ci];
                        let (ew, enw, en) = (&s.err[ry + x - 1], &s.err[rp + x - 1], &s.err[rp + x]);
                        let ene = &s.err[if ne { rp + x + 1 } else { rp + x }];
                        let mut e = [0u32; LANES];
                        for j in 0..LANES {
                            e[j] = ew[j] as u32 + enw[j] as u32 + en[j] as u32 + ene[j] as u32;
                        }
                        let (mut num, mut den) = (0u64, 0u64);
                        for j in 0..n {
                            let wt = weight(e[j]);
                            num += wt * cands[j].clamp(0, 65535) as u64;
                            den += wt;
                        }
                        let pred = ((num + den / 2) / den) as i32;
                        let ctx = energy(e[FIN]);
                        let actual = p[at] as i32;
                        let eq = before.map_or(0, |q| bitlen(st[q].err[ry + x][FIN] as u32).min(EQ - 1));
                        let k = (bitlen(e[FIN] >> 2).saturating_sub(KSHIFT)).min(16) as u32;
                        let rr = residual(c, m, ci, ctx, eq, k, actual - pred)?;
                        let value = (pred + rr) as u16;
                        planes[ci][at] = value;
                        let cell = &mut st[ci].err[ry + x];
                        for j in 0..n {
                            cell[j] = (value as i32 - cands[j].clamp(0, 65535)).unsigned_abs() as u16;
                        }
                        cell[FIN] = rr.unsigned_abs().min(65535) as u16;
                    }
                }
            }
        }
    }
    Ok(())
}

/// The samples of `s` coded under `h`, by a model of its own.
pub fn encode(h: &Header, s: &Samples) -> Vec<u8> {
    let mut planes = s.planes.clone();
    let mut e = ArithEncoder::new(Vec::new());
    code_frame(&mut e, &mut Model::new(h.comps.len()), h, &mut views(&mut planes)).expect("encoding does not fail");
    e.finish()
}

/// The samples that `data` codes under `h`, by a model of its own.
pub fn decode(h: &Header, data: &[u8]) -> Result<Samples, Error> {
    let mut planes: Vec<Vec<u16>> = (0..h.comps.len()).map(|i| vec![0u16; h.samples_of(i)]).collect();
    let mut d = ArithDecoder::new(data);
    code_frame(&mut d, &mut Model::new(h.comps.len()), h, &mut views(&mut planes))?;
    if d.overrun() {
        return err("data cut off");
    }
    Ok(Samples { planes })
}

/// `s` walked and coded by `c` (any coder, as a null one measuring the model alone).
pub fn encode_with<C: Coder>(c: &mut C, h: &Header, s: &Samples) {
    let mut planes = s.planes.clone();
    code_frame(c, &mut Model::new(h.comps.len()), h, &mut views(&mut planes)).expect("encoding does not fail");
}

fn views(planes: &mut [Vec<u16>]) -> Vec<&mut [u16]> {
    planes.iter_mut().map(|p| &mut p[..]).collect()
}

/// The rows of MCUs `r0..r1` of `h` as a frame of their own, for a band to be coded alone.
pub fn band_header(h: &Header, r0: usize, r1: usize) -> Header {
    let vmax = h.comps.iter().map(|c| c.v).max().unwrap_or(1);
    let mut b = h.clone();
    b.height = (r1 * vmax).min(h.height) - r0 * vmax;
    b
}

/// The MCU rows of each of `bands` bands of a frame of `rows` of them.
pub fn band_rows(rows: usize, bands: usize) -> Vec<(usize, usize)> {
    let per = rows.div_ceil(bands.clamp(1, rows.max(1)));
    (0..rows).step_by(per.max(1)).map(|r| (r, (r + per).min(rows))).collect()
}

/// The planes of a frame of `h` cut in bands of the MCU rows `rows` ([`band_rows`]): for each
/// band the part of each plane.
pub fn split_bands<'a>(h: &Header, planes: &'a mut [&mut [u16]], rows: &[(usize, usize)]) -> Vec<Vec<&'a mut [u16]>> {
    let mut out: Vec<Vec<&'a mut [u16]>> = rows.iter().map(|_| Vec::new()).collect();
    for (i, p) in planes.iter_mut().enumerate() {
        let (w, v) = (h.plane(i).0, h.comps[i].v);
        let mut rest: &mut [u16] = std::mem::take(p);
        for (band, &(r0, r1)) in out.iter_mut().zip(rows) {
            let (part, tail) = rest.split_at_mut(((r1 - r0) * v * w).min(rest.len()));
            band.push(part);
            rest = tail;
        }
    }
    out
}

/// `s` coded in `bands` bands of rows (at most; fewer where there are not that many rows),
/// each with a model and a stream of its own, so that they can be coded and decoded at once.
pub fn encode_bands(h: &Header, s: &Samples, bands: usize) -> Vec<Vec<u8>> {
    let mut planes = s.planes.clone();
    encode_bands_of(h, &mut views(&mut planes), bands)
}

/// [`encode_bands`] of `planes` (which the coding leaves as they are).
pub fn encode_bands_of(h: &Header, planes: &mut [&mut [u16]], bands: usize) -> Vec<Vec<u8>> {
    use crate::par::prelude::*;
    let rows = band_rows(h.mcus().1, bands);
    split_bands(h, planes, &rows)
        .into_par_iter()
        .zip(rows.into_par_iter())
        .map(|(mut part, (r0, r1))| {
            let mut e = ArithEncoder::new(Vec::new());
            code_frame(&mut e, &mut Model::new(h.comps.len()), &band_header(h, r0, r1), &mut part)
                .expect("encoding does not fail");
            e.finish()
        })
        .collect()
}

/// The samples that the bands `streams` code under `h` (as many as [`encode_bands`] made).
pub fn decode_bands(h: &Header, streams: &[&[u8]]) -> Result<Samples, Error> {
    let mut planes: Vec<Vec<u16>> = (0..h.comps.len()).map(|i| vec![0u16; h.samples_of(i)]).collect();
    decode_bands_into(h, streams, &mut views(&mut planes))?;
    Ok(Samples { planes })
}

/// [`decode_bands`] into `planes` (zeros).
pub fn decode_bands_into(h: &Header, streams: &[&[u8]], planes: &mut [&mut [u16]]) -> Result<(), Error> {
    use crate::par::prelude::*;
    let rows = band_rows(h.mcus().1, streams.len());
    if rows.len() != streams.len() {
        return err("bands do not fit the frame");
    }
    let done: Vec<Result<(), Error>> = split_bands(h, planes, &rows)
        .into_par_iter()
        .zip(rows.into_par_iter().zip(streams.par_iter()))
        .map(|(mut part, ((r0, r1), data))| {
            let mut d = ArithDecoder::new(data);
            code_frame(&mut d, &mut Model::new(h.comps.len()), &band_header(h, r0, r1), &mut part)?;
            if d.overrun() { err("data cut off") } else { Ok(()) }
        })
        .collect();
    done.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lossless::{self, synthetic};

    fn roundtrip(d: &[u8]) {
        let h = lossless::parse(d, 0).unwrap();
        let s = lossless::decode(&h, &d[h.start..h.end]).unwrap();
        let coded = encode(&h, &s);
        assert_eq!(decode(&h, &coded).unwrap().planes, s.planes);
    }

    #[test]
    fn shapes() {
        roundtrip(&synthetic(14, 37, 19, &[(1, 1), (1, 1)], 1, 0, 0, 1));
        roundtrip(&synthetic(16, 20, 11, &[(1, 1)], 6, 0, 0, 2));
        roundtrip(&synthetic(12, 33, 17, &[(2, 1), (1, 1), (1, 1)], 4, 0, 0, 3));
        // (a component of several rows to the MCU: the north-east sample is not always there)
        roundtrip(&synthetic(12, 33, 17, &[(2, 2), (1, 1), (1, 1)], 1, 2, 0, 4));
        roundtrip(&synthetic(10, 16, 9, &[(1, 1), (1, 1), (1, 1), (1, 1)], 7, 0, 16, 5));
        roundtrip(&synthetic(2, 9, 5, &[(1, 1)], 1, 0, 0, 6));
    }

    /// Samples of any 16-bit value, as a stream with a wild encoder may have them.
    #[test]
    fn extreme_values() {
        let d = synthetic(16, 12, 7, &[(1, 1), (1, 1)], 1, 0, 0, 7);
        let h = lossless::parse(&d, 0).unwrap();
        let mut x = 12345u32;
        let planes = (0..2)
            .map(|i| {
                (0..h.plane(i).0 * h.plane(i).1)
                    .map(|_| {
                        x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                        if x >> 30 == 0 {
                            0
                        } else if x >> 30 == 1 {
                            65535
                        } else {
                            (x >> 8) as u16
                        }
                    })
                    .collect()
            })
            .collect();
        let s = Samples { planes };
        let coded = encode(&h, &s);
        assert_eq!(decode(&h, &coded).unwrap().planes, s.planes);
    }

    /// Bands of rows decode alone and put together give the frame, whatever the number of
    /// bands (more than there are rows too) and the shape of the MCUs.
    #[test]
    fn bands() {
        for (comps, w, hgt) in
            [(&[(1, 1), (1, 1)][..], 37, 19), (&[(2, 2), (1, 1), (1, 1)][..], 33, 17), (&[(1, 1)][..], 8, 3)]
        {
            let d = synthetic(14, w, hgt, comps, 1, 0, 0, 9);
            let h = lossless::parse(&d, 0).unwrap();
            let s = lossless::decode(&h, &d[h.start..h.end]).unwrap();
            for n in [1, 2, 3, 7, 100] {
                let bs = encode_bands(&h, &s, n);
                let refs: Vec<&[u8]> = bs.iter().map(|b| &b[..]).collect();
                assert_eq!(decode_bands(&h, &refs).unwrap().planes, s.planes, "{comps:?} in {n} bands");
            }
        }
    }
    #[test]
    fn cut_off_data_is_an_error_or_garbage_not_a_panic() {
        let d = synthetic(14, 40, 30, &[(1, 1), (1, 1)], 1, 0, 0, 8);
        let h = lossless::parse(&d, 0).unwrap();
        let s = lossless::decode(&h, &d[h.start..h.end]).unwrap();
        let coded = encode(&h, &s);
        for cut in [0, 1, coded.len() / 2] {
            let _ = decode(&h, &coded[..cut]);
        }
        let mut bad = coded.clone();
        bad.iter_mut().for_each(|b| *b = !*b);
        let _ = decode(&h, &bad);
    }
}
