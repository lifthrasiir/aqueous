//! CRC-32 (of zlib, ZIP and PNG): natively crc32fast's, with the CPU's instructions; in
//! WebAssembly, the same 16 bytes at a time as crc32fast's fallback there, but with its tables
//! built on first use rather than in every module (16 KiB that do not compress).

#[cfg(not(target_family = "wasm"))]
pub use crc32fast::{Hasher, hash};

#[cfg(target_family = "wasm")]
pub use sliced::{Hasher, hash};

#[cfg(any(target_family = "wasm", test))]
mod sliced {
    use std::sync::OnceLock;

    type Tables = [[u32; 256]; 16];

    /// `t[k][b]`: the CRC of byte `b` followed by `k` zero bytes.
    fn tables() -> &'static Tables {
        static TABLES: OnceLock<Box<Tables>> = OnceLock::new();
        TABLES.get_or_init(|| {
            let mut t = Box::new([[0; 256]; 16]);
            for b in 0..256 {
                let mut c = b as u32;
                for _ in 0..8 {
                    c = (c >> 1) ^ (0xedb8_8320 & (c & 1).wrapping_neg());
                }
                t[0][b] = c;
            }
            for k in 1..16 {
                for b in 0..256 {
                    let c = t[k - 1][b];
                    t[k][b] = (c >> 8) ^ t[0][(c & 0xff) as usize];
                }
            }
            t
        })
    }

    fn update(crc: u32, mut buf: &[u8]) -> u32 {
        let t = tables();
        let mut crc = !crc;
        while let Some((b, rest)) = buf.split_first_chunk::<16>() {
            crc ^= u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
            crc = t[0x0][b[0xf] as usize]
                ^ t[0x1][b[0xe] as usize]
                ^ t[0x2][b[0xd] as usize]
                ^ t[0x3][b[0xc] as usize]
                ^ t[0x4][b[0xb] as usize]
                ^ t[0x5][b[0xa] as usize]
                ^ t[0x6][b[0x9] as usize]
                ^ t[0x7][b[0x8] as usize]
                ^ t[0x8][b[0x7] as usize]
                ^ t[0x9][b[0x6] as usize]
                ^ t[0xa][b[0x5] as usize]
                ^ t[0xb][b[0x4] as usize]
                ^ t[0xc][(crc >> 24) as usize]
                ^ t[0xd][(crc >> 16 & 0xff) as usize]
                ^ t[0xe][(crc >> 8 & 0xff) as usize]
                ^ t[0xf][(crc & 0xff) as usize];
            buf = rest;
        }
        for &b in buf {
            crc = t[0][((crc ^ b as u32) & 0xff) as usize] ^ (crc >> 8);
        }
        !crc
    }

    /// As `crc32fast::Hasher`.
    #[derive(Debug, Clone, Default)]
    pub struct Hasher(u32);

    impl Hasher {
        pub fn new() -> Self {
            Hasher(0)
        }

        pub fn update(&mut self, buf: &[u8]) {
            self.0 = update(self.0, buf);
        }

        pub fn finalize(self) -> u32 {
            self.0
        }
    }

    /// As `crc32fast::hash`.
    pub fn hash(buf: &[u8]) -> u32 {
        update(0, buf)
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn as_crc32fast() {
            let d: Vec<u8> = (0..1000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
            for (a, b) in [(0, 0), (0, 1), (0, 15), (0, 16), (3, 40), (7, 1000), (0, 1000)] {
                assert_eq!(super::hash(&d[a..b]), crc32fast::hash(&d[a..b]), "{a}..{b}");
            }
            let mut h = super::Hasher::new();
            h.update(&d[..5]);
            h.update(&d[5..]);
            assert_eq!(h.finalize(), crc32fast::hash(&d));
        }
    }
}
