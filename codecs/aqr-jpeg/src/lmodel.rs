//! The entropy model of lossless JPEG's samples (see [`crate::lossless`]): each sample is coded
//! as its difference from a prediction, by adaptive binary models in the context of how large
//! the errors of its neighbours were.
//!
//! The prediction is a blend of several predictors (the six of the standard, a gradient with
//! the sample to the north-east, and the same change as the previous component of the MCU had),
//! each weighted by the inverse square of the error it made at the neighbours. Everything is
//! integers, so that the Wasm module and the native code agree.
//!
//! The samples are walked in the order of the stream, an MCU at a time, so that a component can
//! look at the ones before it in the MCU. [`walk`] is written once for the encoder and the
//! decoder ([`Coder`]).

use aqr_common::arith::{ArithDecoder, ArithEncoder, Bit, Coder};

use crate::jpeg::{Error, err};
use crate::lossless::{Header, Samples};

/// The predictors: a, b, MED, a+b-c, a+(b-c)/2, b+(a-c)/2, (a+b)/2, a+d-b, and with a component
/// before it in the MCU, a+q-qa and b+q-qb.
const SAME: usize = 8;
const K: usize = 10;
/// Contexts of the size of errors.
const ENERGY: usize = 32;
/// Contexts of the size of the error of the component before.
const EQ: usize = 8;

struct Models {
    zero: Vec<Bit>,
    sign: Vec<Bit>,
    /// unary length of the magnitude, by context and position
    exp: Vec<Bit>,
    /// the two bits below the leading one of the magnitude, by its length and the bits before
    man: Vec<Bit>,
}

impl Models {
    fn new(comps: usize) -> Models {
        Models {
            zero: vec![Bit::default(); comps * ENERGY * EQ],
            sign: vec![Bit::default(); comps * ENERGY * EQ * 3],
            exp: vec![Bit::default(); comps * ENERGY * EQ * 18],
            man: vec![Bit::default(); comps * 18 * 4],
        }
    }
}

/// The state of a component: rows of the errors of each predictor and of the blend.
struct Comp {
    w: usize,
    /// rows kept
    rows: usize,
    cand: Vec<Vec<u16>>,
    fin: Vec<u16>,
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
/// within the component) and `sc` (of the sign); returns what was coded.
fn residual<C: Coder>(
    c: &mut C,
    m: &mut Models,
    comp: usize,
    ctx: usize,
    eq: usize,
    sc: usize,
    r: i32,
) -> Result<i32, Error> {
    let i = (comp * ENERGY + ctx) * EQ + eq;
    if c.bit(&mut m.zero[i], r == 0) {
        return Ok(0);
    }
    let neg = c.bit(&mut m.sign[i * 3 + sc], r < 0);
    let x = r.unsigned_abs().max(1);
    let n = bitlen(x) - 1;
    let mut k = 0;
    while c.bit(&mut m.exp[i * 18 + k], k < n) {
        k += 1;
        if k > 16 || c.failed() {
            return err("corrupt model data");
        }
    }
    let mut y = 1u32;
    for j in (0..k).rev() {
        let bit = x >> j & 1 != 0;
        let b = if k - j <= 2 {
            // the first two bits below the leading one are modelled, the rest are raw
            let prefix = if k - j == 1 { 0 } else { 1 + (y & 1) as usize };
            c.bit(&mut m.man[(comp * 18 + k) * 4 + prefix], bit)
        } else {
            c.raw(bit)
        };
        y = y << 1 | b as u32;
    }
    Ok(if neg { -(y as i32) } else { y as i32 })
}

/// Walks the samples of `planes` in the stream's order, coding each by `c`. Decoding, `planes`
/// are zeros that get filled.
fn walk<C: Coder>(c: &mut C, h: &Header, planes: &mut [Vec<u16>]) -> Result<(), Error> {
    let (mx, my) = h.mcus();
    let default = 1i32 << (h.precision - h.point - 1);
    let mut m = Models::new(h.comps.len());
    let mut st: Vec<Comp> = h
        .comps
        .iter()
        .enumerate()
        .map(|(i, comp)| {
            let w = h.plane(i).0;
            let rows = comp.v + 2;
            Comp { w, rows, cand: vec![vec![0; rows * w]; K], fin: vec![0; rows * w] }
        })
        .collect();
    let mut cands = [0i32; K];
    for r in 0..my {
        for col in 0..mx {
            for (pos, &(ci, _)) in h.scan.iter().enumerate() {
                let comp = h.comps[ci];
                let w = st[ci].w;
                // the component before it in the scan, if it has this shape
                let before = pos.checked_sub(1).map(|p| h.scan[p].0).filter(|&p| h.plane(p) == h.plane(ci));
                for v in 0..comp.v {
                    let y = r * comp.v + v;
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
                        let (lo, hi) = (a.min(b), a.max(b));
                        cands[2] = if cc >= hi {
                            lo
                        } else if cc <= lo {
                            hi
                        } else {
                            a + b - cc
                        };
                        cands[3] = a + b - cc;
                        cands[4] = a + ((b - cc) >> 1);
                        cands[5] = b + ((a - cc) >> 1);
                        cands[6] = (a + b + 1) >> 1;
                        cands[7] = a + d - b;
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
                            cands[8] = a + qv - qa;
                            cands[9] = b + qv - qb;
                            n = K;
                        }
                        let s = &mut st[ci];
                        let (ry, rp) = ((y % s.rows) * w, ((y + s.rows - 1) % s.rows) * w);
                        // the errors around: west, north, north-west, north-east
                        let around = |e: &[u16]| -> u32 {
                            let mut t = 0u32;
                            if x > 0 {
                                t += e[ry + x - 1] as u32;
                            }
                            if y > 0 {
                                t += e[rp + x] as u32;
                                if x > 0 {
                                    t += e[rp + x - 1] as u32;
                                }
                                t += e[rp + if ne { x + 1 } else { x }] as u32;
                            }
                            t
                        };
                        let (mut num, mut den) = (0u64, 0u64);
                        for j in 0..n {
                            let e = around(&s.cand[j]) as u64;
                            let wt = weight(e as u32);
                            num += wt * cands[j].clamp(0, 65535) as u64;
                            den += wt;
                        }
                        let pred = ((num + den / 2) / den) as i32;
                        let ef = around(&s.fin);
                        let ctx = energy(ef);
                        // (the sign context: where the prediction lies against the median's)
                        let sc = (cands[2].cmp(&pred) as i32 + 1) as usize;
                        let actual = p[at] as i32;
                        let eq = before.map_or(0, |q| bitlen(st[q].fin[ry + x] as u32).min(EQ - 1));
                        let rr = residual(c, &mut m, ci, ctx, eq, sc, actual - pred)?;
                        let value = (pred + rr) as u16;
                        planes[ci][at] = value;
                        let s = &mut st[ci];
                        for j in 0..n {
                            s.cand[j][ry + x] =
                                (value as i32 - cands[j].clamp(0, 65535)).unsigned_abs().min(65535) as u16;
                        }
                        s.fin[ry + x] = rr.unsigned_abs().min(65535) as u16;
                    }
                }
            }
        }
    }
    Ok(())
}

/// The samples of `s` coded under `h`.
pub fn encode(h: &Header, s: &Samples) -> Vec<u8> {
    let mut planes = s.planes.clone();
    let mut e = ArithEncoder::new(Vec::new());
    walk(&mut e, h, &mut planes).expect("encoding does not fail");
    e.finish()
}

/// The samples that `data` codes under `h`.
pub fn decode(h: &Header, data: &[u8]) -> Result<Samples, Error> {
    let mut planes: Vec<Vec<u16>> = (0..h.comps.len()).map(|i| h.plane(i)).map(|(w, hh)| vec![0u16; w * hh]).collect();
    let mut d = ArithDecoder::new(data);
    walk(&mut d, h, &mut planes)?;
    if d.overrun() {
        return err("data cut off");
    }
    Ok(Samples { planes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lossless::{self, tests::jpeg};

    fn roundtrip(d: &[u8]) {
        let h = lossless::parse(d, 0).unwrap();
        let s = lossless::decode(&h, &d[h.start..h.end]).unwrap();
        let coded = encode(&h, &s);
        assert_eq!(decode(&h, &coded).unwrap().planes, s.planes);
    }

    #[test]
    fn shapes() {
        roundtrip(&jpeg(14, 37, 19, &[(1, 1), (1, 1)], 1, 0, 0, 1));
        roundtrip(&jpeg(16, 20, 11, &[(1, 1)], 6, 0, 0, 2));
        roundtrip(&jpeg(12, 33, 17, &[(2, 1), (1, 1), (1, 1)], 4, 0, 0, 3));
        // (a component of several rows to the MCU: the north-east sample is not always there)
        roundtrip(&jpeg(12, 33, 17, &[(2, 2), (1, 1), (1, 1)], 1, 2, 0, 4));
        roundtrip(&jpeg(10, 16, 9, &[(1, 1), (1, 1), (1, 1), (1, 1)], 7, 0, 16, 5));
        roundtrip(&jpeg(2, 9, 5, &[(1, 1)], 1, 0, 0, 6));
    }

    /// Samples of any 16-bit value, as a stream with a wild encoder may have them.
    #[test]
    fn extreme_values() {
        let d = jpeg(16, 12, 7, &[(1, 1), (1, 1)], 1, 0, 0, 7);
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

    #[test]
    fn cut_off_data_is_an_error_or_garbage_not_a_panic() {
        let d = jpeg(14, 40, 30, &[(1, 1), (1, 1)], 1, 0, 0, 8);
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
