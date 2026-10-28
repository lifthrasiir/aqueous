//! CRC-32C (Castagnoli, reflected polynomial 0x82F63B78): the raw update that the CPU
//! instructions compute, which Snappy hashes with, and the usual checksum that LevelDB masks.

const POLY: u32 = 0x82F6_3B78;

/// `TABLES[k][b]`: the update of a byte `b` followed by `k` zero bytes (slicing by 8).
static TABLES: [[u32; 256]; 8] = {
    let mut t = [[0u32; 256]; 8];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { c >> 1 ^ POLY } else { c >> 1 };
            k += 1;
        }
        t[0][i] = c;
        i += 1;
    }
    let mut k = 1;
    while k < 8 {
        let mut i = 0;
        while i < 256 {
            let c = t[k - 1][i];
            t[k][i] = c >> 8 ^ t[0][(c & 0xff) as usize];
            i += 1;
        }
        k += 1;
    }
    t
};

/// `crc32cw(crc, data)` of ARMv8 (`_mm_crc32_u32` of x86): `crc` updated by the 4 bytes of
/// `data`, little endian, without inversions.
#[inline]
pub fn update32(crc: u32, data: u32) -> u32 {
    let x = crc ^ data;
    TABLES[3][(x & 0xff) as usize]
        ^ TABLES[2][(x >> 8 & 0xff) as usize]
        ^ TABLES[1][(x >> 16 & 0xff) as usize]
        ^ TABLES[0][(x >> 24) as usize]
}

/// `crc` updated by `d`, without inversions.
fn update(mut crc: u32, d: &[u8]) -> u32 {
    let (chunks, rest) = d.as_chunks::<8>();
    for c in chunks {
        let lo = crc ^ u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
        crc = TABLES[7][(lo & 0xff) as usize]
            ^ TABLES[6][(lo >> 8 & 0xff) as usize]
            ^ TABLES[5][(lo >> 16 & 0xff) as usize]
            ^ TABLES[4][(lo >> 24) as usize]
            ^ TABLES[3][c[4] as usize]
            ^ TABLES[2][c[5] as usize]
            ^ TABLES[1][c[6] as usize]
            ^ TABLES[0][c[7] as usize];
    }
    for &b in rest {
        crc = crc >> 8 ^ TABLES[0][((crc ^ b as u32) & 0xff) as usize];
    }
    crc
}

/// The CRC-32C of the concatenation of `parts`.
pub fn checksum(parts: &[&[u8]]) -> u32 {
    !parts.iter().fold(!0, |c, p| update(c, p))
}

/// LevelDB's masked CRC of the concatenation of `parts` (`crc32c::Mask`).
pub fn masked(parts: &[&[u8]]) -> u32 {
    checksum(parts).rotate_right(15).wrapping_add(0xa282_ead8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_values() {
        // RFC 3720 B.4
        assert_eq!(checksum(&[&[0; 32]]), 0x8a91_36aa);
        assert_eq!(checksum(&[&[0xff; 32]]), 0x62a8_ab43);
        let inc: Vec<u8> = (0..32).collect();
        assert_eq!(checksum(&[&inc]), 0x46dd_794e);
        assert_eq!(checksum(&[b"123456789"]), 0xe306_9283);
        assert_eq!(checksum(&[b"1234", b"56789"]), 0xe306_9283);
        // the raw update of 4 bytes is that of the bytes one by one
        for (crc, data) in [(0, 0), (1, 0x8000_0000), (0xdead_beef, 0x0123_4567), (!0, 0x1e35_a7bd)] {
            assert_eq!(update32(crc, data), update(crc, &data.to_le_bytes()));
        }
    }
}
