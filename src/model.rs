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

use crate::rans::{self, BitR, BitW, Decoder, Tables};

use crate::psd::{Error, Result};

const ACT: usize = 8;
const CROSS: usize = 4;
const CTX: usize = ACT * CROSS;
/// Plane kinds: first color plane, other color planes, alpha, lone plane.
const KINDS: usize = 4;
/// Run length tables, by plane kind, transparency and how far the row above stays equal.
const RUN_BASE: usize = KINDS * 2 * CTX;
const RUN_CTX: usize = 8;
/// Run mode needs this many samples above, from above on, equal to the left one (and the
/// upper left one). Short runs are cheap enough with the stretch above as context.
const RUN_LOOK: usize = 4;
const _: () = assert!(RUN_LOOK == 4, "run_above compares 4 samples");
/// Tables of the difference to the hint where the left and upper samples matched it, by
/// plane kind and whether the upper diagonal ones did.
const MATCH_BASE: usize = RUN_BASE + KINDS * 2 * RUN_CTX;
/// Tables of the length of a run of pixels equal to the hint, by whether the pixel above the
/// next one matched it.
const HRUN_BASE: usize = MATCH_BASE + KINDS * 4;
/// Tables of the joint symbol of the second and third color planes (both residuals relative
/// to the first plane's within ±2, or an escape), by transparency and context.
const JOINT_BASE: usize = HRUN_BASE + 2;
pub const TABLES: usize = JOINT_BASE + 2 * CTX;
/// Joint symbols: 5 x 5 pairs of residuals in -2..=2, then the escape.
const JOINT_ESCAPE: u8 = 25;
/// Symbols per chunk of the entropy coded stream.
pub const CHUNK: usize = 1 << 22;

fn corrupt() -> Error {
    Error("corrupt pixel data".into())
}

pub trait Coder {
    /// Whether symbols come from the coder (decoding) rather than from the samples.
    const DECODES: bool;
    /// The residual symbol of the current sample in table `t`; `sym` is the encoder's.
    fn code(&mut self, t: usize, sym: u8) -> u8;
}

/// Gives back the encoder's symbols: walks planes that are not coded, for their residuals.
struct Passthrough;

impl Coder for Passthrough {
    const DECODES: bool = false;
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

/// Whether the left sample `v` starts a run: the upper left one and [`RUN_LOOK`] from above
/// on equal it.
#[inline(always)]
fn run_above(up: &[u8], x: usize, v: u8) -> bool {
    // The row above alone first: it is known, while v (just decoded) may not be yet.
    match up.get(x - 1..x + RUN_LOOK) {
        Some(s) => u32::from_ne_bytes(s[1..5].try_into().unwrap()) == u32::from_ne_bytes([s[0]; 4]) && s[0] == v,
        None => false,
    }
}

/// How many samples of `up` from `x` on equal `v`, up to 128 (a run's context).
#[inline(always)]
fn stretch_above(up: &[u8], x: usize, v: u8) -> usize {
    crate::packbits::count_equal(&up[x..(x + 128).min(up.len())], v)
}

/// How many samples of `a` and `b` are equal from the start, compared 8 at a time.
#[inline(always)]
fn count_same(a: &[u8], b: &[u8]) -> usize {
    let n = a.len().min(b.len());
    let mut i = 0;
    while i + 8 <= n {
        let x = u64::from_ne_bytes(a[i..i + 8].try_into().unwrap()) ^ u64::from_ne_bytes(b[i..i + 8].try_into().unwrap());
        if x != 0 {
            return i + (if cfg!(target_endian = "little") { x.trailing_zeros() } else { x.leading_zeros() } / 8) as usize;
        }
        i += 8;
    }
    i + a[i..n].iter().zip(&b[i..n]).take_while(|(x, y)| x == y).count()
}

/// The run table context of a stretch of `n` equal samples above.
#[inline(always)]
fn run_ctx(n: usize) -> usize {
    ((usize::BITS - n.leading_zeros()) as usize).saturating_sub(1).min(RUN_CTX - 1)
}

/// The rows a sample is coded with, cut to the row width.
struct Rows<'a> {
    up: &'a [u8],
    res_prev: &'a [u8],
    al: &'a [u8],
    alp: &'a [u8],
    hc: &'a [u8],
    hp: &'a [u8],
    base: usize,
    match_t: usize,
}

/// The MED prediction of sample `x` of row `p`, and the neighbors (a, b, c, d) it and the
/// context are made of, with transparent neighbors replaced (see [`code_sample`] for the
/// flags and bounds).
#[inline(always)]
fn predict<const ALPHA: bool, const TOP: bool, const LEFT: bool, const RIGHT: bool>(q: &Rows, p: &[u8], x: usize, vis: u8) -> (u8, [u8; 4]) {
    debug_assert!(x < p.len() && (!LEFT || x > 0) && (!RIGHT || x + 1 < p.len()));
    // SAFETY: as in code_sample.
    unsafe {
        let g = |s: &[u8], i: usize| *s.get_unchecked(i);
        let up = q.up;
        let mut a = if LEFT { g(p, x - 1) } else if !TOP { g(up, x) } else { 0 };
        let mut b = if !TOP { g(up, x) } else { a };
        let mut cc = if LEFT && !TOP { g(up, x - 1) } else { b };
        let d = if !TOP && RIGHT { g(up, x + 1) } else { b };
        if ALPHA && vis & 2 != 0 {
            // Transparent neighbors are replaced by a visible one: their color is arbitrary.
            let jw = (LEFT && g(q.al, x - 1) > 0).then(|| g(p, x - 1));
            let jn = (!TOP && g(q.alp, x) > 0).then(|| g(up, x));
            let jnw = (LEFT && !TOP && g(q.alp, x - 1) > 0).then(|| g(up, x - 1));
            if let Some(fb) = jw.or(jn).or(jnw) {
                a = jw.unwrap_or(fb);
                b = jn.unwrap_or(fb);
                cc = jnw.unwrap_or(fb);
            }
        }
        (med(a, b, cc), [a, b, cc, d])
    }
}

/// Codes sample `x` of row `p` (not in a run); `f` is the first color plane's residual at `x`
/// (with `FIRST`). `TOP`: first row; `LEFT`/`RIGHT`: whether there are samples to the
/// left/right. Indices are in bounds: `x < w`, every row has `w` samples (but the optional
/// ones, which are only read under their flags).
#[inline(always)]
fn code_sample<C: Coder, const ALPHA: bool, const FIRST: bool, const HINT: bool, const TOP: bool, const LEFT: bool, const RIGHT: bool>(
    c: &mut C,
    q: &Rows,
    p: &mut [u8],
    res: &mut [u8],
    x: usize,
    f: u8,
    vis: u8,
) {
    debug_assert!(x < p.len() && (!LEFT || x > 0) && (!RIGHT || x + 1 < p.len()));
    // SAFETY: as documented; LEFT means x >= 1 and RIGHT means x + 1 < w.
    unsafe {
        let g = |s: &[u8], i: usize| *s.get_unchecked(i);
        let up = q.up;
        let transparent = ALPHA && vis & 1 != 0;
        let (pred, [a, b, cc, d]) = predict::<ALPHA, TOP, LEFT, RIGHT>(q, p, x, vis);
        if HINT && LEFT && !TOP && g(p, x - 1) == g(q.hc, x - 1) && g(up, x) == g(q.hp, x) {
            let m = (g(up, x - 1) == g(q.hp, x - 1)) as usize + 2 * (RIGHT && g(up, x + 1) == g(q.hp, x + 1)) as usize;
            let s = c.code(q.match_t + m, g(p, x).wrapping_sub(g(q.hc, x)));
            let v = g(q.hc, x).wrapping_add(s);
            *p.get_unchecked_mut(x) = v;
            *res.get_unchecked_mut(x) = v.wrapping_sub(pred);
            return;
        }
        let rw = if LEFT { mag(g(res, x - 1)) } else { 0 };
        let rn = if !TOP { mag(g(q.res_prev, x)) } else { 0 };
        let act = a.abs_diff(cc) as u32 + b.abs_diff(cc) as u32 + b.abs_diff(d) as u32;
        let (off, cross) = if FIRST { (f, bucket(mag(f), CROSS)) } else { (0, 0) };
        let ctx = bucket(act + 2 * (rw + rn), ACT) * CROSS + cross;
        let t = q.base + transparent as usize * CTX + ctx;
        let rr = g(p, x).wrapping_sub(pred);
        let s = c.code(t, rr.wrapping_sub(off));
        let rr = s.wrapping_add(off);
        *res.get_unchecked_mut(x) = rr;
        *p.get_unchecked_mut(x) = pred.wrapping_add(rr);
    }
}

/// Codes samples `x` of the second and third color planes together; `f` is the first plane's
/// residual at `x`. Where both residuals (relative to `f`) are within ±2, one joint symbol;
/// otherwise an escape, then each in its plane's tables as [`code_sample`] does.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn code_pair<C: Coder, const ALPHA: bool, const TOP: bool, const LEFT: bool, const RIGHT: bool>(
    c: &mut C,
    q: [&Rows; 2],
    p: [&mut [u8]; 2],
    res: [&mut [u8]; 2],
    x: usize,
    f: u8,
    vis: u8,
) {
    let [p1, p2] = p;
    let [r1, r2] = res;
    let transparent = ALPHA && vis & 1 != 0;
    let cross = bucket(mag(f), CROSS);
    // Each plane's prediction and table, as code_sample would choose it.
    let side = |q: &Rows, p: &[u8], r: &[u8]| {
        let (pred, [a, b, cc, d]) = predict::<ALPHA, TOP, LEFT, RIGHT>(q, p, x, vis);
        let rw = if LEFT { mag(r[x - 1]) } else { 0 };
        let rn = if !TOP { mag(q.res_prev[x]) } else { 0 };
        let act = a.abs_diff(cc) as u32 + b.abs_diff(cc) as u32 + b.abs_diff(d) as u32;
        (pred, act + 2 * (rw + rn))
    };
    let (pred1, e1) = side(q[0], p1, r1);
    let (pred2, e2) = side(q[1], p2, r2);
    let t1 = q[0].base + transparent as usize * CTX + bucket(e1, ACT) * CROSS + cross;
    let t2 = q[1].base + transparent as usize * CTX + bucket(e2, ACT) * CROSS + cross;
    let tj = JOINT_BASE + transparent as usize * CTX + bucket((e1 + e2) / 2, ACT) * CROSS + cross;
    let s1 = p1[x].wrapping_sub(pred1).wrapping_sub(f);
    let s2 = p2[x].wrapping_sub(pred2).wrapping_sub(f);
    let small = |s: u8| (s as i8 + 2) as u8;
    let j = if small(s1) < 5 && small(s2) < 5 { small(s1) * 5 + small(s2) } else { JOINT_ESCAPE };
    let j = c.code(tj, j);
    let (s1, s2) = if j < JOINT_ESCAPE {
        ((j / 5).wrapping_sub(2), (j % 5).wrapping_sub(2))
    } else {
        (c.code(t1, s1), c.code(t2, s2))
    };
    let (rr1, rr2) = (s1.wrapping_add(f), s2.wrapping_add(f));
    (r1[x], r2[x]) = (rr1, rr2);
    (p1[x], p2[x]) = (pred1.wrapping_add(rr1), pred2.wrapping_add(rr2));
}

/// Visibility of pixel `x` for the color planes, from the alpha rows: 1 if transparent, 2 if
/// visible with a transparent neighbor (whose color is then not used).
#[inline(always)]
fn visibility<const ALPHA: bool, const TOP: bool, const LEFT: bool>(q: &Rows, x: usize) -> u8 {
    if !ALPHA {
        return 0;
    }
    if q.al[x] == 0 {
        1
    } else if (LEFT && q.al[x - 1] == 0) || (!TOP && (q.alp[x] == 0 || LEFT && q.alp[x - 1] == 0)) {
        2
    } else {
        0
    }
}

/// Codes one row `p` of a plane, writing its residuals to `res`. The flags say which of the
/// optional rows of `r` are there (alpha, first plane's residuals, hint), and whether this is
/// the first row.
fn code_row<C: Coder, const ALPHA: bool, const FIRST: bool, const HINT: bool, const TOP: bool>(c: &mut C, r: &RowCtx, p: &mut [u8], res: &mut [u8]) {
    let w = p.len();
    let res = &mut res[..w];
    let (al, alp) = match r.alpha {
        Some((a, b)) if ALPHA => (&a[..w], &b[..w]),
        _ => (&[][..], &[][..]),
    };
    let (hc, hp) = match r.hint {
        Some((a, b)) if HINT => (&a[..w], &b[..w]),
        _ => (&[][..], &[][..]),
    };
    let first = match r.first {
        Some(f) if FIRST => &f[..w],
        _ => &[][..],
    };
    let fx = |x: usize| if FIRST { first[x] } else { 0 };
    let q = Rows {
        up: &r.prev[..w],
        res_prev: &r.res_prev[..w],
        al,
        alp,
        hc,
        hp,
        base: r.kind * 2 * CTX,
        match_t: MATCH_BASE + r.kind * 4,
    };
    let run_t = RUN_BASE + r.kind * 2 * RUN_CTX;
    if w == 1 {
        code_sample::<C, ALPHA, FIRST, HINT, TOP, false, false>(c, &q, p, res, 0, fx(0), visibility::<ALPHA, TOP, false>(&q, 0));
        return;
    }
    code_sample::<C, ALPHA, FIRST, HINT, TOP, false, true>(c, &q, p, res, 0, fx(0), visibility::<ALPHA, TOP, false>(&q, 0));
    let mut x = 1;
    let mut after_run = false;
    while x + 1 < w {
        // Run mode: where the left sample equals the 33 above from the upper left on, the
        // number of following samples equal to it, in chunks of up to 254 (255 continues).
        if !TOP && !after_run {
            let v = p[x - 1];
            if run_above(q.up, x, v) {
                let t = run_t + (ALPHA && al[x] == 0) as usize * RUN_CTX + run_ctx(stretch_above(q.up, x, v));
                let mut n = 0;
                loop {
                    let avail = if C::DECODES { 0 } else { crate::packbits::count_equal(&p[x + n..(x + n + 255).min(w)], v) };
                    let s = (c.code(t, avail as u8) as usize).min(w - x - n);
                    if C::DECODES {
                        p[x + n..x + n + s].fill(v);
                    }
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
        code_sample::<C, ALPHA, FIRST, HINT, TOP, true, true>(c, &q, p, res, x, fx(x), visibility::<ALPHA, TOP, true>(&q, x));
        x += 1;
    }
    if x + 1 == w {
        code_sample::<C, ALPHA, FIRST, HINT, TOP, true, false>(c, &q, p, res, x, fx(x), visibility::<ALPHA, TOP, true>(&q, x));
    }
}

/// The three color planes of a row, coded pixel by pixel (R, G, B at x, then x + 1): the
/// planes are independent chains of dependencies, which the CPU can overlap. Plane 0 is coded
/// if `how[0] == 1`, walked for its residuals if 2; planes 1 and 2 are coded if their `how`
/// is 1, given (known) if 2, and absent (copied after the row) if 0. A run is where every
/// plane there equals its left sample over the 33 above (from the upper left), as many pixels
/// as they all equal it.
struct Colors<'a, 'b> {
    p: [&'b mut [u8]; 3],
    res: [&'b mut [u8]; 3],
    q: [Rows<'a>; 3],
    how: [u8; 3],
    run_t: usize,
}

fn code_colors<C: Coder, const ALPHA: bool, const HINT: bool, const TOP: bool>(c: &mut C, k: &mut Colors) {
    let w = k.p[0].len();
    let [p0, p1, p2] = &mut k.p;
    let [r0, r1, r2] = &mut k.res;
    let [q0, q1, q2] = &k.q;
    let how = k.how;
    let pixel = |c: &mut C, p0: &mut [u8], p1: &mut [u8], p2: &mut [u8], r0: &mut [u8], r1: &mut [u8], r2: &mut [u8], x: usize, left: bool, right: bool| {
        let vis = if left { visibility::<ALPHA, TOP, true>(q0, x) } else { visibility::<ALPHA, TOP, false>(q0, x) };
        macro_rules! one {
            ($c:expr, $cc:ty, $q:expr, $p:expr, $r:expr, $first:literal, $f:expr) => {
                match (left, right) {
                    (true, true) => code_sample::<$cc, ALPHA, $first, HINT, TOP, true, true>($c, $q, $p, $r, x, $f, vis),
                    (false, true) => code_sample::<$cc, ALPHA, $first, HINT, TOP, false, true>($c, $q, $p, $r, x, $f, vis),
                    (true, false) => code_sample::<$cc, ALPHA, $first, HINT, TOP, true, false>($c, $q, $p, $r, x, $f, vis),
                    (false, false) => code_sample::<$cc, ALPHA, $first, HINT, TOP, false, false>($c, $q, $p, $r, x, $f, vis),
                }
            };
        }
        match how[0] {
            1 => one!(c, C, q0, p0, r0, false, 0),
            _ => one!(&mut Passthrough, Passthrough, q0, p0, r0, false, 0),
        }
        let f = r0[x];
        if !HINT && how[1] == 1 && how[2] == 1 {
            let (q, p, r) = ([q1, q2], [&mut *p1, &mut *p2], [&mut *r1, &mut *r2]);
            match (left, right) {
                (true, true) => code_pair::<C, ALPHA, TOP, true, true>(c, q, p, r, x, f, vis),
                (false, true) => code_pair::<C, ALPHA, TOP, false, true>(c, q, p, r, x, f, vis),
                (true, false) => code_pair::<C, ALPHA, TOP, true, false>(c, q, p, r, x, f, vis),
                (false, false) => code_pair::<C, ALPHA, TOP, false, false>(c, q, p, r, x, f, vis),
            }
            return;
        }
        if how[1] == 1 {
            one!(c, C, q1, p1, r1, true, f);
        }
        if how[2] == 1 {
            one!(c, C, q2, p2, r2, true, f);
        }
    };
    pixel(c, p0, p1, p2, r0, r1, r2, 0, false, w > 1);
    let mut x = 1;
    let mut after_run = false;
    while x + 1 < w {
        // Hint run: where the left and upper pixels equal the hint, the number of pixels from
        // here equal to it (in chunks of up to 254; 255 continues).
        if HINT && !TOP && !after_run {
            let m = |p: &[u8], q: &Rows| p[x - 1] == q.hc[x - 1] && q.up[x] == q.hp[x];
            if m(p0, q0) && (how[1] == 0 || m(p1, q1)) && (how[2] == 0 || m(p2, q2)) {
                let t = HRUN_BASE + (q0.up[x + 1] == q0.hp[x + 1]) as usize;
                let mut n = 0;
                loop {
                    let avail = if C::DECODES {
                        0
                    } else {
                        let end = (x + n + 255).min(w);
                        let mut a = count_same(&p0[x + n..end], &q0.hc[x + n..end]);
                        if how[1] != 0 {
                            a = a.min(count_same(&p1[x + n..end], &q1.hc[x + n..end]));
                        }
                        if how[2] != 0 {
                            a = a.min(count_same(&p2[x + n..end], &q2.hc[x + n..end]));
                        }
                        a
                    };
                    let s = (c.code(t, avail as u8) as usize).min(w - x - n);
                    if C::DECODES {
                        let r = x + n..x + n + s;
                        if how[0] == 1 {
                            p0[r.clone()].copy_from_slice(&q0.hc[r.clone()]);
                        }
                        if how[1] == 1 {
                            p1[r.clone()].copy_from_slice(&q1.hc[r.clone()]);
                        }
                        if how[2] == 1 {
                            p2[r.clone()].copy_from_slice(&q2.hc[r]);
                        }
                    }
                    n += s;
                    if s < 255 {
                        break;
                    }
                }
                for r in [&mut **r0, &mut **r1, &mut **r2] {
                    r[x..x + n].fill(0);
                }
                x += n;
                after_run = true;
                continue;
            }
        }
        if !TOP && !after_run {
            // The rows above first (see run_above).
            let flat = |q: &Rows| q.up.get(x - 1..x + RUN_LOOK).is_some_and(|s| u32::from_ne_bytes(s[1..5].try_into().unwrap()) == u32::from_ne_bytes([s[0]; 4]));
            if flat(q0) && (how[1] == 0 || flat(q1)) && (how[2] == 0 || flat(q2))
                && p0[x - 1] == q0.up[x - 1] && (how[1] == 0 || p1[x - 1] == q1.up[x - 1]) && (how[2] == 0 || p2[x - 1] == q2.up[x - 1])
            {
                let v = [p0[x - 1], p1[x - 1], p2[x - 1]];
                let t = k.run_t + (ALPHA && q0.al[x] == 0) as usize * RUN_CTX + run_ctx(stretch_above(q0.up, x, v[0]));
                let mut n = 0;
                loop {
                    let avail = if C::DECODES {
                        0
                    } else {
                        let end = (x + n + 255).min(w);
                        let mut a = crate::packbits::count_equal(&p0[x + n..end], v[0]);
                        if how[1] != 0 {
                            a = a.min(crate::packbits::count_equal(&p1[x + n..end], v[1]));
                        }
                        if how[2] != 0 {
                            a = a.min(crate::packbits::count_equal(&p2[x + n..end], v[2]));
                        }
                        a
                    };
                    let s = (c.code(t, avail as u8) as usize).min(w - x - n);
                    if C::DECODES {
                        if how[0] == 1 {
                            p0[x + n..x + n + s].fill(v[0]);
                        }
                        if how[1] == 1 {
                            p1[x + n..x + n + s].fill(v[1]);
                        }
                        if how[2] == 1 {
                            p2[x + n..x + n + s].fill(v[2]);
                        }
                    }
                    n += s;
                    if s < 255 {
                        break;
                    }
                }
                for r in [&mut **r0, &mut **r1, &mut **r2] {
                    r[x..x + n].fill(0);
                }
                x += n;
                after_run = true;
                continue;
            }
        }
        after_run = false;
        pixel(c, p0, p1, p2, r0, r1, r2, x, true, true);
        x += 1;
    }
    if x + 1 == w {
        pixel(c, p0, p1, p2, r0, r1, r2, x, true, false);
    }
}

fn code_colors_any<C: Coder>(c: &mut C, k: &mut Colors, alpha: bool, hint: bool, top: bool) {
    match (alpha, hint, top) {
        (false, false, false) => code_colors::<C, false, false, false>(c, k),
        (false, false, true) => code_colors::<C, false, false, true>(c, k),
        (false, true, false) => code_colors::<C, false, true, false>(c, k),
        (false, true, true) => code_colors::<C, false, true, true>(c, k),
        (true, false, false) => code_colors::<C, true, false, false>(c, k),
        (true, false, true) => code_colors::<C, true, false, true>(c, k),
        (true, true, false) => code_colors::<C, true, true, false>(c, k),
        (true, true, true) => code_colors::<C, true, true, true>(c, k),
    }
}

/// [`code_row`] for the rows `r` has.
fn code_row_any<C: Coder>(c: &mut C, r: &RowCtx, p: &mut [u8], res: &mut [u8]) {
    macro_rules! go {
        ($a:literal, $f:literal, $h:literal) => {
            if r.top { code_row::<C, $a, $f, $h, true>(c, r, p, res) } else { code_row::<C, $a, $f, $h, false>(c, r, p, res) }
        };
    }
    match (r.alpha.is_some(), r.first.is_some(), r.hint.is_some()) {
        (false, false, false) => go!(false, false, false),
        (true, false, false) => go!(true, false, false),
        (false, true, false) => go!(false, true, false),
        (true, true, false) => go!(true, true, false),
        (false, false, true) => go!(false, false, true),
        (true, false, true) => go!(true, false, true),
        (false, true, true) => go!(false, true, true),
        (true, true, true) => go!(true, true, true),
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

    /// Codes row `k` of `rows` on its own, as a plane of `kind`.
    fn code_plane<C: Coder>(&mut self, c: &mut C, rows: &mut [Vec<u8>], hint: Option<&[Vec<u8>]>, k: usize, kind: usize) {
        let coded = self.coded(k);
        let n = rows.len();
        let colors = n - self.has_alpha as usize;
        let is_alpha = self.has_alpha && k == colors;
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
            code_row_any(c, &ctx, p, res_k);
        } else {
            code_row_any(&mut Passthrough, &ctx, p, res_k);
        }
    }

    /// The last row coded of plane `k`.
    pub fn last(&self, k: usize) -> &[u8] {
        &self.prev[k]
    }

    /// Whether plane `k` is coded (rather than known or a copy of another).
    pub fn coded(&self, k: usize) -> bool {
        !self.known[k] && self.same_as[k].is_none()
    }

    /// Codes the next row: `rows` holds one row per plane (the encoder's samples, or for the
    /// decoder the known planes' and room for the others), `hint` the hint's rows. Afterwards
    /// the rows of the planes are in [`GroupCoder::last`], and `rows` and `hint` hold
    /// buffers of the right size with arbitrary contents.
    pub fn code_row<C: Coder>(&mut self, c: &mut C, rows: &mut [Vec<u8>], mut hint: Option<&mut [Vec<u8>]>) {
        let n = rows.len();
        let colors = n - self.has_alpha as usize;
        let lone = colors == 1 && !self.has_alpha;
        let copy = |rows: &mut [Vec<u8>], k: usize, j: usize| {
            let (a, b) = if j < k {
                let (a, b) = rows.split_at_mut(k);
                (&a[j], &mut b[0])
            } else {
                let (a, b) = rows.split_at_mut(j);
                (&b[0], &mut a[k])
            };
            b.copy_from_slice(a);
        };
        if colors == 3 {
            if self.has_alpha {
                self.code_plane(c, rows, hint.as_deref(), colors, 2);
            }
            // Plane 0 is needed during the row; the others equal to a plane are copied after.
            if let Some(j) = self.same_as[0] {
                copy(rows, 0, j);
            }
            let w = self.w;
            let how: [u8; 3] = std::array::from_fn(|k| if self.coded(k) { 1 } else if k == 0 || self.same_as[k].is_none() { 2 } else { 0 });
            let (cr, ar) = rows.split_at_mut(3);
            let (al, alp) = if self.has_alpha { (&ar[0][..w], &self.prev[3][..w]) } else { (&[][..], &[][..]) };
            let hint_rows = hint.as_deref();
            let q: [Rows; 3] = std::array::from_fn(|k| Rows {
                up: &self.prev[k][..w],
                res_prev: &self.res_prev[k][..w],
                al,
                alp,
                hc: hint_rows.map_or(&[][..], |h| &h[k][..w]),
                hp: if hint_rows.is_some() { &self.hint_prev[k][..w] } else { &[][..] },
                base: (if k == 0 { 0 } else { 1 }) * 2 * CTX,
                match_t: MATCH_BASE + (if k == 0 { 0 } else { 1 }) * 4,
            });
            let [c0, c1, c2] = cr else { unreachable!() };
            let [e0, e1, e2] = &mut self.res[..3] else { unreachable!() };
            let mut k = Colors {
                p: [&mut c0[..w], &mut c1[..w], &mut c2[..w]],
                res: [&mut e0[..w], &mut e1[..w], &mut e2[..w]],
                q,
                how,
                run_t: RUN_BASE,
            };
            let (alpha, hinted, top) = (self.has_alpha, hint_rows.is_some(), self.top);
            if how[0] == 1 || how[1] == 1 || how[2] == 1 {
                code_colors_any(c, &mut k, alpha, hinted, top);
            } else {
                code_colors_any(&mut Passthrough, &mut k, alpha, hinted, top);
            }
            for k in 1..3 {
                if let Some(j) = self.same_as[k] {
                    copy(rows, k, j);
                }
            }
        } else {
            for k in Self::order(n, self.has_alpha) {
                if let Some(j) = self.same_as[k] {
                    copy(rows, k, j);
                }
                let is_alpha = self.has_alpha && k == colors;
                let kind = if lone { 3 } else if is_alpha { 2 } else if k == 0 { 0 } else { 1 };
                // Planes not coded are walked only for the first color plane's residuals.
                if !(self.coded(k) || kind == 0 && colors > 1) {
                    continue;
                }
                self.code_plane(c, rows, hint.as_deref(), k, kind);
            }
        }
        // The callers fill every row anew: the buffers are swapped rather than copied.
        for k in 0..n {
            std::mem::swap(&mut self.prev[k], &mut rows[k]);
            std::mem::swap(&mut self.res_prev[k], &mut self.res[k]);
            if let Some(h) = hint.as_deref_mut() {
                std::mem::swap(&mut self.hint_prev[k], &mut h[k]);
            }
        }
        self.top = false;
    }
}

/// Records symbols, and codes them a chunk at a time into `out`.
pub struct Recorder<'o> {
    syms: Vec<(u16, u16)>,
    /// Symbol counts of the chunk, per table.
    counts: Vec<[u32; 256]>,
    out: &'o mut Vec<u8>,
    pub chunks: usize,
    /// Symbols coded so far.
    pub total: usize,
}

impl<'o> Recorder<'o> {
    pub fn new(out: &'o mut Vec<u8>) -> Self {
        Recorder { syms: Vec::with_capacity(CHUNK), counts: vec![[0; 256]; TABLES], out, chunks: 0, total: 0 }
    }

    /// Writes the recorded symbols as a chunk: symbol count, tables, rANS stream.
    fn flush(&mut self) {
        if self.syms.is_empty() {
            return;
        }
        let counts = &mut self.counts;
        let mut w = BitW::default();
        let mut freqs = Vec::with_capacity(TABLES);
        for c in counts.iter() {
            let used = c.iter().any(|&n| n > 0);
            w.bits(1, used as u32);
            freqs.push(if used { rans::write_table(&mut w, c) } else { unused() });
        }
        let head = w.finish();
        counts.iter_mut().for_each(|c| *c = [0; 256]);
        let body = rans::encode(&self.syms, &freqs);
        let o = &mut *self.out;
        crate::container::put_varint(o, self.syms.len() as u64);
        crate::container::put_varint(o, head.len() as u64);
        o.extend_from_slice(&head);
        crate::container::put_varint(o, body.len() as u64);
        o.extend_from_slice(&body);
        self.total += self.syms.len();
        self.syms.clear();
        self.chunks += 1;
    }

    /// Writes what is left; the stream ends with a chunk of no symbols. Returns the number
    /// of symbols.
    pub fn finish(mut self) -> usize {
        self.flush();
        crate::container::put_varint(self.out, 0);
        self.total
    }
}

impl Coder for Recorder<'_> {
    const DECODES: bool = false;
    #[inline(always)]
    fn code(&mut self, t: usize, sym: u8) -> u8 {
        if self.syms.len() == CHUNK {
            self.flush();
        }
        self.syms.push((t as u16, sym as u16));
        // SAFETY: t is a table of the model, below TABLES.
        unsafe { self.counts.get_unchecked_mut(t)[sym as usize] += 1 };
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
    /// The chunk's used tables, compacted, and the index of every model table among them.
    tables: Tables,
    map: Vec<u16>,
    dec: Decoder<'a>,
    left: usize,
    /// Set when the stream turns out corrupt; decoding then yields zeros.
    pub failed: bool,
    ended: bool,
}

impl<'a> Player<'a> {
    pub fn new(src: &'a [u8]) -> Player<'a> {
        let mut p = Player { src, at: 0, tables: Tables::new(&[]), map: vec![0; TABLES], dec: Decoder::new(&[]), left: 0, failed: false, ended: false };
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
            // Table 0 stands for every unused one.
            let mut freqs = vec![unused()];
            for t in 0..TABLES {
                if r.bits(1) == 1 {
                    self.map[t] = freqs.len() as u16;
                    freqs.push(rans::read_table(&mut r, 256).map_err(|_| corrupt())?);
                } else {
                    self.map[t] = 0;
                }
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
    const DECODES: bool = true;
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
        // SAFETY: t is a table of the model, below TABLES.
        let t = unsafe { *self.map.get_unchecked(t) } as usize;
        self.dec.get(&self.tables, t) as u8
    }
}
