//! The formula module: files through `formula::encode`, `Formula::eval` and
//! `formula::decode`, over the inputs of the round trip tests.

mod common;

use aqr_formula::Formula;
use aqr_png::formula;
use common::*;

/// The formula of `file` (which must be coded), evaluated on several thread counts.
#[track_caller]
fn roundtrip(file: &[u8]) -> Formula {
    let f = formula::encode(file, 1).expect("coded");
    assert_eq!(f.len(), file.len() as u64);
    for t in [1, 2, 3, 8] {
        assert!(f.eval(&modules(t)).expect("evaluates") == file, "round trip differs with {t} threads");
        let g = formula::encode(file, t).expect("coded");
        assert!(g == f, "formula differs with {t} threads");
    }
    f
}

/// The first byte of each record of the formula of an image file.
fn record_kinds(f: &Formula) -> Vec<u8> {
    let (index, records) = (&f.streams[1].data, &f.streams[2].data);
    let (mut at, mut from) = (0, 0);
    let mut kinds = vec![];
    while at < index.len() {
        let n = aqr_common::varint::get(index, &mut at).unwrap() as usize;
        kinds.push(records[from]);
        from += n;
    }
    assert_eq!(from, records.len());
    kinds
}

fn single(w: u32, h: u32, depth: u8, color: u8, interlace: u8, z: &[u8], trailer: &[u8]) -> Vec<u8> {
    let mut cs = vec![(b"IHDR", ihdr(w, h, depth, color, interlace))];
    if color == 3 {
        cs.push((b"PLTE", (0..768).map(|i| (i * 7) as u8).collect()));
    }
    cs.extend(idats(z, 8192));
    cs.push((b"IEND", vec![]));
    let mut f = png(&cs);
    f.extend_from_slice(trailer);
    f
}

#[test]
fn every_format() {
    for (depth, color) in [(1, 0), (4, 0), (8, 0), (16, 0), (8, 2), (16, 2), (4, 3), (8, 3), (8, 4), (8, 6), (16, 6)] {
        for interlace in [0, 1] {
            let data = image_data(137, 99, depth, color, interlace == 1, depth as u32 * 7 + color as u32);
            let z = zlib(&zlib6(), &data);
            // Some formats are not worth coding at this size.
            let file = single(137, 99, depth, color, interlace, &z, b"");
            if let Some(f) = formula::encode(&file, 1) {
                assert!(f.eval(&modules(1)).expect("evaluates") == file);
            }
        }
    }
}

#[test]
fn every_encoder() {
    let data = image_data(61, 40, 8, 2, false, 1);
    for b in backends() {
        for enc in presets_of(*b, Sweep::Default) {
            let f = single(61, 40, 8, 2, 0, &zlib(&enc, &data), b"");
            // Small images may not be worth coding with every encoder.
            if let Some(g) = formula::encode(&f, 1) {
                assert!(g.eval(&modules(1)).expect("evaluates") == f);
            }
        }
    }
}

#[test]
fn skeleton_and_slots() {
    let data = image_data(300, 200, 8, 6, false, 3);
    let file = single(300, 200, 8, 6, 0, &zlib(&zlib6(), &data), b"trailing bytes");
    let f = roundtrip(&file);
    let classes: Vec<_> = f.streams.iter().map(|s| &*s.class).collect();
    assert_eq!(classes, ["png/skeleton", "png/index", "png/records"]);
    assert_eq!(record_kinds(&f), [1]);
    assert!(f.streams[2].data.len() < file.len());
    assert!(f.streams[0].data.ends_with(b"trailing bytes"));
}

#[test]
fn several_zlib_streams() {
    let data = image_data(300, 200, 8, 6, false, 4);
    let cut: Vec<usize> = (0..=3).map(|k| data.len() / 3 * k).collect();
    let mut z = vec![];
    for w in cut.windows(2) {
        z.extend_from_slice(&zlib(&zlib6(), &data[w[0]..w[1]]));
    }
    roundtrip(&single(300, 200, 8, 6, 0, &z, b""));
}

#[test]
fn apng_frames() {
    let f = roundtrip(&apng(&[(300, 200), (250, 180), (300, 200), (120, 100)], 21));
    assert_eq!(record_kinds(&f), [1, 1, 1, 1]);
    roundtrip(&apng(&[(500, 400), (500, 400), (400, 300)], 31));
}

#[test]
fn kept_image_among_coded() {
    // A frame whose data is no zlib stream is kept as it is, beside coded images.
    let file = apng(&[(300, 200), (250, 180)], 41);
    let mut cs: Vec<(Vec<u8>, Vec<u8>)> = vec![];
    let mut at = 8;
    while at < file.len() {
        let n = u32::from_be_bytes(file[at..at + 4].try_into().unwrap()) as usize;
        cs.push((file[at + 4..at + 8].to_vec(), file[at + 8..at + 8 + n].to_vec()));
        at += n + 12;
    }
    // The frames' chunks end before IEND; a third frame of junk goes there.
    cs.pop();
    let mut fctl = 100u32.to_be_bytes().to_vec();
    for v in [200u32, 150, 0, 0] {
        fctl.extend_from_slice(&v.to_be_bytes());
    }
    fctl.extend_from_slice(&[0, 1, 0, 10, 0, 0]);
    cs.push((b"fcTL".to_vec(), fctl));
    let mut junk = 101u32.to_be_bytes().to_vec();
    junk.extend((0..3000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8));
    cs.push((b"fdAT".to_vec(), junk));
    cs.push((b"IEND".to_vec(), vec![]));
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    for (k, d) in &cs {
        chunk(&mut out, k.as_slice().try_into().unwrap(), d);
    }
    let f = roundtrip(&out);
    assert_eq!(record_kinds(&f), [1, 1, 0]);
}

#[test]
fn not_coded() {
    // Not PNG files, files cut short, and a PNG whose image cannot be coded give None.
    assert!(formula::encode(b"", 1).is_none());
    assert!(formula::encode(b"not a png at all", 1).is_none());
    let data = image_data(100, 80, 8, 2, false, 6);
    let file = single(100, 80, 8, 2, 0, &zlib(&zlib6(), &data), b"");
    for cut in [0, 5, 8, 20, 40] {
        assert!(formula::encode(&file[..cut], 1).is_none(), "cut at {cut}");
    }
    let mut broken = zlib(&zlib6(), &data);
    broken[100] ^= 0x55;
    broken.truncate(300);
    assert!(formula::encode(&single(100, 80, 8, 2, 0, &broken, b""), 1).is_none());
}

#[test]
fn malformed_files() {
    // Whatever encodes round trips: junk after the stream, a wrong CRC, a short image.
    let data = image_data(100, 80, 8, 2, false, 6);
    let z = zlib(&zlib6(), &data);
    let mut junk = z.clone();
    junk.extend_from_slice(b"junk after the stream");
    let mut files = vec![single(100, 80, 8, 2, 0, &junk, b"")];
    let mut bad_crc = single(100, 80, 8, 2, 0, &z, b"");
    let n = bad_crc.len();
    bad_crc[n - 20] ^= 1;
    files.push(bad_crc);
    files.push(single(100, 80, 8, 2, 0, &zlib(&zlib6(), &data[..data.len() / 2]), b"trailer"));
    for f in files {
        let g = formula::encode(&f, 1).expect("coded");
        assert!(g.eval(&modules(1)).expect("evaluates") == f);
    }
}

#[test]
fn corrupt_inputs_fail_cleanly() {
    let data = image_data(120, 90, 8, 2, false, 7);
    let file = single(120, 90, 8, 2, 0, &zlib(&zlib6(), &data), b"");
    let f = formula::encode(&file, 1).unwrap();
    let inputs: Vec<&[u8]> = f.streams.iter().map(|s| &s.data[..]).collect();
    let len = file.len() as u64;
    assert!(formula::decode(&[], &inputs, len, 1).unwrap() == file);
    // Wrong parameters, input count, length.
    assert!(formula::decode(&[0], &inputs, len, 1).is_err());
    assert!(formula::decode(&[], &inputs[..2], len, 1).is_err());
    assert!(formula::decode(&[], &inputs, len + 1, 1).is_err());
    // Damaged inputs: never a panic.
    let mut x = 1u32;
    for i in 0..1500 {
        x = x.wrapping_mul(1103515245).wrapping_add(12345);
        let mut copies: Vec<Vec<u8>> = inputs.iter().map(|s| s.to_vec()).collect();
        // The skeleton is left alone: a damaged IHDR can ask for an image of terabytes.
        let k = 1 + i % 2;
        let at = (x as usize >> 4) % copies[k].len();
        copies[k][at] ^= 1 << (i % 8);
        let refs: Vec<&[u8]> = copies.iter().map(|c| &c[..]).collect();
        let _ = formula::decode(&[], &refs, len, 1);
    }
    for cut in 0..inputs[1].len() {
        let _ = formula::decode(&[], &[inputs[0], &inputs[1][..cut], inputs[2]], len, 1);
    }
}
