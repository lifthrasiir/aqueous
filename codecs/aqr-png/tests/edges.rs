//! Edge cases of the PNG layer: tiny images (empty Adam7 passes), unusual zlib headers and
//! chunk layouts. Every file evaluates back to itself where it has a formula.

mod common;

use aqr_formula::Formula;
use aqr_png::formula;
use common::*;

#[track_caller]
fn roundtrip(file: &[u8]) -> Option<Formula> {
    use aqr_archive::Evaluate;
    let f = formula::encode(file, 1)?;
    assert_eq!(f.len(), file.len() as u64);
    assert!(f.eval(&modules(1)).expect("evaluates") == file, "round trip differs");
    Some(f)
}

fn simple(w: u32, h: u32, depth: u8, color: u8, il: u8) -> Vec<u8> {
    let data = image_data(w as usize, h as usize, depth, color, il == 1, w * 31 + h);
    let mut cs = vec![(b"IHDR", ihdr(w, h, depth, color, il))];
    if color == 3 {
        cs.push((b"PLTE", (0..768).map(|i| (i * 7) as u8).collect()));
    }
    cs.extend(idats(&zlib(&zlib6(), &data), 8192));
    cs.push((b"IEND", vec![]));
    png(&cs)
}

#[test]
fn tiny_images() {
    let mut coded = 0;
    for il in [0, 1] {
        for (depth, color) in [(1, 0), (4, 3), (8, 0), (8, 2), (8, 6), (16, 4)] {
            for (w, h) in [
                (1, 1),
                (1, 2),
                (1, 9),
                (9, 1),
                (2, 1),
                (2, 2),
                (3, 3),
                (5, 1),
                (1, 5),
                (8, 8),
                (9, 9),
                (1, 300),
                (300, 1),
            ] {
                coded += roundtrip(&simple(w, h, depth, color, il)).is_some() as usize;
            }
        }
    }
    assert!(coded > 0, "{coded} coded");
}

fn with_z(z: &[u8], size: usize) -> Vec<u8> {
    let mut cs = vec![(b"IHDR", ihdr(40, 30, 8, 2, 0))];
    cs.extend(idats(z, size));
    cs.push((b"IEND", vec![]));
    png(&cs)
}

#[test]
fn zlib_headers() {
    let data = image_data(40, 30, 8, 2, false, 4);
    let z = zlib(&zlib6(), &data);
    // Other levels' headers (FCHECK valid), a bad FCHECK, a deflate window of 256 bytes, a
    // method other than 8.
    for h in
        [[0x78, 0x01], [0x78, 0x5e], [0x78, 0xda], [0x58, 0x85], [0x08, 0x1d], [0x78, 0x9d], [0x79, 0x9c], [0x00, 0x00]]
    {
        let mut v = z.clone();
        v[..2].copy_from_slice(&h);
        roundtrip(&with_z(&v, 500));
    }
    // FDICT set: a dictionary id follows the header (valid FCHECK: 0x78bb), and the stream
    // after it is not preceded by one.
    let mut v = vec![0x78, 0xbb, 1, 2, 3, 4];
    v.extend_from_slice(&z[2..]);
    roundtrip(&with_z(&v, 500));
    let mut v = z.clone();
    v[1] = 0xbb;
    roundtrip(&with_z(&v, 500));
    // Too short for a header.
    for n in 0..4 {
        roundtrip(&with_z(&z[..n], 500));
    }
}

#[test]
fn zero_length_and_split_idat() {
    let data = image_data(40, 30, 8, 2, false, 5);
    let z = zlib(&zlib6(), &data);
    let mut cs = vec![(b"IHDR", ihdr(40, 30, 8, 2, 0)), (b"IDAT", vec![])];
    for c in z.chunks(300) {
        cs.push((b"IDAT", c.to_vec()));
        cs.push((b"IDAT", vec![]));
    }
    cs.push((b"IEND", vec![]));
    roundtrip(&png(&cs));
    // Only empty IDATs.
    roundtrip(&png(&[(b"IHDR", ihdr(40, 30, 8, 2, 0)), (b"IDAT", vec![]), (b"IDAT", vec![]), (b"IEND", vec![])]));
    // IDAT runs split by another chunk (not allowed, but seen).
    let h = z.len() / 2;
    let cs = vec![
        (b"IHDR", ihdr(40, 30, 8, 2, 0)),
        (b"IDAT", z[..h].to_vec()),
        (b"tEXt", b"Comment\0split".to_vec()),
        (b"IDAT", z[h..].to_vec()),
        (b"IEND", vec![]),
    ];
    roundtrip(&png(&cs));
    // And every other chunk of the run.
    let mut cs = vec![(b"IHDR", ihdr(40, 30, 8, 2, 0))];
    for (i, c) in z.chunks(200).enumerate() {
        cs.push((b"IDAT", c.to_vec()));
        if i % 2 == 0 {
            cs.push((b"tEXt", b"k\0v".to_vec()));
        }
    }
    cs.push((b"IEND", vec![]));
    roundtrip(&png(&cs));
}

fn fctl(seq: u32, w: u32, h: u32) -> Vec<u8> {
    let mut d = seq.to_be_bytes().to_vec();
    for v in [w, h, 0, 0] {
        d.extend_from_slice(&v.to_be_bytes());
    }
    d.extend_from_slice(&[0, 1, 0, 10, 0, 0]);
    d
}

#[test]
fn apng_oddities() {
    let (w, h) = (40u32, 30u32);
    let z0 = zlib(&zlib6(), &image_data(w as usize, h as usize, 8, 6, false, 1));
    let z1 = zlib(&zlib6(), &image_data(20, 10, 8, 6, false, 2));
    let mut actl = 2u32.to_be_bytes().to_vec();
    actl.extend_from_slice(&0u32.to_be_bytes());
    let fd = |seq: u32, c: &[u8]| {
        let mut d = seq.to_be_bytes().to_vec();
        d.extend_from_slice(c);
        d
    };
    let build = |frame: Vec<(&'static [u8; 4], Vec<u8>)>| {
        let mut cs = vec![(b"IHDR", ihdr(w, h, 8, 6, 0)), (b"acTL", actl.clone()), (b"fcTL", fctl(0, w, h))];
        cs.extend(idats(&z0, 4000));
        cs.extend(frame);
        cs.push((b"IEND", vec![]));
        png(&cs)
    };
    // Reference: well formed.
    let ok: Vec<_> = std::iter::once((b"fcTL", fctl(1, 20, 10)))
        .chain(z1.chunks(500).enumerate().map(|(i, c)| (b"fdAT", fd(2 + i as u32, c))))
        .collect();
    roundtrip(&build(ok.clone()));
    // fdAT of 0..=3 bytes (no sequence number, or a number alone).
    for n in 0..=4 {
        let mut f = vec![(b"fcTL", fctl(1, 20, 10))];
        f.push((b"fdAT", vec![0, 0, 0, 2][..n].to_vec()));
        f.extend(z1.chunks(500).enumerate().map(|(i, c)| (b"fdAT", fd(3 + i as u32, c))));
        roundtrip(&build(f));
    }
    // fdAT without a preceding fcTL.
    roundtrip(&build(ok[1..].to_vec()));
    // fcTL of other lengths.
    for n in [0, 4, 25, 27, 40] {
        let mut d = fctl(1, 20, 10);
        d.resize(n, 7);
        let mut f = ok.clone();
        f[0] = (b"fcTL", d);
        roundtrip(&build(f));
    }
    // fcTL claiming a frame larger than the image, or of no area.
    for (fw, fh) in [(0, 0), (0, 10), (10, 0), (41, 30), (u32::MAX, u32::MAX), (1 << 31, 2)] {
        let mut f = ok.clone();
        f[0] = (b"fcTL", fctl(1, fw, fh));
        roundtrip(&build(f));
    }
}
