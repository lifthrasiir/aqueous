//! How a block's numbers are written: the encoder's choice of number mode per kind of
//! line, from a sample of the block. Only the encoder uses it; the modes are in the
//! templates.

use crate::block::{K_F, M_FIX, MAX_NUMS, kind_of};
use crate::{dec, num};

/// How the encoder writes the numbers of a kind first: a mode (`dec`), the precision for
/// `%g` and `%e`, and the exponent width of `%g` tokens without an exponent.
#[derive(Clone, Copy, Default)]
pub(crate) struct Style {
    pub(crate) mode: u8,
    pub(crate) p: u8,
    pub(crate) exp3: bool,
}

/// The styles of a block: per numeric kind, and whether normals are unit vectors.
#[derive(Clone, Copy, Default)]
pub struct Styles {
    pub(crate) kinds: [Style; 4],
    pub(crate) unit: bool,
}

/// The styles for block `src` after a block with `prev` (kinds absent from `src`'s sample
/// keep theirs).
pub fn next_styles(prev: &Styles, src: &[u8]) -> Styles {
    let mut st = *prev;
    choose_styles(src, &mut st.kinds, &mut st.unit);
    st
}

/// Rough cost of coding `t` in `st`: digits of the integer, plus a scale change from the
/// previous number in its position (`prev`, updated), or a penalty. `dom` is the usual
/// number of digits after the point of fixed numbers (others cost a template change).
fn style_cost(t: &[u8], st: Style, dom: u8, prev: &mut i32, tmp: &mut Vec<u8>) -> usize {
    let nd = |v: i64| v.unsigned_abs().checked_ilog10().unwrap_or(0) as usize + 1;
    if st.mode == M_FIX {
        return num::parse(t).map_or(25, |(v, f)| nd(v) + if f & num::F_FRAC != dom { 3 } else { 0 });
    }
    dec::encode(t, st.mode, st.p, st.exp3, *prev, tmp).map_or(25, |(v, s, _)| {
        let c = nd(v) + if s != *prev { 2 } else { 0 };
        *prev = s;
        c
    })
}

/// Chooses the styles from lines spread over a block: `%.Pg` when most numbers of a kind
/// are that and their fixed formats vary; unit normals when most normals have length 1. A
/// kind not seen keeps its style (`st`, `unit`: those of the previous block).
fn choose_styles(src: &[u8], st: &mut [Style; 4], unit: &mut bool) {
    const PARTS: usize = 16;
    const PART: usize = 2 << 10;
    // Tokens with their position in the line.
    let mut toks: [Vec<(usize, &[u8])>; 4] = Default::default();
    for i in 0..PARTS {
        let a = src.len() / PARTS * i;
        let sample = &src[a..(a + PART).min(src.len())];
        // Whole lines only: from the first line start.
        let b = if a == 0 { 0 } else { memchr::memchr(b'\n', sample).map_or(sample.len(), |p| p + 1) };
        let mut lines = sample[b..].split(|&c| c == b'\n');
        lines.next_back();
        for l in lines {
            let Some((k, kw)) = kind_of(l) else { continue };
            if k >= K_F {
                continue;
            }
            for (j, t) in l[kw..].split(|&c| matches!(c, b' ' | b'\t' | b'\r')).filter(|t| !t.is_empty()).enumerate() {
                toks[k as usize].push((j.min(MAX_NUMS - 1), t));
            }
        }
    }
    for k in 0..4 {
        let ts = &toks[k];
        if ts.is_empty() {
            continue;
        }
        st[k] = Style::default();
        let mut hist = [0usize; 19];
        let mut maxsig = 1u8;
        let mut sci_p = 1u8;
        let mut exp3 = false;
        for &(_, t) in ts {
            if let Some((_, f)) = num::parse(t) {
                hist[(f & num::F_FRAC) as usize] += 1;
            }
            let e = t.iter().position(|&c| c == b'e' || c == b'E');
            exp3 |= e.is_some_and(|e| t.len() - e == 5);
            let e = e.unwrap_or(t.len());
            let digits: Vec<u8> = t[..e].iter().copied().filter(u8::is_ascii_digit).collect();
            let a = digits.iter().position(|&c| c != b'0').unwrap_or(digits.len());
            let b = digits.iter().rposition(|&c| c != b'0').map_or(a, |i| i + 1);
            maxsig = maxsig.max((b - a).min(17) as u8);
            if e < t.len() {
                sci_p = sci_p.max(digits.len().min(19) as u8);
            }
        }
        let dom = (0..19).max_by_key(|&i| hist[i]).unwrap() as u8;
        let s = |r, src, p| Style { mode: dec::mode(r, src), p, exp3 };
        let cands = [
            Style::default(),
            s(dec::R_GEN, dec::SRC_DEC, maxsig),
            s(dec::R_SCI, dec::SRC_DEC, sci_p),
            s(dec::R_PY, dec::SRC_DEC, 0),
            s(dec::R_JS, dec::SRC_DEC, 0),
            s(dec::R_POS, dec::SRC_DEC, 0),
            s(dec::R_GEN, dec::SRC_F32, maxsig),
            s(dec::R_GEN, dec::SRC_F64, maxsig),
            s(dec::R_SCI, dec::SRC_F32, sci_p),
            s(dec::R_SCI, dec::SRC_F64, sci_p),
            s(dec::R_PY, dec::SRC_F32, 0),
            s(dec::R_JS, dec::SRC_F32, 0),
        ];
        // Whole lines (consecutive tokens), up to about 512 numbers.
        let ts = &ts[..ts.len().min(512)];
        let mut best = (usize::MAX, Style::default());
        let mut tmp = Vec::with_capacity(64);
        for c in cands {
            let mut cost = 0;
            let mut prev = [0i32; MAX_NUMS];
            for &(j, t) in ts {
                cost += style_cost(t, c, dom, &mut prev[j], &mut tmp);
                if cost >= best.0 {
                    break;
                }
            }
            if cost < best.0 {
                best = (cost, c);
            }
        }
        st[k] = best.1;
    }
    // Normals of length 1 within rounding.
    let (mut nvn, mut nunit) = (0, 0);
    for t in toks[2].chunks_exact(3) {
        let x: Vec<f64> = t.iter().filter_map(|t| std::str::from_utf8(t.1).ok()?.parse().ok()).collect();
        if x.len() == 3 {
            nvn += 1;
            nunit += ((x[0] * x[0] + x[1] * x[1] + x[2] * x[2] - 1.0).abs() < 1e-3) as usize;
        }
    }
    if nvn > 0 {
        *unit = nunit * 10 >= nvn * 8;
    }
}

/// A token with an exponent in another style than its line's kind: `%e` with the
/// precision of its digits, or a shortest form. Returns the value, scale, mode and flags.
#[cold]
pub(crate) fn exp_fallback(t: &[u8], ps: i32, tmp: &mut Vec<u8>) -> Option<(i64, i32, u8, u8)> {
    let e = t.iter().position(|&c| c == b'e')?;
    let p = t[..e].iter().filter(|c| c.is_ascii_digit()).count().clamp(1, 19) as u8;
    let exp3 = t.len() - e == 5;
    let modes = [
        (dec::mode(dec::R_SCI, dec::SRC_DEC), p),
        (dec::mode(dec::R_SCI, dec::SRC_F64), p),
        (dec::mode(dec::R_SCI, dec::SRC_F32), p),
        (dec::mode(dec::R_PY, dec::SRC_DEC), 0),
        (dec::mode(dec::R_JS, dec::SRC_DEC), 0),
    ];
    modes.into_iter().find_map(|(m, p)| dec::encode(t, m, p, exp3, ps, tmp).map(|(v, s, f)| (v, s, m, f)))
}
