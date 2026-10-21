//! Deterministic pseudo-random numbers for tests and benchmarks: xorshift64*, the same
//! everywhere.

/// The seed of [`noise`].
const NOISE_SEED: u64 = 0x9e37_79b9_7f4a_7c15;

/// A xorshift64* generator.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    /// A generator from `seed` (0 counts as another, as xorshift stays at 0).
    pub fn new(seed: u64) -> Rng {
        Rng(if seed == 0 { NOISE_SEED } else { seed })
    }

    pub fn u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// A number below `n` (> 0), nearly uniform.
    pub fn below(&mut self, n: u64) -> u64 {
        ((self.u64() as u128 * n as u128) >> 64) as u64
    }

    /// `n` bytes.
    pub fn bytes(&mut self, n: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(n + 8);
        while out.len() < n {
            out.extend_from_slice(&self.u64().to_le_bytes());
        }
        out.truncate(n);
        out
    }
}

/// `n` bytes that no compressor shrinks.
pub fn noise(n: usize) -> Vec<u8> {
    Rng::new(NOISE_SEED).bytes(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic() {
        assert_eq!(noise(20), noise(100)[..20]);
        assert_ne!(Rng::new(1).bytes(16), Rng::new(2).bytes(16));
        assert_eq!(Rng::new(0).u64(), Rng::new(NOISE_SEED).u64());
        let mut r = Rng::new(7);
        assert!((0..1000).all(|_| r.below(10) < 10));
        // every byte value, about evenly
        let mut counts = [0u32; 256];
        noise(1 << 16).iter().for_each(|&b| counts[b as usize] += 1);
        assert!(counts.iter().all(|&c| (192..320).contains(&c)), "{counts:?}");
    }
}
