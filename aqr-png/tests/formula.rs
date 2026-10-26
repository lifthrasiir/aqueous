//! The formula module: files through `formula::encode`, `Formula::eval` and
//! `formula::decode`, over the inputs of the round trip tests.

mod common;

use aqr_formula::{Formula, Node};
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

/// A PNG file of a picture (8 bits per sample, `color`), with the pixels of `rect` changed.
fn picture(w: usize, h: usize, color: u8, seed: u32, rect: Option<(usize, usize, usize, usize)>) -> Vec<u8> {
    let ch = match color {
        2 => 3,
        4 => 2,
        6 => 4,
        _ => 1,
    };
    let data = filter_rows(&raw_rows(w, h, ch, seed, rect), ch);
    single(w as u32, h as u32, 8, color, 0, &zlib(&zlib6(), &data), b"")
}

/// The records' bytes of a formula.
fn records(f: &Formula) -> usize {
    f.streams.iter().find(|s| s.class == "png/records").unwrap().data.len()
}

/// A variant of a picture coded against it: records of kind 2, far smaller than alone, the
/// reference the last input; the same on any thread count, and evaluated back.
#[test]
fn against_reference() {
    for color in [0, 2, 3, 4, 6] {
        let base = picture(300, 200, color, 5, None);
        let variant = picture(300, 200, color, 5, Some((40, 60, 70, 50)));
        let f = formula::encode_against(&variant, &base, 1).expect("coded");
        assert_eq!(record_kinds(&f), [2], "colour {color}");
        let classes: Vec<_> = f.streams.iter().map(|s| &*s.class).collect();
        assert_eq!(classes, ["png/skeleton", "png/index", "png/records", formula::REFERENCE]);
        assert_eq!(f.streams[3].data, base);
        let alone = formula::encode(&variant, 1).unwrap();
        assert!(records(&f) * 3 < records(&alone), "colour {color}: {} against {}", records(&f), records(&alone));
        for t in [1, 2, 8] {
            assert!(f.eval(&modules(t)).expect("evaluates") == variant, "colour {color}, {t} threads");
            assert!(formula::encode_against(&variant, &base, t).unwrap() == f, "colour {color}, {t} threads");
        }
        // The same picture costs next to nothing.
        let same = formula::encode_against(&base, &base, 1).unwrap();
        assert!(records(&same) * 20 < records(&alone), "colour {color}: {}", records(&same));
        assert!(same.eval(&modules(1)).unwrap() == base);
    }
}

/// Against a reference of another geometry, or not a PNG file, or with nothing alike, the
/// images are coded alone and the reference is no input.
#[test]
fn reference_not_taken() {
    let variant = picture(300, 200, 2, 5, Some((40, 60, 70, 50)));
    for reference in [picture(300, 201, 2, 5, None), picture(300, 200, 6, 5, None), b"not a png".to_vec()] {
        let f = formula::encode_against(&variant, &reference, 1).unwrap();
        assert_eq!(record_kinds(&f), [1]);
        assert_eq!(f.streams.len(), 3);
        assert!(f == formula::encode(&variant, 1).unwrap());
    }
}

/// Frames of an APNG file are coded against the reference's frames of the same index.
#[test]
fn reference_frames() {
    let base = apng(&[(300, 200), (250, 180), (300, 200)], 21);
    let f = formula::encode_against(&base, &base, 1).unwrap();
    assert_eq!(record_kinds(&f), [2, 2, 2]);
    assert!(f.eval(&modules(1)).unwrap() == base);
    // A frame of another size than the reference's of its index is coded alone.
    let other = apng(&[(300, 200), (240, 180), (300, 200)], 21);
    let f = formula::encode_against(&other, &base, 1).unwrap();
    assert_eq!(record_kinds(&f)[1], 1);
    assert!(f.eval(&modules(1)).unwrap() == other);
}

/// Records coded against a reference need it: without it decoding fails, with another or
/// with damaged inputs it never panics.
#[test]
fn reference_inputs_fail_cleanly() {
    let base = picture(200, 150, 6, 9, None);
    let variant = picture(200, 150, 6, 9, Some((10, 20, 60, 40)));
    let f = formula::encode_against(&variant, &base, 1).unwrap();
    let inputs: Vec<&[u8]> = f.streams.iter().map(|s| &s.data[..]).collect();
    let len = variant.len() as u64;
    assert!(formula::decode(&[], &inputs, len, 1).unwrap() == variant);
    assert!(formula::decode(&[], &inputs[..3], len, 1).is_err());
    assert!(formula::decode(&[], &[inputs[0], inputs[1], inputs[2], inputs[3], inputs[3]], len, 1).is_err());
    assert!(formula::decode(&[], &[inputs[0], inputs[1], inputs[2], b"not a png"], len, 1).is_err());
    let other = picture(200, 150, 6, 10, None);
    let _ = formula::decode(&[], &[inputs[0], inputs[1], inputs[2], &other], len, 1);
    let mut x = 1u32;
    for i in 0..1500 {
        x = x.wrapping_mul(1103515245).wrapping_add(12345);
        let mut copies: Vec<Vec<u8>> = inputs.iter().map(|s| s.to_vec()).collect();
        // The records, or the reference.
        let k = 2 + i % 2;
        let at = (x as usize >> 4) % copies[k].len();
        copies[k][at] ^= 1 << (i % 8);
        let refs: Vec<&[u8]> = copies.iter().map(|c| &c[..]).collect();
        let _ = formula::decode(&[], &refs, len, 1);
    }
}

/// aqr-png's modules, sets included, on `threads` threads.
struct Set(usize);

impl aqr_formula::Modules for Set {
    fn apply(&self, a: &aqr_formula::Apply, inputs: &[&[u8]]) -> aqr_formula::Result<Vec<u8>> {
        let r = match a.module {
            formula::MODULE => formula::decode(&a.params, inputs, a.len, self.0),
            _ => formula::decode_set(&a.params, inputs, &[a.len], self.0).map(|mut o| o.remove(0)),
        };
        r.map_err(|e| aqr_formula::Error::new(e.to_string()))
    }

    fn apply_shared(&self, s: &aqr_formula::Shared, inputs: &[&[u8]]) -> aqr_formula::Result<Vec<Vec<u8>>> {
        assert_eq!(s.apply.module, formula::SET_MODULE);
        formula::decode_set(&s.apply.params, inputs, &s.sizes, self.0)
            .map_err(|e| aqr_formula::Error::new(e.to_string()))
    }
}

/// Variants of a picture, each coded against the one before: an output each, far smaller than
/// coded alone, the same on any thread count, evaluated and through an archive.
#[test]
fn set_of_variants() {
    let files = [
        picture(300, 200, 6, 5, None),
        picture(300, 200, 6, 5, Some((40, 60, 70, 50))),
        picture(300, 200, 6, 5, Some((100, 20, 30, 150))),
        picture(120, 80, 2, 7, None),
    ];
    let refs: Vec<&[u8]> = files.iter().map(|f| &f[..]).collect();
    let parents = [None, Some(0), Some(0), None];
    let f = formula::encode_set(&refs, &parents, None, 1).expect("coded");
    let Node::Concat(outputs) = &f.root else { panic!() };
    assert_eq!(outputs.len(), 4);
    assert!(f.eval(&Set(1)).unwrap() == files.concat());
    // the same from the formulas of the files alone, not made again
    let alone: Vec<Formula> = files.iter().map(|d| formula::encode(d, 1).unwrap()).collect();
    assert!(formula::encode_set(&refs, &parents, Some(&alone), 1).unwrap() == f);
    for t in [2, 8] {
        assert!(formula::encode_set(&refs, &parents, None, t).unwrap() == f, "{t} threads");
        assert!(f.eval(&Set(t)).unwrap() == files.concat(), "{t} threads");
    }
    let records: usize = f.streams.iter().filter(|s| s.class == "png/records").map(|s| s.data.len()).sum();
    let alone: usize = files.iter().map(|d| records_of(&formula::encode(d, 1).unwrap())).sum();
    assert!(records * 2 < alone, "{records} against {alone}");
    // through an archive, its outputs in another order than the files'
    let mut g = f.clone();
    let Node::Concat(outputs) = &mut g.root else { panic!() };
    outputs.reverse();
    let want: Vec<u8> = files.iter().rev().flatten().copied().collect();
    let c = aqr_formula::aqr::encode(&g, None, aqr_formula::aqr::Level::Zstd(3)).unwrap();
    let natives = aqr_formula::aqr::natives(&[formula::MODULE, formula::SET_MODULE], &Set(1));
    assert!(aqr_formula::aqr::decode(&c, &natives).unwrap() == want);
    // a file that is not a PNG file gives no set
    assert!(formula::encode_set(&[&files[0], b"not a png"], &[None, Some(0)], None, 1).is_none());
}

fn records_of(f: &Formula) -> usize {
    f.streams.iter().find(|s| s.class == "png/records").unwrap().data.len()
}

/// Sets of bad parameters or inputs fail, damaged ones never panic.
#[test]
fn set_inputs_fail_cleanly() {
    let files = [picture(200, 150, 6, 9, None), picture(200, 150, 6, 9, Some((10, 20, 60, 40)))];
    let refs: Vec<&[u8]> = files.iter().map(|f| &f[..]).collect();
    let f = formula::encode_set(&refs, &[None, Some(0)], None, 1).unwrap();
    let s = &f.shared[0];
    let inputs: Vec<Vec<u8>> = s
        .apply
        .inputs
        .iter()
        .map(|n| Formula { streams: f.streams.clone(), root: n.clone(), shared: vec![] }.eval(&Set(1)).unwrap())
        .collect();
    let ins: Vec<&[u8]> = inputs.iter().map(|i| &i[..]).collect();
    let (params, sizes) = (&s.apply.params[..], &s.sizes[..]);
    assert!(formula::decode_set(params, &ins, sizes, 1).unwrap() == files);
    assert!(formula::decode_set(params, &ins, &sizes[..1], 1).is_err());
    assert!(formula::decode_set(params, &ins[..2], sizes, 1).is_err());
    // the second's parent itself, or none (its records need one)
    assert!(formula::decode_set(&[2, 0, 2], &ins, sizes, 1).is_err());
    assert!(formula::decode_set(&[2, 0, 0], &ins, sizes, 1).is_err());
    assert!(formula::decode_set(&[2, 0, 1, 0], &ins, sizes, 1).is_err());
    let mut x = 1u32;
    for i in 0..1000 {
        x = x.wrapping_mul(1103515245).wrapping_add(12345);
        let mut copies = inputs.clone();
        let k = 1 + i % 2;
        let at = (x as usize >> 4) % copies[k].len();
        copies[k][at] ^= 1 << (i % 8);
        let refs: Vec<&[u8]> = copies.iter().map(|c| &c[..]).collect();
        let _ = formula::decode_set(params, &refs, sizes, 1);
    }
}
