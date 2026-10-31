//! Ogg's CRC-32: polynomial 0x04c11db7, most significant bit first, of initial value 0 and no
//! final XOR, 8 bytes at a time with its tables built on first use (not in every module).

use std::sync::OnceLock;

type Tables = [[u32; 256]; 8];

/// `t[k][b]`: the CRC of byte `b` followed by `k` zero bytes.
fn tables() -> &'static Tables {
    static TABLES: OnceLock<Box<Tables>> = OnceLock::new();
    TABLES.get_or_init(|| {
        let mut t = Box::new([[0; 256]; 8]);
        for b in 0..256 {
            let mut c = (b as u32) << 24;
            for _ in 0..8 {
                c = c << 1 ^ (0x04c1_1db7 & (c >> 31).wrapping_neg());
            }
            t[0][b] = c;
        }
        for k in 1..8 {
            for b in 0..256 {
                let c = t[k - 1][b];
                t[k][b] = c << 8 ^ t[0][(c >> 24) as usize];
            }
        }
        t
    })
}

/// The CRC of `buf` continued from `crc`.
pub fn update(mut crc: u32, mut buf: &[u8]) -> u32 {
    let t = tables();
    while let Some((b, rest)) = buf.split_first_chunk::<8>() {
        crc ^= u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
        crc = t[7][(crc >> 24) as usize]
            ^ t[6][(crc >> 16 & 0xff) as usize]
            ^ t[5][(crc >> 8 & 0xff) as usize]
            ^ t[4][(crc & 0xff) as usize]
            ^ t[3][b[4] as usize]
            ^ t[2][b[5] as usize]
            ^ t[1][b[6] as usize]
            ^ t[0][b[7] as usize];
        buf = rest;
    }
    for &b in buf {
        crc = crc << 8 ^ t[0][((crc >> 24) ^ b as u32) as usize];
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bitwise(buf: &[u8]) -> u32 {
        let mut c = 0u32;
        for &b in buf {
            c ^= (b as u32) << 24;
            for _ in 0..8 {
                c = if c >> 31 != 0 { c << 1 ^ 0x04c1_1db7 } else { c << 1 };
            }
        }
        c
    }

    #[test]
    fn matches_bitwise() {
        let d: Vec<u8> = (0..1000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
        for n in [0, 1, 7, 8, 9, 63, 1000] {
            assert_eq!(update(0, &d[..n]), bitwise(&d[..n]), "{n}");
        }
        assert_eq!(update(update(0, &d[..13]), &d[13..]), bitwise(&d));
        // CRC-32/CKSUM without its final XOR (its check is 0x765e7680)
        assert_eq!(update(0, b"123456789"), 0x89a1_897f);
    }
}
