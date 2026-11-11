//! Searches of whole pixels (`u64`) of the tile model, two to a vector where the target has SIMD
//! (SIMD128, NEON, SSE2), eight a round: the place of a pixel in the list, and the run of a
//! pixel's equals. Loops of early exits, which the compilers leave scalar.

/// Pixels a round.
const ROUND: usize = 8;

/// The first place of `v` in `s`.
#[cfg_attr(target_family = "wasm", inline(never))]
#[cfg_attr(target_family = "wasm", target_feature(enable = "simd128"))]
#[cfg_attr(not(target_family = "wasm"), inline)]
pub fn find(s: &[u64], v: u64) -> Option<usize> {
    let (chunks, _) = s.as_chunks::<ROUND>();
    for (k, c) in chunks.iter().enumerate() {
        if any_eq(c, v) {
            return c.iter().position(|&p| p == v).map(|j| k * ROUND + j);
        }
    }
    let done = chunks.len() * ROUND;
    s[done..].iter().position(|&p| p == v).map(|j| done + j)
}

/// How many pixels from the start of `s` are `v`.
#[cfg_attr(target_family = "wasm", inline(never))]
#[cfg_attr(target_family = "wasm", target_feature(enable = "simd128"))]
#[cfg_attr(not(target_family = "wasm"), inline)]
pub fn run(s: &[u64], v: u64) -> usize {
    let (chunks, _) = s.as_chunks::<ROUND>();
    for (k, c) in chunks.iter().enumerate() {
        if !all_eq(c, v) {
            return k * ROUND + c.iter().take_while(|&&p| p == v).count();
        }
    }
    let done = chunks.len() * ROUND;
    done + s[done..].iter().take_while(|&&p| p == v).count()
}

#[cfg(not(any(target_family = "wasm", target_arch = "aarch64", target_arch = "x86_64")))]
#[inline(always)]
fn any_eq(c: &[u64; ROUND], v: u64) -> bool {
    c.iter().any(|&p| p == v)
}

#[cfg(not(any(target_family = "wasm", target_arch = "aarch64", target_arch = "x86_64")))]
#[inline(always)]
fn all_eq(c: &[u64; ROUND], v: u64) -> bool {
    c.iter().all(|&p| p == v)
}

/// The lanes of `c` against `v`, or-ed (`ANY`) or and-ed: all ones where they hold.
#[cfg(target_family = "wasm")]
#[inline(always)]
fn fold_eq<const ANY: bool>(c: &[u64; ROUND], v: u64) -> bool {
    use core::arch::wasm32::*;
    // SAFETY: SIMD128 is of the profile of the modules (an engine without it does not run them;
    // `find` and `run` are compiled with it, see below); the loads are of the chunk.
    unsafe {
        let x = i64x2_splat(v as i64);
        let p = c.as_ptr() as *const v128;
        let e = |k: usize| i64x2_eq(v128_load(p.add(k)), x);
        if ANY {
            v128_any_true(v128_or(v128_or(e(0), e(1)), v128_or(e(2), e(3))))
        } else {
            i64x2_all_true(v128_and(v128_and(e(0), e(1)), v128_and(e(2), e(3))))
        }
    }
}

#[cfg(target_arch = "aarch64")]
#[inline(always)]
fn fold_eq<const ANY: bool>(c: &[u64; ROUND], v: u64) -> bool {
    use std::arch::aarch64::*;
    // SAFETY: NEON is always there on AArch64; the loads are of the chunk.
    unsafe {
        let x = vdupq_n_u64(v);
        let e = |k: usize| vceqq_u64(vld1q_u64(c.as_ptr().add(2 * k)), x);
        let m = if ANY {
            vorrq_u64(vorrq_u64(e(0), e(1)), vorrq_u64(e(2), e(3)))
        } else {
            vandq_u64(vandq_u64(e(0), e(1)), vandq_u64(e(2), e(3)))
        };
        let (a, b) = (vgetq_lane_u64::<0>(m), vgetq_lane_u64::<1>(m));
        if ANY { (a | b) != 0 } else { (a & b) == u64::MAX }
    }
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn fold_eq<const ANY: bool>(c: &[u64; ROUND], v: u64) -> bool {
    use std::arch::x86_64::*;
    // SAFETY: SSE2 is always there on x86-64; the loads are of the chunk.
    unsafe {
        let x = _mm_set1_epi64x(v as i64);
        // (64-bit lanes equal where both their 32-bit halves are: SSE2 has no pcmpeqq)
        let e = |k: usize| {
            let h = _mm_cmpeq_epi32(_mm_loadu_si128((c.as_ptr() as *const __m128i).add(k)), x);
            _mm_and_si128(h, _mm_shuffle_epi32::<0b10_11_00_01>(h))
        };
        if ANY {
            _mm_movemask_epi8(_mm_or_si128(_mm_or_si128(e(0), e(1)), _mm_or_si128(e(2), e(3)))) != 0
        } else {
            _mm_movemask_epi8(_mm_and_si128(_mm_and_si128(e(0), e(1)), _mm_and_si128(e(2), e(3)))) == 0xffff
        }
    }
}

#[cfg(any(target_family = "wasm", target_arch = "aarch64", target_arch = "x86_64"))]
#[inline(always)]
fn any_eq(c: &[u64; ROUND], v: u64) -> bool {
    fold_eq::<true>(c, v)
}

#[cfg(any(target_family = "wasm", target_arch = "aarch64", target_arch = "x86_64"))]
#[inline(always)]
fn all_eq(c: &[u64; ROUND], v: u64) -> bool {
    fold_eq::<false>(c, v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_scalar() {
        let mut rng = 5u64;
        for n in [0usize, 1, 7, 8, 9, 16, 31, 64, 256] {
            for t in 0..200 {
                let s: Vec<u64> = (0..n)
                    .map(|_| {
                        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                        // (few values, and ones equal in a 32-bit half alone)
                        [7, 7 << 32, (7 << 32) | 7, 9, u64::MAX][(rng >> 40) as usize % (2 + t % 4)]
                    })
                    .collect();
                for v in [7, 7 << 32, (7 << 32) | 7, 9, u64::MAX, 0] {
                    assert_eq!(find(&s, v), s.iter().position(|&p| p == v), "{s:x?} {v:x}");
                    assert_eq!(run(&s, v), s.iter().take_while(|&&p| p == v).count(), "{s:x?} {v:x}");
                }
            }
        }
    }
}
