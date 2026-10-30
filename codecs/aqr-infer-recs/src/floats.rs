//! Float fields of records whose values are whole multiples of a scale: numbers written in
//! decimal (0.01″, 1 ms of time, 0.1 °C) and stored as floats in other units (radians,
//! kelvins), as catalogues and measurements are. Each such float is lifted to the whole number
//! q and the difference, in units in the last place, between the float and q × k (or q / k)
//! as the decoder computes it: a few bits, as the float was converted along another path. The
//! lifted records are then split into fields as any; `recs-floats` puts the floats back.
//!
//! The scale is found from the values alone: the smallest gaps between them are a few times
//! it, and least squares over ever larger multiples refine it; the floats a few units in the
//! last place around that are tried as k (or 1 / k, to divide by), and the one whose
//! differences cost least is taken.

use std::borrow::Cow;

use crate::kt;
use crate::module::{Float, lifted_width, mask, ordered};

/// A float is lifted where its q and differences cost this much of its bytes at most.
const KEEP: f64 = 0.85;
/// k is tried this many units in the last place each way of the least-squares scale.
const NEAR: i64 = 64;
/// The widest difference lifted.
const MAX_WR: usize = 2;
/// The largest q: q × k is exact for whole numbers below 2^53.
const MAX_Q: f64 = (1u64 << 52) as f64;
/// Values a scale is looked for in: 4 runs of this many records spread over the records.
const ROWS: usize = 256;
/// The most values of a float's top byte (its sign and exponent) where one is looked for:
/// floats of a scale span a few binades, other bytes most of the byte's values.
const TOP_VALUES: usize = 24;
/// Most of a float's values (90%) are 0 or within 2^±EXP_RANGE of 1 (f64s; f32s within
/// 2^±EXP_RANGE_32): integers read as floats are subnormal, text and floats read out of
/// phase far from 1.
const EXP_RANGE: i64 = 128;
const EXP_RANGE_32: i64 = 40;

/// The bits of the float at `at` of m (zero past its end).
fn bits(m: &[u8], at: usize, wide: bool, big: bool) -> u64 {
    let w = if wide { 8 } else { 4 };
    let b = |i: usize| *m.get(at + i).unwrap_or(&0) as u64;
    if big { (0..w).fold(0, |v, i| v << 8 | b(i)) } else { (0..w).rev().fold(0, |v, i| v << 8 | b(i)) }
}

fn to_f64(b: u64, wide: bool) -> f64 {
    if wide { f64::from_bits(b) } else { f32::from_bits(b as u32) as f64 }
}

/// The rows a scale is looked for in: ROWS from each of 4 places over n (all of them where
/// that is not fewer).
fn rows(n: usize) -> Vec<usize> {
    if n <= 4 * ROWS {
        return (0..n).collect();
    }
    (0..4).flat_map(|i| (n - ROWS) * i / 3..(n - ROWS) * i / 3 + ROWS).collect()
}

/// k refined from a guess on the values a (positive, sorted, distinct), where nearly all of
/// them are a whole multiple of it (to 5%); None where they are not. Least squares on their
/// distances from the median d (sorted), up to b multiples of k, b growing until all are in
/// (the values may all be far from 0, where the guess is too coarse to tell their multiples),
/// then on the values themselves.
fn refine(a: &[f64], d: &[f64], mut k: f64) -> Option<f64> {
    let fit = |v: &[f64], k: f64| {
        let (mut num, mut den, mut good) = (0.0, 0.0, 0);
        for &x in v {
            let t = x / k;
            let q = t.round();
            if q > 0.0 && (t - q).abs() < 0.05 {
                (num, den, good) = (num + q * x, den + q * q, good + 1);
            }
        }
        let k = num / den;
        (den > 0.0 && good * 20 >= v.len() * 19 && k > 0.0 && k.is_finite()).then_some(k)
    };
    let top = *d.last()?;
    let mut b = 64.0;
    for _ in 0..32 {
        if !(k > 0.0 && k.is_finite()) {
            return None;
        }
        // the median's own distance is 0, a multiple of anything
        let s = d[..d.partition_point(|&x| x / k <= b + 0.5)].get(1..).unwrap_or_default();
        if s.len() >= 8 {
            k = fit(s, k)?;
        }
        if b >= top / k {
            return fit(a, k);
        }
        b *= 16.0;
    }
    None
}

/// The scale of values v: k such that 90% of them are a whole multiple of it (to 1%),
/// coarser than 64 units in the last place of the largest and fine enough for q × k to be
/// exact. None where there is none.
fn scale(v: &[f64], wide: bool) -> Option<f64> {
    let finite = v.iter().filter(|x| x.is_finite()).count();
    let mut a: Vec<f64> = v.iter().filter(|x| x.is_finite() && **x != 0.0).map(|x| x.abs()).collect();
    if a.len() < 16 || 2 * a.len() < v.len() {
        return None;
    }
    a.sort_unstable_by(f64::total_cmp);
    a.dedup();
    if a.len() < 16 {
        return None;
    }
    let top = *a.last().unwrap();
    let eps = if wide { f64::EPSILON } else { f32::EPSILON as f64 };
    // gaps of a few units in the last place: values converted along two paths
    let mut g: Vec<f64> = a.windows(2).map(|w| w[1] - w[0]).filter(|&x| x > 64.0 * eps * top).collect();
    if g.len() < 8 {
        return None;
    }
    g.sort_unstable_by(f64::total_cmp);
    // the gaps are multiples of k, and so are the differences between the gaps of different
    // multiples, which are smaller where the values are far apart: the smallest of either
    let tol = 16.0 * eps * top;
    let mut groups: Vec<f64> = Vec::new();
    for &x in &g {
        if groups.last().is_none_or(|&c| x - c > tol) {
            groups.push(x);
        }
    }
    let mut small: Vec<f64> = groups.iter().take(4).copied().collect();
    small.extend(groups.windows(2).map(|w| w[1] - w[0]).filter(|&d| d > 64.0 * eps * top));
    small.sort_unstable_by(f64::total_cmp);
    small.dedup_by(|x, c| *x <= *c * 1.001);
    small.truncate(4);
    let mid = a[a.len() / 2];
    let mut d: Vec<f64> = a.iter().map(|&x| (x - mid).abs()).collect();
    d.sort_unstable_by(f64::total_cmp);
    for g0 in small {
        for j in 1..=8 {
            let Some(k) = refine(&a, &d, g0 / j as f64) else { continue };
            // (integers read as subnormals are multiples of the least of them: no floats)
            let least = if wide { f64::MIN_POSITIVE } else { f32::MIN_POSITIVE as f64 };
            if top / k > MAX_Q || k < 64.0 * eps * top || k < least {
                continue;
            }
            let on = v.iter().filter(|x| x.is_finite() && (*x / k - (*x / k).round()).abs() < 0.01).count();
            if on * 10 >= finite * 9 {
                return Some(k);
            }
        }
    }
    None
}

/// The q of a float (0 for none that is not finite, or too large) and its difference from
/// q's float, as an ordered integer of the float's width, sign-extended.
fn lift_one(f: &Float, b: u64) -> (i64, i64) {
    let x = to_f64(b, f.wide);
    let t = if f.div { x * f.k } else { x / f.k };
    let q = if t.is_finite() && t.abs() <= MAX_Q { t.round() as i64 } else { 0 };
    let w = 8 * f.width() as u32;
    let r = ordered(b, f.wide).wrapping_sub(f.base(q)) & mask(f.width());
    (q, ((r << (64 - w)) as i64) >> (64 - w))
}

/// The fewest bytes (1, 2, 4 or 8) that hold numbers from lo to hi in two's complement.
fn signed_width(lo: i64, hi: i64) -> usize {
    [1, 2, 4, 8].into_iter().find(|&w| w == 8 || (lo >= -(1 << (8 * w - 1)) && hi < 1 << (8 * w - 1))).unwrap()
}

/// The code length in bits of numbers' bytes, plane by plane, as they are or less the one
/// before, whichever is less.
fn cost(v: &[i64], width: usize) -> f64 {
    let plane = |delta: bool| {
        let mut bits = 0.0;
        for i in 0..width {
            let mut c = [0u32; 256];
            let mut prev = 0i64;
            for &x in v {
                let t = if delta { x.wrapping_sub(prev) } else { x };
                prev = x;
                c[(t >> (8 * i)) as u8 as usize] += 1;
            }
            bits += kt::bits(&c);
        }
        bits
    };
    plane(false).min(plane(true))
}

/// The code length of differences: one byte each, and 64 bits more for one beyond a byte.
fn diff_cost(r: &[i64]) -> f64 {
    let mut c = [0u32; 256];
    let mut far = 0;
    for &x in r {
        c[x as u8 as usize] += 1;
        far += (x != x as i8 as i64) as usize;
    }
    kt::bits(&c) + 64.0 * far as f64
}

/// The float of bits b (their order: big-endian or not, f64 or f32) found to be lifted, with
/// k, and what it saves in bits on them; None where it saves too little.
fn fit(b: &[u64], wide: bool, big: bool) -> Option<(Float, f64)> {
    let (sh, bias, range) = if wide { (52, 1023u64, EXP_RANGE) } else { (23, 127, EXP_RANGE_32) };
    let zero = |x: u64| (x << 1) & mask(if wide { 8 } else { 4 }) == 0;
    let near = b.iter().filter(|&&x| zero(x) || ((x >> sh & (2 * bias + 1)) as i64 - bias as i64).abs() <= range);
    if near.count() * 10 < b.len() * 9 {
        return None;
    }
    let v: Vec<f64> = b.iter().map(|&x| to_f64(x, wide)).collect();
    let k = scale(&v, wide)?;
    let w = if wide { 8 } else { 4 };
    let raw: Vec<i64> = b.iter().map(|&x| x as i64).collect();
    let before = cost(&raw, w);
    // the cost of the differences changes slowly with k: every 8th unit, then those around
    // the best
    let try_k = |div: bool, d: i64| {
        let k0 = if div { 1.0 / k } else { k };
        // an f32's scale is known to its precision alone: tried in its units
        let k = match wide {
            true => f64::from_bits(k0.to_bits().wrapping_add(d as u64)),
            false => f32::from_bits((k0 as f32).to_bits().wrapping_add(d as u32)) as f64,
        };
        let f = Float { off: 0, wide, big, div, k, wq: 8, wr: 8 };
        let r: Vec<i64> = b.iter().map(|&x| lift_one(&f, x).1).collect();
        (diff_cost(&r), d, f)
    };
    let least = |x: (f64, i64, Float), y: (f64, i64, Float)| if y.0 < x.0 { y } else { x };
    let coarse = [false, true]
        .into_iter()
        .flat_map(|div| (-NEAR..=NEAR).step_by(8).map(move |d| (div, d)))
        .map(|(div, d)| try_k(div, d))
        .reduce(least)?;
    let (rc, _, f) = (coarse.1 - 7..=coarse.1 + 7).map(|d| try_k(coarse.2.div, d)).fold(coarse, least);
    let q: Vec<i64> = b.iter().map(|&x| lift_one(&f, x).0).collect();
    let (lo, hi) = q.iter().fold((0, 0), |(lo, hi), &x| (x.min(lo), x.max(hi)));
    let after = rc + cost(&q, signed_width(lo, hi));
    (after <= KEEP * before).then_some((f, before - after))
}

/// The floats of records m (rows of k bytes, at `at` of the file) found on a sample of them,
/// at any offset of the record (a float may run into the next record): the phase that keeps
/// them whole, and their offsets from it, without widths (see [`widths`]). Of the phases that
/// do, the one first in the file's phase (mod k): tables of the same records found apart start
/// alike.
pub fn find(m: &[u8], k: usize, at: usize) -> (usize, Vec<Float>) {
    let n = m.len() / k;
    if n < crate::MIN_RECORDS {
        return (0, Vec::new());
    }
    let rows = rows(n);
    // the values each byte of the record takes: a float's top byte takes few
    let few: Vec<bool> = (0..k)
        .map(|c| {
            let mut seen = [false; 256];
            for &i in &rows {
                seen[m[i * k + c] as usize] = true;
            }
            seen.iter().filter(|&&s| s).count() <= TOP_VALUES
        })
        .collect();
    let mut found: Vec<(f64, Float)> = Vec::new();
    for wide in [true, false] {
        let w = if wide { 8 } else { 4 };
        if w > k {
            continue;
        }
        for off in 0..k {
            for big in [false, true] {
                if !few[(off + if big { 0 } else { w - 1 }) % k] {
                    continue;
                }
                let b: Vec<u64> = rows
                    .iter()
                    .filter(|&&i| i * k + off + w <= m.len())
                    .map(|&i| bits(m, i * k + off, wide, big))
                    .collect();
                if let Some((f, gain)) = fit(&b, wide, big) {
                    found.push((gain, Float { off, ..f }));
                }
            }
        }
    }
    found.sort_unstable_by(|x, y| y.0.total_cmp(&x.0));
    // the best apart, around the record
    let mut taken: Vec<Float> = Vec::new();
    for (_, f) in found {
        let apart = taken.iter().all(|t| {
            let d = (f.off + k - t.off) % k;
            d >= t.width() && k - d >= f.width()
        });
        if apart {
            taken.push(f);
        }
    }
    let inside = |p: usize| taken.iter().any(|f| (1..f.width()).contains(&((p + k - f.off) % k)));
    let phase = (0..k).filter(|&p| !inside(p)).min_by_key(|&p| (at + p) % k).unwrap();
    for f in &mut taken {
        f.off = (f.off + k - phase) % k;
    }
    taken.sort_unstable_by_key(|f| f.off);
    (phase, taken)
}

/// The floats of n records m (rows of k bytes) with the widths their q and differences
/// need, those whose differences are wider than MAX_WR bytes dropped.
pub fn widths(m: &[u8], k: usize, floats: &[Float]) -> Vec<Float> {
    floats
        .iter()
        .filter_map(|f| {
            let (mut lo, mut hi, mut rlo, mut rhi) = (0i64, 0i64, 0i64, 0i64);
            for r in m.chunks_exact(k) {
                let (q, d) = lift_one(f, bits(r, f.off, f.wide, f.big));
                (lo, hi, rlo, rhi) = (lo.min(q), hi.max(q), rlo.min(d), rhi.max(d));
            }
            let wq = signed_width(lo, hi);
            let wr = if rlo == 0 && rhi == 0 { 0 } else { signed_width(rlo, rhi) };
            (wr <= MAX_WR).then_some(Float { wq, wr, ..*f })
        })
        .collect()
}

/// Whether the record r's floats lift within their widths.
pub fn fits(r: &[u8], floats: &[Float]) -> bool {
    floats.iter().all(|f| {
        let (q, d) = lift_one(f, bits(r, f.off, f.wide, f.big));
        let w = signed_width(q.min(0), q.max(0));
        w <= f.wq && (d == 0 || (f.wr > 0 && signed_width(d.min(0), d.max(0)) <= f.wr))
    })
}

/// The records m (rows of k bytes) with their floats lifted: each float's q, then its
/// difference, in its place.
pub fn lift<'a>(m: &'a [u8], k: usize, floats: &[Float]) -> Cow<'a, [u8]> {
    if floats.is_empty() {
        return m.into();
    }
    let n = m.len() / k;
    let mut out = Vec::with_capacity(n * lifted_width(k, floats));
    for r in m.chunks_exact(k) {
        let mut at = 0;
        for f in floats {
            out.extend_from_slice(&r[at..f.off]);
            let (q, d) = lift_one(f, bits(r, f.off, f.wide, f.big));
            out.extend_from_slice(&q.to_le_bytes()[..f.wq]);
            out.extend_from_slice(&d.to_le_bytes()[..f.wr]);
            at = f.off + f.width();
        }
        out.extend_from_slice(&r[at..]);
    }
    out.into()
}

/// The floats in a line: `f64@0×7.27e-8 f32be@20/100 …`.
pub fn describe(floats: &[Float]) -> String {
    floats
        .iter()
        .map(|f| {
            let (op, k) = if f.div { ('/', f.k) } else { ('×', f.k) };
            format!(
                "f{}{}@{}{op}{k:.6e} (q{} r{})",
                8 * f.width(),
                if f.big { "be" } else { "" },
                f.off,
                8 * f.wq,
                8 * f.wr
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module::unlift;

    /// Records of 24 bytes: an f64 of a value in 0.01″ as radians (by a constant of its own,
    /// so a unit or two off q × k), a u32, an f32 of a decimal with one place, and a u64.
    fn records(n: usize) -> Vec<u8> {
        let mut m = Vec::new();
        let mut x = 7u64;
        for i in 0..n as i64 {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let cs = 1_000_000 + 37 * i + (x >> 60) as i64;
            m.extend((cs as f64 / 360000.0 * 0.0174532925199433).to_le_bytes());
            m.extend((i as u32 * 3).to_le_bytes());
            m.extend(((((x >> 40) % 2000) as f32 - 1000.0) / 10.0).to_le_bytes());
            m.extend((x >> 20).to_be_bytes());
        }
        m
    }

    #[test]
    fn floats_lift_and_back() {
        let k = 24;
        let m = records(5000);
        let (phase, fl) = find(&m, k, 0);
        assert_eq!(phase, 0);
        let fl = widths(&m, k, &fl);
        assert_eq!(
            fl.iter().map(|f| (f.off, f.wide)).collect::<Vec<_>>(),
            [(0, true), (12, false)],
            "{}",
            describe(&fl)
        );
        // the decimal divides exactly
        assert!(fl[1].div && fl[1].wr == 0, "{}", describe(&fl));
        assert!(fl[0].wr <= 1, "{}", describe(&fl));
        let l = lift(&m, k, &fl);
        assert_eq!(l.len(), 5000 * lifted_width(k, &fl));
        assert_eq!(unlift(&l, 5000, k, &fl).unwrap(), m);
    }

    #[test]
    fn floats_in_phase() {
        // the same records from their 13th byte: the phase is found again
        let k = 24;
        let m = records(5000);
        let (phase, fl) = find(&m[13..m.len() - 11], k, 13);
        assert_eq!((phase, fl.len()), (11, 2), "{}", describe(&fl));
    }

    #[test]
    fn noise_has_no_floats() {
        let mut x = 1u64;
        let m: Vec<u8> = (0..24 * 4000)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect();
        assert!(find(&m, 24, 0).1.is_empty());
    }
}
