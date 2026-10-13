//! The encoder's side: how a group of numbers is printed (a style), chosen from a sample,
//! and the coding of a token in a style with fallbacks. Not part of the format: whatever
//! the encoder picks, a number decodes from its descriptor alone.

use crate::dec::{self, R_FIX, R_GEN, R_JS, R_POS, R_PY, R_SCI, SRC_DEC, SRC_F32, SRC_F64};
use crate::{M_GEN, fixed, general};

/// How the encoder writes a group of numbers first: a mode (`dec`), the precision for
/// `%g` and `%e`, and the exponent width of `%g` tokens without an exponent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub mode: u8,
    pub p: u8,
    pub exp3: bool,
}

impl Style {
    /// Codes token `t`: in this style if it is printed that way, else fixed, else `%g` (of
    /// the style's precision within 6..=17), else `exp_fallback`. Returns the value,
    /// scale, mode and flags; None if no mode prints `t`. `prev_scale` is the scale given
    /// to zero (not fixed); `tmp` scratch space.
    #[inline]
    pub fn encode(&self, t: &[u8], prev_scale: i32, tmp: &mut Vec<u8>) -> Option<(i64, i32, u8, u8)> {
        if self.mode != R_FIX
            && let Some((v, s, f)) = dec::encode(t, self.mode, self.p, self.exp3, prev_scale, tmp)
        {
            Some((v, s, self.mode, f))
        } else if let Some((v, f)) = fixed::parse(t) {
            Some((v, (f & fixed::F_FRAC) as i32, R_FIX, f))
        } else if let Some((v, s, f)) = general::parse_gen(t, self.p.clamp(6, 17), prev_scale, self.exp3, tmp) {
            Some((v, s, M_GEN, f))
        } else {
            exp_fallback(t, prev_scale, tmp)
        }
    }
}

/// Rough cost of coding `t` in `st`: digits of the integer, plus a scale change from the
/// previous number in its column (`prev`, updated), or a penalty. `dom` is the usual
/// number of digits after the point of fixed numbers (others cost a template change).
fn cost(t: &[u8], st: Style, dom: u8, prev: &mut i32, tmp: &mut Vec<u8>) -> usize {
    let nd = |v: i64| v.unsigned_abs().checked_ilog10().unwrap_or(0) as usize + 1;
    if st.mode == R_FIX {
        return fixed::parse(t).map_or(25, |(v, f)| nd(v) + if f & fixed::F_FRAC != dom { 3 } else { 0 });
    }
    dec::encode(t, st.mode, st.p, st.exp3, *prev, tmp).map_or(25, |(v, s, _)| {
        let c = nd(v) + if s != *prev { 2 } else { 0 };
        *prev = s;
        c
    })
}

/// Chooses the style of a group of numbers from a sample of its tokens, each with its
/// column (position in its line or record), in order; whole lines are best. Each mode is
/// tried on the first 512 tokens: the one with the fewest digits and scale changes wins
/// (fixed when nothing does better).
pub fn choose(toks: &[(usize, &[u8])]) -> Style {
    let mut hist = [0usize; 19];
    let mut maxsig = 1u8;
    let mut sci_p = 1u8;
    let mut exp3 = false;
    for &(_, t) in toks {
        if let Some((_, f)) = fixed::parse(t) {
            hist[(f & fixed::F_FRAC) as usize] += 1;
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
        s(R_GEN, SRC_DEC, maxsig),
        s(R_SCI, SRC_DEC, sci_p),
        s(R_PY, SRC_DEC, 0),
        s(R_JS, SRC_DEC, 0),
        s(R_POS, SRC_DEC, 0),
        s(R_GEN, SRC_F32, maxsig),
        s(R_GEN, SRC_F64, maxsig),
        s(R_SCI, SRC_F32, sci_p),
        s(R_SCI, SRC_F64, sci_p),
        s(R_PY, SRC_F32, 0),
        s(R_JS, SRC_F32, 0),
    ];
    let ts = &toks[..toks.len().min(512)];
    let mut prev = vec![0i32; ts.iter().map(|&(j, _)| j + 1).max().unwrap_or(0)];
    let mut best = (usize::MAX, Style::default());
    let mut tmp = Vec::with_capacity(64);
    for c in cands {
        let mut cost_c = 0;
        prev.fill(0);
        for &(j, t) in ts {
            cost_c += cost(t, c, dom, &mut prev[j], &mut tmp);
            if cost_c >= best.0 {
                break;
            }
        }
        if cost_c < best.0 {
            best = (cost_c, c);
        }
    }
    best.1
}

/// A token with an exponent in another style than its group's: `%e` with the precision of
/// its digits, or a shortest form. Returns the value, scale, mode and flags.
#[cold]
pub fn exp_fallback(t: &[u8], ps: i32, tmp: &mut Vec<u8>) -> Option<(i64, i32, u8, u8)> {
    let e = t.iter().position(|&c| c == b'e')?;
    let p = t[..e].iter().filter(|c| c.is_ascii_digit()).count().clamp(1, 19) as u8;
    let exp3 = t.len() - e == 5;
    let modes = [
        (dec::mode(R_SCI, SRC_DEC), p),
        (dec::mode(R_SCI, SRC_F64), p),
        (dec::mode(R_SCI, SRC_F32), p),
        (dec::mode(R_PY, SRC_DEC), 0),
        (dec::mode(R_JS, SRC_DEC), 0),
    ];
    modes.into_iter().find_map(|(m, p)| dec::encode(t, m, p, exp3, ps, tmp).map(|(v, s, f)| (v, s, m, f)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pick(lines: &[&str]) -> Style {
        let toks: Vec<(usize, &[u8])> =
            lines.iter().flat_map(|l| l.split(' ').map(str::as_bytes).enumerate()).collect();
        choose(&toks)
    }

    #[test]
    fn chooses_the_writer() {
        assert_eq!(pick(&["1.000000 -2.500000 0.125000", "0.333333 4.000000 -0.000000"]).mode, R_FIX);
        // %.17g of doubles
        let st = pick(&["0.10000000000000001 12.759518999999999 -2.470218", "39.257838999999997 1 0"]);
        assert_eq!((st.mode, st.p), (dec::mode(R_GEN, SRC_F64), 17));
        // Python repr of widened floats
        let st = pick(&["0.10000000149011612 476.1831970214844 1.0", "-0.0 128.5883026123047 0.5"]);
        assert_eq!(st.mode, dec::mode(R_PY, SRC_F32));
        // %.6g (of floats here: f32 has these digits, and codes fewer scale changes)
        let st = pick(&["1.64901e-08 0.5 -123.456", "1 100000 2.5e+06", "0.25 3.14159 -0.0001"]);
        assert_eq!((dec::render_of(st.mode), st.p), (R_GEN, 6));
    }

    #[test]
    fn encode_falls_back() {
        let mut tmp = vec![];
        let fix = Style::default();
        assert_eq!(fix.encode(b"1.50", 0, &mut tmp), Some((150, 2, R_FIX, 2 | fixed::F_POINT)));
        assert_eq!(fix.encode(b"1.5e+30", 0, &mut tmp).map(|r| r.2), Some(M_GEN));
        assert_eq!(fix.encode(b"1.50e+30", 0, &mut tmp).map(|r| r.2), Some(dec::mode(R_SCI, SRC_DEC)));
        assert_eq!(fix.encode(b"1E5", 0, &mut tmp), None);
        assert_eq!(fix.encode(b"nan", 0, &mut tmp), None);
    }
}
