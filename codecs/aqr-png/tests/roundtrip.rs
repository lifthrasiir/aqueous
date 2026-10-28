//! Round trips of synthetic PNG files: every DEFLATE encoder of flate-zoo, every pixel
//! format, interlacing, several zlib streams, APNG frames, and malformed files, through the
//! formula module.

mod common;

use aqr_formula::Formula;
use aqr_png::formula;
use common::*;

/// The formula of `file`, if it has one: it evaluates to `file`.
#[track_caller]
fn roundtrip(file: &[u8]) -> Option<Formula> {
    let f = formula::encode(file, 1)?;
    assert_eq!(f.len(), file.len() as u64);
    assert!(f.eval(&modules(1)).expect("evaluates") == file, "round trip differs");
    Some(f)
}

/// The formula's streams, all but the skeleton: what the records are made of.
fn records_len(f: &Formula) -> usize {
    f.streams[2].data.len()
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
    let c = roundtrip(&f).expect("coded");
    assert!(records_len(&c) < f.len(), "{} -> {}", f.len(), records_len(&c));
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
        let c = roundtrip(&f).expect("coded");
        assert!(records_len(&c) < f.len());
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
    let c = roundtrip(&f).expect("coded");
    assert!(records_len(&c) < f.len() * 9 / 10, "{} -> {}", f.len(), records_len(&c));
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

// Threads: the same formula whatever the thread count, and the same outcome evaluating it.

const THREADS: [usize; 4] = [1, 2, 3, 8];

#[track_caller]
fn same_for_threads(file: &[u8]) -> Option<Formula> {
    let f = formula::encode(file, 1);
    for t in THREADS {
        assert!(formula::encode(file, t) == f, "formula differs with {t} threads");
        if let Some(f) = &f {
            assert!(f.eval(&modules(t)).expect("evaluates") == file, "round trip differs with {t} threads");
        }
    }
    f
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
        assert!(same_for_threads(&f).is_some(), "coded");
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
    assert!(same_for_threads(&png(&cs)).is_some());
}

#[test]
fn threads_apng() {
    let f = apng(&[(300, 200), (250, 180), (300, 200), (120, 100)], 21);
    assert!(same_for_threads(&f).is_some());
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
    // A big image with a broken stream has the same outcome whatever the threads.
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
    let f = formula::encode(&apng(&[(120, 90), (100, 80), (120, 90)], 41), 1).expect("coded");
    let inputs: Vec<&[u8]> = f.streams.iter().map(|s| &s.data[..]).collect();
    let len = f.len();
    let mut x = 7u32;
    for i in 0..1500 {
        x = x.wrapping_mul(1103515245).wrapping_add(12345);
        // The skeleton is left alone: a damaged IHDR can ask for an image of terabytes.
        let mut copies: Vec<Vec<u8>> = inputs.iter().map(|s| s.to_vec()).collect();
        let k = 1 + i % 2;
        let at = (x as usize >> 4) % copies[k].len();
        copies[k][at] ^= 1 << (i % 8);
        let refs: Vec<&[u8]> = copies.iter().map(|c| &c[..]).collect();
        let serial = formula::decode(&[], &refs, len, 1).ok();
        for t in [2, 8] {
            assert!(formula::decode(&[], &refs, len, t).ok() == serial, "outcome differs at {at} with {t} threads");
        }
    }
    for cut in (0..inputs[2].len()).step_by(inputs[2].len() / 200 + 1) {
        let refs = [inputs[0], inputs[1], &inputs[2][..cut]];
        let serial = formula::decode(&[], &refs, len, 1).ok();
        assert!(formula::decode(&[], &refs, len, 4).ok() == serial);
    }
}
