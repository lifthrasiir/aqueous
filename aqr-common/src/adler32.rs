//! Adler-32 (of zlib, RFC 1950): natively simd-adler32's; in WebAssembly, plainly.

/// The Adler-32 of `d`.
#[cfg(not(target_family = "wasm"))]
pub fn hash(d: &[u8]) -> u32 {
    let mut a = simd_adler32::Adler32::new();
    a.write(d);
    a.finish()
}

#[cfg(target_family = "wasm")]
pub use plain as hash;

/// The Adler-32 of `d`, a byte at a time.
#[cfg(any(target_family = "wasm", test))]
pub fn plain(d: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
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
    }
}
