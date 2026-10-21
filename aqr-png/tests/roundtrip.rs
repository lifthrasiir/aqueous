//! Round trips of synthetic PNG files: every DEFLATE encoder of flate-zoo, every pixel
//! format, interlacing, several zlib streams, APNG frames, and malformed files.

mod common;

use common::*;

#[track_caller]
fn roundtrip(file: &[u8]) -> Vec<u8> {
    let c = aqr_png::container::try_compress(file);
    let d = aqr_png::decompress(&c).expect("decompresses");
    assert!(d == file, "round trip differs");
    c
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
    for (depth, color) in [
        (1, 0),
        (2, 0),
        (4, 0),
        (8, 0),
        (16, 0),
        (8, 2),
        (16, 2),
        (1, 3),
        (4, 3),
        (8, 3),
        (8, 4),
        (16, 4),
        (8, 6),
        (16, 6),
    ] {
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
    assert!(c.len() < f.len() && c[8] == 1, "{} -> {}", f.len(), c.len());
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
        assert!(c.len() < f.len() && c[8] == 1);
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
    let c = aqr_png::container::try_compress(&png(&cs));
    let mut x = 1u32;
    for i in 0..2000 {
        x = x.wrapping_mul(1103515245).wrapping_add(12345);
        let mut d = c.clone();
        let at = (x as usize >> 4) % d.len();
        d[at] ^= 1 << (i % 8);
        // Never a panic; an error or (rarely) the same file.
        let _ = aqr_png::decompress(&d);
    }
    for cut in 0..c.len().min(400) {
        let _ = aqr_png::decompress(&c[..cut]);
    }
}

// Threads: the same bytes whatever the thread count, and the same outcome decompressing.

const THREADS: [usize; 4] = [1, 2, 3, 8];

#[track_caller]
fn same_for_threads(file: &[u8]) -> Vec<u8> {
    let c = aqr_png::container::try_compress(file);
    for t in THREADS {
        assert!(aqr_png::compress_threads(file, t) == c, "compressed bytes differ with {t} threads");
        let d = aqr_png::decompress_threads(&c, t).expect("decompresses");
        assert!(d == file, "round trip differs with {t} threads");
    }
    c
}

#[test]
fn threads_single_large_image() {
    // Past the sizes at which rows and symbol counts are split between threads; several
    // pixel sizes, and interlaced.
    for (color, interlace) in [(6, false), (2, false), (0, false), (4, true)] {
        let (w, h) = (700usize, 500usize);
        let data = image_data(w, h, 8, color, interlace, 9 + color as u32);
        let mut cs = vec![(b"IHDR", ihdr(w as u32, h as u32, 8, color, interlace as u8))];
        cs.extend(idats(&zlib(&zlib6(), &data), 65536));
        cs.push((b"IEND", vec![]));
        let f = png(&cs);
        let c = same_for_threads(&f);
        assert!(c[8] == 1, "coded");
    }
}

#[test]
fn threads_several_zlib_streams() {
    let data = image_data(600, 400, 8, 6, false, 11);
    let cut: Vec<usize> = (0..=3).map(|k| (data.len() / 3 * k).min(data.len())).collect();
    let mut z = vec![];
    for w in cut.windows(2) {
        z.extend_from_slice(&zlib(&zlib6(), &data[w[0]..w[1]]));
    }
    let mut cs = vec![(b"IHDR", ihdr(600, 400, 8, 6, 0))];
    cs.extend(idats(&z, 20000));
    cs.push((b"IEND", vec![]));
    let c = same_for_threads(&png(&cs));
    assert!(c[8] == 1);
}

#[test]
fn threads_apng() {
    let f = apng(&[(300, 200), (250, 180), (300, 200), (120, 100)], 21);
    let c = same_for_threads(&f);
    assert!(c[8] == 1);
    // Frames past the image-parallel threshold of the rows as well.
    same_for_threads(&apng(&[(500, 400), (500, 400), (400, 300)], 31));
}

#[test]
fn threads_small_and_malformed() {
    let data = image_data(100, 80, 8, 2, false, 6);
    let z = zlib(&zlib6(), &data);
    let base = |z: &[u8]| {
        let mut cs = vec![(b"IHDR", ihdr(100, 80, 8, 2, 0))];
        cs.extend(idats(z, 3000));
        cs.push((b"IEND", vec![]));
        png(&cs)
    };
    let f = base(&z);
    same_for_threads(&f);
    for cut in [0, 5, 20, f.len() / 2, f.len() - 1] {
        same_for_threads(&f[..cut]);
    }
    let mut broken = z.clone();
    broken[100] ^= 0x55;
    same_for_threads(&base(&broken));
    same_for_threads(b"not a png at all");
    same_for_threads(b"");
    // A big image with a broken stream is stored whatever the threads.
    let big = image_data(700, 500, 8, 6, false, 12);
    let mut zb = zlib(&zlib6(), &big);
    let at = zb.len() / 2;
    zb[at] ^= 0x55;
    let mut cs = vec![(b"IHDR", ihdr(700, 500, 8, 6, 0))];
    cs.extend(idats(&zb, 65536));
    cs.push((b"IEND", vec![]));
    same_for_threads(&png(&cs));
}

#[test]
fn threads_corrupt_input_same_outcome() {
    let c = aqr_png::container::try_compress(&apng(&[(120, 90), (100, 80), (120, 90)], 41));
    let mut x = 7u32;
    for i in 0..1500 {
        x = x.wrapping_mul(1103515245).wrapping_add(12345);
        let mut d = c.clone();
        let at = (x as usize >> 4) % d.len();
        d[at] ^= 1 << (i % 8);
        let serial = aqr_png::decompress(&d).ok();
        for t in [2, 8] {
            assert!(aqr_png::decompress_threads(&d, t).ok() == serial, "outcome differs at {at} with {t} threads");
        }
    }
    for cut in (0..c.len()).step_by(c.len() / 200 + 1) {
        let serial = aqr_png::decompress(&c[..cut]).ok();
        assert!(aqr_png::decompress_threads(&c[..cut], 4).ok() == serial);
    }
}

/// The header (magic, version, mode), and nothing may follow the skeleton, a record's
/// parts or the last record.
#[test]
fn header_and_trailing_bytes() {
    let data = image_data(60, 40, 8, 2, false, 3);
    let mut cs = vec![(b"IHDR", ihdr(60, 40, 8, 2, 0))];
    cs.extend(idats(&zlib(&zlib6(), &data), 1 << 20));
    cs.push((b"IEND", vec![]));
    let f = png(&cs);
    let c = aqr_png::compress(&f);
    assert_eq!(c[..9], [0x69, 0x04, 0x7f, 0xa7, 0x59, 0xe0, 0, 0, 1]);
    assert_eq!(aqr_png::decompress(&c).unwrap(), f);
    for i in 0..8 {
        let mut d = c.clone();
        d[i] ^= 1;
        assert!(aqr_png::decompress(&d).is_err(), "header byte {i}");
    }
    let mut d = c.clone();
    d.push(0);
    assert!(aqr_png::decompress(&d).is_err(), "after the last record");

    // The single record: its length, mode 1, then three varint-framed parts.
    let mut at = 9 + 8;
    let varint = |src: &[u8], at: &mut usize| aqr_png::container::get_varint(src, at).unwrap() as usize;
    let n = varint(&c, &mut at);
    let sk_at = at;
    at += n;
    let rec_at = at;
    assert_eq!(u32::from_le_bytes(c[at..at + 4].try_into().unwrap()) as usize, c.len() - at - 4);
    assert_eq!(c[at + 4], 1);
    let mut d = c.clone();
    d.push(0);
    d[rec_at..rec_at + 4].copy_from_slice(&((c.len() - rec_at - 3) as u32).to_le_bytes());
    assert!(aqr_png::decompress(&d).is_err(), "after a record's model payload");

    // The skeleton with a byte after it.
    let mut sk = zstd::bulk::decompress(&c[sk_at..sk_at + n], 1 << 20).unwrap();
    sk.push(0);
    let z = zstd::bulk::compress(&sk, 19).unwrap();
    let mut d = c[..9 + 8].to_vec();
    aqr_png::container::put_varint(&mut d, z.len() as u64);
    d.extend_from_slice(&z);
    d.extend_from_slice(&c[rec_at..]);
    assert!(aqr_png::decompress(&d).is_err(), "after the skeleton");
    // The same rebuilt without the extra byte is fine (the test edits what it means to).
    sk.pop();
    let z = zstd::bulk::compress(&sk, 19).unwrap();
    let mut d = c[..9 + 8].to_vec();
    aqr_png::container::put_varint(&mut d, z.len() as u64);
    d.extend_from_slice(&z);
    d.extend_from_slice(&c[rec_at..]);
    assert_eq!(aqr_png::decompress(&d).unwrap(), f);
}
