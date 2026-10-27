//! The residual model: an adaptive sign-sign LMS stage on top of the FLAC residuals, then
//! a token of the scaled error magnitude (rANS, static tables by scale context) and raw
//! bits (low bits, sign, escape mantissa).
//!
//! The two parts run as separate passes over a frame: the scale context depends only on
//! past errors, so the tokens decode without the LMS stage, and the LMS stage then turns
//! errors back into residuals. Both states run across frames per channel index.

use crate::audio::bits::{BitReader, FastBits};
use crate::audio::rans::{DecLocal, EncLocal};
use aqr_common::wire::Wire;

/// LMS taps.
pub const TAPS: usize = 32;
/// Weight step (weights are in 1/4096).
const MU: i16 = 4;
const W_SHIFT: u32 = 12;
/// Typical magnitude (bits) of the history entries.
const IN_BITS: u32 = 9;

/// Tokens: magnitudes below `DIRECT` directly, then two per bit length.
const DIRECT: u32 = 16;
pub const N_TOK: usize = DIRECT as usize + 2 * 28;
/// Scale buckets (two per octave of the running mean).
pub const N_SCALE: usize = 62;
/// Contexts: scale bucket × (first channel, other channels).
pub const N_CTX: usize = N_SCALE * 2;

/// Weight updates are applied this many samples late (delayed LMS), which takes the
/// update out of the dependency chain from one prediction to the next.
const DELAY: usize = 2;

/// LMS state of a channel.
#[derive(Clone)]
pub struct Lms {
    w: [i16; TAPS],
    /// The last `TAPS` inputs, oldest first, shifted right by `in_shift` and clamped to i16.
    h: [i16; TAPS],
    /// Pending weight steps, oldest first.
    u: [[i16; TAPS]; DELAY],
    /// Right shift of the inputs into the history.
    in_shift: u32,
}

impl Default for Lms {
    fn default() -> Self {
        Lms { w: [0; TAPS], h: [0; TAPS], u: [[0; TAPS]; DELAY], in_shift: 0 }
    }
}

impl Lms {
    /// Sets the input shift from the channel's scale state, so that the history keeps the
    /// residuals at about `IN_BITS` bits; the stored history is rescaled.
    pub fn set_scale(&mut self, sc: &Scale) {
        let l = 31 - sc.avg.leading_zeros();
        let sh = l.saturating_sub(5 + IN_BITS).min(16);
        if sh != self.in_shift {
            for v in &mut self.h {
                *v = if sh > self.in_shift {
                    *v >> (sh - self.in_shift)
                } else {
                    ((*v as i32) << (self.in_shift - sh)).clamp(-32768, 32767) as i16
                };
            }
            self.in_shift = sh;
        }
    }

    /// Runs the stage over `buf` in place: residuals to errors (`DEC` false) or back.
    pub fn run<const DEC: bool>(&mut self, buf: &mut [i32]) {
        self.run_in::<DEC, false>(buf)
    }

    /// [`Lms::run`] with a separate instance for the pipeline stages (`PIPE`, see
    /// `residual::run_lms`).
    pub(crate) fn run_in<const DEC: bool, const PIPE: bool>(&mut self, buf: &mut [i32]) {
        lms_run::<DEC, PIPE>(self, buf)
    }
}

/// Per sample: the prediction is the dot product of the weights and the history, the
/// error's sign times the history's signs is the weight step, applied `DELAY` samples
/// later. The newest input's term is computed separately (scalar), so that decoding's
/// chain from one sample to the next does not go through the vector sum.
#[cfg(target_arch = "aarch64")]
mod neon {
    use super::*;
    use std::arch::aarch64::*;

    /// An [`Lms`] in registers.
    pub struct Regs {
        w: [int16x8_t; 4],
        h: [int16x8_t; 4],
        u0: [int16x8_t; 4],
        u1: [int16x8_t; 4],
        /// The newest input and the last history vector without it.
        last: i32,
        hm: int16x8_t,
        sh: u32,
    }

    impl Regs {
        #[inline(always)]
        pub fn load(st: &Lms) -> Regs {
            const _: () = assert!(DELAY == 2 && TAPS == 32);
            // SAFETY: NEON is always available on aarch64; loads stay within the arrays.
            unsafe {
                let ld =
                    |p: *const i16| [vld1q_s16(p), vld1q_s16(p.add(8)), vld1q_s16(p.add(16)), vld1q_s16(p.add(24))];
                let h = ld(st.h.as_ptr());
                Regs {
                    w: ld(st.w.as_ptr()),
                    u0: ld(st.u[0].as_ptr()),
                    u1: ld(st.u[1].as_ptr()),
                    last: vgetq_lane_s16(h[3], 7) as i32,
                    hm: vsetq_lane_s16(0, h[3], 7),
                    h,
                    sh: st.in_shift,
                }
            }
        }

        #[inline(always)]
        pub fn store(&self, st: &mut Lms) {
            // SAFETY: stores stay within the arrays.
            unsafe {
                let stv = |p: *mut i16, v: &[int16x8_t; 4]| {
                    for k in 0..4 {
                        vst1q_s16(p.add(8 * k), v[k]);
                    }
                };
                stv(st.w.as_mut_ptr(), &self.w);
                stv(st.h.as_mut_ptr(), &self.h);
                stv(st.u[0].as_mut_ptr(), &self.u0);
                stv(st.u[1].as_mut_ptr(), &self.u1);
            }
        }

        #[inline(always)]
        pub fn step<const DEC: bool>(&mut self, v: &mut i32) {
            // SAFETY: NEON is always available on aarch64.
            unsafe {
                let (w, h) = (&mut self.w, &mut self.h);
                let mut a0 = vmull_s16(vget_low_s16(w[0]), vget_low_s16(h[0]));
                let mut a1 = vmull_high_s16(w[0], h[0]);
                let mut a2 = vmull_s16(vget_low_s16(w[1]), vget_low_s16(h[1]));
                let mut a3 = vmull_high_s16(w[1], h[1]);
                a0 = vmlal_s16(a0, vget_low_s16(w[2]), vget_low_s16(h[2]));
                a1 = vmlal_high_s16(a1, w[2], h[2]);
                a2 = vmlal_s16(a2, vget_low_s16(w[3]), vget_low_s16(self.hm));
                a3 = vmlal_high_s16(a3, w[3], self.hm);
                // Pairwise adds keep the four accumulators independent (plain adds would
                // be folded into one serial chain of multiply-accumulates).
                let older = vaddvq_s32(vpaddq_s32(vpaddq_s32(a0, a1), vpaddq_s32(a2, a3)));
                let acc = older.wrapping_add(vgetq_lane_s16(w[3], 7) as i32 * self.last);
                let p = (acc >> W_SHIFT) << self.sh;
                let (x, e) = if DEC { ((*v).wrapping_add(p), *v) } else { (*v, (*v).wrapping_sub(p)) };
                *v = if DEC { x } else { e };
                // Apply the oldest pending step; queue this sample's.
                let sg = ((e > 0) as i16 - (e < 0) as i16) * MU;
                let (one, mone) = (vdupq_n_s16(1), vdupq_n_s16(-1));
                for k in 0..4 {
                    w[k] = vaddq_s16(w[k], self.u0[k]);
                    self.u0[k] = self.u1[k];
                    let s = vminq_s16(vmaxq_s16(h[k], mone), one);
                    self.u1[k] = vmulq_n_s16(s, sg);
                }
                let xn = (x >> self.sh).clamp(-32768, 32767) as i16;
                self.hm = vextq_s16(h[3], vdupq_n_s16(0), 1);
                *h = [
                    vextq_s16(h[0], h[1], 1),
                    vextq_s16(h[1], h[2], 1),
                    vextq_s16(h[2], h[3], 1),
                    vextq_s16(h[3], vdupq_n_s16(xn), 1),
                ];
                self.last = xn as i32;
            }
        }
    }
}

/// [`neon`] in SIMD128 (of the profile of the modules, enabled here alone). The dot product
/// is `i32x4.dot_i16x8_s`'s: its sums wrap as NEON's multiply-accumulates do, so that the
/// predictions are the same.
#[cfg(target_family = "wasm")]
mod simd {
    use super::*;
    use core::arch::wasm32::*;

    /// An [`Lms`] in registers.
    pub struct Regs {
        w: [v128; 4],
        h: [v128; 4],
        u0: [v128; 4],
        u1: [v128; 4],
        /// The newest input and the last history vector without it.
        last: i32,
        hm: v128,
        sh: u32,
    }

    /// The four vectors from `p`.
    #[target_feature(enable = "simd128")]
    unsafe fn ld(p: *const i16) -> [v128; 4] {
        // SAFETY: as the callers make sure, 32 values from `p`
        unsafe { core::array::from_fn(|k| v128_load(p.add(8 * k) as *const v128)) }
    }

    impl Regs {
        #[target_feature(enable = "simd128")]
        pub fn load(st: &Lms) -> Regs {
            const _: () = assert!(DELAY == 2 && TAPS == 32);
            // SAFETY: the arrays are of 32 values
            let (w, h, u0, u1) =
                unsafe { (ld(st.w.as_ptr()), ld(st.h.as_ptr()), ld(st.u[0].as_ptr()), ld(st.u[1].as_ptr())) };
            Regs {
                w,
                u0,
                u1,
                last: i16x8_extract_lane::<7>(h[3]) as i32,
                hm: i16x8_replace_lane::<7>(h[3], 0),
                h,
                sh: st.in_shift,
            }
        }

        #[target_feature(enable = "simd128")]
        pub fn store(&self, st: &mut Lms) {
            let stv = |p: *mut i16, v: &[v128; 4]| {
                for (k, v) in v.iter().enumerate() {
                    // SAFETY: the arrays are of 32 values
                    unsafe { v128_store(p.add(8 * k) as *mut v128, *v) }
                }
            };
            stv(st.w.as_mut_ptr(), &self.w);
            stv(st.h.as_mut_ptr(), &self.h);
            stv(st.u[0].as_mut_ptr(), &self.u0);
            stv(st.u[1].as_mut_ptr(), &self.u1);
        }

        #[target_feature(enable = "simd128")]
        pub fn step<const DEC: bool>(&mut self, v: &mut i32) {
            let (w, h) = (&mut self.w, &mut self.h);
            let a = i32x4_add(
                i32x4_add(i32x4_dot_i16x8(w[0], h[0]), i32x4_dot_i16x8(w[1], h[1])),
                i32x4_add(i32x4_dot_i16x8(w[2], h[2]), i32x4_dot_i16x8(w[3], self.hm)),
            );
            let older = (i32x4_extract_lane::<0>(a).wrapping_add(i32x4_extract_lane::<1>(a)))
                .wrapping_add(i32x4_extract_lane::<2>(a).wrapping_add(i32x4_extract_lane::<3>(a)));
            let acc = older.wrapping_add(i16x8_extract_lane::<7>(w[3]) as i32 * self.last);
            let p = (acc >> W_SHIFT) << self.sh;
            let (x, e) = if DEC { ((*v).wrapping_add(p), *v) } else { (*v, (*v).wrapping_sub(p)) };
            *v = if DEC { x } else { e };
            // Apply the oldest pending step; queue this sample's.
            let sg = i16x8_splat(((e > 0) as i16 - (e < 0) as i16) * MU);
            let (one, mone) = (i16x8_splat(1), i16x8_splat(-1));
            for k in 0..4 {
                w[k] = i16x8_add(w[k], self.u0[k]);
                self.u0[k] = self.u1[k];
                let s = i16x8_min(i16x8_max(h[k], mone), one);
                self.u1[k] = i16x8_mul(s, sg);
            }
            let xn = (x >> self.sh).clamp(-32768, 32767) as i16;
            let next = |a: v128, b: v128| i16x8_shuffle::<1, 2, 3, 4, 5, 6, 7, 8>(a, b);
            self.hm = next(h[3], i16x8_splat(0));
            *h = [next(h[0], h[1]), next(h[1], h[2]), next(h[2], h[3]), next(h[3], i16x8_splat(xn))];
            self.last = xn as i32;
        }
    }

    #[target_feature(enable = "simd128")]
    pub fn run<const DEC: bool>(st: &mut Lms, buf: &mut [i32]) {
        let mut r = Regs::load(st);
        for v in buf.iter_mut() {
            r.step::<DEC>(v);
        }
        r.store(st);
    }

    #[target_feature(enable = "simd128")]
    pub fn run2<const DEC: bool>(sa: &mut Lms, a: &mut [i32], sb: &mut Lms, b: &mut [i32]) {
        let n = a.len().min(b.len());
        let mut ra = Regs::load(sa);
        let mut rb = Regs::load(sb);
        for (x, y) in a[..n].iter_mut().zip(b[..n].iter_mut()) {
            ra.step::<DEC>(x);
            rb.step::<DEC>(y);
        }
        for x in &mut a[n..] {
            ra.step::<DEC>(x);
        }
        for y in &mut b[n..] {
            rb.step::<DEC>(y);
        }
        ra.store(sa);
        rb.store(sb);
    }
}

#[cfg(target_family = "wasm")]
fn lms_run<const DEC: bool, const PIPE: bool>(st: &mut Lms, buf: &mut [i32]) {
    // SAFETY: SIMD128 is of the profile of the modules (an engine without it does not run them)
    unsafe { simd::run::<DEC>(st, buf) }
}

#[cfg(target_family = "wasm")]
pub(crate) fn lms_run2_in<const DEC: bool, const PIPE: bool>(sa: &mut Lms, a: &mut [i32], sb: &mut Lms, b: &mut [i32]) {
    // SAFETY: as in `lms_run`
    unsafe { simd::run2::<DEC>(sa, a, sb, b) }
}

#[cfg(target_arch = "aarch64")]
fn lms_run<const DEC: bool, const PIPE: bool>(st: &mut Lms, buf: &mut [i32]) {
    let mut r = neon::Regs::load(st);
    for v in buf.iter_mut() {
        r.step::<DEC>(v);
    }
    r.store(st);
}

/// Two channels in one loop (independent dependency chains).
pub fn lms_run2<const DEC: bool>(sa: &mut Lms, a: &mut [i32], sb: &mut Lms, b: &mut [i32]) {
    lms_run2_in::<DEC, false>(sa, a, sb, b)
}

/// [`lms_run2`] with a separate instance for the pipeline stages (`PIPE`, see
/// `residual::run_lms`).
#[cfg(target_arch = "aarch64")]
pub(crate) fn lms_run2_in<const DEC: bool, const PIPE: bool>(sa: &mut Lms, a: &mut [i32], sb: &mut Lms, b: &mut [i32]) {
    let n = a.len().min(b.len());
    let mut ra = neon::Regs::load(sa);
    let mut rb = neon::Regs::load(sb);
    for (x, y) in a[..n].iter_mut().zip(b[..n].iter_mut()) {
        ra.step::<DEC>(x);
        rb.step::<DEC>(y);
    }
    for x in &mut a[n..] {
        ra.step::<DEC>(x);
    }
    for y in &mut b[n..] {
        rb.step::<DEC>(y);
    }
    ra.store(sa);
    rb.store(sb);
}

#[cfg(not(any(target_arch = "aarch64", target_family = "wasm")))]
pub(crate) fn lms_run2_in<const DEC: bool, const PIPE: bool>(sa: &mut Lms, a: &mut [i32], sb: &mut Lms, b: &mut [i32]) {
    lms_run::<DEC, PIPE>(sa, a);
    lms_run::<DEC, PIPE>(sb, b);
}

#[cfg(not(any(target_arch = "aarch64", target_family = "wasm")))]
fn lms_run<const DEC: bool, const PIPE: bool>(st: &mut Lms, buf: &mut [i32]) {
    let sh = st.in_shift;
    for v in buf.iter_mut() {
        let mut acc = 0i32;
        for j in 0..TAPS {
            acc = acc.wrapping_add(st.w[j] as i32 * st.h[j] as i32);
        }
        let p = (acc >> W_SHIFT) << sh;
        let (x, e) = if DEC { ((*v).wrapping_add(p), *v) } else { (*v, (*v).wrapping_sub(p)) };
        *v = if DEC { x } else { e };
        let sg = ((e > 0) as i16 - (e < 0) as i16) * MU;
        for j in 0..TAPS {
            st.w[j] = st.w[j].wrapping_add(st.u[0][j]);
        }
        st.u[0] = st.u[1];
        for j in 0..TAPS {
            let s = (st.h[j] > 0) as i16 - (st.h[j] < 0) as i16;
            st.u[1][j] = sg.wrapping_mul(s);
        }
        st.h.copy_within(1.., 0);
        st.h[TAPS - 1] = (x >> sh).clamp(-32768, 32767) as i16;
    }
}

/// Scale state of a channel: the running mean of the error magnitude (4 fractional
/// bits) and the newest magnitude, which enters the mean one sample late (so that the next
/// context does not wait for it).
#[derive(Clone, Copy)]
pub struct Scale {
    avg: u32,
    pend: u32,
}

impl aqr_common::wire::Wire for Scale {
    fn put(&self, out: &mut Vec<u8>) {
        (self.avg, self.pend).put(out)
    }

    fn get(b: &mut &[u8]) -> Option<Self> {
        let (avg, pend) = Wire::get(b)?;
        Some(Scale { avg, pend })
    }
}

impl Default for Scale {
    fn default() -> Self {
        Scale { avg: 16 << 4, pend: 0 }
    }
}

impl Scale {
    #[inline(always)]
    fn next(&mut self, m: u32) {
        self.avg = adapt(self.avg, self.pend);
        self.pend = m;
    }
}

/// Context bucket and shift for the next error, from the running mean of the zigzag
/// values (4 fractional bits; always in 15..2^31, see [`adapt`]).
#[inline(always)]
fn ctx(avg: u32) -> (usize, u32) {
    debug_assert!((15..1 << 31).contains(&avg));
    let n = avg.leading_zeros();
    let l = 31 - n;
    let half = (avg << n) >> 30 & 1;
    ((2 * l + half) as usize, l.saturating_sub(6))
}

/// Decays the mean by 1/16 and adds `m` (capped at 2^26, so the mean stays below 2^30;
/// it never falls below 15 from its initial value).
#[inline(always)]
fn adapt(avg: u32, m: u32) -> u32 {
    avg - (avg >> 4) + m.min(1 << 26)
}

#[inline(always)]
fn token(v: u32) -> u32 {
    if v < DIRECT {
        v
    } else {
        let l = 32 - v.leading_zeros();
        DIRECT + (l - 5) * 2 + ((v >> (l - 2)) & 1)
    }
}

#[inline(always)]
fn zigzag(e: i32) -> u32 {
    ((e << 1) ^ (e >> 31)) as u32
}

/// Codes error `e` with scale state `sc` (context class `cc`): the token of its zigzag
/// value shifted right by the context's shift to `enc`, then the escape mantissa and the
/// low bits to `raw`.
#[inline(always)]
pub fn encode_one(sc: &mut Scale, cc: usize, e: i32, enc: &mut EncLocal, raw: &mut FastBits) {
    let (b, s) = ctx(sc.avg);
    let u = zigzag(e);
    sc.next(u);
    let v = u >> s;
    let t = token(v);
    enc.put(b * 2 + cc, t);
    let low = (u & ((1u32 << s) - 1)) as u64;
    if t >= DIRECT {
        let nb = (t - DIRECT) / 2 + 3;
        let mant = (v & ((1 << nb) - 1)) as u64;
        raw.put(mant << s | low, nb + s);
    } else {
        raw.put(low, s);
    }
}

/// log2 of the alias buckets per context ([`crate::audio::rans::Tables::flat_log`]).
pub const FLAT_LOG: u32 = N_TOK.next_power_of_two().trailing_zeros();
/// Most raw bits per error.
pub const MAX_RAW_BITS: usize = 56;

/// Decodes one error (inverse of [`encode_one`]). The tables must be flat with
/// [`FLAT_LOG`]. The bits read depend only on the token and the context. Without
/// `CHECKED`, the rANS decoder must have room for a word and `raw` for
/// [`MAX_RAW_BITS`] bits.
#[inline(always)]
pub fn decode_one<const CHECKED: bool>(sc: &mut Scale, cc: usize, dec: &mut DecLocal, raw: &mut BitReader) -> i32 {
    let (b, s) = ctx(sc.avg);
    let t = dec.get_flat::<FLAT_LOG, CHECKED>(b * 2 + cc);
    let esc = t >= DIRECT;
    let nb = if esc { (t - DIRECT) / 2 + 3 } else { 0 };
    // SAFETY: room checked by the caller unless CHECKED.
    let w = if CHECKED { raw.window() } else { unsafe { raw.window_unchecked() } };
    raw.skip(nb + s);
    let mant = (w >> 1 >> (63 - nb)) as u32;
    let v = if esc { (2 | (t & 1)) << nb | mant } else { t };
    let low = (w << nb >> 1 >> (63 - s)) as u32;
    let u = v << s | low;
    sc.next(u);
    (u >> 1) as i32 ^ -((u & 1) as i32)
}

/// Context and token of an error, for training.
pub fn count_one(sc: &mut Scale, cc: usize, e: i32) -> (usize, u32) {
    let (b, s) = ctx(sc.avg);
    let u = zigzag(e);
    sc.next(u);
    (b * 2 + cc, token(u >> s))
}

impl Scale {
    /// [`Scale::next`] with the decay and the add side by side (same result: no overflow
    /// below 2^31), for [`advance`].
    #[inline(always)]
    fn next_short(&mut self, m: u32) {
        self.avg = (self.avg + self.pend.min(1 << 26)) - (self.avg >> 4);
        self.pend = m;
    }
}

/// Advances the scale states over the errors of one or two channels as [`encode_one`]
/// does, without coding (for an LMS stage that runs ahead of the coder); two channels in
/// one loop (independent chains).
pub fn advance(a: &mut Scale, ea: &[i32], b: Option<(&mut Scale, &[i32])>) {
    let (mut sa, ea) = (*a, ea);
    let rest = if let Some((b, eb)) = b {
        let n = ea.len().min(eb.len());
        let mut sb = *b;
        for (&x, &y) in ea[..n].iter().zip(&eb[..n]) {
            sa.next_short(zigzag(x));
            sb.next_short(zigzag(y));
        }
        for &y in &eb[n..] {
            sb.next_short(zigzag(y));
        }
        *b = sb;
        &ea[n..]
    } else {
        ea
    };
    for &x in rest {
        sa.next_short(zigzag(x));
    }
    *a = sa;
}
