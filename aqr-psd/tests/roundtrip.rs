//! Round trips of synthetic PSD files built here, so that the tests need no corpus.

mod common;

use aqr_psd::packbits::{self, Encoder};
use common::*;

/// The version of a container (0 stored, 1 coded), after its magic.
fn version(packed: &[u8]) -> u16 {
    assert_eq!(packed[..6], [0x69, 0x04, 0x7f, 0xfe, 0x9b, 0x1d]);
    u16::from_le_bytes([packed[6], packed[7]])
}

fn check(file: &[u8]) {
    let packed = aqr_psd::compress(file);
    assert_eq!(version(&packed), 1, "not coded");
    assert!(packed.len() < file.len(), "{} >= {}", packed.len(), file.len());
    assert_eq!(aqr_psd::decompress(&packed).unwrap(), file);
}

#[test]
fn photoshop_rle() {
    check(&psd(300, 40, &layers(300, 40, true), packbits::NO_TILES, false, &[]));
}

#[test]
fn tiles_and_exceptions() {
    let enc = Encoder::Photoshop { phase: 77, period: 368 };
    check(&psd(500, 30, &layers(500, 30, true), enc, true, &[]));
}

#[test]
fn greedy_writer_and_raw_layer() {
    check(&psd(260, 35, &layers(260, 35, false), Encoder::Greedy { min_run: 3, split_rest: true }, false, &[0]));
}

#[test]
fn not_a_psd_is_stored() {
    let junk: Vec<u8> = (0..5000u32).map(|i| (i * 7 % 13) as u8).collect();
    let packed = aqr_psd::compress(&junk);
    assert_eq!(version(&packed), 0);
    assert_eq!(aqr_psd::decompress(&packed).unwrap(), junk);
}

#[test]
fn corrupt_input_is_an_error() {
    let file = psd(300, 40, &layers(300, 40, true), packbits::NO_TILES, false, &[]);
    let packed = aqr_psd::compress(&file);
    for cut in [7, 20, packed.len() / 2, packed.len() - 1] {
        assert!(aqr_psd::decompress(&packed[..cut]).is_err());
    }
    let mut flipped = packed.clone();
    let k = flipped.len() - 10;
    flipped[k] ^= 0x40;
    assert!(aqr_psd::decompress(&flipped).is_err());
    // Bytes after the last stream, another version.
    let mut longer = packed.clone();
    longer.push(0);
    assert!(aqr_psd::decompress(&longer).is_err());
    let mut other = packed.clone();
    other[6] = 2;
    assert!(aqr_psd::decompress(&other).is_err());
}

#[test]
fn duplicate_layers_and_gray_layer() {
    let (w, h) = (200, 30);
    let mut ls = layers(w, h, true);
    // A copy of the first layer, moved; and a gray layer (equal color planes).
    let mut copy = Layer { top: 7, left: 2, w, h, channels: ls[0].channels.clone(), mask: None };
    copy.channels.swap(1, 2);
    ls.push(copy);
    let g = image(w, h, 99);
    ls.push(Layer { top: 0, left: 0, w, h, channels: vec![(0, g.clone()), (1, g.clone()), (2, g)], mask: None });
    let file = psd(w, h, &ls, packbits::NO_TILES, false, &[]);
    check(&file);
    // Copies cost next to nothing.
    let single = psd(w, h, &ls[..2], packbits::NO_TILES, false, &[]);
    let (a, b) = (aqr_psd::compress(&file).len(), aqr_psd::compress(&single).len());
    assert!(a < b + b / 4, "{a} vs {b}");
}

#[test]
fn same_bytes_other_size_is_no_copy() {
    // Raw channels with equal bytes but different sizes (100 x 2 and 50 x 4).
    let data = image(100, 2, 5);
    let l = |w, h| Layer {
        top: 0,
        left: 0,
        w,
        h,
        channels: vec![(0, data.clone()), (1, data.clone()), (2, data.clone())],
        mask: None,
    };
    check(&psd(100, 4, &[l(100, 2), l(50, 4)], packbits::NO_TILES, false, &[0, 1]));
}

#[test]
fn layer_outside_the_canvas() {
    // Layers right of, below and left of the canvas; the composite is rendered as hint.
    let (w, h) = (120, 20);
    let mut ls = layers(w, h, true);
    for (k, (top, left)) in [(0, 500), (300, 0), (0, -400)].into_iter().enumerate() {
        ls.push(Layer {
            top,
            left,
            w: 50,
            h: 10,
            channels: ls[k % 2].channels.iter().map(|(id, d)| (*id, d[..500].to_vec())).collect(),
            mask: None,
        });
    }
    // A composite the renderer predicts: the hint is used.
    check(&psd(w, h, &ls, packbits::NO_TILES, false, &[]));
}

#[test]
fn masks_anywhere() {
    // User masks left of, right of, inside and around their layers.
    let (w, h) = (100, 16);
    let mut ls = layers(w, h, true);
    for (k, (mtop, mleft, mw, mh, def)) in
        [(0, -300, 40, 8, 0), (2, 500, 30, 4, 255), (3, 10, 20, 6, 0), (-5, -5, 200, 40, 255)].into_iter().enumerate()
    {
        let mut channels = ls[k % 2].channels.clone();
        channels.push((-2, image(mw, mh, 50 + k as u32)));
        ls.push(Layer { top: k as i32, left: 3, w, h, channels, mask: Some((mtop, mleft, mw, mh, def)) });
    }
    check(&psd(w, h, &ls, packbits::NO_TILES, false, &[]));
}

#[test]
fn repeated_rows_with_exceptions() {
    // Identical rows, every third written as literals only (an exception): repeated rows must
    // not reuse an exception's bytes, nor skip one.
    let (w, h) = (300, 24);
    let row: Vec<u8> = (0..w).map(|x| if x < 100 { 7 } else { (x * 13 % 256) as u8 }).collect();
    let plane: Vec<u8> = row.iter().copied().cycle().take(w * h).collect();
    let l =
        Layer { top: 0, left: 0, w, h, channels: vec![(0, plane.clone()), (1, plane.clone()), (2, plane)], mask: None };
    check(&psd(w, h, &[l], packbits::NO_TILES, true, &[]));
}

const THREADS: [usize; 4] = [1, 2, 3, 8];

/// The same bytes whatever the threads, and each decodes back (to `file`) with each.
fn check_threads(file: &[u8]) {
    let one = aqr_psd::compress(file);
    for t in THREADS {
        let packed = aqr_psd::compress_with(file, &aqr_psd::Options { threads: t });
        assert!(packed == one, "compress differs with {t} threads");
        for d in THREADS {
            assert!(aqr_psd::decompress_threads(&packed, d).unwrap() == file, "decompress {d} threads");
        }
    }
}

/// Outcomes (bytes or error) of decompressing `packed` agree across thread counts.
fn same_outcome(packed: &[u8]) {
    let one = aqr_psd::decompress(packed).ok();
    for t in THREADS {
        assert!(aqr_psd::decompress_threads(packed, t).ok() == one, "outcome differs with {t} threads");
    }
}

#[test]
fn threads_small_and_large_groups() {
    check_threads(&psd(300, 40, &layers(300, 40, true), packbits::NO_TILES, false, &[]));
    check_threads(&psd(
        260,
        35,
        &layers(260, 35, false),
        Encoder::Greedy { min_run: 3, split_rest: true },
        false,
        &[0],
    ));
    // Past the size where rows are pipelined through helper threads.
    let enc = Encoder::Photoshop { phase: 77, period: 368 };
    check_threads(&psd(800, 400, &layers(800, 400, true), enc, true, &[]));
    check_threads(&psd(700, 500, &layers(700, 500, false), packbits::NO_TILES, false, &[1]));
}

#[test]
fn threads_corrupt_input() {
    for file in [
        psd(300, 40, &layers(300, 40, true), packbits::NO_TILES, false, &[]),
        psd(800, 400, &layers(800, 400, true), packbits::NO_TILES, true, &[]),
    ] {
        let packed = aqr_psd::compress(&file);
        for cut in [7, 20, packed.len() / 2, packed.len() - 1] {
            same_outcome(&packed[..cut]);
            assert!(aqr_psd::decompress_threads(&packed[..cut], 8).is_err());
        }
        for at in [10, packed.len() / 3, packed.len() / 2, packed.len() - 40, packed.len() - 10] {
            let mut flipped = packed.clone();
            flipped[at] ^= 0x40;
            same_outcome(&flipped);
        }
    }
}

/// Corpus files with embedded files (smart objects), when the corpus is there.
#[test]
fn threads_embedded_files() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus");
    let files = [
        "fixtures/psd-tools_psd-tools/tests/psd_files/placedLayer.psd",
        "fixtures/Agamnentzar_ag-psd/test/read/smart-object-curves/src.psd",
        "oga/Toast--Toast.psd",
    ];
    for f in files {
        let Ok(file) = std::fs::read(root.join(f)) else { continue };
        check_threads(&file);
        let packed = aqr_psd::compress(&file);
        for at in [packed.len() / 4, packed.len() / 2, packed.len() - 20] {
            let mut flipped = packed.clone();
            flipped[at] ^= 0x10;
            same_outcome(&flipped);
        }
        same_outcome(&packed[..packed.len() * 3 / 4]);
    }
}

fn get_varint(src: &[u8], at: &mut usize) -> usize {
    let mut v = 0;
    for shift in (0..).step_by(7) {
        let b = src[*at];
        *at += 1;
        v |= ((b & 0x7f) as usize) << shift;
        if b < 0x80 {
            return v;
        }
    }
    unreachable!()
}

fn put_varint(out: &mut Vec<u8>, mut v: usize) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

#[test]
fn hint_needs_8_bit_rgb() {
    let file = hinted_file(120, 20);
    check(&file);
    // The same container with the document's depth (in the skeleton) made 16: an error, not
    // a renderer reading 8-bit rows as 16-bit ones.
    let packed = aqr_psd::compress(&file);
    let (mut at, mut streams) = (8, Vec::new());
    while at < packed.len() {
        let n = get_varint(&packed, &mut at);
        streams.push(packed[at..at + n].to_vec());
        at += n;
    }
    let mut sk = zstd::decode_all(&streams[1][..]).unwrap();
    sk[22..24].copy_from_slice(&16u16.to_be_bytes());
    streams[1] = zstd::encode_all(&sk[..], 3).unwrap();
    let mut bad = packed[..8].to_vec();
    for s in &streams {
        put_varint(&mut bad, s.len());
        bad.extend_from_slice(s);
    }
    assert!(aqr_psd::decompress(&bad).is_err());
}
