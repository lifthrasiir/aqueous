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
    plain_from(1, d)
}

/// The Adler-32 of what gave `sum` then `d`, a byte at a time.
#[cfg(any(target_family = "wasm", test))]
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
            self.0 = plain_from(self.0, d);
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
