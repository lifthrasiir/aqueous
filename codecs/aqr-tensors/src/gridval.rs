//! The elements of a span of a grid's run (FORMAT.md §4.5): of each index its grid value, times
//! the tile's scale (less its zero point first), rounded to the run's type. One at a time, or
//! natively by AVX2 where the processor has it and the span is long enough: 8 at a time, the
//! grid's values by two permutes of 8 where they are 16 at most, else by a gather, F16 by F16C.
//! Every way gives the same bits, a NaN the quiet NaN of the type.

use crate::layout::{self, Float};

/// The elements of indices `qs` (each below `g.len()`) of grid `g`, scale `s` and zero point `z`
/// (where `zero`), of type `f`, into `dst` (`f`'s bytes for each).
pub fn values(f: Float, g: &[f32], qs: &[u8], s: f32, z: f32, zero: bool, dst: &mut [u8]) {
    #[cfg(target_arch = "x86_64")]
    if avx2() && (qs.len() >= 32 || f == Float::F16) {
        // SAFETY: the processor has AVX2 and F16C ([`avx2`]); the indices are below the grid's
        // length, and `dst` is of their elements' bytes.
        return unsafe { x86::values(f, g, qs, s, z, zero, dst) };
    }
    match (f, zero) {
        (Float::Bf16 | Float::F16, false) => scalar::<2, false>(f, g, qs, s, z, dst),
        (Float::Bf16 | Float::F16, true) => scalar::<2, true>(f, g, qs, s, z, dst),
        (_, false) => scalar::<4, false>(f, g, qs, s, z, dst),
        (_, true) => scalar::<4, true>(f, g, qs, s, z, dst),
    }
}

/// [`values`] one at a time, of `FB` bytes each.
#[inline(always)]
fn scalar<const FB: usize, const ZERO: bool>(f: Float, g: &[f32], qs: &[u8], s: f32, z: f32, dst: &mut [u8]) {
    for (&q, d) in qs.iter().zip(dst.as_chunks_mut::<FB>().0) {
        let v = layout::grid_value(f, g[q as usize], s, z, ZERO);
        d.copy_from_slice(&v.to_le_bytes()[..FB]);
    }
}

/// Whether the processor has what the AVX2 kernel takes (found once). It is taken for spans of
/// 32 elements or more, or of F16 (whose rounding one at a time is slow): on shorter ones (NVFP4's
/// tiles of 16) one at a time is 8% faster.
#[cfg(target_arch = "x86_64")]
fn avx2() -> bool {
    static HAS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *HAS.get_or_init(|| std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("f16c"))
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use std::arch::x86_64::*;

    use crate::layout::Float;

    /// [`super::values`] by AVX2 and F16C.
    ///
    /// # Safety
    ///
    /// The processor has AVX2 and F16C; every index is below `g.len()`; `dst` is of `qs.len()`
    /// elements of `f`.
    #[target_feature(enable = "avx2,f16c")]
    pub unsafe fn values(f: Float, g: &[f32], qs: &[u8], s: f32, z: f32, zero: bool, dst: &mut [u8]) {
        let n = qs.len();
        let fb = f.bytes();
        let mut t = [0f32; 16];
        let small = g.len() <= 16;
        if small {
            t[..g.len()].copy_from_slice(g);
        }
        // SAFETY (of the loads and stores below): `i + 8 <= n`, and every index below `g.len()`
        unsafe {
            let (lo, hi) = (_mm256_loadu_ps(t.as_ptr()), _mm256_loadu_ps(t.as_ptr().add(8)));
            let (vs, vz) = (_mm256_set1_ps(s), _mm256_set1_ps(z));
            let seven = _mm256_set1_epi32(7);
            let mut i = 0;
            while i + 8 <= n {
                let q = _mm256_cvtepu8_epi32(_mm_loadl_epi64(qs.as_ptr().add(i) as *const __m128i));
                let gv = if small {
                    let (a, b) = (_mm256_permutevar8x32_ps(lo, q), _mm256_permutevar8x32_ps(hi, q));
                    _mm256_blendv_ps(a, b, _mm256_castsi256_ps(_mm256_cmpgt_epi32(q, seven)))
                } else {
                    _mm256_i32gather_ps::<4>(g.as_ptr(), q)
                };
                let x = if zero { _mm256_mul_ps(_mm256_sub_ps(gv, vz), vs) } else { _mm256_mul_ps(gv, vs) };
                let nan = _mm256_cmp_ps::<_CMP_UNORD_Q>(x, x);
                let out = dst.as_mut_ptr().add(i * fb);
                match f {
                    Float::Bf16 => {
                        let u = _mm256_castps_si256(x);
                        let odd = _mm256_and_si256(_mm256_srli_epi32::<16>(u), _mm256_set1_epi32(1));
                        let r = _mm256_srli_epi32::<16>(_mm256_add_epi32(
                            _mm256_add_epi32(u, _mm256_set1_epi32(0x7fff)),
                            odd,
                        ));
                        let r = _mm256_blendv_epi8(r, _mm256_set1_epi32(0x7fc0), _mm256_castps_si256(nan));
                        let p = _mm256_permute4x64_epi64::<0b1000>(_mm256_packus_epi32(r, r));
                        _mm_storeu_si128(out as *mut __m128i, _mm256_castsi256_si128(p));
                    }
                    Float::F16 => {
                        let h = _mm256_cvtps_ph::<_MM_FROUND_TO_NEAREST_INT>(x);
                        let m = _mm256_castps_si256(nan);
                        let m = _mm256_castsi256_si128(_mm256_permute4x64_epi64::<0b1000>(_mm256_packs_epi32(m, m)));
                        _mm_storeu_si128(out as *mut __m128i, _mm_blendv_epi8(h, _mm_set1_epi16(0x7e00), m));
                    }
                    _ => {
                        let r = _mm256_blendv_ps(x, _mm256_castsi256_ps(_mm256_set1_epi32(0x7fc0_0000)), nan);
                        _mm256_storeu_ps(out as *mut f32, r);
                    }
                }
                i += 8;
            }
            // (the rest one at a time)
            let (q, d) = (&qs[i..], &mut dst[i * fb..]);
            for (k, &q) in q.iter().enumerate() {
                let v = crate::layout::grid_value(f, g[q as usize], s, z, zero);
                d[k * fb..(k + 1) * fb].copy_from_slice(&v.to_le_bytes()[..fb]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every way gives the bits of [`layout::grid_value`]: of grids of 16 and of more, scales and
    /// zero points of every magnitude (subnormal results, overflows, NaNs of 0 times infinity).
    #[test]
    fn same() {
        let mut r = aqr_common::rng::Rng::new(5);
        for k in [16usize, 9, 254] {
            let mut g: Vec<f32> = (0..k).map(|_| f32::from_bits(r.below(1 << 32) as u32 & 0xbfff_ffff)).collect();
            g[0] = 0.0;
            g[1] = -0.0;
            for f in [Float::Bf16, Float::F16, Float::F32] {
                for zero in [false, true] {
                    for s in [1.0f32, 3.7e-5, 1e30, 1e-40, f32::INFINITY, 0.123] {
                        let qs: Vec<u8> = (0..1001).map(|_| r.below(k as u64) as u8).collect();
                        let mut a = vec![0u8; qs.len() * f.bytes()];
                        let mut b = a.clone();
                        values(f, &g, &qs, s, 0.5, zero, &mut a);
                        for (i, &q) in qs.iter().enumerate() {
                            let v = layout::grid_value(f, g[q as usize], s, 0.5, zero);
                            b[i * f.bytes()..(i + 1) * f.bytes()].copy_from_slice(&v.to_le_bytes()[..f.bytes()]);
                        }
                        assert!(a == b, "{k} {f:?} {zero} {s}");
                    }
                }
            }
        }
    }
}
