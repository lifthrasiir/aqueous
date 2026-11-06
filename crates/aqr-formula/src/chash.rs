//! Content hashes that compose over concatenation: CRC-64 (of NVMe's polynomial) and the length,
//! so that the hash of `a ‖ b` is told from the hashes of `a` and `b` alone (the CRC of `a`
//! carried over the length of `b`). However bytes are cut and put together (slices,
//! concatenations, a stream cut into blocks), the hash of the whole is the same, which lets a value
//! be compared with another made otherwise of the same bytes without the bytes. A CRC tells apart
//! bytes that differ by chance (other than by design: it is not a hash against an adversary) but
//! with odds of 2^-64.

use crc_fast::CrcAlgorithm::Crc64Nvme;

/// A content hash ([`Ch::of`]): the CRC, and the length of the bytes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct Ch {
    crc: u64,
    len: u64,
}

impl Ch {
    /// The hash of no bytes.
    pub const EMPTY: Ch = Ch { crc: 0, len: 0 };

    /// The hash of `d`.
    pub fn of(d: &[u8]) -> Ch {
        Ch { crc: crc_fast::checksum(Crc64Nvme, d), len: d.len() as u64 }
    }

    /// The hash of the bytes hashed and `d` after them.
    pub fn then(self, d: &[u8]) -> Ch {
        self.cat(Ch::of(d))
    }

    /// The hash of the bytes of `self` and those of `o` after them.
    pub fn cat(self, o: Ch) -> Ch {
        match (self.len, o.len) {
            (0, _) => o,
            (_, 0) => self,
            _ => Ch { crc: crc_fast::checksum_combine(Crc64Nvme, self.crc, o.crc, o.len), len: self.len + o.len },
        }
    }

    /// The length of the bytes hashed.
    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// As bytes, for keys of other hashes.
    pub fn to_bytes(self) -> [u8; 16] {
        let mut out = [0; 16];
        out[..8].copy_from_slice(&self.crc.to_le_bytes());
        out[8..].copy_from_slice(&self.len.to_le_bytes());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hash of bytes cut anywhere and put together again, or hashed on in pieces, is that of
    /// the whole; other bytes, or the same in another order, hash otherwise.
    #[test]
    fn composes() {
        let d: Vec<u8> = (0..10_007u32).map(|i| (i.wrapping_mul(2654435761) >> 11) as u8).collect();
        let whole = Ch::of(&d);
        assert_eq!(whole.len(), d.len() as u64);
        for cut in [0, 1, 3, 4, 5, 16, 17, 999, 10_006, 10_007] {
            assert_eq!(Ch::of(&d[..cut]).cat(Ch::of(&d[cut..])), whole, "{cut}");
            assert_eq!(Ch::of(&d[..cut]).then(&d[cut..]), whole, "{cut}");
        }
        let pieces = d.chunks(13).fold(Ch::EMPTY, |h, p| h.then(p));
        assert_eq!(pieces, whole);
        assert_eq!(Ch::EMPTY.cat(whole).cat(Ch::EMPTY), whole);
        let mut other = d.clone();
        other[5000] ^= 1;
        assert_ne!(Ch::of(&other), whole);
        other[5000] ^= 1;
        other.swap(10, 11);
        assert_ne!(Ch::of(&other), whole);
        assert_ne!(Ch::of(&[0]), Ch::of(&[0, 0]));
        assert_ne!(Ch::of(&[1, 0]), Ch::of(&[0, 1]));
    }
}
