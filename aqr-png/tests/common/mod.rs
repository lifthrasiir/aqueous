//! Helpers building synthetic PNG files, shared by the test crates.

#![allow(dead_code)]

// Not every test crate takes all of them.
#[allow(unused_imports)]
pub use flate_zoo::{Encoder, Sweep, backend, backends, presets_of};

/// The modules a formula of aqr-png applies: itself, on `threads` threads.
pub fn modules(threads: usize) -> impl Fn(&aqr_formula::Apply, &[&[u8]]) -> aqr_formula::Result<Vec<u8>> {
    move |a, inputs| {
        assert_eq!(a.module, aqr_png::formula::MODULE);
        aqr_png::formula::decode(&a.params, inputs, a.len, threads).map_err(|e| aqr_formula::Error::new(e.to_string()))
    }
}

pub fn crc(kind: &[u8], data: &[u8]) -> u32 {
    let mut h = crc32fast::Hasher::new();
    h.update(kind);
    h.update(data);
    h.finalize()
}

pub fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc(kind, data).to_be_bytes());
}

pub fn adler32(d: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in d {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    b << 16 | a
}

/// A zlib stream of `data` by `enc`.
pub fn zlib(enc: &Encoder, data: &[u8]) -> Vec<u8> {
    let mut z = vec![0x78, 0x9c];
    z.extend_from_slice(&enc.compress(data).unwrap());
    z.extend_from_slice(&adler32(data).to_be_bytes());
    z
}

pub fn ihdr(w: u32, h: u32, depth: u8, color: u8, interlace: u8) -> Vec<u8> {
    let mut d = w.to_be_bytes().to_vec();
    d.extend_from_slice(&h.to_be_bytes());
    d.extend_from_slice(&[depth, color, 0, 0, interlace]);
    d
}

/// Bytes per row (without the filter byte) and rows of each pass.
pub fn passes(w: usize, h: usize, bits: usize, interlace: bool) -> Vec<(usize, usize)> {
    let list: &[(usize, usize, usize, usize)] = if interlace {
        &[(0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8), (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2)]
    } else {
        &[(0, 0, 1, 1)]
    };
    list.iter()
        .filter_map(|&(x0, y0, dx, dy)| {
            let pw = if w > x0 { (w - x0).div_ceil(dx) } else { 0 };
            let ph = if h > y0 { (h - y0).div_ceil(dy) } else { 0 };
            (pw > 0 && ph > 0).then(|| ((pw * bits).div_ceil(8), ph))
        })
        .collect()
}

/// Filtered image data: smooth gradients with noise, each row with its own filter, as a
/// real encoder would write it.
pub fn image_data(w: usize, h: usize, depth: u8, color: u8, interlace: bool, seed: u32) -> Vec<u8> {
    let ch = match color {
        0 | 3 => 1,
        2 => 3,
        4 => 2,
        _ => 4,
    };
    let bits = ch * depth as usize;
    let bpp = bits.div_ceil(8);
    let mut rng = seed.wrapping_mul(2654435761) | 1;
    let mut rnd = || {
        rng ^= rng << 13;
        rng ^= rng >> 17;
        rng ^= rng << 5;
        rng
    };
    let mut out = vec![];
    for (rb, rows) in passes(w, h, bits, interlace) {
        let mut prev = vec![0u8; rb];
        for y in 0..rows {
            let raw: Vec<u8> = (0..rb).map(|x| (((x * 3 + y * 5) as u32 + seed) / 4 % 251 + rnd() % 7) as u8).collect();
            let ft = (y % 5) as u8;
            out.push(ft);
            for x in 0..rb {
                let a = if x >= bpp { raw[x - bpp] } else { 0 };
                let (b, c) = (prev[x], if x >= bpp { prev[x - bpp] } else { 0 });
                let p = match ft {
                    1 => a,
                    2 => b,
                    3 => ((a as u16 + b as u16) / 2) as u8,
                    4 => {
                        let p = a as i16 + b as i16 - c as i16;
                        let (pa, pb, pc) = ((p - a as i16).abs(), (p - b as i16).abs(), (p - c as i16).abs());
                        if pa <= pb && pa <= pc {
                            a
                        } else if pb <= pc {
                            b
                        } else {
                            c
                        }
                    }
                    _ => 0,
                };
                out.push(raw[x].wrapping_sub(p));
            }
            prev = raw;
        }
    }
    out
}

pub fn png(chunks: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
    let mut f = b"\x89PNG\r\n\x1a\n".to_vec();
    for (k, d) in chunks {
        chunk(&mut f, k, d);
    }
    f
}

/// IDAT chunks of `z`, `size` bytes each.
pub fn idats(z: &[u8], size: usize) -> Vec<(&'static [u8; 4], Vec<u8>)> {
    z.chunks(size).map(|c| (b"IDAT", c.to_vec())).collect()
}

pub fn zlib6() -> Encoder {
    Encoder::new(backend("zlib").unwrap())
}

pub fn apng(frames: &[(u32, u32)], seed: u32) -> Vec<u8> {
    let mut actl = (frames.len() as u32).to_be_bytes().to_vec();
    actl.extend_from_slice(&0u32.to_be_bytes());
    let fctl = |seq: u32, w: u32, h: u32| {
        let mut d = seq.to_be_bytes().to_vec();
        for v in [w, h, 0, 0] {
            d.extend_from_slice(&v.to_be_bytes());
        }
        d.extend_from_slice(&[0, 1, 0, 10, 0, 0]);
        d
    };
    let (w0, h0) = frames[0];
    let mut cs = vec![(b"IHDR", ihdr(w0, h0, 8, 6, 0)), (b"acTL", actl), (b"fcTL", fctl(0, w0, h0))];
    cs.extend(idats(&zlib(&zlib6(), &image_data(w0 as usize, h0 as usize, 8, 6, false, seed)), 8192));
    let mut seq = 1;
    for &(w, h) in &frames[1..] {
        cs.push((b"fcTL", fctl(seq, w, h)));
        seq += 1;
        let z = zlib(&zlib6(), &image_data(w as usize, h as usize, 8, 6, false, seed + seq));
        for c in z.chunks(7000) {
            let mut d = seq.to_be_bytes().to_vec();
            d.extend_from_slice(c);
            cs.push((b"fdAT", d));
            seq += 1;
        }
    }
    cs.push((b"IEND", vec![]));
    png(&cs)
}

/// Raw rows of an 8-bit image of `ch` channels, without filter bytes: as [`image_data`]'s,
/// with the pixels of `rect` (x, y, width, height) given values of their own (a layer shown
/// over the picture).
pub fn raw_rows(w: usize, h: usize, ch: usize, seed: u32, rect: Option<(usize, usize, usize, usize)>) -> Vec<Vec<u8>> {
    let mut rng = seed.wrapping_mul(2654435761) | 1;
    let mut rnd = || {
        rng ^= rng << 13;
        rng ^= rng >> 17;
        rng ^= rng << 5;
        rng
    };
    (0..h)
        .map(|y| {
            (0..w * ch)
                .map(|x| {
                    let v = (((x * 3 + y * 5) as u32 + seed) / 4 % 251 + rnd() % 7) as u8;
                    match rect {
                        Some((rx, ry, rw, rh)) if (rx..rx + rw).contains(&(x / ch)) && (ry..ry + rh).contains(&y) => {
                            (x as u8).wrapping_mul(31) ^ (y as u8).wrapping_mul(17)
                        }
                        _ => v,
                    }
                })
                .collect()
        })
        .collect()
}

/// `rows` filtered as [`image_data`] does (filter `y % 5` for row `y`), `bpp` bytes per pixel.
pub fn filter_rows(rows: &[Vec<u8>], bpp: usize) -> Vec<u8> {
    let mut out = vec![];
    let zero = vec![0u8; rows.first().map_or(0, |r| r.len())];
    for (y, raw) in rows.iter().enumerate() {
        let prev = if y == 0 { &zero } else { &rows[y - 1] };
        let ft = (y % 5) as u8;
        out.push(ft);
        for x in 0..raw.len() {
            let a = if x >= bpp { raw[x - bpp] } else { 0 };
            let (b, c) = (prev[x], if x >= bpp { prev[x - bpp] } else { 0 });
            let p = match ft {
                1 => a,
                2 => b,
                3 => ((a as u16 + b as u16) / 2) as u8,
                4 => {
                    let p = a as i16 + b as i16 - c as i16;
                    let (pa, pb, pc) = ((p - a as i16).abs(), (p - b as i16).abs(), (p - c as i16).abs());
                    if pa <= pb && pa <= pc {
                        a
                    } else if pb <= pc {
                        b
                    } else {
                        c
                    }
                }
                _ => 0,
            };
            out.push(raw[x].wrapping_sub(p));
        }
    }
    out
}
