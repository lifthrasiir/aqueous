//! zlib streams as zlib 1.3.1 writes them (windowBits 15, memLevel 8, default strategy, one
//! `deflate(Z_FINISH)`; git's chunked calls give the same bytes).

use aqr_flate::deflate::{self, Body, Stream};
use aqr_flate::zlib::{Emulator, Params, zlib_block};

/// The second header byte zlib writes at `level` (FLEVEL and FCHECK, after 0x78).
pub fn header(level: u8) -> [u8; 2] {
    let flevel: u16 = match level {
        0 | 1 => 0,
        2..=5 => 1,
        6 => 2,
        _ => 3,
    };
    let h = 0x7800 | flevel << 6;
    let h = h + 31 - h % 31;
    [(h >> 8) as u8, h as u8]
}

/// The levels whose header is `h`.
pub fn levels(h: [u8; 2]) -> &'static [u8] {
    if h[0] != 0x78 {
        return &[];
    }
    match h[1] {
        0x01 => &[1],
        0x5e => &[2, 3, 4, 5],
        0x9c => &[6],
        0xda => &[7, 8, 9],
        _ => &[],
    }
}

pub fn adler32(d: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
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

/// The raw DEFLATE data zlib writes for `data` at `level` (1-9).
pub fn deflate(data: &[u8], level: u8) -> Vec<u8> {
    let p = Params { level, ..Params::default() };
    let mut e = Emulator::new(data, p);
    let mut blocks = vec![];
    let mut pos = 0;
    while let Some(b) = zlib_block(&mut e, pos) {
        pos += match &b.body {
            Body::Stored { len, .. } => *len as usize,
            Body::Fixed { tokens } | Body::Dynamic { tokens, .. } => tokens.iter().map(|t| t.span()).sum(),
        };
        let last = b.last;
        blocks.push(b);
        if last {
            break;
        }
    }
    deflate::write(&Stream { blocks, ..Default::default() }, data).expect("emulated stream")
}

/// The whole zlib stream.
pub fn zlib(data: &[u8], level: u8, out: &mut Vec<u8>) {
    out.extend_from_slice(&header(level));
    out.extend_from_slice(&deflate(data, level));
    out.extend_from_slice(&adler32(data).to_be_bytes());
}

/// The level at which zlib writes `z` for `data`, trying `first` before the others the
/// header allows.
pub fn find_level(z: &[u8], data: &[u8], first: u8) -> Option<u8> {
    if z.len() < 6 || z[z.len() - 4..] != adler32(data).to_be_bytes() {
        return None;
    }
    let ls = levels([z[0], z[1]]);
    let body = &z[2..z.len() - 4];
    let order = ls.iter().filter(|&&l| l == first).chain(ls.iter().filter(|&&l| l != first));
    order.copied().find(|&l| deflate(data, l) == body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers() {
        for l in 1..=9 {
            let h = header(l);
            assert!(levels(h).contains(&l), "{l}: {h:02x?}");
            assert_eq!((h[0] as u16 * 256 + h[1] as u16) % 31, 0);
        }
    }

    #[test]
    fn matches_zlib() {
        let mut data = include_bytes!("typed.rs").to_vec();
        data.extend_from_slice(&data.clone());
        for d in [&b""[..], b"a", &data] {
            for l in 1..=9 {
                let enc = flate_zoo::Encoder::new(flate_zoo::backend("zlib").unwrap()).set("level", l as i64).unwrap();
                let raw = enc.compress(d).unwrap();
                assert_eq!(deflate(d, l), raw, "level {l}, {} bytes", d.len());
                let mut z = vec![];
                zlib(d, l, &mut z);
                assert!(levels([z[0], z[1]]).contains(&find_level(&z, d, 6).unwrap()));
            }
        }
    }
}
