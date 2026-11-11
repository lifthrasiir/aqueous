//! Byte slices compared: natively the slices' `==` (the C library's `memcmp`, in SIMD); in
//! WebAssembly 16 bytes at a time in SIMD128, where `==` would be compiler-builtins' `memcmp`, a
//! byte at a time (the modules are not built for SIMD).

/// Whether `a` and `b` are the same bytes.
#[cfg(not(target_family = "wasm"))]
#[inline]
pub fn same(a: &[u8], b: &[u8]) -> bool {
    a == b
}

/// Whether `a` and `b` are the same bytes.
#[cfg(target_family = "wasm")]
#[inline(never)]
#[target_feature(enable = "simd128")]
pub fn same(a: &[u8], b: &[u8]) -> bool {
    use core::arch::wasm32::*;
    if a.len() != b.len() {
        return false;
    }
    let (ac, at) = a.as_chunks::<64>();
    let (bc, bt) = b.as_chunks::<64>();
    for (p, q) in ac.iter().zip(bc) {
        // SAFETY: the loads are within the chunks of 64 bytes.
        let d = unsafe {
            let (p, q) = (p.as_ptr() as *const v128, q.as_ptr() as *const v128);
            let x = |k: usize| v128_xor(v128_load(p.add(k)), v128_load(q.add(k)));
            v128_or(v128_or(x(0), x(1)), v128_or(x(2), x(3)))
        };
        if v128_any_true(d) {
            return false;
        }
    }
    let (ac, at) = at.as_chunks::<16>();
    let (bc, bt) = bt.as_chunks::<16>();
    for (p, q) in ac.iter().zip(bc) {
        // SAFETY: the loads are of the chunks of 16 bytes.
        let d = unsafe { v128_xor(v128_load(p.as_ptr() as *const v128), v128_load(q.as_ptr() as *const v128)) };
        if v128_any_true(d) {
            return false;
        }
    }
    at.iter().zip(bt).all(|(x, y)| x == y)
}

#[cfg(test)]
mod tests {
    #[test]
    fn same_bytes() {
        let a: Vec<u8> = (0..301u32).map(|i| (i * 7) as u8).collect();
        for n in [0, 1, 15, 16, 17, 63, 64, 65, 130, 300] {
            assert!(super::same(&a[..n], &a[..n]));
            for k in 0..n {
                let mut b = a[..n].to_vec();
                b[k] ^= 0x40;
                assert!(!super::same(&a[..n], &b), "{n} at {k}");
            }
            assert!(!super::same(&a[..n], &a[..n + 1]));
        }
    }
}
