//! Adler-32 (of zlib, RFC 1950): natively simd-adler32's; in WebAssembly, 32 bytes at a time in
//! SIMD128 (simd-adler32 picks its SIMD at compile time, and the modules are not built for it).

/// The Adler-32 of `d`.
#[cfg(not(target_family = "wasm"))]
pub fn hash(d: &[u8]) -> u32 {
    let mut a = simd_adler32::Adler32::new();
    a.write(d);
    a.finish()
}

/// The Adler-32 of `d`.
#[cfg(target_family = "wasm")]
pub fn hash(d: &[u8]) -> u32 {
    simd_from(1, d)
}

/// [`plain_from`] 32 bytes at a time: for each block, `a` grows by the sum of its bytes and `b`
/// by 32 times `a` before it and the sum of its bytes weighted 32 down to 1; the sums of `a`
/// before each block are kept by lane, as are the weighted sums, and added up once a chunk.
#[cfg(target_family = "wasm")]
#[target_feature(enable = "simd128")]
fn simd_from(sum: u32, d: &[u8]) -> u32 {
    use core::arch::wasm32::*;
    let (mut a, mut b) = (sum & 0xffff, sum >> 16);
    let w = [
        i16x8(32, 31, 30, 29, 28, 27, 26, 25),
        i16x8(24, 23, 22, 21, 20, 19, 18, 17),
        i16x8(16, 15, 14, 13, 12, 11, 10, 9),
        i16x8(8, 7, 6, 5, 4, 3, 2, 1),
    ];
    let hsum = |v: v128| {
        (i32x4_extract_lane::<0>(v) as u32)
            .wrapping_add(i32x4_extract_lane::<1>(v) as u32)
            .wrapping_add(i32x4_extract_lane::<2>(v) as u32)
            .wrapping_add(i32x4_extract_lane::<3>(v) as u32)
    };
    // (5536 bytes, 173 blocks, at most before `b` can overflow, as zlib's 5552)
    for c in d.chunks(5536) {
        let (blocks, tail) = c.as_chunks::<32>();
        let (mut s1, mut ps, mut s2) = (i32x4_splat(0), i32x4_splat(0), i32x4_splat(0));
        for k in blocks {
            // SAFETY: the loads are of the block's 32 bytes.
            let (x, y) =
                unsafe { (v128_load(k.as_ptr() as *const v128), v128_load(k.as_ptr().add(16) as *const v128)) };
            ps = i32x4_add(ps, s1);
            let sum = |v: v128| i32x4_extadd_pairwise_u16x8(i16x8_extadd_pairwise_u8x16(v));
            s1 = i32x4_add(s1, i32x4_add(sum(x), sum(y)));
            let dot = |v: v128, lo: v128, hi: v128| {
                i32x4_add(
                    i32x4_dot_i16x8(u16x8_extend_low_u8x16(v), lo),
                    i32x4_dot_i16x8(u16x8_extend_high_u8x16(v), hi),
                )
            };
            s2 = i32x4_add(s2, i32x4_add(dot(x, w[0], w[1]), dot(y, w[2], w[3])));
        }
        b = b.wrapping_add(a.wrapping_mul(32 * blocks.len() as u32)).wrapping_add(hsum(ps) << 5).wrapping_add(hsum(s2));
        a = a.wrapping_add(hsum(s1));
        for &x in tail {
            a += x as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    b << 16 | a
}

/// The Adler-32 of `d`, a byte at a time.
#[cfg(test)]
pub fn plain(d: &[u8]) -> u32 {
    plain_from(1, d)
}

/// The Adler-32 of what gave `sum` then `d`, a byte at a time.
#[cfg(test)]
fn plain_from(sum: u32, d: &[u8]) -> u32 {
    let (mut a, mut b) = (sum & 0xffff, sum >> 16);
    // (the most bytes before `b` can overflow)
    for c in d.chunks(5552) {
        for &x in c {
            a += x as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    b << 16 | a
}

/// The Adler-32 of bytes given a piece at a time.
#[derive(Clone)]
pub struct Hasher(#[cfg(not(target_family = "wasm"))] simd_adler32::Adler32, #[cfg(target_family = "wasm")] u32);

impl Default for Hasher {
    fn default() -> Hasher {
        Hasher::new()
    }
}

impl Hasher {
    pub fn new() -> Hasher {
        #[cfg(not(target_family = "wasm"))]
        return Hasher(simd_adler32::Adler32::new());
        #[cfg(target_family = "wasm")]
        Hasher(1)
    }

    pub fn update(&mut self, d: &[u8]) {
        #[cfg(not(target_family = "wasm"))]
        self.0.write(d);
        #[cfg(target_family = "wasm")]
        {
            self.0 = simd_from(self.0, d);
        }
    }

    pub fn finalize(&self) -> u32 {
        #[cfg(not(target_family = "wasm"))]
        return self.0.finish();
        #[cfg(target_family = "wasm")]
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known() {
        assert_eq!(hash(b""), 1);
        assert_eq!(hash(b"Wikipedia"), 0x11e6_0398);
        let long: Vec<u8> = (0..100_000u32).map(|i| (i * 7 + i / 300) as u8).collect();
        for n in [0, 1, 5551, 5552, 5553, 100_000] {
            assert_eq!(hash(&long[..n]), plain(&long[..n]), "{n}");
        }
        for (a, b) in [(0, 0), (1, 7), (5551, 5553), (3, 77_777)] {
            let mut h = Hasher::new();
            h.update(&long[..a]);
            h.update(&long[a..b]);
            h.update(&long[b..]);
            assert_eq!(h.finalize(), hash(&long), "{a} {b}");
            assert_eq!(plain_from(plain(&long[..a]), &long[a..]), hash(&long));
        }
    }
}
