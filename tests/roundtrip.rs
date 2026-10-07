//! Round trips of synthetic PNG files: every DEFLATE encoder of flate-zoo, every pixel
//! format, interlacing, several zlib streams, APNG frames, and malformed files.

use flate_zoo::{Encoder, Sweep, backend, backends, presets_of};

fn crc(kind: &[u8], data: &[u8]) -> u32 {
    let mut h = crc32fast::Hasher::new();
    h.update(kind);
    h.update(data);
    h.finalize()
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc(kind, data).to_be_bytes());
}

fn adler32(d: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in d {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    b << 16 | a
}

/// A zlib stream of `data` by `enc`.
fn zlib(enc: &Encoder, data: &[u8]) -> Vec<u8> {
    let mut z = vec![0x78, 0x9c];
    z.extend_from_slice(&enc.compress(data).unwrap());
    z.extend_from_slice(&adler32(data).to_be_bytes());
    z
}

fn ihdr(w: u32, h: u32, depth: u8, color: u8, interlace: u8) -> Vec<u8> {
    let mut d = w.to_be_bytes().to_vec();
    d.extend_from_slice(&h.to_be_bytes());
    d.extend_from_slice(&[depth, color, 0, 0, interlace]);
    d
}

/// Bytes per row (without the filter byte) and rows of each pass.
fn passes(w: usize, h: usize, bits: usize, interlace: bool) -> Vec<(usize, usize)> {
    let list: &[(usize, usize, usize, usize)] =
        if interlace { &[(0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8), (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2)] } else { &[(0, 0, 1, 1)] };
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
fn image_data(w: usize, h: usize, depth: u8, color: u8, interlace: bool, seed: u32) -> Vec<u8> {
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
            let raw: Vec<u8> = (0..rb)
                .map(|x| (((x * 3 + y * 5) as u32 + seed) / 4 % 251 + rnd() % 7) as u8)
                .collect();
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
                        if pa <= pb && pa <= pc { a } else if pb <= pc { b } else { c }
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

fn png(chunks: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
    let mut f = b"\x89PNG\r\n\x1a\n".to_vec();
    for (k, d) in chunks {
        chunk(&mut f, k, d);
    }
    f
}

/// IDAT chunks of `z`, `size` bytes each.
fn idats(z: &[u8], size: usize) -> Vec<(&'static [u8; 4], Vec<u8>)> {
    z.chunks(size).map(|c| (b"IDAT", c.to_vec())).collect()
}

#[track_caller]
fn roundtrip(file: &[u8]) -> Vec<u8> {
    let c = png_recons::container::try_compress(file);
    let d = png_recons::decompress(&c).expect("decompresses");
    assert!(d == file, "round trip differs");
    c
}

fn zlib6() -> Encoder {
    Encoder::new(backend("zlib").unwrap())
}

#[test]
fn every_encoder() {
    let data = image_data(61, 40, 8, 2, false, 1);
    for b in backends() {
        for enc in presets_of(*b, Sweep::Default) {
            let z = zlib(&enc, &data);
            let mut cs = vec![(b"IHDR", ihdr(61, 40, 8, 2, 0))];
            cs.extend(idats(&z, 1000));
            cs.push((b"IEND", vec![]));
            roundtrip(&png(&cs));
        }
    }
}

#[test]
fn every_format() {
    for (depth, color) in [(1, 0), (2, 0), (4, 0), (8, 0), (16, 0), (8, 2), (16, 2), (1, 3), (4, 3), (8, 3), (8, 4), (16, 4), (8, 6), (16, 6)] {
        for interlace in [0, 1] {
            let data = image_data(37, 29, depth, color, interlace == 1, depth as u32 * 7 + color as u32);
            let z = zlib(&zlib6(), &data);
            let mut cs = vec![(b"IHDR", ihdr(37, 29, depth, color, interlace))];
            if color == 3 {
                cs.push((b"PLTE", (0..768).map(|i| (i * 7) as u8).collect()));
            }
            cs.extend(idats(&z, 8192));
            cs.push((b"IEND", vec![]));
            roundtrip(&png(&cs));
        }
    }
}

#[test]
fn coded_smaller() {
    // A natural-sized image is coded, and smaller than the PNG.
    let data = image_data(300, 200, 8, 2, false, 3);
    let z = zlib(&zlib6(), &data);
    let mut cs = vec![(b"IHDR", ihdr(300, 200, 8, 2, 0))];
    cs.extend(idats(&z, 8192));
    cs.push((b"IEND", vec![]));
    let f = png(&cs);
    let c = roundtrip(&f);
    assert!(c.len() < f.len() && c[5] == 1, "{} -> {}", f.len(), c.len());
}

#[test]
fn several_zlib_streams() {
    // The image data in two zlib streams (one after the other in the IDAT chunks), and
    // three.
    let data = image_data(300, 200, 8, 6, false, 4);
    for parts in [2, 3] {
        let cut: Vec<usize> = (0..=parts).map(|k| (data.len() / parts * k).min(data.len())).collect();
        let mut z = vec![];
        for w in cut.windows(2) {
            z.extend_from_slice(&zlib(&zlib6(), &data[w[0]..w[1]]));
        }
        let mut cs = vec![(b"IHDR", ihdr(300, 200, 8, 6, 0))];
        cs.extend(idats(&z, 5000));
        cs.push((b"IEND", vec![]));
        let f = png(&cs);
        let c = roundtrip(&f);
        assert!(c.len() < f.len() && c[5] == 1);
    }
}

#[test]
fn apng_frames() {
    let mut actl = 3u32.to_be_bytes().to_vec();
    actl.extend_from_slice(&0u32.to_be_bytes());
    let fctl = |seq: u32, w: u32, h: u32| {
        let mut d = seq.to_be_bytes().to_vec();
        for v in [w, h, 0, 0] {
            d.extend_from_slice(&v.to_be_bytes());
        }
        d.extend_from_slice(&[0, 1, 0, 10, 0, 0]);
        d
    };
    let mut cs = vec![(b"IHDR", ihdr(200, 150, 8, 6, 0)), (b"acTL", actl), (b"fcTL", fctl(0, 200, 150))];
    cs.extend(idats(&zlib(&zlib6(), &image_data(200, 150, 8, 6, false, 5)), 8192));
    let mut seq = 1;
    for (w, h) in [(120, 100), (200, 150)] {
        cs.push((b"fcTL", fctl(seq, w, h)));
        seq += 1;
        let z = zlib(&zlib6(), &image_data(w as usize, h as usize, 8, 6, false, seq));
        for c in z.chunks(7000) {
            let mut d = seq.to_be_bytes().to_vec();
            d.extend_from_slice(c);
            cs.push((b"fdAT", d));
            seq += 1;
        }
    }
    cs.push((b"IEND", vec![]));
    let f = png(&cs);
    let c = roundtrip(&f);
    assert!(c.len() < f.len() * 9 / 10, "{} -> {}", f.len(), c.len());
}

#[test]
fn malformed() {
    let data = image_data(100, 80, 8, 2, false, 6);
    let z = zlib(&zlib6(), &data);
    let base = |z: &[u8]| {
        let mut cs = vec![(b"IHDR", ihdr(100, 80, 8, 2, 0))];
        cs.extend(idats(z, 3000));
        cs.push((b"IEND", vec![]));
        png(&cs)
    };
    // Junk after the zlib stream, a wrong CRC, bytes after IEND, a truncated file, too
    // little and too much image data, a broken stream, not a PNG at all.
    let mut junk = z.clone();
    junk.extend_from_slice(b"junk after the stream");
    roundtrip(&base(&junk));
    let mut f = base(&z);
    let n = f.len();
    f[n - 20] ^= 1;
    roundtrip(&f);
    let mut f = base(&z);
    f.extend_from_slice(b"trailer");
    roundtrip(&f);
    let f = base(&z);
    for cut in [0, 5, 8, 20, 40, f.len() / 2, f.len() - 1] {
        roundtrip(&f[..cut]);
    }
    roundtrip(&base(&zlib(&zlib6(), &data[..data.len() / 2])));
    let mut more = data.clone();
    more.extend_from_slice(&data[..500]);
    roundtrip(&base(&zlib(&zlib6(), &more)));
    let mut broken = z.clone();
    broken[100] ^= 0x55;
    roundtrip(&base(&broken));
    roundtrip(b"not a png at all");
    roundtrip(b"");
}

#[test]
fn corrupt_input_fails_cleanly() {
    let data = image_data(120, 90, 8, 2, false, 7);
    let mut cs = vec![(b"IHDR", ihdr(120, 90, 8, 2, 0))];
    cs.extend(idats(&zlib(&zlib6(), &data), 8192));
    cs.push((b"IEND", vec![]));
    let c = png_recons::container::try_compress(&png(&cs));
    let mut x = 1u32;
    for i in 0..2000 {
        x = x.wrapping_mul(1103515245).wrapping_add(12345);
        let mut d = c.clone();
        let at = (x as usize >> 4) % d.len();
        d[at] ^= 1 << (i % 8);
        // Never a panic; an error or (rarely) the same file.
        let _ = png_recons::decompress(&d);
    }
    for cut in 0..c.len().min(400) {
        let _ = png_recons::decompress(&c[..cut]);
    }
}
