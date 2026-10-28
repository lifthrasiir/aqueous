//! The pixel model's pieces, shared by its coders (aqr-png's literals, [`super::raster`]).
//!
//! Samples are read from a *raw buffer*: rows of `rs2` bytes, each `bpp` zero bytes then the
//! row's samples, after a zero row and a margin of at least one byte, so that the neighbours
//! of any sample (and those of the sample before, for the channel decorrelation) are in
//! bounds and zero outside the image.

/// Activity buckets: 0, 1, 2–3, 4–7, ..., 1024 and more.
pub const ACT: usize = 12;

/// The activity bucket of `v`.
#[inline(always)]
pub fn bucket(v: u32) -> usize {
    ((32 - v.leading_zeros()) as usize).min(ACT - 1)
}

/// The magnitude of `x` as a signed residual.
#[inline(always)]
pub fn mag(x: u8) -> u32 {
    (x as i8).unsigned_abs() as u32
}

/// A residual as a symbol: 0, −1, 1, −2, ... to 0, 1, 2, 3, ...
#[inline(always)]
pub fn zigzag(e: u8) -> u32 {
    let e = e as i8;
    ((e << 1) ^ (e >> 7)) as u8 as u32
}

#[inline(always)]
pub fn unzigzag(z: u32) -> u8 {
    ((z >> 1) as u8) ^ ((z & 1) as u8).wrapping_neg()
}

/// LOCO-I's median edge detector: the median of `a`, `b` and `a + b - c`, branch-free.
#[inline(always)]
pub fn med(a: i32, b: i32, c: i32) -> i32 {
    (a + b - c).max(a.min(b)).min(a.max(b))
}

/// Whether channel `ch` of `BPP`-byte pixels adds the previous channel's residual to its
/// prediction: the second and third of three or more.
#[inline(always)]
pub const fn decorrelated<const BPP: usize>(ch: usize) -> bool {
    BPP >= 3 && (ch == 1 || ch == 2)
}

/// `s[i]` without a bounds check, for the offsets a raw buffer keeps in bounds (see the
/// module): checked in debug builds.
#[inline(always)]
pub fn get(s: &[u8], i: usize) -> u8 {
    debug_assert!(i < s.len());
    // SAFETY: see above.
    unsafe { *s.get_unchecked(i) }
}

#[inline(always)]
pub fn put(s: &mut [u8], i: usize, v: u8) {
    debug_assert!(i < s.len());
    // SAFETY: as in `get`.
    unsafe { *s.get_unchecked_mut(i) = v }
}

/// The model of a sample: its prediction, activity `|a - c| + |b - c|` and the neighbours
/// left (`a`), above (`b`) and above left (`c`).
#[derive(Clone, Copy)]
pub struct Pred {
    pub pred: u8,
    pub act: u32,
    pub a: u8,
    pub b: u8,
    pub c: u8,
}

/// The model of the sample at `q` of a raw buffer of rows of `rs2` bytes, of channel `ch` of
/// `BPP`-byte pixels.
#[inline(always)]
pub fn predict<const BPP: usize>(raw: &[u8], q: usize, rs2: usize, ch: usize) -> Pred {
    let up = q - rs2;
    let (a, b, c) = (get(raw, q - BPP) as i32, get(raw, up) as i32, get(raw, up - BPP) as i32);
    let mut pred = med(a, b, c);
    if decorrelated::<BPP>(ch) {
        let (a1, b1, c1) = (get(raw, q - 1 - BPP) as i32, get(raw, up - 1) as i32, get(raw, up - 1 - BPP) as i32);
        pred = (pred + get(raw, q - 1) as i32 - med(a1, b1, c1)).clamp(0, 255);
    }
    let act = ((a - c).abs() + (b - c).abs()) as u32;
    Pred { pred: pred as u8, act, a: a as u8, b: b as u8, c: c as u8 }
}

/// The model on 16 samples at a time (NEON is always there on AArch64).
#[cfg(target_arch = "aarch64")]
pub mod neon {
    use super::ACT;
    use std::arch::aarch64::*;

    /// Bit lengths of `v` capped at `ACT - 1` (the [`super::bucket`] of 16-bit lanes).
    #[inline(always)]
    pub fn bucket16(v: uint16x8_t) -> uint16x8_t {
        unsafe { vminq_u16(vsubq_u16(vdupq_n_u16(16), vclzq_u16(v)), vdupq_n_u16(ACT as u16 - 1)) }
    }

    /// MED of 8 lanes (widened).
    #[inline(always)]
    pub fn med16(a: int16x8_t, b: int16x8_t, c: int16x8_t) -> int16x8_t {
        unsafe { vminq_s16(vmaxq_s16(vsubq_s16(vaddq_s16(a, b), c), vminq_s16(a, b)), vmaxq_s16(a, b)) }
    }

    #[inline(always)]
    pub fn lo(v: uint8x16_t) -> int16x8_t {
        unsafe { vreinterpretq_s16_u16(vmovl_u8(vget_low_u8(v))) }
    }

    #[inline(always)]
    pub fn hi(v: uint8x16_t) -> int16x8_t {
        unsafe { vreinterpretq_s16_u16(vmovl_high_u8(v)) }
    }

    /// The prediction of 16 samples from their neighbours (MED, with the previous channel's
    /// residual added where `dec` is set: `v1` the previous samples and `a1`, `b1`, `c1`
    /// their neighbours).
    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    pub fn pred(
        a: uint8x16_t,
        b: uint8x16_t,
        c: uint8x16_t,
        v1: uint8x16_t,
        a1: uint8x16_t,
        b1: uint8x16_t,
        c1: uint8x16_t,
        dec: uint8x16_t,
    ) -> uint8x16_t {
        unsafe {
            let m_lo = med16(lo(a), lo(b), lo(c));
            let m_hi = med16(hi(a), hi(b), hi(c));
            let n_lo = vsubq_s16(vaddq_s16(m_lo, lo(v1)), med16(lo(a1), lo(b1), lo(c1)));
            let n_hi = vsubq_s16(vaddq_s16(m_hi, hi(v1)), med16(hi(a1), hi(b1), hi(c1)));
            let m = vcombine_u8(vqmovun_s16(m_lo), vqmovun_s16(m_hi));
            let n = vcombine_u8(vqmovun_s16(n_lo), vqmovun_s16(n_hi));
            vbslq_u8(dec, n, m)
        }
    }

    /// The prediction of 16 samples by MED alone.
    #[inline(always)]
    pub fn pred_med(a: uint8x16_t, b: uint8x16_t, c: uint8x16_t) -> uint8x16_t {
        unsafe {
            let m_lo = med16(lo(a), lo(b), lo(c));
            let m_hi = med16(hi(a), hi(b), hi(c));
            vcombine_u8(vqmovun_s16(m_lo), vqmovun_s16(m_hi))
        }
    }

    /// The zigzag symbols of 16 residuals.
    #[inline(always)]
    pub fn zigzag16(e: uint8x16_t) -> uint8x16_t {
        unsafe { veorq_u8(vshlq_n_u8(e, 1), vreinterpretq_u8_s8(vshrq_n_s8(vreinterpretq_s8_u8(e), 7))) }
    }

    /// The activities `|a - c| + |b - c|` of 16 samples, as low and high halves.
    #[inline(always)]
    pub fn act16(a: uint8x16_t, b: uint8x16_t, c: uint8x16_t) -> (uint16x8_t, uint16x8_t) {
        unsafe {
            let ac = vabdq_u8(a, c);
            let bc = vabdq_u8(b, c);
            (vaddl_u8(vget_low_u8(ac), vget_low_u8(bc)), vaddl_high_u8(ac, bc))
        }
    }

    /// The magnitudes of two sets of 16 residuals added, as low and high halves.
    #[inline(always)]
    pub fn mag_sum16(x: uint8x16_t, y: uint8x16_t) -> (uint16x8_t, uint16x8_t) {
        unsafe {
            let x = vreinterpretq_u8_s8(vabsq_s8(vreinterpretq_s8_u8(x)));
            let y = vreinterpretq_u8_s8(vabsq_s8(vreinterpretq_s8_u8(y)));
            (vaddl_u8(vget_low_u8(x), vget_low_u8(y)), vaddl_high_u8(x, y))
        }
    }
}
