//! bzip2's CRC-32: the polynomial 0x04C11DB7 of zlib's, but MSB first.

const fn table() -> [[u32; 256]; 4] {
    let mut t = [[0u32; 256]; 4];
    let mut i = 0;
    while i < 256 {
        let mut c = (i as u32) << 24;
        let mut k = 0;
        while k < 8 {
            c = if c & 0x8000_0000 != 0 { (c << 1) ^ 0x04C1_1DB7 } else { c << 1 };
            k += 1;
        }
        t[0][i] = c;
        i += 1;
    }
    let mut i = 0;
    while i < 256 {
        let mut k = 1;
        while k < 4 {
            let c = t[k - 1][i];
            t[k][i] = (c << 8) ^ t[0][(c >> 24) as usize];
            k += 1;
        }
        i += 1;
    }
    t
}

static TABLE: [[u32; 256]; 4] = table();

/// `crc` (not inverted) updated with `d`, four bytes at a time.
pub fn update(mut crc: u32, d: &[u8]) -> u32 {
    let (chunks, rest) = d.as_chunks::<4>();
    for c in chunks {
        let x = crc ^ u32::from_be_bytes([c[0], c[1], c[2], c[3]]);
        crc = TABLE[3][(x >> 24) as usize]
            ^ TABLE[2][(x >> 16 & 255) as usize]
            ^ TABLE[1][(x >> 8 & 255) as usize]
            ^ TABLE[0][(x & 255) as usize];
    }
    for &b in rest {
        crc = (crc << 8) ^ TABLE[0][((crc >> 24) as u8 ^ b) as usize];
    }
    crc
}

/// The CRC of a block's data.
pub fn hash(d: &[u8]) -> u32 {
    !update(!0, d)
}

#[cfg(test)]
mod tests {
    #[test]
    fn check_value() {
        // CRC-32/BZIP2
        assert_eq!(super::hash(b"123456789"), 0xFC89_1918);
        let d: Vec<u8> = (0..1000u32).map(|i| (i * 7 + i / 13) as u8).collect();
        let slow = d.iter().fold(!0u32, |c, &b| (c << 8) ^ super::TABLE[0][((c >> 24) as u8 ^ b) as usize]);
        assert_eq!(super::hash(&d), !slow);
    }
}
