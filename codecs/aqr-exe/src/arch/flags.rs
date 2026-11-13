//! What the streams of the two remaps (`x86::remap`, `arm64::remap`) share: flag streams, their
//! maps and the streams' names.

/// Names of the streams of a remap: those of `base`, then `extra`.
pub(crate) const fn names<const N: usize, const M: usize>(
    base: [&'static str; N],
    extra: [&'static str; 4],
) -> [&'static str; M] {
    assert!(M == N + 4);
    let mut n = [""; M];
    let mut i = 0;
    while i < N {
        n[i] = base[i];
        i += 1;
    }
    while i < M {
        n[i] = extra[i - N];
        i += 1;
    }
    n
}

/// The values of a map of a remap's `decode` (a stream of varints).
pub(crate) fn map_values(map: &[u8]) -> u64 {
    map.iter().filter(|&&b| b < 0x80).count() as u64
}

/// The flags set in flag streams `flags`.
pub(crate) fn ones(flags: &[&[u8]]) -> u64 {
    flags.iter().flat_map(|f| f.iter()).map(|b| b.count_ones() as u64).sum()
}

/// Bits, the most significant of each byte first.
#[cfg(feature = "encode")]
#[derive(Default)]
pub(crate) struct Bits {
    pub(crate) out: alloc::vec::Vec<u8>,
    pub(crate) n: usize,
}

#[cfg(feature = "encode")]
impl Bits {
    pub(crate) fn push(&mut self, b: bool) {
        if self.n.is_multiple_of(8) {
            self.out.push(0);
        }
        *self.out.last_mut().unwrap() |= (b as u8) << (7 - self.n % 8);
        self.n += 1;
    }
}

/// Flags, the most significant bit of each byte first.
#[cfg(feature = "decode")]
pub(crate) struct Flags<'a> {
    pub(crate) s: &'a [u8],
    pub(crate) n: usize,
}

#[cfg(feature = "decode")]
impl Flags<'_> {
    pub(crate) fn next(&mut self) -> Option<bool> {
        let b = *self.s.get(self.n / 8)? >> (7 - self.n % 8) & 1;
        self.n += 1;
        Some(b != 0)
    }

    /// Whether all were read, the bits left in the last byte 0.
    pub(crate) fn done(&self) -> bool {
        let pad = if self.n.is_multiple_of(8) { 0 } else { 0xff >> (self.n % 8) };
        self.s.len() == self.n.div_ceil(8) && self.s.last().is_none_or(|&b| b & pad == 0)
    }
}
