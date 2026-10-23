//! Round trips through the formula module: `formula::encode`, evaluated with the modules of
//! aqr-psd and of the formats it embeds.

mod common;

use aqr_formula::{Apply, Formula};
use aqr_psd::formula;
use aqr_psd::packbits::{self, Encoder};
use common::*;

/// The modules of every format a formula of aqr-psd uses.
fn modules(threads: usize) -> impl Fn(&Apply, &[&[u8]]) -> aqr_formula::Result<Vec<u8>> {
    move |a, inputs| {
        let fail = |e: String| aqr_formula::Error::new(e);
        match a.module.name {
            "psd-recons" => formula::decode(&a.params, inputs, a.len, threads).map_err(|e| fail(e.to_string())),
            "png-recons" => {
                aqr_png::formula::decode(&a.params, inputs, a.len, threads).map_err(|e| fail(format!("{e:?}")))
            }
            "jpeg-recons" => {
                aqr_jpeg::formula::decode(&a.params, inputs, a.len, threads).map_err(|e| fail(format!("{e:?}")))
            }
            other => Err(fail(format!("unknown module {other}"))),
        }
    }
}

/// Names of the modules applied in `f`, parents first.
fn applied(f: &Formula) -> Vec<&'static str> {
    f.applies().iter().map(|a| a.module.name).collect()
}

/// The formula of `file`, which evaluates to it.
fn check(file: &[u8]) -> Formula {
    let f = formula::encode(file, 1).expect("not coded");
    assert_eq!(f.len(), file.len() as u64);
    let size: usize = f.streams.iter().map(|s| s.data.len()).sum();
    assert!(size < file.len(), "{size} >= {}", file.len());
    for threads in [1, 3] {
        assert_eq!(f.eval(&modules(threads)).unwrap(), file, "{threads} threads");
    }
    // The threads of the encoder make no difference.
    assert_eq!(formula::encode(file, 3).as_ref(), Some(&f));
    f
}

#[test]
fn photoshop_rle() {
    check(&psd(300, 40, &layers(300, 40, true), packbits::NO_TILES, 0, &[]));
}

#[test]
fn tiles_and_exceptions() {
    let enc = Encoder::Photoshop { phase: 77, period: 368 };
    check(&psd(500, 30, &layers(500, 30, true), enc, 3, &[]));
}

#[test]
fn unknown_writer() {
    // No encoder reproduces the layers' rows: they stay in the skeleton rather than being
    // coded and stored verbatim as exceptions as well; the composite is still coded.
    let file = psd(300, 40, &layers(300, 40, true), packbits::NO_TILES, 1, &[]);
    let f = check(&file);
    let head = f.streams.iter().find(|s| s.class == "psd/head").unwrap().data.len();
    assert!(head < 200, "head of {head} bytes");
}

#[test]
fn greedy_writer_and_raw_layer() {
    check(&psd(260, 35, &layers(260, 35, false), Encoder::Greedy { min_run: 3, split_rest: true }, 0, &[0]));
}

#[test]
fn copies_and_a_gray_layer() {
    let (w, h) = (200, 30);
    let mut ls = layers(w, h, true);
    let mut copy = Layer { top: 7, left: 2, w, h, channels: ls[0].channels.clone(), mask: None };
    copy.channels.swap(1, 2);
    ls.push(copy);
    let g = image(w, h, 99);
    ls.push(Layer { top: 0, left: 0, w, h, channels: vec![(0, g.clone()), (1, g.clone()), (2, g)], mask: None });
    check(&psd(w, h, &ls, packbits::NO_TILES, 0, &[]));
}

#[test]
fn hinted_composite() {
    check(&hinted_file(120, 20));
}

#[test]
fn duplicate_layers_cost_next_to_nothing() {
    let (w, h) = (200, 30);
    let mut ls = layers(w, h, true);
    let mut copy = Layer { top: 7, left: 2, w, h, channels: ls[0].channels.clone(), mask: None };
    copy.channels.swap(1, 2);
    let single = psd(w, h, &ls[..2], packbits::NO_TILES, 0, &[]);
    ls.push(copy);
    let size = |f: &Formula| f.streams.iter().map(|s| s.data.len()).sum::<usize>();
    let (a, b) = (size(&check(&psd(w, h, &ls, packbits::NO_TILES, 0, &[]))), size(&check(&single)));
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
    check(&psd(100, 4, &[l(100, 2), l(50, 4)], packbits::NO_TILES, 0, &[0, 1]));
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
    check(&psd(w, h, &ls, packbits::NO_TILES, 0, &[]));
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
    check(&psd(w, h, &ls, packbits::NO_TILES, 0, &[]));
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
    check(&psd(w, h, &[l], packbits::NO_TILES, 3, &[]));
}

const THREADS: [usize; 4] = [1, 2, 3, 8];

/// The same formula whatever the threads of the encoder, and each decodes back (to `file`)
/// with each of the decoder.
fn check_threads(file: &[u8]) {
    let one = formula::encode(file, 1).expect("not coded");
    for t in THREADS {
        assert!(formula::encode(file, t).as_ref() == Some(&one), "encode differs with {t} threads");
        for d in THREADS {
            assert!(one.eval(&modules(d)).unwrap() == file, "decode with {d} threads");
        }
    }
}

/// Outcomes (bytes or error) of evaluating `f` agree across thread counts.
fn same_outcome(f: &Formula) {
    let one = f.eval(&modules(1)).ok();
    for t in THREADS {
        assert!(f.eval(&modules(t)).ok() == one, "outcome differs with {t} threads");
    }
}

#[test]
fn threads_small_and_large_groups() {
    check_threads(&psd(300, 40, &layers(300, 40, true), packbits::NO_TILES, 0, &[]));
    check_threads(&psd(260, 35, &layers(260, 35, false), Encoder::Greedy { min_run: 3, split_rest: true }, 0, &[0]));
    // Past the size where rows are pipelined through helper threads.
    let enc = Encoder::Photoshop { phase: 77, period: 368 };
    check_threads(&psd(800, 400, &layers(800, 400, true), enc, 3, &[]));
    check_threads(&psd(700, 500, &layers(700, 500, false), packbits::NO_TILES, 0, &[1]));
}

#[test]
fn threads_corrupt_streams() {
    for file in [
        psd(300, 40, &layers(300, 40, true), packbits::NO_TILES, 0, &[]),
        psd(800, 400, &layers(800, 400, true), packbits::NO_TILES, 3, &[]),
    ] {
        let f = formula::encode(&file, 1).unwrap();
        for (i, s) in f.streams.iter().enumerate() {
            for at in [0, s.data.len() / 3, s.data.len() / 2, s.data.len().saturating_sub(10)] {
                let mut g = f.clone();
                g.streams[i].data.truncate(at);
                same_outcome(&g);
                let mut g = f.clone();
                if let Some(b) = g.streams[i].data.get_mut(at) {
                    *b ^= 0x40;
                }
                same_outcome(&g);
            }
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
        let formula = formula::encode(&file, 1).unwrap();
        for (i, s) in formula.streams.iter().enumerate().filter(|(_, s)| s.class.starts_with("psd/")) {
            for at in [s.data.len() / 4, s.data.len() / 2, s.data.len().saturating_sub(20)] {
                let mut g = formula.clone();
                if let Some(b) = g.streams[i].data.get_mut(at) {
                    *b ^= 0x10;
                }
                same_outcome(&g);
            }
        }
    }
}

#[test]
fn hint_needs_8_bit_rgb() {
    let mut f = check(&hinted_file(120, 20));
    // The document's depth (in the skeleton) made 16: an error, not a renderer reading 8-bit
    // rows as 16-bit ones.
    let sk = f.streams.iter_mut().find(|s| s.class == "psd/skeleton").unwrap();
    sk.data[22..24].copy_from_slice(&16u16.to_be_bytes());
    assert!(f.eval(&modules(1)).is_err());
}

#[test]
fn large_group_uses_threads() {
    // Big enough for the pipelined decoder.
    check(&psd(700, 400, &layers(700, 400, true), packbits::NO_TILES, 0, &[]));
}

#[test]
fn stored_cases_give_none() {
    assert!(formula::encode(b"", 1).is_none());
    assert!(formula::encode(b"not a psd file at all", 1).is_none());
    let mut truncated = psd(100, 20, &layers(100, 20, false), packbits::NO_TILES, 0, &[]);
    truncated.truncate(truncated.len() / 2);
    assert!(formula::encode(&truncated, 1).is_none());
    // 16 bits per channel: no pixel data to take out.
    let mut deep = psd(100, 20, &layers(100, 20, false), packbits::NO_TILES, 0, &[]);
    deep[22..24].copy_from_slice(&16u16.to_be_bytes());
    assert!(formula::encode(&deep, 1).is_none());
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xedb88320 ^ c >> 1 } else { c >> 1 };
        }
    }
    !c
}

/// A PNG of a smooth RGB image, its zlib stream stored.
fn png(w: usize, h: usize) -> Vec<u8> {
    let mut raw = Vec::new();
    for y in 0..h {
        raw.push(0);
        for x in 0..w {
            raw.extend_from_slice(&[(x * 2 + y) as u8, (x + y * 3) as u8, (x ^ y) as u8 / 2 + 60]);
        }
    }
    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(60000).collect();
    for (i, b) in blocks.iter().enumerate() {
        z.push((i + 1 == blocks.len()) as u8);
        z.extend_from_slice(&(b.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(b.len() as u16)).to_le_bytes());
        z.extend_from_slice(b);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&(b << 16 | a).to_be_bytes());
    let mut out = vec![0x89, b'P', b'N', b'G', 13, 10, 26, 10];
    let mut chunk = |kind: &[u8; 4], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    };
    let mut ihdr = (w as u32).to_be_bytes().to_vec();
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    out
}

const JPEG: &[u8] = include_bytes!("../../aqr-jpeg/tests/data/gradient.jpg");

/// `file` with a `lnk2` block at the end of its layer and mask information, holding the
/// embedded `files` (file type, bytes).
fn with_linked(file: &[u8], files: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
    let be32 = |at: usize| u32::from_be_bytes(file[at..at + 4].try_into().unwrap()) as usize;
    let m_len_at = 34 + be32(30);
    let m_end = m_len_at + 4 + be32(m_len_at);
    let mut data = Vec::new();
    for (ty, bytes) in files {
        let mut e = b"liFD".to_vec();
        e.extend_from_slice(&1u32.to_be_bytes());
        e.extend_from_slice(&[4, b'a', b'b', b'c', b'd']);
        e.extend_from_slice(&3u32.to_be_bytes());
        e.extend_from_slice(&[0, b'x', 0, b'y', 0, b'z']);
        e.extend_from_slice(*ty);
        e.extend_from_slice(b"\0\0\0\0");
        e.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
        e.extend_from_slice(bytes);
        data.extend_from_slice(&(e.len() as u64).to_be_bytes());
        data.extend_from_slice(&e);
        while data.len() % 4 != 0 {
            data.push(0);
        }
    }
    let mut tag = b"8BIMlnk2".to_vec();
    tag.extend_from_slice(&(data.len() as u32).to_be_bytes());
    tag.extend_from_slice(&data);
    let mut out = file[..m_end].to_vec();
    out.extend_from_slice(&tag);
    out.extend_from_slice(&file[m_end..]);
    let new_len = (m_end - m_len_at - 4 + tag.len()) as u32;
    out[m_len_at..m_len_at + 4].copy_from_slice(&new_len.to_be_bytes());
    out
}

fn small_psd(seed: usize) -> Vec<u8> {
    psd(150 + seed, 30, &layers(150 + seed, 30, true), packbits::NO_TILES, 0, &[])
}

#[test]
fn embedded_png_jpeg_and_psd() {
    let (png, inner) = (png(120, 90), small_psd(1));
    assert!(aqr_png::formula::encode(&png, 1).is_some(), "the fixture PNG is not coded");
    let file = with_linked(&small_psd(0), &[(b"png ", &png), (b"JPEG", JPEG), (b"8BPS", &inner)]);
    let f = check(&file);
    let mods = applied(&f);
    assert_eq!(mods[0], "psd-recons");
    for m in ["png-recons", "jpeg-recons"] {
        assert_eq!(mods.iter().filter(|&&n| n == m).count(), 1, "{mods:?}");
    }
    assert_eq!(mods.iter().filter(|&&n| n == "psd-recons").count(), 2, "{mods:?}");
    // The embedded files are an input of the module, in the order of the file.
    let root = f.applies()[0];
    assert_eq!(root.inputs.len(), 4);
    assert_eq!(root.inputs[2].len(), (png.len() + JPEG.len() + inner.len()) as u64);
}

#[test]
fn embedded_files_that_do_not_pay_stay_in_the_skeleton() {
    // A PNG that is too small to gain anything.
    let file = with_linked(&small_psd(0), &[(b"png ", &png(4, 2))]);
    let f = check(&file);
    assert_eq!(applied(&f), ["psd-recons"]);
}

#[test]
fn embedding_stops_at_depth_three() {
    let mut file = small_psd(0);
    for k in 1..6 {
        file = with_linked(&small_psd(k), &[(b"8BPS", &file)]);
    }
    let f = check(&file);
    // The outermost file and three levels below it.
    assert_eq!(applied(&f), ["psd-recons"; 4]);
}

#[test]
fn corrupt_streams_are_errors_not_panics() {
    let file = with_linked(&small_psd(0), &[(b"png ", &png(120, 90)), (b"8BPS", &small_psd(1))]);
    let f = formula::encode(&file, 1).unwrap();
    let a = f.applies()[0].clone();
    let eval = |a: &Apply| {
        let mut g = f.clone();
        g.root = aqr_formula::Node::Apply(Box::new(a.clone()));
        g.eval(&modules(1))
    };
    assert!(eval(&a).is_ok());
    // A wrong length, wrong parameters, a missing input.
    assert!(eval(&Apply { len: a.len + 1, ..a.clone() }).is_err());
    assert!(eval(&Apply { params: vec![0], ..a.clone() }).is_err());
    assert!(eval(&Apply { inputs: a.inputs[..3].to_vec(), ..a.clone() }).is_err());
    // Damaged streams of this module (the children's are theirs to test): whatever comes
    // out, nothing panics.
    for (i, s) in f.streams.iter().enumerate().filter(|(_, s)| s.class.starts_with("psd/")) {
        for at in (0..s.data.len()).step_by(s.data.len() / 40 + 1) {
            let mut g = f.clone();
            g.streams[i].data[at] ^= 0x21;
            let _ = g.eval(&modules(1));
            let mut g = f.clone();
            g.streams[i].data.truncate(at);
            let _ = g.eval(&modules(1));
        }
    }
}
