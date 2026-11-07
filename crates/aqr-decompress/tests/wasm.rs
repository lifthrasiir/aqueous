//! The modules as WebAssembly: those aqr-modules built, run natively and by Wasmtime alike, and
//! the profile and the host functions of the spec. Every module run by Wasmtime runs in the
//! sandbox too (with the feature `sandbox`), and gives the same, failures included.

#![cfg(feature = "wasm")]

use aqr_decompress::{MODULES, Native, Wasm, known, wasm_of};
use aqr_formula::aqr::{self, Encoder, Failure, Level, Loaded, Runner};
use aqr_formula::{Formula, Module, Node};

/// The workers every module of these tests runs in too.
#[cfg(feature = "sandbox")]
static SANDBOX: std::sync::LazyLock<aqr_decompress::sandbox::Sandbox> = std::sync::LazyLock::new(|| {
    aqr_decompress::sandbox::Sandbox::new(env!("CARGO_BIN_EXE_aqr-decompress-worker"), [""; 0])
});

/// Archive `c` decoded as WebAssembly ([`Wasm`]), and the same in the sandbox.
fn decode_wasm(c: &[u8]) -> aqr::Decoded<Vec<u8>> {
    let w = aqr::decode(c, &Wasm);
    #[cfg(feature = "sandbox")]
    {
        let s = aqr::decode(c, &*SANDBOX);
        assert!(s == w, "in the sandbox: {:?}, in this process: {:?}", s.err(), w.as_ref().err());
    }
    w
}

fn fixture(path: &str) -> Vec<u8> {
    std::fs::read(format!("{}/../../{path}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// The modules of the releases that the earlier Apply instruction is valid for
/// (`scripts/released-modules.sh`) are not the current ones (regenerated at a release).
#[test]
fn released_modules_not_current() {
    for h in aqr_formula::released::RELEASED {
        let current = aqr_modules::KNOWN
            .iter()
            .find(|k| k.sha512_256.iter().map(|b| format!("{b:02x}")).collect::<String>() == *h);
        assert!(current.is_none(), "{h}: {:?} is current", current.map(|k| k.name));
    }
}

/// Every module of this crate is built, of its version (else it is stale), and known by its
/// bytes.
#[test]
fn built_modules_known() {
    for m in MODULES {
        assert!(aqr_decompress::built(m).is_some(), "{m}: not built");
    }
    for k in aqr_modules::KNOWN {
        let m = MODULES.iter().find(|m| m.name == k.name);
        assert_eq!(m.map(|m| m.version), Some(k.version), "{}: run crates/aqr-modules/build.sh", k.name);
        let wasm = k.read_wasm().unwrap();
        assert_eq!(known(&wasm), m.copied());
        assert_eq!(wasm_of(*m.unwrap()), Some(aqr::Wasm::Block(k.block.into())));
        assert!(aqr::module_block_data(&wasm).unwrap() == k.block, "{}: run crates/aqr-modules/build.sh", k.name);
    }
    assert_eq!(known(b"\0asm\x01\0\0\0"), None);
}

/// A WAV file of 16-bit stereo, 600 000 frames of noisy tones.
fn wav() -> Vec<u8> {
    let n: u32 = 600_000;
    let mut d = Vec::new();
    d.extend_from_slice(b"RIFF");
    d.extend_from_slice(&(36 + 4 * n).to_le_bytes());
    d.extend_from_slice(b"WAVEfmt ");
    d.extend_from_slice(&16u32.to_le_bytes());
    d.extend_from_slice(&[1, 0, 2, 0]);
    d.extend_from_slice(&44100u32.to_le_bytes());
    d.extend_from_slice(&(44100u32 * 4).to_le_bytes());
    d.extend_from_slice(&[4, 0, 16, 0]);
    d.extend_from_slice(b"data");
    d.extend_from_slice(&(4 * n).to_le_bytes());
    let mut r = 1u32;
    for i in 0..n {
        for c in 0..2 {
            r = r.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let x = ((i as f64 * (0.01 + 0.003 * c as f64)).sin() * 9000.0) as i32 + (r >> 26) as i32 - 32;
            d.extend_from_slice(&(x as i16).to_le_bytes());
        }
    }
    d
}

/// The pipelines of FLAC and PCM (tasks and channels as WebAssembly): every stream of their
/// formulas damaged in turn fails alike natively and as WebAssembly, or gives the same, and
/// never waits for ever.
#[test]
fn pipelines_fail_alike() {
    let mut formulas = Vec::new();
    for t in ["s16_junk.flac", "s32.flac"] {
        let d = fixture(&format!("codecs/aqr-flac/tests/data/{t}"));
        formulas.push((t, aqr_flac::formula::encode(&d, 1).unwrap()));
    }
    formulas.push(("pcm", aqr_raw::formula::encode(&wav(), 1).unwrap()));
    let enc = Encoder::new(Level::Zstd(3)).with_modules(wasm_of);
    for (name, f) in formulas {
        for k in 0..f.streams.len() {
            for at in [1, 2, 3] {
                let mut bad = f.clone();
                let s = &mut bad.streams[k].data;
                if s.is_empty() {
                    continue;
                }
                let i = s.len() * at / 4;
                s[i] ^= 0x10;
                let c = enc.encode(&bad, None).unwrap();
                let (n, w) = (aqr::decode(&c, &Native::default()), decode_wasm(&c));
                assert_eq!(n.is_ok(), w.is_ok(), "{name} stream {k} at {i}: {n:?} {w:?}");
                if let (Ok(n), Ok(w)) = (n, w) {
                    assert!(n == w, "{name} stream {k} at {i}");
                }
            }
        }
    }
}

#[path = "../../../codecs/aqr-psd/tests/common/mod.rs"]
mod psd;

#[path = "../../../codecs/aqr-clip/tests/common/mod.rs"]
mod clip;

/// Clip Studio Paint files, their batches of tiles decoded as tasks: the same natively and as
/// WebAssembly.
#[test]
fn clip_as_wasm() {
    let archive = Encoder::new(Level::Zstd(3)).with_modules(wasm_of);
    for (k, file) in [clip::sample(0), clip::sample(150)].iter().enumerate() {
        let f = aqr_clip::formula::encode(file, 1).expect("a formula");
        let c = archive.encode(&f, None).unwrap();
        assert!(aqr::decode(&c, &Native::default()).unwrap() == *file, "{k}");
        let w = decode_wasm(&c);
        assert!(w.as_ref().is_ok_and(|w| w == file), "{k}: {:?}", w.err());
    }
}

/// clip-recons lays its memory out as a stack, its limit its plan: a file of one batch (decoded
/// by the instance), none, of a few and of more batches than slots (tasks), repeated tiles
/// among them.
#[test]
fn clip_plans_hold() {
    use aqr_clip::tile::Layout;
    let one = clip::clip_file(&[clip::external(1, &clip::block_data(&[clip::made(Layout::Color, 1)]))], None);
    let none = clip::clip_file(&[clip::external(1, &clip::block_data(&[clip::Block::Empty]))], Some(&[5u8; 100]));
    let files = [
        ("one batch", one),
        ("none", none),
        ("few", clip::sample(0)),
        ("many", clip::sample(150)),
        ("more than slots", clip::sample(640)),
    ];
    for (what, d) in files {
        let f = aqr_clip::formula::encode(&d, 1).unwrap_or_else(|| panic!("{what}"));
        if let Node::Apply(a) = &f.root {
            assert_eq!(a.module, aqr_clip::formula::MODULE, "{what}: not modelled");
        } else {
            // (nothing to model: no application)
            continue;
        }
        plan_holds(&format!("clip {what}"), f, &d);
    }
}

/// PSD and PSB files of the shapes of aqr-psd's tests, the same natively and as WebAssembly
/// (whose decoding of a group once called `Instant::now`, which WebAssembly has not).
#[test]
fn psd_as_wasm() {
    use aqr_psd::packbits::{self, Encoder as Rle};
    use psd::{layers, psd};
    let enc = Rle::Photoshop { phase: 77, period: 368 };
    let files = [
        psd(300, 40, &layers(300, 40, true), packbits::NO_TILES, 0, &[]),
        psd(260, 35, &layers(260, 35, false), Rle::Greedy { min_run: 3, split_rest: true }, 0, &[0]),
        psd(800, 400, &layers(800, 400, true), enc, 3, &[]),
        psd::hinted_file(300, 200),
        // (counts of 4 bytes, once checked by a shift past the width of a 32-bit usize)
        psd::psb(300, 40, enc),
    ];
    let archive = Encoder::new(Level::Zstd(3)).with_modules(wasm_of);
    for (k, file) in files.iter().enumerate() {
        let f = aqr_psd::formula::encode(file, 1).expect("a formula");
        let c = archive.encode(&f, None).unwrap();
        assert!(aqr::decode(&c, &Native::default()).unwrap() == *file, "{k}");
        let w = decode_wasm(&c);
        assert!(w.as_ref().is_ok_and(|w| w == file), "{k}: {:?}", w.err());
    }
}

/// psd-recons lays its memory out as a stack, its limit its plan: files of PackBits and raw
/// channels (coded on their own and with the layers rendered as the hint), of a PSB, of many
/// layers with masks, alpha and groups (several nested), and of a model of several chunks.
#[test]
fn psd_plans_hold() {
    use aqr_psd::packbits::{self, Encoder as Rle};
    use psd::{Layer, image, layers, psd};
    let enc = Rle::Photoshop { phase: 77, period: 368 };
    // layers of several sizes, positions, masks and alpha; every `group`th starts a group of
    // two layers, nested in the one before
    let many = |n: usize| {
        let mut ls = Vec::new();
        let mut sections = Vec::new();
        let (w0, h0) = (96, 64);
        let mut open = 0;
        for k in 0..n {
            if k % 5 == 0 {
                ls.push(Layer { top: 0, left: 0, w: 0, h: 0, channels: vec![], mask: None });
                sections.push(3);
                open += 1;
            }
            let (w, h) = (30 + k % 40, 20 + k % 30);
            let mut channels = vec![(0, image(w, h, k as u32)), (1, image(w, h, 3 * k as u32)), (2, image(w, h, 7))];
            if k % 3 == 0 {
                channels.insert(0, (-1, image(w, h, 5).iter().map(|&b| if b < 80 { 0 } else { b }).collect()));
            }
            let mask = (k % 4 == 1).then_some((k as i32 % 7, 2, w - 3, h - 2, 255 - k as u8 % 3));
            if let Some((_, _, mw, mh, _)) = mask {
                channels.push((-2, image(mw, mh, 11)));
            }
            ls.push(Layer { top: (k % 9) as i32 - 3, left: (k % 11) as i32 - 5, w, h, channels, mask });
            sections.push(0);
            if k % 5 == 4 || k + 1 == n {
                ls.push(Layer { top: 0, left: 0, w: 0, h: 0, channels: vec![], mask: None });
                sections.push(if open % 2 == 0 { 2 } else { 1 });
                open -= 1;
            }
        }
        (psd::hinted(w0, h0, &ls, &sections), ls, sections)
    };
    let (small, ..) = many(3);
    let (nested, ..) = many(40);
    let (lots, ..) = many(200);
    let files = [
        ("raw and rle", psd(300, 40, &layers(300, 40, true), packbits::NO_TILES, 0, &[])),
        (
            "greedy, one raw",
            psd(260, 35, &layers(260, 35, false), Rle::Greedy { min_run: 3, split_rest: true }, 0, &[0]),
        ),
        ("tiles and exceptions", psd(800, 400, &layers(800, 400, true), enc, 3, &[])),
        ("hinted", psd::hinted_file(300, 200)),
        ("psb", psd::psb(300, 40, enc)),
        ("hinted, few layers", small),
        ("hinted, groups", nested),
        ("hinted, many layers", lots),
        ("several chunks", psd::hinted_file(900, 700)),
    ];
    for (what, d) in files {
        let f = aqr_psd::formula::encode(&d, 1).unwrap_or_else(|| panic!("{what}"));
        if !matches!(&f.root, Node::Apply(a) if a.module == aqr_psd::formula::MODULE) {
            panic!("{what}: not modelled");
        }
        plan_holds(&format!("psd {what}"), f, &d);
    }
}

/// Formulas of the modules built, archived with them: the same natively and as WebAssembly.
#[test]
fn same_as_wasm() {
    let mut formulas: Vec<(String, Formula, Vec<u8>)> = Vec::new();
    for t in ["table-mul", "mixed-mul", "odd-crc"] {
        let d = fixture(&format!("codecs/aqr-snappy/tests/data/{t}.ldb"));
        formulas.push((t.into(), aqr_snappy::formula::encode(&d, 1).unwrap(), d));
    }
    for t in ["fragments-crc", "mixed-crc"] {
        let s = fixture(&format!("codecs/aqr-snappy/tests/data/{t}.snappy"));
        let d = aqr_snappy::snappy::decode(&s, 1 << 24).unwrap();
        let hash = aqr_snappy::formula::find_hash(&d, &s).unwrap();
        let mut f = Formula::default();
        let data = f.push("raw", d);
        f.root = aqr_snappy::formula::stream(hash, data, s.len() as u64);
        formulas.push((t.into(), f, s));
    }
    for t in ["f64k.lz4", "f256k-checks.lz4", "small-a5.mozlz4", "fempty.lz4"] {
        let d = fixture(&format!("codecs/aqr-lz4/tests/data/{t}"));
        let f = aqr_lz4::formula::encode(&d, 1).unwrap_or_else(|| panic!("{t}"));
        formulas.push((t.into(), f, d));
    }
    for t in ["text-1.bz2", "periodic-1.bz2", "random-1.bz2", "empty-9.bz2"] {
        let d = fixture(&format!("codecs/aqr-bz2/tests/data/{t}"));
        let f = aqr_bz2::formula::encode(&d, 1).unwrap_or_else(|| panic!("{t}"));
        formulas.push((t.into(), f, d));
    }
    // ALZip's bzip2 streams, one after another
    let (mut streams, mut data, mut all) = (Vec::new(), Vec::new(), Vec::new());
    for t in ["a", "b", "text"] {
        let d = fixture(&format!("codecs/aqr-bz2/tests/data/alz/{t}.dlz"));
        let (p, dd) = aqr_bz2::formula::take_alz(&d, 1, usize::MAX, &mut |_, _, _| true).expect(t);
        streams.push(p);
        data.extend_from_slice(&dd);
        all.extend_from_slice(&d);
    }
    let mut f = Formula::default();
    let data = f.push("raw", data);
    f.root = Node::Apply(Box::new(aqr_bz2::formula::apply_alz(&streams, data, all.len() as u64)));
    formulas.push(("alz bzip2".into(), f, all));
    // FLAC and PCM of more batches of their pipelines than tasks (a batch is 2^16 samples)
    for t in ["s16_junk.flac", "s24_f8.flac", "c6.flac"] {
        let d = fixture(&format!("codecs/aqr-flac/tests/data/{t}"));
        formulas.push((t.into(), aqr_flac::formula::encode(&d, 1).unwrap_or_else(|| panic!("{t}")), d));
    }
    formulas.push(("pcm".into(), aqr_raw::formula::encode(&wav(), 1).expect("pcm"), wav()));
    // a git pack with its index and reverse index after it
    let [pack, idx, rev] = ["pack", "idx", "rev"].map(|e| fixture(&format!("codecs/aqr-git/tests/data/small.{e}")));
    let f = aqr_git::formula::encode(&pack, 1).expect("pack");
    let f = aqr_git::formula::encode_index(f, &pack, &idx, Some(&rev), 1).expect("indexes");
    formulas.push(("git pack".into(), f, [pack, idx, rev].concat()));
    let enc = Encoder::new(Level::Zstd(3)).with_modules(wasm_of);
    for (name, f, d) in formulas {
        let c = enc.encode(&f, None).unwrap();
        assert!(aqr::decode(&c, &Native::default()).unwrap() == d, "{name}");
        assert!(decode_wasm(&c).unwrap() == d, "{name}");
        // damaged inputs fail alike (or give the same)
        let Node::Apply(a) = &f.root else { continue };
        let mut bad = f.clone();
        let s = &mut bad.streams[0].data;
        if !s.is_empty() {
            let n = s.len() / 2;
            s[n] ^= 1;
            let c = enc.encode(&bad, None).unwrap();
            let (n, w) = (aqr::decode(&c, &Native::default()), decode_wasm(&c));
            assert_eq!(n.is_ok(), w.is_ok(), "{name} ({}): {n:?} {w:?}", a.module);
            if let (Ok(n), Ok(w)) = (n, w) {
                assert!(n == w, "{name}");
            }
        }
    }
}

/// `.lzma` files natively and as WebAssembly alike, and their records damaged in turn failing
/// alike (or giving the same).
#[test]
fn lzma_same_as_wasm() {
    let enc = Encoder::new(Level::Zstd(3)).with_modules(wasm_of);
    for t in ["text-p6", "binary-p9e", "runs-dict4k", "empty-p6"] {
        let d = fixture(&format!("codecs/aqr-lzma/tests/data/{t}.lzma"));
        let f = aqr_lzma::formula::encode(&d, 1).unwrap_or_else(|| panic!("{t}"));
        let c = enc.encode(&f, None).unwrap();
        assert!(aqr::decode(&c, &Native::default()).unwrap() == d, "{t}");
        assert!(aqr::decode(&c, &Wasm).unwrap() == d, "{t}");
        let r = f.streams.iter().position(|s| s.class == "lzma/record").unwrap();
        for k in 0..8 {
            let mut bad = f.clone();
            let s = &mut bad.streams[r].data;
            // (of the same length: the formula's nodes tell it)
            let at = s.len() * k / 8;
            if k == 7 {
                s[at..].fill(0xff);
            } else if at < s.len() {
                s[at] ^= 1 << k;
            }
            let c = enc.encode(&bad, None).unwrap();
            let (n, w) = (aqr::decode(&c, &Native::default()), aqr::decode(&c, &Wasm));
            assert_eq!(n.is_ok(), w.is_ok(), "{t} ({k}): {n:?} {w:?}");
            if let (Ok(n), Ok(w)) = (n, w) {
                assert!(n == w, "{t} ({k})");
            }
        }
    }
}

/// `.xz` files natively and as WebAssembly alike (blocks by one application of several
/// outputs), and their records damaged in turn failing alike (or giving the same).
#[test]
fn xz_same_as_wasm() {
    let enc = Encoder::new(Level::Zstd(3)).with_modules(wasm_of);
    for t in ["crc64", "blocks", "streams"] {
        let d = fixture(&format!("codecs/aqr-lzma/tests/data/{t}.xz"));
        let f = aqr_lzma::formula::encode_xz(&d, 1).unwrap_or_else(|| panic!("{t}"));
        let c = enc.encode(&f, None).unwrap();
        assert!(aqr::decode(&c, &Native::default()).unwrap() == d, "{t}");
        assert!(aqr::decode(&c, &Wasm).unwrap() == d, "{t}");
        let r = f.streams.iter().position(|s| s.class == "lzma2/record").unwrap();
        for k in 0..8 {
            let mut bad = f.clone();
            let s = &mut bad.streams[r].data;
            let at = s.len() * k / 8;
            if k == 7 {
                s[at..].fill(0xff);
            } else if at < s.len() {
                s[at] ^= 1 << k;
            }
            let c = enc.encode(&bad, None).unwrap();
            let (n, w) = (aqr::decode(&c, &Native::default()), aqr::decode(&c, &Wasm));
            assert_eq!(n.is_ok(), w.is_ok(), "{t} ({k}): {n:?} {w:?}");
            if let (Ok(n), Ok(w)) = (n, w) {
                assert!(n == w, "{t} ({k})");
            }
        }
    }
}

/// `.xz` files of filters and 7z archives of BCJ2 and other filters (aqr-bcj's modules)
/// natively and as WebAssembly alike.
#[test]
fn filters_same_as_wasm() {
    let enc = Encoder::new(Level::Zstd(3)).with_modules(wasm_of);
    let mut formulas = Vec::new();
    for t in ["x86", "powerpc", "ia64", "arm", "armthumb", "sparc", "arm64", "riscv", "delta", "chain"] {
        let d = fixture(&format!("codecs/aqr-lzma/tests/data/filter-{t}.xz"));
        formulas.push((t, aqr_lzma::formula::encode_xz(&d, 1).unwrap(), d));
    }
    for t in ["bcj2", "arm64", "delta"] {
        let d = fixture(&format!("codecs/aqr-7z/tests/data/filter-{t}.7z"));
        formulas.push((t, aqr_7z::formula::encode(&d, 1).unwrap(), d));
    }
    for (t, f, d) in formulas {
        let c = enc.encode(&f, None).unwrap();
        assert!(aqr::decode(&c, &Native::default()).unwrap() == d, "{t}");
        assert!(aqr::decode(&c, &Wasm).unwrap() == d, "{t}");
    }
}

/// The Delta filter (bcj-delta, a part module of windows of a page) on more than its windows,
/// natively and as WebAssembly alike, streamed as an archive is written out.
#[test]
fn delta_streamed() {
    let d: Vec<u8> = (0..1_000_003u32).map(|i| (i * i / 7) as u8).collect();
    let mut want = d.clone();
    aqr_bcj::delta::encode(&mut want, 4);
    let mut f = Formula::default();
    let data = f.push("raw", d);
    f.root = Node::apply(aqr_bcj::formula::DELTA, vec![3], vec![data], want.len() as u64);
    let c = Encoder::new(Level::Zstd(3)).with_modules(wasm_of).encode(&f, None).unwrap();
    let wasm = fixture("crates/aqr-modules/wasm/bcj-delta.wasm");
    for runner in [&Native::default() as &dyn Runner, &Wasm] {
        assert!(runner.load(&wasm).unwrap().is_part());
        assert!(aqr::decode(&c, runner).unwrap() == want);
        let mut out = Vec::new();
        aqr::open(&c, runner)
            .unwrap()
            .write_file(2, &mut |p: &[u8]| {
                out.extend_from_slice(p);
                Ok(())
            })
            .unwrap();
        assert!(out == want);
    }
}

/// DEFLATE streams inflated (flate-inflate) natively and as WebAssembly alike, and damaged in
/// turn failing alike (or giving the same).
#[test]
fn inflate_same_as_wasm() {
    let enc = Encoder::new(Level::Zstd(3)).with_modules(wasm_of);
    let text: Vec<u8> = (0..40_000).flat_map(|i| format!("line {} of {}\n", i % 211, i % 17).into_bytes()).collect();
    for (level, strategy) in [(1, aqr_flate::zlib::Strategy::Default), (9, aqr_flate::zlib::Strategy::Fixed)] {
        let p = aqr_flate::zlib::Params { level, strategy, ..Default::default() };
        let raw = aqr_flate::zlib::compress(&text, p).unwrap();
        let mut f = Formula::default();
        let s = f.push("flate/deflated", raw.clone());
        f.root = aqr_flate::formula::inflated(s, text.len() as u64);
        let c = enc.encode(&f, None).unwrap();
        assert!(aqr::decode(&c, &Native::default()).unwrap() == text, "{level}");
        assert!(decode_wasm(&c).unwrap() == text, "{level}");
        for at in [1, 2, 3] {
            let mut bad = f.clone();
            let i = raw.len() * at / 4;
            bad.streams[0].data.to_mut()[i] ^= 0x10;
            let c = enc.encode(&bad, None).unwrap();
            let (n, w) = (aqr::decode(&c, &Native::default()), decode_wasm(&c));
            assert_eq!(n.is_ok(), w.is_ok(), "{level} at {i}: {n:?} {w:?}");
            if let (Ok(n), Ok(w)) = (n, w) {
                assert!(n == w, "{level} at {i}");
            }
        }
    }
}

/// DEFLATE streams rebuilt (flate-set) natively and as WebAssembly alike: alone and several,
/// small and large (2 MiB of data or more), with what each params value derives of them.
#[test]
fn flate_set_same_as_wasm() {
    use aqr_flate::formula::Derived;
    let enc = Encoder::new(Level::Zstd(3)).with_modules(wasm_of);
    let text: Vec<u8> = (0..300_000).flat_map(|i| format!("line {} of {}\n", i % 211, i % 17).into_bytes()).collect();
    let p = aqr_flate::zlib::Params { level: 6, ..Default::default() };
    let parts: Vec<(Vec<u8>, Vec<u8>, usize)> = [&text[..1000], &text[..], &text[..30_000]]
        .iter()
        .map(|d| {
            let raw = aqr_flate::zlib::compress(d, p).unwrap();
            let (data, record) = aqr_flate::deconstruct(&raw).unwrap();
            (data, record, raw.len())
        })
        .collect();
    for set in [&[0][..], &[1], &[0, 2], &[2, 1, 0]] {
        for derived in [Derived::None, Derived::Zip, Derived::Gzip, Derived::ZipBefore] {
            let mut f = Formula::default();
            let data = f.push("raw", set.iter().flat_map(|&i| parts[i].0.clone()).collect::<Vec<u8>>());
            let records = f.push("flate/record", set.iter().flat_map(|&i| parts[i].1.clone()).collect::<Vec<u8>>());
            let lens: Vec<(usize, usize, usize)> =
                set.iter().map(|&i| (parts[i].0.len(), parts[i].1.len(), parts[i].2)).collect();
            let index = match aqr_flate::module::index(&lens) {
                i if i.is_empty() => Node::Concat(Vec::new()),
                i => f.push("flate/index", i),
            };
            let len = set.iter().map(|&i| (parts[i].2 + derived.bytes()) as u64).sum();
            let a = aqr_flate::formula::apply(aqr_flate::DEFAULT_PROFILE, data, records, index, len, derived).unwrap();
            f.root = Node::Apply(Box::new(a));
            let c = enc.encode(&f, None).unwrap();
            let n = aqr::decode(&c, &Native::default()).unwrap();
            let w = decode_wasm(&c);
            assert!(w.as_ref().is_ok_and(|w| *w == n), "{set:?} {derived:?}: {:?}", w.err());
        }
    }
}

/// flate-set lays its memory out as a stack, its limit its plan of the instance and as many tasks
/// at once as their slots allow: streams alone and several (runs of small ones, groups of them
/// as tasks), large ones pulled (more segments than the slots: several batches), of a static
/// profile and an adaptive one with zlib emulation (segments resumed), stored blocks among them,
/// with what each params value derives (ZipBefore on one thread).
#[test]
fn flate_plans_hold() {
    use aqr_flate::formula::Derived;
    let text: Vec<u8> =
        (0..400_000).flat_map(|i| format!("line {} of {}\n", i % 211, i * 7 % 1013).into_bytes()).collect();
    let mut noise = Vec::with_capacity(200_000);
    let mut x = 7u32;
    while noise.len() < 200_000 {
        x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
        noise.push((x >> 24) as u8);
    }
    let mixed: Vec<u8> = [&text[..100_000], &noise[..], &text[100_000..300_000]].concat();
    let p = aqr_flate::zlib::Params { level: 6, ..Default::default() };
    for profile in [aqr_flate::DEFAULT_PROFILE, aqr_flate::ADAPTIVE] {
        let mut o = aqr_flate::Options::for_profile(profile).unwrap();
        o.segment = 200_000;
        // (the last with blocks of a hundred tokens, thousands of them: a round of blocks written
        // after the first batch, before the next one's tasks)
        let small = aqr_flate::zlib::Params { mem: 1, ..p };
        let parts: Vec<(Vec<u8>, Vec<u8>, usize)> =
            [&text[..1000], &text[..], &mixed[..], &text[..30_000], &noise[..5000], &text[..]]
                .iter()
                .enumerate()
                .map(|(i, d)| {
                    let raw = aqr_flate::zlib::compress(d, if i == 5 { small } else { p }).unwrap();
                    let (data, record) = aqr_flate::deconstruct_with(&raw, &o).unwrap();
                    assert_eq!(aqr_flate::reconstruct(&data, &record).unwrap(), raw);
                    (raw, (data, record))
                })
                .map(|(raw, (d, r))| (d, r, raw.len()))
                .collect();
        let raws: Vec<Vec<u8>> = parts.iter().map(|(d, r, _)| aqr_flate::reconstruct(d, r).unwrap()).collect();
        for set in [&[0][..], &[1], &[0, 3, 4], &[3, 1, 0, 2], &[2], &[5]] {
            for derived in [Derived::None, Derived::Zip, Derived::ZipBefore] {
                let mut f = Formula::default();
                let data = f.push("raw", set.iter().flat_map(|&i| parts[i].0.clone()).collect::<Vec<u8>>());
                let records = f.push("flate/record", set.iter().flat_map(|&i| parts[i].1.clone()).collect::<Vec<u8>>());
                let lens: Vec<(usize, usize, usize)> =
                    set.iter().map(|&i| (parts[i].0.len(), parts[i].1.len(), parts[i].2)).collect();
                let index = match aqr_flate::module::index(&lens) {
                    i if i.is_empty() => Node::Concat(Vec::new()),
                    i => f.push("flate/index", i),
                };
                let len = set.iter().map(|&i| (parts[i].2 + derived.bytes()) as u64).sum();
                let a = aqr_flate::formula::apply(profile, data, records, index, len, derived).unwrap();
                f.root = Node::Apply(Box::new(a));
                let mut want = Vec::new();
                for &i in set {
                    if derived.before() {
                        derived.put(&parts[i].0, parts[i].2, &mut want);
                    }
                    want.extend_from_slice(&raws[i]);
                }
                if !derived.before() {
                    set.iter().for_each(|&i| derived.put(&parts[i].0, parts[i].2, &mut want));
                }
                plan_holds(&format!("flate {profile} {set:?} {derived:?}"), f, &want);
            }
        }
    }
}

/// flate-inflate lays its memory out as a stack, its limit its plan, the same whatever the
/// lengths: streams of several MB of many blocks (more than the stack's holes) of every kind in
/// turn (stored, fixed, dynamic) and of every size (a few tokens to a whole window and past it),
/// stored runs of noise, and a stream of one fixed block.
#[test]
fn inflate_plans_hold() {
    use aqr_flate::deflate::{self, Body};
    let text: Vec<u8> =
        (0..250_000).flat_map(|i| format!("line {} of {}\n", i % 211, i * 7 % 1013).into_bytes()).collect();
    let mut noise = Vec::with_capacity(300_000);
    let mut x = 7u32;
    while noise.len() < 300_000 {
        x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
        noise.push((x >> 24) as u8);
    }
    let mixed: Vec<u8> = [&text[..1_500_000], &noise[..], &text[1_500_000..]].concat();
    assert!(mixed.len() > 3_000_000, "{}", mixed.len());
    let mut seen = 0;
    for (what, data, mem) in [("text mem 1", &text, 1), ("mixed mem 8", &mixed, 8), ("mixed mem 3", &mixed, 3)] {
        let p = aqr_flate::zlib::Params { level: 6, mem, ..Default::default() };
        let raw = aqr_flate::zlib::compress(data, p).unwrap();
        let (mut stream, parsed) = deflate::parse(&raw).unwrap();
        assert!(parsed == *data);
        // (blocks of a few hundred tokens, and of the default's sixteen thousand)
        let few = if mem == 8 { 32 } else { 64 };
        assert!(stream.blocks.len() > few, "{what}: {} blocks", stream.blocks.len());
        let mut raws = vec![("zlib", raw)];
        // (every kind of block in turn: dynamic ones as fixed ones, as stored ones where they
        // fit)
        for (i, b) in stream.blocks.iter_mut().enumerate() {
            let tokens = match &b.body {
                Body::Dynamic { tokens, .. } => tokens.clone(),
                _ => continue,
            };
            let span: usize = tokens.iter().map(|t| t.span()).sum();
            match i % 4 {
                1 => b.body = Body::Fixed { tokens },
                2 if span <= 65535 => b.body = Body::Stored { pad: 0, len: span as u16 },
                _ => {}
            }
        }
        let kinds: Vec<usize> = (0..3).map(|k| stream.blocks.iter().filter(|b| b.body.btype() == k).count()).collect();
        assert!(kinds.iter().all(|&n| n >= few / 4), "{what}: {kinds:?}");
        raws.push(("mixed blocks", deflate::write(&stream, data).unwrap()));
        for (kind, raw) in raws {
            let mut f = Formula::default();
            let s = f.push("flate/deflated", raw);
            f.root = aqr_flate::formula::inflated(s, data.len() as u64);
            plan_holds(&format!("inflate {what} {kind}"), f, data);
            seen += 1;
        }
    }
    // (one fixed block: "abc" with its end of block)
    let mut f = Formula::default();
    let s = f.push("flate/deflated", vec![0x4b, 0x4c, 0x4a, 0x06, 0x00]);
    f.root = aqr_flate::formula::inflated(s, 3);
    plan_holds("inflate one fixed block", f, b"abc");
    assert_eq!(seen, 6);
}

/// A PNG chunk of `kind` and `data`, appended to `out`.
fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&aqr_common::crc32::hash(&[&kind[..], data].concat()).to_be_bytes());
}

/// The filtered rows of a picture of `w` by `h` pixels of `bpp` bytes (a pass of it), smooth with
/// noise, each row's filter in turn.
fn png_rows(w: usize, h: usize, bpp: usize, seed: u32) -> Vec<u8> {
    let mut x = seed;
    let mut d = Vec::with_capacity(h * (w * bpp + 1));
    for y in 0..h {
        // (a variant (a seed of 100 or more) different in a band of rows)
        if seed >= 100 && y % 40 == 7 {
            x = x.wrapping_add(seed);
        }
        d.push((y % 5) as u8);
        for i in 0..w * bpp {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
            // (an alpha channel opaque)
            d.push(if bpp == 4 && i % 4 == 3 { 255 } else { ((i / bpp + y) as u32 / 3 + (x >> 29)) as u8 });
        }
    }
    d
}

/// A PNG file of a picture of `w` by `h` pixels of `depth` bits and color type `color`
/// (interlaced where `interlace`), its data in IDAT chunks of `idat` bytes; with `frames` more
/// frames of it (an APNG).
fn png_file(w: u32, h: u32, depth: u8, color: u8, interlace: bool, idat: usize, frames: u32) -> Vec<u8> {
    png_file_of(w, h, depth, color, interlace, idat, frames, 0)
}

/// [`png_file`] of a variant (`variant` more than 0) of the picture, different in some rows.
#[allow(clippy::too_many_arguments)]
fn png_file_of(
    w: u32,
    h: u32,
    depth: u8,
    color: u8,
    interlace: bool,
    idat: usize,
    frames: u32,
    variant: u32,
) -> Vec<u8> {
    let channels = [1, 0, 3, 1, 2, 0, 4][color as usize];
    let bpp = (channels * depth as usize).div_ceil(8);
    let p = aqr_flate::zlib::Params { level: 6, ..Default::default() };
    let data = |seed: u32| {
        let rows = match interlace {
            false => png_rows(w as usize, h as usize, bpp, seed),
            true => [(0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8), (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2)]
                .iter()
                .filter_map(|&(x0, y0, dx, dy)| {
                    let (pw, ph) =
                        ((w as usize).saturating_sub(x0).div_ceil(dx), (h as usize).saturating_sub(y0).div_ceil(dy));
                    (pw > 0 && ph > 0).then(|| png_rows(pw, ph, bpp, seed))
                })
                .flatten()
                .collect(),
        };
        let z = aqr_flate::zlib::compress(&rows, p).unwrap();
        let mut zs = vec![0x78, 0x9c];
        zs.extend_from_slice(&z);
        zs.extend_from_slice(&adler32(&rows).to_be_bytes());
        zs
    };
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = [w.to_be_bytes(), h.to_be_bytes()].concat();
    ihdr.extend_from_slice(&[depth, color, 0, 0, interlace as u8]);
    png_chunk(&mut out, b"IHDR", &ihdr);
    let mut seq = 0u32;
    let fctl =
        |seq: u32| [&seq.to_be_bytes()[..], &w.to_be_bytes(), &h.to_be_bytes(), &[0; 8], &[0, 1, 0, 10, 0, 0]].concat();
    if frames > 0 {
        png_chunk(&mut out, b"acTL", &[(frames + 1).to_be_bytes(), 0u32.to_be_bytes()].concat());
        png_chunk(&mut out, b"fcTL", &fctl(seq));
        seq += 1;
    }
    let v = if variant > 0 { 100 * variant } else { 0 };
    for c in data(1 + v).chunks(idat) {
        png_chunk(&mut out, b"IDAT", c);
    }
    for f in 0..frames {
        png_chunk(&mut out, b"fcTL", &fctl(seq));
        seq += 1;
        for c in data(2 + f + v).chunks(idat) {
            png_chunk(&mut out, b"fdAT", &[&seq.to_be_bytes()[..], c].concat());
            seq += 1;
        }
    }
    png_chunk(&mut out, b"IEND", &[]);
    out
}

/// The Adler-32 of `d`.
fn adler32(d: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in d {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    b << 16 | a
}

/// png-recons lays its memory out as a stack, its limit its plan of the instance and as many
/// tasks at once as their slots allow: of RGB, RGBA interlaced and 16-bit gray pictures, a large
/// one (its data in windows), IDAT chunks of several sizes, and APNGs (each frame a task).
#[test]
fn png_plans_hold() {
    let files = [
        ("rgb", png_file(300, 200, 8, 2, false, 1 << 20, 0)),
        ("rgba interlaced", png_file(257, 131, 8, 6, true, 8192, 0)),
        ("gray16", png_file(200, 150, 16, 0, false, 1000, 0)),
        ("large", png_file(1400, 1100, 8, 2, false, 1 << 16, 0)),
        ("apng", png_file(160, 120, 8, 6, false, 1 << 20, 3)),
        ("apng of many", png_file(160, 120, 8, 6, false, 1 << 20, 11)),
    ];
    for (what, d) in files {
        let f = aqr_png::formula::encode(&d, 1).unwrap_or_else(|| panic!("{what}"));
        plan_holds(&format!("png {what}"), f, &d);
    }
}

/// png-against lays its memory out as a stack, its limit its plan (as png-recons'), the
/// references of the reference file made first: variants of an RGB picture and of an APNG, each
/// coded against its picture.
#[test]
fn png_against_plans_hold() {
    let pairs = [
        ("rgb", png_file_of(300, 200, 8, 2, false, 1 << 20, 0, 0), png_file_of(300, 200, 8, 2, false, 1 << 20, 0, 1)),
        ("apng", png_file_of(160, 120, 8, 6, false, 5000, 9, 0), png_file_of(160, 120, 8, 6, false, 5000, 9, 1)),
    ];
    for (what, reference, d) in pairs {
        let mut f = aqr_png::formula::encode_against(&d, &reference, 1).unwrap_or_else(|| panic!("{what}"));
        let Node::Apply(a) = &f.root else { panic!("{what}") };
        assert_eq!(a.module, aqr_png::formula::AGAINST_MODULE, "{what}: not coded against its reference");
        let r = f.streams.iter().position(|s| s.class == aqr_png::formula::REFERENCE).unwrap();
        f.streams[r].data = reference.into();
        plan_holds(&format!("png-against {what}"), f, &d);
    }
}

/// png-set lays its memory out as a stack, its limit its plan of the instance and as many tasks
/// at once as their slots allow: trees of variants of pictures (a depth of several files, each a
/// task giving back its file; a depth of one, its images tasks), of RGB pictures and APNGs, a
/// chain of 70 files and a depth of more files than the slots.
#[test]
fn png_set_plans_hold() {
    let rgb = |v| png_file_of(300, 200, 8, 2, false, 1 << 20, 0, v);
    let apng = |v| png_file_of(160, 120, 8, 6, false, 5000, 4, v);
    let sets: [(&str, Vec<Vec<u8>>, Vec<Option<usize>>); 4] = [
        ("rgb", vec![rgb(0), rgb(1), rgb(2), rgb(3)], vec![None, Some(0), Some(0), Some(1)]),
        ("apng", vec![apng(0), apng(1), apng(2)], vec![None, Some(0), Some(1)]),
        // (deeper than the holes the stack keeps)
        ("chain", (0..70).map(rgb).collect(), (0..70usize).map(|i| i.checked_sub(1)).collect()),
        ("wide", (0..11).map(rgb).collect(), (0..11).map(|i| (i > 0).then_some(0)).collect()),
    ];
    for (what, files, parents) in sets {
        let refs: Vec<&[u8]> = files.iter().map(|f| &f[..]).collect();
        let f = aqr_png::formula::encode_set(&refs, &parents, &[], 1).unwrap_or_else(|| panic!("{what}"));
        let d = files.concat();
        // (the files' application whole: its output the files one after another)
        let mut g = f.clone();
        let mut a = g.shared[0].apply.clone();
        a.len = d.len() as u64;
        g.shared.clear();
        g.root = Node::Apply(Box::new(a));
        plan_holds(&format!("png-set {what}"), g, &d);
    }
}

/// lzop files natively and as WebAssembly alike, and their records damaged in turn failing alike
/// (or giving the same).
#[test]
fn lzo_same_as_wasm() {
    let enc = Encoder::new(Level::Zstd(3)).with_modules(wasm_of);
    for t in ["big.1", "big.9", "mixed.9"] {
        let d = fixture(&format!("codecs/aqr-lzo/tests/data/{t}.lzo"));
        let f = aqr_lzo::formula::encode(&d, 1).unwrap_or_else(|| panic!("{t}"));
        let c = enc.encode(&f, None).unwrap();
        assert!(aqr::decode(&c, &Native::default()).unwrap() == d, "{t}");
        assert!(aqr::decode(&c, &Wasm).unwrap() == d, "{t}");
        let r = f.streams.iter().position(|s| s.class == "lzo/record").unwrap();
        for k in 0..8 {
            let mut bad = f.clone();
            let s = &mut bad.streams[r].data;
            let at = s.len() * k / 8;
            if k == 7 {
                s[at..].fill(0xff);
            } else if at < s.len() {
                s[at] ^= 1 << k;
            }
            let c = enc.encode(&bad, None).unwrap();
            let (n, w) = (aqr::decode(&c, &Native::default()), aqr::decode(&c, &Wasm));
            assert_eq!(n.is_ok(), w.is_ok(), "{t} ({k}): {n:?} {w:?}");
            if let (Ok(n), Ok(w)) = (n, w) {
                assert!(n == w, "{t} ({k})");
            }
        }
    }
}

/// RAR archives (RAR5's and RAR 3.x's, LZ and PPMd, with filters) natively and as WebAssembly
/// alike, and their records damaged in turn failing alike (or giving the same).
#[test]
fn rar_same_as_wasm() {
    let enc = Encoder::new(Level::Zstd(3)).with_modules(wasm_of);
    for t in ["r5_solid", "r4_solid", "r4_ppmd", "r4_audio"] {
        let d = fixture(&format!("codecs/aqr-rar/tests/data/{t}.rar"));
        let f = aqr_rar::formula::encode(&d, 1).unwrap_or_else(|| panic!("{t}"));
        let c = enc.encode(&f, None).unwrap();
        assert!(aqr::decode(&c, &Native::default()).unwrap() == d, "{t}");
        assert!(aqr::decode(&c, &Wasm).unwrap() == d, "{t}");
        let r = f.streams.iter().position(|s| s.class == "rar/record").unwrap();
        for k in 0..8 {
            let mut bad = f.clone();
            let s = &mut bad.streams[r].data;
            let at = s.len() * k / 8;
            if k == 7 {
                s[at..].fill(0xff);
            } else if at < s.len() {
                s[at] ^= 1 << k;
            }
            let c = enc.encode(&bad, None).unwrap();
            let (n, w) = (aqr::decode(&c, &Native::default()), aqr::decode(&c, &Wasm));
            assert_eq!(n.is_ok(), w.is_ok(), "{t} ({k}): {n:?} {w:?}");
            if let (Ok(n), Ok(w)) = (n, w) {
                assert!(n == w, "{t} ({k})");
            }
        }
    }
}

/// LHA's streams (two of an archive, of `-lh5-` and `-lh6-`) natively and as WebAssembly alike,
/// and their records damaged in turn failing alike (or giving the same).
#[test]
fn lha_same_as_wasm() {
    let enc = Encoder::new(Level::Zstd(3)).with_modules(wasm_of);
    let mut streams = Vec::new();
    for name in ["dos255e_lh5", "unix_h1_lh6"] {
        let d = fixture(&format!("codecs/aqr-lha/tests/data/{name}.lzh"));
        let e = aqr_lha::lha::parse(&d).unwrap().entries.remove(0);
        streams.push((d[e.data.clone()].to_vec(), e.size as usize, aqr_lha::lzh::Method::of(&e.method).unwrap()));
    }
    let (mut data, mut records, mut lens, mut whole) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (s, size, m) in &streams {
        let sp = aqr_lha::recons::split(s, *size, *m).unwrap();
        lens.push((sp.data.len(), sp.record.len()));
        data.extend(sp.data);
        records.extend(sp.record);
        whole.extend_from_slice(s);
    }
    let mut f = Formula::default();
    let data = f.push("raw", data);
    let record = f.push("lha/record", records);
    let index = f.push("lha/index", aqr_lha::formula::index(&lens));
    f.root = Node::Apply(Box::new(aqr_lha::formula::apply_recons(data, record, index, whole.len() as u64)));
    let c = enc.encode(&f, None).unwrap();
    assert!(aqr::decode(&c, &Native::default()).unwrap() == whole);
    assert!(aqr::decode(&c, &Wasm).unwrap() == whole);
    for k in 0..8 {
        let mut bad = f.clone();
        let r = &mut bad.streams[1].data;
        let at = r.len() * k / 8;
        if k == 7 {
            r[at..].fill(0xff);
        } else {
            r[at] ^= 1 << k;
        }
        let c = enc.encode(&bad, None).unwrap();
        let (n, w) = (aqr::decode(&c, &Native::default()), aqr::decode(&c, &Wasm));
        assert_eq!(n.is_ok(), w.is_ok(), "{k}: {n:?} {w:?}");
        if let (Ok(n), Ok(w)) = (n, w) {
            assert!(n == w, "{k}");
        }
    }
}

/// AZO streams (of EGG archives) natively and as WebAssembly alike, and their records damaged in
/// turn failing alike (or giving the same).
#[test]
fn azo_same_as_wasm() {
    let enc = Encoder::new(Level::Zstd(3)).with_modules(wasm_of);
    let d = fixture("codecs/aqr-egg/tests/data/azonumbers.egg");
    // (the archive's one AZO stream: its block's data after the block header and its end marker)
    let at = d.windows(4).position(|w| w == 0x02B5_0C13u32.to_le_bytes()).unwrap() + 4;
    let len = u32::from_le_bytes(d[at + 6..at + 10].try_into().unwrap()) as usize;
    let s = &d[at + 18..at + 18 + len];
    let sp = aqr_azo::recons::split(s, usize::MAX, 2).unwrap();
    let mut f = Formula::default();
    let data = f.push("raw", sp.data);
    let record = f.push("azo/record", sp.record);
    let index = f.push("azo/index", Vec::new());
    f.root = Node::Apply(Box::new(aqr_azo::formula::apply(data, record, index, s.len() as u64)));
    let c = enc.encode(&f, None).unwrap();
    assert!(aqr::decode(&c, &Native::default()).unwrap() == s);
    assert!(aqr::decode(&c, &Wasm).unwrap() == s);
    for k in 0..8 {
        let mut bad = f.clone();
        let r = &mut bad.streams[1].data;
        let at = r.len() * k / 8;
        if k == 7 {
            r[at..].fill(0xff);
        } else {
            r[at] ^= 1 << k;
        }
        let c = enc.encode(&bad, None).unwrap();
        let (n, w) = (aqr::decode(&c, &Native::default()), aqr::decode(&c, &Wasm));
        assert_eq!(n.is_ok(), w.is_ok(), "{k}: {n:?} {w:?}");
        if let (Ok(n), Ok(w)) = (n, w) {
            assert!(n == w, "{k}");
        }
    }
}

/// Streams of LZFSE and LZVN blocks natively and as WebAssembly alike, and their records damaged
/// in turn failing alike (or giving the same).
#[test]
fn bvx_same_as_wasm() {
    let enc = Encoder::new(Level::Zstd(3)).with_modules(wasm_of);
    for t in ["text", "words", "icon", "small"] {
        let d = fixture(&format!("codecs/aqr-bvx/tests/data/{t}.lzfse"));
        let f = aqr_bvx::formula::encode(&d, 1).unwrap_or_else(|| panic!("{t}"));
        let c = enc.encode(&f, None).unwrap();
        assert!(aqr::decode(&c, &Native::default()).unwrap() == d, "{t}");
        assert!(aqr::decode(&c, &Wasm).unwrap() == d, "{t}");
        let r = f.streams.iter().position(|s| s.class == "bvx/record").unwrap();
        for k in 0..8 {
            let mut bad = f.clone();
            let s = &mut bad.streams[r].data;
            let at = s.len() * k / 8;
            if k == 7 {
                s[at..].fill(0xff);
            } else if at < s.len() {
                s[at] ^= 1 << k;
            }
            let c = enc.encode(&bad, None).unwrap();
            let (n, w) = (aqr::decode(&c, &Native::default()), aqr::decode(&c, &Wasm));
            assert_eq!(n.is_ok(), w.is_ok(), "{t} ({k}): {n:?} {w:?}");
            if let (Ok(n), Ok(w)) = (n, w) {
                assert!(n == w, "{t} ({k})");
            }
        }
    }
}

/// ProRes files natively and as WebAssembly alike (the stripes as tasks), and their coded
/// chunks damaged in turn failing alike (or giving the same).
#[test]
fn prores_same_as_wasm() {
    let enc = Encoder::new(Level::Zstd(3)).with_modules(wasm_of);
    for t in ["ks-422", "ks-4444-alpha", "ks-interlaced", "aw-422hq x80"] {
        let d = match t.split_once(" x") {
            // (frames of more than a stripe's bytes: stripes as several tasks)
            Some((t, n)) => fixture(&format!("codecs/aqr-prores/tests/data/{t}.mov")).repeat(n.parse().unwrap()),
            None => fixture(&format!("codecs/aqr-prores/tests/data/{t}.mov")),
        };
        let f = aqr_prores::formula::encode(&d, 1).unwrap_or_else(|| panic!("{t}"));
        let c = enc.encode(&f, None).unwrap();
        assert!(aqr::decode(&c, &Native::default()).unwrap() == d, "{t}");
        assert!(aqr::decode(&c, &Wasm).unwrap() == d, "{t}");
        let r = f.streams.iter().position(|s| s.class == "prores/coded").unwrap();
        for k in 0..8 {
            let mut bad = f.clone();
            let s = &mut bad.streams[r].data;
            let at = s.len() * k / 8;
            if k == 7 {
                s[at..].fill(0xff);
            } else {
                s[at] ^= 1 << k;
            }
            let c = enc.encode(&bad, None).unwrap();
            let (n, w) = (aqr::decode(&c, &Native::default()), aqr::decode(&c, &Wasm));
            assert_eq!(n.is_ok(), w.is_ok(), "{t} ({k}): {n:?} {w:?}");
            if let (Ok(n), Ok(w)) = (n, w) {
                assert!(n == w, "{t} ({k})");
            }
        }
    }
}

/// GIF files natively and as WebAssembly alike, and their records damaged in turn failing alike
/// (or giving the same).
#[test]
fn gif_same_as_wasm() {
    let enc = Encoder::new(Level::Zstd(3)).with_modules(wasm_of);
    for t in ["im-photo", "im-il-art", "im-anim", "ff-anim", "cut-photo", "dup-box2"] {
        let d = fixture(&format!("codecs/aqr-gif/tests/data/{t}.gif"));
        let f = aqr_gif::formula::encode(&d, 1).unwrap_or_else(|| panic!("{t}"));
        let c = enc.encode(&f, None).unwrap();
        assert!(aqr::decode(&c, &Native::default()).unwrap() == d, "{t}");
        assert!(aqr::decode(&c, &Wasm).unwrap() == d, "{t}");
        let r = f.streams.iter().position(|s| s.class == "gif/record").unwrap();
        for k in 0..8 {
            let mut bad = f.clone();
            let s = &mut bad.streams[r].data;
            let at = s.len() * k / 8;
            if k == 7 {
                s[at..].fill(0xff);
            } else if at < s.len() {
                s[at] ^= 1 << k;
            }
            let c = enc.encode(&bad, None).unwrap();
            let (n, w) = (aqr::decode(&c, &Native::default()), aqr::decode(&c, &Wasm));
            assert_eq!(n.is_ok(), w.is_ok(), "{t} ({k}): {n:?} {w:?}");
            if let (Ok(n), Ok(w)) = (n, w) {
                assert!(n == w, "{t} ({k})");
            }
        }
    }
}

/// LZX chunks (aqr-lzx's fixture, of its test encoder) natively and as WebAssembly alike, in
/// one stream and in two, and their records damaged in turn failing alike (or giving the same).
#[test]
fn lzx_same_as_wasm() {
    let enc = Encoder::new(Level::Zstd(3)).with_modules(wasm_of);
    let d = fixture("codecs/aqr-lzx/tests/data/greedy.chunks");
    let (mut chunks, mut at) = (Vec::new(), 0);
    while at < d.len() {
        let n = u32::from_le_bytes(d[at..at + 4].try_into().unwrap()) as usize;
        let len = u32::from_le_bytes(d[at + 4..at + 8].try_into().unwrap()) as usize;
        chunks.push((&d[at + 8..at + 8 + len], n));
        at += 8 + len;
    }
    for streams in [vec![chunks.clone()], vec![chunks[..1].to_vec(), chunks[1..].to_vec()]] {
        // (bytes kept before each stream and after the last)
        let mut f = Formula::default();
        f.push("raw", Vec::new());
        f.push("kept", b"<before><between><after>".to_vec());
        let mut parts = Vec::new();
        let mut want = Vec::new();
        for (i, (s, sp)) in streams.iter().zip(aqr_lzx::formula::split(&streams, 1)).enumerate() {
            let sp = sp.unwrap();
            let at = f.streams[0].data.len() as u64;
            f.streams[0].data.extend_from_slice(&sp.data);
            let kept = if i == 0 { Node::slice(1, 0, 8) } else { Node::slice(1, 8, 9) };
            want.extend_from_slice(if i == 0 { b"<before>" } else { b"<between>" });
            s.iter().for_each(|c| want.extend_from_slice(c.0));
            parts.push(aqr_lzx::formula::Stream {
                kept,
                data: Node::slice(0, at, sp.data.len() as u64),
                record: sp.record,
            });
        }
        want.extend_from_slice(b"<after>");
        f.root = aqr_lzx::formula::apply(&mut f, parts, Node::slice(1, 17, 7), want.len() as u64);
        let whole = &want;
        let c = enc.encode(&f, None).unwrap();
        assert!(aqr::decode(&c, &Native::default()).unwrap() == *whole);
        assert!(decode_wasm(&c).unwrap() == *whole);
        let r = f.streams.iter().position(|s| s.class == "lzx/record").unwrap();
        for k in 0..8 {
            let mut bad = f.clone();
            let s = &mut bad.streams[r].data;
            let at = s.len() * k / 8;
            if k == 7 {
                s[at..].fill(0xff);
            } else if at < s.len() {
                s[at] ^= 1 << k;
            }
            let c = enc.encode(&bad, None).unwrap();
            let (n, w) = (aqr::decode(&c, &Native::default()), decode_wasm(&c));
            assert_eq!(n.is_ok(), w.is_ok(), "{k}: {n:?} {w:?}");
            if let (Ok(n), Ok(w)) = (n, w) {
                assert!(n == w, "{k}");
            }
        }
    }
}

/// A CAB stream of LZX (aqr-lzx's fixture: a window of 64 KiB, e8) natively and as WebAssembly
/// alike, and its record damaged in turn failing alike (or giving the same).
#[test]
fn lzx_cab_same_as_wasm() {
    let enc = Encoder::new(Level::Zstd(3)).with_modules(wasm_of);
    let s = fixture("codecs/aqr-lzx/tests/data/greedy.cab-lzx");
    let v = aqr_lzx::lzx::Variant::cab(s[0]).unwrap();
    let n = u32::from_le_bytes(s[1..5].try_into().unwrap()) as usize;
    let (mut src, mut at) = (Vec::new(), 5);
    while at < s.len() {
        let len = u32::from_le_bytes(s[at..at + 4].try_into().unwrap()) as usize;
        src.extend_from_slice(&s[at + 4..at + 4 + len]);
        at += 4 + len;
    }
    let sp = aqr_lzx::recons::split_stream(&src, n, v).unwrap();
    let mut f = Formula::default();
    f.push("raw", sp.data.clone());
    let stream = aqr_lzx::formula::Stream {
        kept: Node::Concat(Vec::new()),
        data: Node::slice(0, 0, n as u64),
        record: sp.record,
    };
    let a = aqr_lzx::formula::apply_as(&mut f, v, vec![stream], Node::Concat(Vec::new()), src.len() as u64);
    f.root = Node::Apply(Box::new(a));
    let c = enc.encode(&f, None).unwrap();
    assert!(aqr::decode(&c, &Native::default()).unwrap() == src);
    assert!(decode_wasm(&c).unwrap() == src);
    let r = f.streams.iter().position(|s| s.class == "lzx/record").unwrap();
    for k in 0..8 {
        let mut bad = f.clone();
        let s = &mut bad.streams[r].data;
        let at = s.len() * k / 8;
        s[at] ^= 1 << k;
        let c = enc.encode(&bad, None).unwrap();
        let (n, w) = (aqr::decode(&c, &Native::default()), decode_wasm(&c));
        assert_eq!(n.is_ok(), w.is_ok(), "{k}: {n:?} {w:?}");
        if let (Ok(n), Ok(w)) = (n, w) {
            assert!(n == w, "{k}");
        }
    }
}

/// CAB files of MSZIP and stored folders (aqr-mszip's fixtures) natively and as WebAssembly
/// alike, and their bytes kept damaged in turn failing alike (or giving the same).
#[test]
fn mszip_same_as_wasm() {
    let enc = Encoder::new(Level::Zstd(3)).with_modules(wasm_of);
    for name in ["mixed", "fixed"] {
        let d = fixture(&format!("codecs/aqr-mszip/tests/data/{name}.cab"));
        let f = aqr_mszip::formula::encode(&d, 1).unwrap();
        let c = enc.encode(&f, None).unwrap();
        assert!(aqr::decode(&c, &Native::default()).unwrap() == d, "{name}");
        assert!(decode_wasm(&c).unwrap() == d, "{name}");
        let i = f.streams.iter().position(|s| s.class == "mszip/index").unwrap();
        for k in 0..8 {
            let mut bad = f.clone();
            let s = &mut bad.streams[i].data;
            let at = s.len() * k / 8;
            s[at] ^= 1 << k;
            let c = enc.encode(&bad, None).unwrap();
            let (n, w) = (aqr::decode(&c, &Native::default()), decode_wasm(&c));
            assert_eq!(n.is_ok(), w.is_ok(), "{name} {k}: {n:?} {w:?}");
            if let (Ok(n), Ok(w)) = (n, w) {
                assert!(n == w, "{name} {k}");
            }
        }
    }
}

/// A module written by hand, of `wat`.
fn module(wat: &str) -> Vec<u8> {
    wat::parse_str(wat).unwrap()
}

/// `wat` loaded ([`Wasm`]), and loaded alike in the sandbox.
fn load(wat: &str) -> aqr::Decoded<Box<dyn Loaded>> {
    let m = module(wat);
    let l = Wasm.load(&m);
    #[cfg(feature = "sandbox")]
    assert_eq!(SANDBOX.load(&m).map(drop), l.as_ref().map(drop).map_err(Clone::clone), "in the sandbox");
    l
}

/// `wat` applied ([`Wasm`]), and applied alike in the sandbox.
fn apply(wat: &str, inputs: &[&[u8]], sizes: &[u64], limit: u64) -> aqr::Decoded<Vec<Vec<u8>>> {
    apply_v2(wat, inputs, sizes, limit, &[])
}

/// [`apply`] with task limits.
fn apply_v2(wat: &str, inputs: &[&[u8]], sizes: &[u64], limit: u64, tasks: &[u64]) -> aqr::Decoded<Vec<Vec<u8>>> {
    let r = load(wat)?.apply(b"pp", &[], inputs, sizes, limit, tasks);
    #[cfg(feature = "sandbox")]
    {
        let s = SANDBOX.load(&module(wat))?.apply(b"pp", &[], inputs, sizes, limit, tasks);
        assert!(s == r, "in the sandbox: {:?}, in this process: {:?}", s.as_ref().err(), r.as_ref().err());
    }
    r
}

/// [`apply`] of a module as one of a release is, with no task limits and the earlier `aqr_spawn`.
fn apply_earlier(wat: &str, inputs: &[&[u8]], sizes: &[u64], limit: u64) -> aqr::Decoded<Vec<Vec<u8>>> {
    load_earlier(wat)?.apply(b"pp", &[], inputs, sizes, limit, &[])
}

/// [`load`] of a module as one of a release.
fn load_earlier(wat: &str) -> aqr::Decoded<Box<dyn Loaded>> {
    aqr_decompress::load_released(&module(wat))
}

const MEMORIES: &str = r#"
    (import "aqr" "aqr_memory" (memory 1))
    (import "aqr" "aqr_params" (memory i64 0))
    (import "aqr" "aqr_input0" (memory i64 0))
    (import "aqr" "aqr_output0" (memory i64 0))
"#;

fn invalid<T>(r: aqr::Decoded<T>, what: &str) {
    match r {
        Err(Failure::Invalid(m)) => assert!(m.contains(what), "{m}"),
        Err(f) => panic!("{f:?}"),
        Ok(_) => panic!("not a failure"),
    }
}

/// Whole modules: the input copied to the output, the parameters after it, with the memories
/// copied between directly; traps and logs.
#[test]
fn whole() {
    let cat = format!(
        r#"(module {MEMORIES}
            (import "aqr" "aqr_log" (func $log (param i32 i32)))
            (data (memory 0) (i32.const 0) "failed")
            (func (export "aqr_apply_whole") (param $p i64) (param $i i64) (param $o i64)
                (if (i64.ne (local.get $o) (i64.add (local.get $p) (local.get $i)))
                    (then (call $log (i32.const 0) (i32.const 6)) unreachable))
                (memory.copy 3 2 (i64.const 0) (i64.const 0) (local.get $i))
                (memory.copy 3 1 (local.get $i) (i64.const 0) (local.get $p))))"#
    );
    assert_eq!(apply(&cat, &[b"abc"], &[5], u64::MAX).unwrap(), [b"abcpp"]);
    assert_eq!(apply(&cat, &[&[7; 70_000]], &[70_002], u64::MAX).unwrap()[0].len(), 70_002);
    invalid(apply(&cat, &[b"abc"], &[4], u64::MAX), "unreachable");
    invalid(apply(&cat, &[b"abc"], &[4], u64::MAX), "failed");
    // an access out of bounds, of a 32-bit memory: a fault on its guard pages, caught
    let oob = format!(
        r#"(module {MEMORIES}
            (func (export "aqr_apply_whole") (param i64 i64 i64)
                (drop (i32.load (i32.const 0x20000)))))"#
    );
    invalid(apply(&oob, &[b"abc"], &[5], u64::MAX), "out of bounds");
    // the entry point's arity is that of the application
    invalid(apply(&cat, &[b"abc", b""], &[5], u64::MAX), "parameters");
    invalid(apply(&cat, &[], &[5], u64::MAX), "parameters");
    // the memories: 1 + 1 + 1 + 1 pages
    assert!(apply(&cat, &[b"abc"], &[5], 4 << 16).is_ok());
    assert!(matches!(apply(&cat, &[b"abc"], &[5], (4 << 16) - 1), Err(Failure::Resource(_))));
    // an output not imported is zeros
    let none = r#"(module (import "aqr" "aqr_memory" (memory 0))
        (func (export "aqr_apply_whole") (param i64 i64 i64)))"#;
    assert_eq!(apply(none, &[b"abc"], &[2], u64::MAX).unwrap(), [[0, 0]]);
}

/// memory.grow within the limit, and past it a resource failure.
#[test]
fn grow() {
    let grow = r#"(module (import "aqr" "aqr_memory" (memory 1))
        (func (export "aqr_apply_whole") (param i64 i64) (param $n i64)
            (drop (memory.grow (i32.wrap_i64 (local.get $n))))))"#;
    assert!(apply(grow, &[b""], &[3], 4 << 16).is_ok());
    assert!(matches!(apply(grow, &[b""], &[4], 4 << 16), Err(Failure::Resource(_))));
}

/// Part modules: an input streamed to the output a byte at a time through windows, by
/// `aqr_request`, `aqr_release` and `aqr_commit`; their violations.
#[test]
fn part() {
    let copy = format!(
        r#"(module {MEMORIES}
            (import "aqr" "aqr_request" (func $request (param i32 i64)))
            (import "aqr" "aqr_release" (func $release (param i32 i64)))
            (import "aqr" "aqr_commit" (func $commit (param i32 i64)))
            (func (export "aqr_apply_part") (param i64) (param $n i64) (param i64)
                (local $o i64)
                (loop $next
                    (if (i64.lt_u (local.get $o) (local.get $n)) (then
                        (call $request (i32.const 0) (i64.add (local.get $o) (i64.const 1)))
                        (i64.store8 3 (i64.rem_u (local.get $o) (i64.shl (memory.size 3) (i64.const 16)))
                            (i64.load8_u 2 (i64.rem_u (local.get $o) (i64.shl (memory.size 2) (i64.const 16)))))
                        (call $release (i32.const 0) (i64.add (local.get $o) (i64.const 1)))
                        (call $commit (i32.const 0) (i64.add (local.get $o) (i64.const 1)))
                        (local.set $o (i64.add (local.get $o) (i64.const 1)))
                        (br $next))))))"#
    );
    let d: Vec<u8> = (0..200_000u32).map(|i| (i * 7) as u8).collect();
    assert!(apply(&copy, &[&d], &[d.len() as u64], u64::MAX).unwrap()[0] == d);
    // committing past the size declared
    invalid(apply(&copy, &[&d], &[10], u64::MAX), "aqr_commit");
    let early = format!(
        r#"(module {MEMORIES}
            (import "aqr" "aqr_request" (func $request (param i32 i64)))
            (func (export "aqr_apply_part") (param i64 i64 i64)
                (call $request (i32.const 0) (i64.const 70000))))"#
    );
    // past the input
    invalid(apply(&early, &[&d[..60_000]], &[1], u64::MAX), "aqr_request");
    // through windows of a page (ring buffers), which the memory limit counts rather than the
    // data; of a whole module, none
    let windowed = copy
        .replace(r#""aqr_input0" (memory i64 0)"#, r#""aqr_input0" (memory i64 0 1)"#)
        .replace(r#""aqr_output0" (memory i64 0)"#, r#""aqr_output0" (memory i64 0 1)"#);
    assert!(apply(&windowed, &[&d], &[d.len() as u64], 4 << 16).unwrap()[0] == d);
    assert!(matches!(apply(&copy, &[&d], &[d.len() as u64], 4 << 16), Err(Failure::Resource(_))));
    let whole = format!(
        r#"(module {}
            (func (export "aqr_apply_whole") (param i64 i64 i64)))"#,
        MEMORIES.replace(r#""aqr_input0" (memory i64 0)"#, r#""aqr_input0" (memory i64 0 1)"#)
    );
    invalid(apply(&whole, &[b"x"], &[1], u64::MAX), "maximum");

    // streamed (Loaded::apply_part): through windows of a page, its input pulled as requested
    // and its output pushed as committed, the first bytes pushed before most of the input is
    // pulled; a failure to pull is the application's
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    struct Feed<'a>(&'a [u8], Arc<AtomicU64>, Option<Failure>);
    impl aqr::PartIn for Feed<'_> {
        fn len(&self) -> u64 {
            self.0.len() as u64
        }
        fn pull(&mut self, hi: u64, put: &mut dyn FnMut(&[u8])) -> aqr::Decoded<()> {
            if let Some(f) = &self.2 {
                return Err(f.clone());
            }
            put(&self.0[self.1.load(Relaxed) as usize..hi as usize]);
            self.1.store(hi, Relaxed);
            Ok(())
        }
    }
    // (the bytes pushed, and how much of the input was pulled as the first were)
    struct Drain(Vec<u8>, Arc<AtomicU64>, Option<u64>);
    impl aqr::PartOut for Drain {
        fn push(&mut self, d: &[u8]) -> aqr::Decoded<()> {
            self.2.get_or_insert(self.1.load(Relaxed));
            self.0.extend_from_slice(d);
            Ok(())
        }
    }
    let l = load(&windowed).unwrap();
    assert!(l.is_part());
    let pulled = Arc::new(AtomicU64::new(0));
    let (mut feed, mut drain) = (Feed(&d, pulled.clone(), None), Drain(Vec::new(), pulled, None));
    l.apply_part(b"pp", &[], &mut [&mut feed], &mut [&mut drain], &[d.len() as u64], 4 << 16, &[]).unwrap();
    assert!(drain.0 == d);
    assert!(drain.2.is_some_and(|n| n < 1 << 16), "{:?}", drain.2);
    let upstream = Failure::Invalid("upstream".into());
    let mut feed = Feed(&d, Arc::default(), Some(upstream.clone()));
    let mut drain = Drain(Vec::new(), Arc::default(), None);
    let r = l.apply_part(b"pp", &[], &mut [&mut feed], &mut [&mut drain], &[d.len() as u64], 4 << 16, &[]);
    assert_eq!(r, Err(upstream));
}

/// Large inputs and outputs (in the sandbox's arenas, as it grows them and uses them again),
/// and small ones about them; failing, and sizes declared beyond the memory limit.
#[test]
fn large() {
    let cat = format!(
        r#"(module {MEMORIES}
            (func (export "aqr_apply_whole") (param $p i64) (param $i i64) (param $o i64)
                (if (i64.ne (local.get $o) (i64.add (local.get $p) (local.get $i))) (then unreachable))
                (memory.copy 3 2 (i64.const 0) (i64.const 0) (local.get $i))
                (memory.copy 3 1 (local.get $i) (i64.const 0) (local.get $p))))"#
    );
    let d: Vec<u8> = (0..3u32 << 20).map(|i| (i * 7 + (i >> 13)) as u8).collect();
    let cat_of = |n: usize| [&d[..n], b"pp"].concat();
    // about where they go in the arena (inputs and outputs of 64 KiB together)
    for n in [32 * 1024 - 2, 32 * 1024 - 1, 32 * 1024, 3 << 20, 1 << 20, 100, 2 << 20] {
        let out = apply(&cat, &[&d[..n]], &[n as u64 + 2], u64::MAX).unwrap();
        assert!(out == [cat_of(n)], "{n}");
    }
    invalid(apply(&cat, &[&d[..1 << 20]], &[1 << 20], u64::MAX), "unreachable");
    assert!(apply(&cat, &[&d[..1 << 20]], &[(1 << 20) + 2], u64::MAX).unwrap() == [cat_of(1 << 20)]);
    // checked before anything is made
    let r = apply(&cat, &[b"abc"], &[1 << 50], 1 << 30);
    assert!(matches!(r, Err(Failure::Resource(ref m)) if m.contains("memory limit")), "{r:?}");
}

/// Modules not of the profile.
#[test]
fn profile() {
    let entry = r#"(func (export "aqr_apply_whole") (param i64 i64 i64))"#;
    let of = |rest: &str| load(&format!("(module {rest} {entry})"));
    invalid(of(r#"(memory 1)"#), "a memory defined");
    invalid(of(r#"(import "aqr" "aqr_memory" (memory 1)) (memory 1)"#), "");
    invalid(of(r#"(import "env" "aqr_memory" (memory 1))"#), "an import from env");
    invalid(of(r#"(import "aqr" "aqr_memory" (memory i64 1))"#), "64-bit working memory");
    invalid(of(r#"(import "aqr" "aqr_memory" (memory 1)) (import "aqr" "aqr_input0" (memory 0))"#), "64-bit");
    invalid(of(r#"(import "aqr" "aqr_memory" (memory 1)) (import "aqr" "aqr_input0" (memory i64 1))"#), "minimum 0");
    invalid(of(r#"(import "aqr" "aqr_memory" (memory 1)) (import "aqr" "aqr_input01" (memory i64 0))"#), "an import");
    invalid(of(r#"(import "aqr" "aqr_memory" (memory 1)) (import "aqr" "aqr_memory" (memory 1))"#), "twice");
    invalid(of(r#"(import "aqr" "aqr_memory" (memory 1)) (import "aqr" "aqr_log" (func (param i32)))"#), "type");
    invalid(
        of(r#"(import "aqr" "aqr_memory" (memory 1)) (import "aqr" "aqr_commit" (func (param i32 i64)))"#),
        "whole",
    );
    invalid(of(r#"(import "aqr" "aqr_memory" (memory 1)) (import "aqr" "t" (table 1 funcref))"#), "table");
    invalid(of(r#""#), "no working memory");
    invalid(load(r#"(module (import "aqr" "aqr_memory" (memory 1)))"#), "no entry point");
    invalid(
        load(r#"(module (import "aqr" "aqr_memory" (memory 1)) (func (export "aqr_apply_whole") (param i32)))"#),
        "i64",
    );
    invalid(
        load(&format!(
            r#"(module {MEMORIES}
                (func (export "aqr_apply_whole") (param i64 i64 i64) (drop (memory.grow 3 (i64.const 1)))))"#
        )),
        "memory.grow",
    );
    // tail calls are of 3.0
    invalid(
        load(
            r#"(module (import "aqr" "aqr_memory" (memory 1)) (func $f (return_call $f))
            (func (export "aqr_apply_whole") (param i64 i64 i64)))"#,
        ),
        "not a module of the profile",
    );
    assert!(of(r#"(import "aqr" "aqr_memory" (memory 1))"#).is_ok());
}

/// An archive of a module not known runs as WebAssembly, with [`Native`] too.
#[test]
fn unknown_module_runs() {
    let rev = module(&format!(
        r#"(module {MEMORIES}
            (func (export "aqr_apply_whole") (param i64) (param $n i64) (param i64)
                (local $o i64)
                (loop $next
                    (if (i64.lt_u (local.get $o) (local.get $n)) (then
                        (i64.store8 3 (i64.sub (i64.sub (local.get $n) (local.get $o)) (i64.const 1))
                            (i64.load8_u 2 (local.get $o)))
                        (local.set $o (i64.add (local.get $o) (i64.const 1)))
                        (br $next))))))"#
    ));
    const REV: Module = Module::new("rev", 1);
    static WASM: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    WASM.set(rev).unwrap();
    let mut f = Formula::default();
    let a = f.push("a", b"hello".to_vec());
    f.root = Node::apply(REV, b"pp".to_vec(), vec![a], 5);
    let enc =
        Encoder::new(Level::Zstd(3)).with_modules(|m| (m == REV).then(|| aqr::Wasm::Bytes(WASM.get().unwrap().into())));
    let c = enc.encode(&f, None).unwrap();
    assert_eq!(aqr::decode(&c, &Native::default()).unwrap(), b"olleh");
    #[cfg(feature = "sandbox")]
    {
        let sandboxed = aqr_decompress::sandbox::Sandboxed { native: Native::default(), sandbox: &SANDBOX };
        assert_eq!(aqr::decode(&c, &sandboxed).unwrap(), b"olleh");
    }
    assert!(Encoder::new(Level::Zstd(3)).with_modules(wasm_of).encode(&f, None).is_err());
}

/// The imports of tasks of the earlier `aqr_spawn` (two parameters), which modules of releases have.
const TASKS: &str = r#"
    (@custom "aqr_task_slots" "\08")
    (import "aqr" "aqr_spawn" (func $spawn (param i32 i32) (result i32)))
    (import "aqr" "aqr_result" (func $result (param i32 i32)))
    (import "aqr" "aqr_join" (func $join (param i32 i32) (result i32)))
    (import "aqr" "aqr_take" (func $take (param i32 i32 i32 i32)))
"#;

/// A task per byte of the input, each giving the byte plus 1 and zeroing it in its copy of the
/// working memory; the output is their results, then the input as the module has it (as it was).
fn incr(entry_tail: &str) -> String {
    format!(
        r#"(module {MEMORIES} {TASKS}
            (func (export "aqr_task") (param $arg i32) (param $i i32)
                ;; the other memories are empty to tasks
                (if (i64.ne (memory.size 2) (i64.const 0)) (then unreachable))
                (if (i32.eq (i32.load8_u (i32.add (local.get $arg) (local.get $i))) (i32.const 0x21))
                    (then unreachable))
                (i32.store8 (i32.add (i32.const 0x8000) (local.get $i))
                    (i32.add (i32.load8_u (i32.add (local.get $arg) (local.get $i))) (i32.const 1)))
                (i32.store8 (i32.add (local.get $arg) (local.get $i)) (i32.const 0))
                (call $result (i32.add (i32.const 0x8000) (local.get $i)) (i32.const 1)))
            (func (export "aqr_apply_whole") (param i64) (param $n i64) (param i64)
                (local $h i32) (local $i i32)
                (memory.copy 0 2 (i32.const 0x100) (i64.const 0) (i32.wrap_i64 (local.get $n)))
                (local.set $h (call $spawn (i32.const 0x100) (i32.wrap_i64 (local.get $n))))
                (loop $next (if (i32.lt_u (local.get $i) (i32.wrap_i64 (local.get $n))) (then
                    (if (i32.ne (call $join (local.get $h) (local.get $i)) (i32.const 1)) (then unreachable))
                    (call $take (local.get $h) (local.get $i) (i32.add (i32.const 0x4000) (local.get $i)) (i32.const 1))
                    (local.set $i (i32.add (local.get $i) (i32.const 1)))
                    (br $next))))
                (memory.copy 3 0 (i64.const 0) (i32.const 0x4000) (i32.wrap_i64 (local.get $n)))
                (memory.copy 3 0 (local.get $n) (i32.const 0x100) (i32.wrap_i64 (local.get $n)))
                {entry_tail}))"#
    )
}

/// Tasks: each of a copy of the working memory at the spawn, giving back only its result; their
/// failures, violations and memory.
#[test]
fn tasks() {
    let ok = incr("");
    assert_eq!(apply_earlier(&ok, &[b"abc"], &[6], u64::MAX).unwrap(), [b"bcdabc"]);
    assert_eq!(apply_earlier(&ok, &[b""], &[0], u64::MAX).unwrap(), [b""]);
    let many: Vec<u8> = (0..200u8).map(|b| b % 32 + 64).collect();
    let want: Vec<u8> = many.iter().map(|b| b + 1).chain(many.iter().copied()).collect();
    assert_eq!(apply_earlier(&ok, &[&many], &[400], u64::MAX).unwrap(), [want]);
    // a task trapping ('!'), as the application does
    invalid(apply_earlier(&ok, &[b"a!c"], &[6], u64::MAX), "trap");
    // the memory of the tasks counts: 4 pages for the module's memories, and one per task
    assert!(apply_earlier(&ok, &[b"a"], &[2], 5 << 16).is_ok());
    assert!(matches!(apply_earlier(&ok, &[b"a"], &[2], 4 << 16), Err(Failure::Resource(_))));

    // results taken but for one, a result outside a task, a take before the join
    let skip = incr("")
        .replace("(i32.lt_u (local.get $i) (i32.wrap_i64 (local.get $n)))", "(i32.lt_u (local.get $i) (i32.const 1))");
    invalid(apply_earlier(&skip, &[b"ab"], &[4], u64::MAX), "tasks not taken");
    invalid(
        apply_earlier(&incr("(call $result (i32.const 0) (i32.const 1))"), &[b"a"], &[2], u64::MAX),
        "outside a task",
    );
    let early = incr("").replace(
        "(if (i32.ne (call $join",
        "(call $take (local.get $h) (i32.const 0) (i32.const 0) (i32.const 0)) (if (i32.ne (call $join",
    );
    invalid(apply_earlier(&early, &[b"a"], &[2], u64::MAX), "not joined");

    // the profile: tasks need aqr_task, and nothing starts
    invalid(load_earlier(&incr("").replace(r#"(export "aqr_task")"#, "")), "without aqr_task");
    invalid(
        load_earlier(
            r#"(module (import "aqr" "aqr_memory" (memory 1)) (func $s) (start $s)
            (func (export "aqr_apply_whole") (param i64 i64 i64)))"#,
        ),
        "start",
    );
}

/// A task's working memory is the spawner's, not what the data segments wrote: a page they
/// wrote, zeroed before the spawn, is of zeros to the task, and a page written past them is of
/// what was written.
#[test]
fn tasks_over_data() {
    let m = format!(
        r#"(module {MEMORIES} {TASKS}
            (data (memory 0) (i32.const 0x2000) "xyz")
            (func (export "aqr_task") (param $arg i32) (param $i i32)
                (i32.store8 (i32.const 0x8000) (i32.load8_u (i32.const 0x2001)))
                (i32.store8 (i32.const 0x8001) (i32.load8_u (i32.const 0x9000)))
                (call $result (i32.const 0x8000) (i32.const 2)))
            (func (export "aqr_apply_whole") (param i64) (param i64) (param i64)
                (local $h i32)
                (if (i32.ne (i32.load8_u (i32.const 0x2001)) (i32.const 0x79)) (then unreachable))
                (memory.fill 0 (i32.const 0x2000) (i32.const 0) (i32.const 0x1000))
                (i32.store8 (i32.const 0x9000) (i32.const 7))
                (local.set $h (call $spawn (i32.const 0) (i32.const 1)))
                (if (i32.ne (call $join (local.get $h) (i32.const 0)) (i32.const 2)) (then unreachable))
                (call $take (local.get $h) (i32.const 0) (i32.const 0x4000) (i32.const 2))
                (memory.copy 3 0 (i64.const 0) (i32.const 0x4000) (i32.const 2))))"#
    );
    assert_eq!(apply_earlier(&m, &[b""], &[2], u64::MAX).unwrap(), [[0, 7]]);
}

/// The imports of channels.
const CHANS: &str = r#"
    (import "aqr" "aqr_chan" (func $chan (param i32) (result i32)))
    (import "aqr" "aqr_send" (func $send (param i32 i32 i32)))
    (import "aqr" "aqr_recv" (func $recv (param i32 i32 i32) (result i32)))
    (import "aqr" "aqr_close" (func $close (param i32)))
"#;

/// The input through a pipeline of tasks and channels of 2 messages, a byte a message: task 0
/// sends each byte plus 1 to channel 0 and closes it, task 1 sends each it receives from channel
/// 0 plus 1 to channel 1 and closes it, which the module receives into the output. `before` runs before the tasks are
/// spawned, `after` after.
fn piped(before: &str, after: &str) -> String {
    format!(
        r#"(module {MEMORIES} {TASKS} {CHANS}
            (func (export "aqr_task") (param $arg i32) (param $i i32)
                (local $k i32) (local $n i32) (local $c0 i32) (local $c1 i32)
                (local.set $c0 (i32.load (local.get $arg)))
                (local.set $c1 (i32.load offset=4 (local.get $arg)))
                (local.set $n (i32.load offset=8 (local.get $arg)))
                (if (i32.eqz (local.get $i))
                    (then
                        (loop $next (if (i32.lt_u (local.get $k) (local.get $n)) (then
                            (i32.store8 (i32.const 0x9000)
                                (i32.add (i32.load8_u offset=12 (i32.add (local.get $arg) (local.get $k))) (i32.const 1)))
                            (call $send (local.get $c0) (i32.const 0x9000) (i32.const 1))
                            (local.set $k (i32.add (local.get $k) (i32.const 1)))
                            (br $next))))
                        ;; (an end closes only a channel it sent to: not one of no message)
                        (call $close (local.get $c0)))
                    (else
                        (loop $next
                            (if (i32.eq (call $recv (local.get $c0) (i32.const 0x9000) (i32.const 1)) (i32.const 1)) (then
                                (i32.store8 (i32.const 0x9000) (i32.add (i32.load8_u (i32.const 0x9000)) (i32.const 1)))
                                (call $send (local.get $c1) (i32.const 0x9000) (i32.const 1))
                                (br $next))))
                        (call $close (local.get $c1)))))
            (func (export "aqr_apply_whole") (param i64) (param $n i64) (param i64)
                (local $h i32) (local $o i32) (local $r i32)
                (i32.store (i32.const 0x100) (call $chan (i32.const 2)))
                (i32.store (i32.const 0x104) (call $chan (i32.const 2)))
                (i32.store (i32.const 0x108) (i32.wrap_i64 (local.get $n)))
                (memory.copy 0 2 (i32.const 0x10c) (i64.const 0) (i32.wrap_i64 (local.get $n)))
                {before}
                (local.set $h (call $spawn (i32.const 0x100) (i32.const 2)))
                {after}
                (loop $next
                    (local.set $r (call $recv (i32.load (i32.const 0x104)) (i32.add (i32.const 0x4000) (local.get $o)) (i32.const 1)))
                    (if (i32.ge_s (local.get $r) (i32.const 0)) (then
                        (local.set $o (i32.add (local.get $o) (local.get $r)))
                        (br $next))))
                (drop (call $join (local.get $h) (i32.const 0)))
                (drop (call $join (local.get $h) (i32.const 1)))
                (call $take (local.get $h) (i32.const 0) (i32.const 0) (i32.const 0))
                (call $take (local.get $h) (i32.const 1) (i32.const 0) (i32.const 0))
                (memory.copy 3 0 (i64.const 0) (i32.const 0x4000) (local.get $o))))"#
    )
}

/// Channels between tasks: a pipeline, its senders and receivers waiting on each other; too
/// small a buffer; the ends of a channel; a deadlock.
#[test]
fn channels() {
    let ok = piped("", "");
    assert_eq!(apply_earlier(&ok, &[b"abc"], &[3], u64::MAX).unwrap(), [b"cde"]);
    assert_eq!(apply_earlier(&ok, &[b""], &[0], u64::MAX).unwrap(), [b""]);
    let many: Vec<u8> = (0..3000u32).map(|i| (i % 200) as u8).collect();
    let want: Vec<u8> = many.iter().map(|b| b + 2).collect();
    for _ in 0..20 {
        assert_eq!(apply_earlier(&ok, &[&many], &[3000], u64::MAX).unwrap(), std::slice::from_ref(&want));
    }
    // a message longer than the buffer stays, its length told
    let small = piped(
        "",
        "(if (i32.ne (call $recv (i32.load (i32.const 0x104)) (i32.const 0x4000) (i32.const 0)) (i32.const 1)) (then unreachable))",
    );
    assert_eq!(apply_earlier(&small, &[b"ab"], &[2], u64::MAX).unwrap(), [b"cd"]);
    // the module sending to the channel the task 0 sends to
    let two = piped("(call $send (i32.load (i32.const 0x100)) (i32.const 0x10c) (i32.const 1))", "");
    invalid(apply_earlier(&two, &[b"ab"], &[2], u64::MAX), "two instances");
    // the module receiving from the channel the task 1 receives from
    let two = piped("", "(drop (call $recv (i32.load (i32.const 0x100)) (i32.const 0x4000) (i32.const 1)))");
    invalid(apply_earlier(&two, &[b"ab"], &[2], u64::MAX), "two instances");
    // waiting for a channel nobody sends to
    let dead = format!(
        r#"(module {MEMORIES} {TASKS} {CHANS}
            (func (export "aqr_task") (param i32 i32))
            (func (export "aqr_apply_whole") (param i64 i64 i64)
                (drop (call $recv (call $chan (i32.const 1)) (i32.const 0) (i32.const 0)))))"#
    );
    invalid(apply_earlier(&dead, &[b""], &[0], u64::MAX), "deadlock");
    // a task waiting so, joined
    let dead = piped("(i32.store (i32.const 0x108) (i32.const 0))", "").replace(
        "(call $close (local.get $c1))",
        "(drop (call $recv (call $chan (i32.const 1)) (i32.const 0) (i32.const 0)))",
    );
    invalid(apply_earlier(&dead, &[b"ab"], &[0], u64::MAX), "deadlock");
}

/// A task per byte of the input, task `i` receiving its byte from channel `i` (which the module
/// sends to after the spawn) and giving it back; the tasks of `waiting` (none if `None`) wait
/// instead for a message from the last task, which sends one to each of them.
fn slotted(waiting: Option<usize>) -> String {
    let wait = match waiting {
        None => String::new(),
        Some(w) => format!(
            r#"(if (i32.lt_u (local.get $i) (i32.const {w}))
                    (then (drop (call $recv (i32.add (i32.const 0x40) (local.get $i)) (i32.const 0x8000) (i32.const 1)))))
                (if (i32.eq (local.get $i) (i32.const {w}))
                    (then (loop $next (if (i32.lt_u (local.get $k) (i32.const {w})) (then
                        (call $send (i32.add (i32.const 0x40) (local.get $k)) (i32.const 0x8000) (i32.const 1))
                        (local.set $k (i32.add (local.get $k) (i32.const 1)))
                        (br $next))))))"#
        ),
    };
    format!(
        r#"(module {MEMORIES} {TASKS} {CHANS}
            (func (export "aqr_task") (param $arg i32) (param $i i32)
                (local $k i32)
                {wait}
                (drop (call $recv (local.get $i) (i32.const 0x8000) (i32.const 1)))
                (call $result (i32.const 0x8000) (i32.const 1)))
            (func (export "aqr_apply_whole") (param i64) (param $n i64) (param i64)
                (local $h i32) (local $i i32)
                (memory.copy 0 2 (i32.const 0x100) (i64.const 0) (i32.wrap_i64 (local.get $n)))
                ;; channels 0 to 0x7f: the inputs' and those of the waiting
                (loop $next (if (i32.lt_u (local.get $i) (i32.const 0x80)) (then
                    (drop (call $chan (i32.const 1)))
                    (local.set $i (i32.add (local.get $i) (i32.const 1)))
                    (br $next))))
                (local.set $h (call $spawn (i32.const 0) (i32.wrap_i64 (local.get $n))))
                (local.set $i (i32.const 0))
                (loop $next (if (i32.lt_u (local.get $i) (i32.wrap_i64 (local.get $n))) (then
                    (call $send (local.get $i) (i32.add (i32.const 0x100) (local.get $i)) (i32.const 1))
                    (local.set $i (i32.add (local.get $i) (i32.const 1)))
                    (br $next))))
                (local.set $i (i32.const 0))
                (loop $next (if (i32.lt_u (local.get $i) (i32.wrap_i64 (local.get $n))) (then
                    (drop (call $join (local.get $h) (local.get $i)))
                    (call $take (local.get $h) (local.get $i) (i32.add (i32.const 0x4000) (local.get $i)) (i32.const 1))
                    (local.set $i (i32.add (local.get $i) (i32.const 1)))
                    (br $next))))
                (memory.copy 3 0 (i64.const 0) (i32.const 0x4000) (i32.wrap_i64 (local.get $n)))))"#
    )
}

/// At most as many of the tasks an instance spawns as the module's task slots (8 here) run at
/// once, the others started in order as they end: 16 tasks waiting for their messages fit in the
/// memory of 8, and 8 tasks waiting for a ninth never see it start (a deadlock), as 7 waiting for
/// an eighth do; with 2 slots, 2 tasks waiting for a third do not.
#[test]
fn task_slots() {
    let input: Vec<u8> = (0..16).map(|b| b + 0x41).collect();
    let all = slotted(None);
    // 4 pages for the module's memories, 1 a task running and the messages queued (16 tasks at
    // once would take 20 pages)
    let limit = (4 + 8) << 16 | 0x100;
    assert_eq!(apply_earlier(&all, &[&input], &[16], limit).unwrap(), std::slice::from_ref(&input));
    assert_eq!(apply_earlier(&slotted(Some(7)), &[&input[..8]], &[8], u64::MAX).unwrap(), [&input[..8]]);
    invalid(apply_earlier(&slotted(Some(8)), &[&input[..9]], &[9], u64::MAX), "deadlock");
    let two = |w| slotted(w).replace(r#""aqr_task_slots" "\08""#, r#""aqr_task_slots" "\02""#);
    assert_eq!(apply_earlier(&two(Some(1)), &[&input[..2]], &[2], u64::MAX).unwrap(), [&input[..2]]);
    invalid(apply_earlier(&two(Some(2)), &[&input[..3]], &[3], u64::MAX), "deadlock");
    assert_eq!(apply_earlier(&two(None), &[&input], &[16], u64::MAX).unwrap(), std::slice::from_ref(&input));
}

/// Tasks of `aqr_spawn`: a task of class 0 per byte of the input, started from one region (the
/// input at 0x100, placed at 0x30000: past the module's page) and giving the byte plus 1, with
/// its memory size checked (5 pages: the region's end). `nested` has each task spawn one of class
/// `nested` first.
fn regions(nested: Option<u32>) -> String {
    let inner = match nested {
        Some(c) => {
            format!("(drop (call $spawn2 (i32.const {c}) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0)))")
        }
        None => String::new(),
    };
    format!(
        r#"(module {MEMORIES}
            (@custom "aqr_task_slots" "\04\02")
            (import "aqr" "aqr_spawn" (func $spawn2 (param i32 i32 i32 i32 i32) (result i32)))
            (import "aqr" "aqr_result" (func $result (param i32 i32)))
            (import "aqr" "aqr_join" (func $join (param i32 i32) (result i32)))
            (import "aqr" "aqr_take" (func $take (param i32 i32 i32 i32)))
            (func (export "aqr_task") (param $arg i32) (param $i i32)
                (if (i32.ne (memory.size 0) (i32.const 4)) (then unreachable))
                ;; the spawner's bytes at 0x100 are not there
                (if (i32.ne (i32.load8_u (i32.const 0x100)) (i32.const 0)) (then unreachable))
                {inner}
                (i32.store8 (i32.const 0x8000)
                    (i32.add (i32.load8_u (i32.add (i32.const 0x30000) (local.get $i))) (i32.const 1)))
                (call $result (i32.const 0x8000) (i32.const 1)))
            (func (export "aqr_apply_whole") (param i64) (param $n i64) (param i64)
                (local $h i32) (local $i i32)
                (memory.copy 0 2 (i32.const 0x100) (i64.const 0) (i32.wrap_i64 (local.get $n)))
                ;; the record: from 0x100, n bytes, to 0x30000
                (i32.store (i32.const 0x200) (i32.const 0x100))
                (i32.store (i32.const 0x204) (i32.wrap_i64 (local.get $n)))
                (i32.store (i32.const 0x208) (i32.const 0x30000))
                (local.set $h (call $spawn2 (i32.const 0) (i32.const 1) (i32.const 0x200) (i32.const 0)
                    (i32.wrap_i64 (local.get $n))))
                (loop $next (if (i32.lt_u (local.get $i) (i32.wrap_i64 (local.get $n))) (then
                    (if (i32.ne (call $join (local.get $h) (local.get $i)) (i32.const 1)) (then unreachable))
                    (call $take (local.get $h) (local.get $i) (i32.add (i32.const 0x4000) (local.get $i)) (i32.const 1))
                    (local.set $i (i32.add (local.get $i) (i32.const 1)))
                    (br $next))))
                (memory.copy 3 0 (i64.const 0) (i32.const 0x4000) (i32.wrap_i64 (local.get $n)))))"#
    )
}

/// `aqr_spawn`: tasks of the regions named, under their class's limit each with task limits
/// (and not in the instance's), spawning only classes above their own.
#[test]
fn tasks_of_regions() {
    let m = regions(None);
    assert_eq!(apply_v2(&m, &[b"abc"], &[3], u64::MAX, &[u64::MAX, u64::MAX]).unwrap(), [b"bcd"]);
    // a task is 4 pages, the instance 4 (its working memory, and the parameters, input and output a
    // page each)
    let pages = |n: u64| n << 16;
    assert_eq!(apply_v2(&m, &[b"abc"], &[3], pages(4), &[pages(4), 1]).unwrap(), [b"bcd"]);
    assert!(matches!(apply_v2(&m, &[b"abc"], &[3], pages(4), &[pages(3), 1]), Err(Failure::Resource(_))));
    // as many task limits as classes, none being no limits only for modules of releases
    invalid(apply_v2(&m, &[b"abc"], &[3], u64::MAX, &[u64::MAX]), "task limits");
    invalid(apply_v2(&m, &[b"abc"], &[3], u64::MAX, &[]), "task limits");
    // a task of class 0 spawning one of class 1, not of class 0
    let up = regions(Some(1));
    assert_eq!(apply_v2(&up, &[b"ab"], &[2], u64::MAX, &[u64::MAX, u64::MAX]).unwrap(), [b"bc"]);
    let same = regions(Some(0));
    invalid(apply_v2(&same, &[b"ab"], &[2], u64::MAX, &[u64::MAX, u64::MAX]), "spawning one of class 0");
    // a class past the module's
    invalid(apply_v2(&regions(Some(2)), &[b"ab"], &[2], u64::MAX, &[u64::MAX, u64::MAX]), "class 2");
}

/// `wat` (written for the earlier `aqr_spawn`, `$spawn` of an argument and a count) with the
/// current `aqr_spawn`, of one region of all of the working memory as aqr-api's tasks start,
/// and `$spawnc` of a class besides.
fn current(wat: &str) -> String {
    wat.replace(
        r#"(import "aqr" "aqr_spawn" (func $spawn (param i32 i32) (result i32)))"#,
        r#"(import "aqr" "aqr_spawn" (func $spawn5 (param i32 i32 i32 i32 i32) (result i32)))"#,
    )
    .replace(
        r#"(func (export "aqr_task")"#,
        r#"(func $spawnc (param $class i32) (param $arg i32) (param $n i32) (result i32)
                (i32.store (i32.const 0xf004) (i32.shl (memory.size 0) (i32.const 16)))
                (call $spawn5 (local.get $class) (i32.const 1) (i32.const 0xf000) (local.get $arg) (local.get $n)))
            (func $spawn (param $arg i32) (param $n i32) (result i32)
                (call $spawnc (i32.const 0) (local.get $arg) (local.get $n)))
            (func (export "aqr_task")"#,
    )
}

/// `incr`'s module (a task per byte) with the current `aqr_spawn`.
fn incr_spawn() -> String {
    current(&incr(""))
}

/// The current `aqr_spawn` (five parameters) is the only spawn of a module that is not of a release,
/// and a module with classes is applied with a task limit for each.
#[test]
fn spawn_of_one_region() {
    let m = incr_spawn();
    let pages = |n: u64| n << 16;
    // (the instance 4 pages; a task 1: its working memory)
    assert_eq!(apply_v2(&m, &[b"abc"], &[6], pages(4), &[pages(1)]).unwrap(), [b"bcdabc"]);
    assert!(matches!(apply_v2(&m, &[b"abc"], &[6], pages(4), &[pages(1) - 1]), Err(Failure::Resource(_))));
    // the instance's limit does not count the tasks'
    assert!(matches!(apply_v2(&m, &[b"abc"], &[6], pages(4) - 1, &[pages(1)]), Err(Failure::Resource(_))));
    // no task limits for a module with classes: not a valid application
    invalid(apply_v2(&m, &[b"abc"], &[6], u64::MAX, &[]), "task limits");
    // the earlier spawn of two parameters: an import of another type for a module not of a release
    // (which is run as before, without limits)
    let earlier = incr("");
    invalid(load(&earlier), "aqr_spawn of another type");
    assert_eq!(apply_earlier(&earlier, &[b"abc"], &[6], u64::MAX).unwrap(), [b"bcdabc"]);
    assert_eq!(
        load_earlier(&earlier).unwrap().apply(b"pp", &[], &[b"abc"], &[6], u64::MAX, &[pages(1)]).unwrap(),
        [b"bcdabc"]
    );
    // and aqr_spawn_v2 is no import at all, of a release or not
    let v2 = m.replace("\"aqr_spawn\"", "\"aqr_spawn_v2\"");
    invalid(load(&v2), "aqr_spawn_v2");
    invalid(load_earlier(&v2), "aqr_spawn_v2");
}

/// The pipeline of `piped` as one gang (the class 0 a gang class of a slot): the channels made
/// before the spawn are the gang's, and its two tasks start together.
fn gang_piped(before: &str, after: &str) -> String {
    current(&piped(before, after)).replace(r#""aqr_task_slots" "\08""#, r#""aqr_task_slots" "\81""#)
}

/// Gangs: the channels made belong to the gang of the next spawn of the instance, which must be of
/// a gang class; only the gang's tasks and the instance that spawned it take their ends, not both
/// of a channel, and what they hold (the tasks' memories and the messages queued) is the gang's
/// limit.
#[test]
fn gangs() {
    let pages = |n: u64| n << 16;
    let ok = gang_piped("", "");
    // (the instance 4 pages; the gang two tasks of a page each, and the messages 4 bytes at most)
    let run = |m: &str, input: &[u8], gang: u64| apply_v2(m, &[input], &[input.len() as u64], pages(4), &[gang]);
    assert_eq!(run(&ok, b"abc", pages(2) + 4).unwrap(), [b"cde"]);
    assert_eq!(run(&ok, b"", pages(2) + 4).unwrap(), [b""]);
    let many: Vec<u8> = (0..3000u32).map(|i| (i % 200) as u8).collect();
    let want: Vec<u8> = many.iter().map(|b| b + 2).collect();
    for _ in 0..10 {
        assert_eq!(run(&ok, &many, pages(2) + 4).unwrap(), std::slice::from_ref(&want));
    }
    // the tasks' memories are the gang's: short of the two, or of the messages, is a resource failure
    assert!(matches!(run(&ok, b"abc", pages(2) - 1), Err(Failure::Resource(_))));
    assert!(matches!(run(&ok, b"abc", pages(2)), Err(Failure::Resource(_))));
    // (and the instance's limit does not hold the messages: 4 pages, the memories alone)
    assert_eq!(apply_v2(&ok, &[b"abc"], &[3], pages(4), &[u64::MAX]).unwrap(), [b"cde"]);
    // channels made and a spawn of tasks that are no gang
    let plain = current(&piped("", ""));
    invalid(apply_v2(&plain, &[b"abc"], &[3], u64::MAX, &[u64::MAX]), "no gang");
    // channels never given to a gang: made and the instance ends; a channel used before its gang
    let spawnless = |body: &str| {
        format!(
            r#"(module {MEMORIES} {TASKS} {CHANS}
                (func (export "aqr_task") (param i32 i32))
                (func (export "aqr_apply_whole") (param i64 i64 i64) (local $c i32) {body}))"#
        )
    };
    let made = current(&spawnless("(drop (call $chan (i32.const 1)))"));
    invalid(apply_v2(&made, &[b""], &[0], u64::MAX, &[u64::MAX]), "not given to a gang");
    let early = current(&spawnless("(call $send (call $chan (i32.const 1)) (i32.const 0) (i32.const 0))"));
    invalid(apply_v2(&early, &[b""], &[0], u64::MAX, &[u64::MAX]), "before a spawn");
    // both ends of a channel taken by one instance (the instance that spawned the gang)
    let both = current(&spawnless(
        r#"(local.set $c (call $chan (i32.const 1)))
           (drop (call $spawn (i32.const 0) (i32.const 0)))
           (call $send (local.get $c) (i32.const 0) (i32.const 1))
           (drop (call $recv (local.get $c) (i32.const 0x4000) (i32.const 1)))"#,
    ))
    .replace(r#""aqr_task_slots" "\08""#, r#""aqr_task_slots" "\81""#);
    invalid(apply_v2(&both, &[b""], &[0], u64::MAX, &[u64::MAX]), "both ends");
    // a task that is no gang's taking an end: classes 0 (a gang) and 1 (not), a task of class 1 sends
    let outside = format!(
        r#"(module {MEMORIES} {TASKS} {CHANS}
            (func (export "aqr_task") (param $arg i32) (param $i i32)
                (if (i32.eq (local.get $arg) (i32.const 1))
                    (then (call $send (i32.const 0) (i32.const 0) (i32.const 1)))))
            (func (export "aqr_apply_whole") (param i64 i64 i64) (local $h i32)
                (drop (call $chan (i32.const 1)))
                (local.set $h (call $spawnc (i32.const 0) (i32.const 0) (i32.const 1)))
                (drop (call $join (local.get $h) (i32.const 0)))
                (call $take (local.get $h) (i32.const 0) (i32.const 0) (i32.const 0))
                (local.set $h (call $spawnc (i32.const 1) (i32.const 1) (i32.const 1)))
                (drop (call $join (local.get $h) (i32.const 0)))
                (call $take (local.get $h) (i32.const 0) (i32.const 0) (i32.const 0))))"#
    );
    let outside = current(&outside).replace(r#""aqr_task_slots" "\08""#, r#""aqr_task_slots" "\81\02""#);
    invalid(apply_v2(&outside, &[b""], &[0], u64::MAX, &[u64::MAX, u64::MAX]), "outside its gang");
    // a task ending with channels it made not given to a gang
    let task_made = format!(
        r#"(module {MEMORIES} {TASKS} {CHANS}
            (func (export "aqr_task") (param i32 i32) (drop (call $chan (i32.const 1))))
            (func (export "aqr_apply_whole") (param i64 i64 i64) (local $h i32)
                (local.set $h (call $spawn (i32.const 0) (i32.const 1)))
                (drop (call $join (local.get $h) (i32.const 0)))))"#
    );
    invalid(apply_v2(&current(&task_made), &[b""], &[0], u64::MAX, &[u64::MAX]), "not given to a gang");
}

/// The tasks of a gang start together: nine tasks of which eight wait for the ninth need nine
/// at once, which a class of one slot (a gang is one unit) allows, where as tasks eight slots
/// would not (`task_slots`: a deadlock).
#[test]
fn gang_starts_together() {
    let input: Vec<u8> = (0..9).map(|b| b + 0x41).collect();
    let gang = current(&slotted(Some(8))).replace(r#""aqr_task_slots" "\08""#, r#""aqr_task_slots" "\81""#);
    // (nine tasks of a page each, and the messages of 128 channels: one byte each)
    let limit = (9 << 16) + 0x100;
    assert_eq!(apply_v2(&gang, &[&input], &[9], u64::MAX, &[limit]).unwrap(), [&input[..]]);
}

/// The task slots of a module: one custom section of one byte from 1 to 64 where it spawns
/// tasks, none where it does not.
#[test]
fn task_slots_declared() {
    let ok = slotted(None);
    let with = |data: &str| ok.replace(r#""aqr_task_slots" "\08""#, &format!(r#""aqr_task_slots" "{data}""#));
    invalid(load_earlier(&ok.replace(r#"(@custom "aqr_task_slots" "\08")"#, "")), "aqr_task_slots");
    invalid(load_earlier(&with("\\00")), "aqr_task_slots");
    invalid(load_earlier(&with("\\41")), "aqr_task_slots");
    invalid(load_earlier(&with("\\08\\00")), "aqr_task_slots");
    invalid(load_earlier(&with("\\80")), "aqr_task_slots");
    invalid(
        load_earlier(&ok.to_string().replacen("(module", r#"(module (@custom "aqr_task_slots" "\08")"#, 1)),
        "aqr_task_slots",
    );
    invalid(load_earlier(&with("\\c0\\00")), "aqr_task_slots");
    assert!(load_earlier(&with("\\40")).is_ok());
    // a gang class, of 8 slots or 64
    assert!(load_earlier(&with("\\88")).is_ok());
    assert!(load_earlier(&with("\\c0")).is_ok());
    assert!(load_earlier(&with("\\08\\88")).is_ok());
    let plain = r#"(module (import "aqr" "aqr_memory" (memory 1)) (@custom "aqr_task_slots" "\08")
        (func (export "aqr_apply_whole") (param i64)))"#;
    invalid(load_earlier(plain), "aqr_task_slots");
}

/// Formula `f` of a root application of slices, its plan declared as its limit alone (no room),
/// archived and decoded as WebAssembly: `want`, and the application's working memory its plan,
/// to the page.
fn plan_holds(what: &str, f: Formula, want: &[u8]) {
    plan_holds_queued(what, f, want, |_| 0)
}

/// [`plan_holds`] of a module whose channels may hold `queued(usage)` bytes of messages in
/// the plan of its class 0, a gang class: the most the instance took its plan to the page, and
/// the memories of the tasks of a unit of each class added up the plan to the page (a gang's
/// tasks do not all live at once in a short stream: the sum is of the ones that ended; and the
/// messages its channels queue are up to the plan), the most it held at once within it.
fn plan_holds_queued(what: &str, mut f: Formula, want: &[u8], queued: impl Fn(&aqr_formula::Usage) -> u64) {
    let pages = |n: u64| n.div_ceil(65536) * 65536;
    let Node::Apply(a) = &f.root else { panic!("{what}: not an application") };
    // (an input of another node evaluated natively)
    let inputs: Vec<Vec<u8>> = (a.inputs.iter())
        .map(|n| match *n {
            Node::Slice { stream, start, len } => f.streams[stream].data[start as usize..][..len as usize].to_vec(),
            ref n => {
                let mut g = f.clone();
                g.root = n.clone();
                g.eval(&Native::default()).unwrap_or_else(|e| panic!("{what}: an input: {e}"))
            }
        })
        .collect();
    let Node::Apply(a) = &mut f.root else { unreachable!() };
    let contents: Vec<&[u8]> = inputs.iter().map(|i| &i[..]).collect();
    let lens: Vec<u64> = inputs.iter().map(|i| i.len() as u64).collect();
    let resources: Vec<u64> = a.resources.iter().map(|r| r.len() as u64).collect();
    let usage = aqr_formula::Usage {
        params: &a.params,
        resources: &resources,
        inputs: &lens,
        outputs: &[a.len],
        contents: &contents,
        initial_pages: aqr_decompress::initial_pages_of(a.module).unwrap(),
    };
    let plan = a.module.plan_for(&usage).unwrap();
    let queued = queued(&usage);
    let imported = [a.params.len() as u64, a.len].iter().chain(&resources).chain(&lens).map(|&n| pages(n)).sum::<u64>();
    let limit = imported + plan.instance;
    (a.memory_limit, a.task_limits) = (limit, plan.tasks.clone());
    let c = Encoder::new(Level::Zstd(3)).with_modules(wasm_of).encode(&f, None).unwrap();
    // (the applications kept by one test at a time: the log is the process's)
    static RECORDING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let recording = RECORDING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    aqr_decompress::record_applications(true);
    let out = aqr::decode(&c, &Wasm);
    let applied = aqr_decompress::record_applications(false);
    drop(recording);
    assert!(out.as_ref().is_ok_and(|o| o == want), "{what}: {:?}", out.err());
    // (those of this application: other tests' run meanwhile, without limits)
    let mine: Vec<_> = applied.iter().filter(|a| a.limit == limit).collect();
    assert!(!mine.is_empty(), "{what}: not applied");
    for a in mine {
        assert!(a.working <= plan.instance, "{what}: working {} past {}", a.working, plan.instance);
        assert!(
            a.instance <= plan.instance && plan.instance - a.instance < 65536,
            "{what}: instance plan {}, took {}",
            plan.instance,
            a.instance
        );
        assert_eq!(a.tasks.len(), plan.tasks.len(), "{what}: classes");
        for (c, &p) in plan.tasks.iter().enumerate() {
            // (a gang's limit also holds the messages its channels may queue, which only the
            // module's input decides how many it does)
            let (took, p) = (a.task_memories[c], p - queued * (c == 0) as u64);
            assert!(took <= p && p - took < 65536, "{what}: class {c}: plan {p} ({queued} queued), took {took}");
            assert!(a.tasks[c] <= plan.tasks[c], "{what}: class {c}: plan {}, took {}", plan.tasks[c], a.tasks[c]);
        }
    }
}

/// Each application of `f` (the root, or the parts of a concatenation) of `d` as
/// [`plan_holds_queued`] checks it.
fn applications_hold(what: &str, f: &Formula, d: &[u8], queued: impl Fn(&aqr_formula::Usage) -> u64 + Copy) -> usize {
    let parts = match &f.root {
        Node::Concat(nodes) => nodes.clone(),
        n => vec![n.clone()],
    };
    let (mut at, mut applied) = (0, 0);
    for n in parts {
        let len = match &n {
            Node::Apply(a) => a.len,
            Node::Slice { len, .. } => *len,
            _ => panic!("{what}: a node of another kind"),
        } as usize;
        if let Node::Apply(_) = n {
            let mut g = f.clone();
            g.root = n;
            plan_holds_queued(&format!("{what} {applied}"), g, &d[at..at + len], queued);
            applied += 1;
        }
        at += len;
    }
    applied
}

/// A WAV file of `n` sample frames of `channels` channels of `bytes` bytes each: tones with
/// noise, and silence, a constant run and full-scale noise (verbatim subframes) along the way.
fn wav_of(n: u32, channels: u16, bytes: u16) -> Vec<u8> {
    let data = n * channels as u32 * bytes as u32;
    let mut d = Vec::new();
    d.extend_from_slice(b"RIFF");
    d.extend_from_slice(&(36 + data).to_le_bytes());
    d.extend_from_slice(b"WAVEfmt ");
    d.extend_from_slice(&16u32.to_le_bytes());
    d.extend_from_slice(&1u16.to_le_bytes());
    d.extend_from_slice(&channels.to_le_bytes());
    d.extend_from_slice(&44100u32.to_le_bytes());
    d.extend_from_slice(&(44100 * channels as u32 * bytes as u32).to_le_bytes());
    d.extend_from_slice(&(channels * bytes).to_le_bytes());
    d.extend_from_slice(&(8 * bytes).to_le_bytes());
    d.extend_from_slice(b"data");
    d.extend_from_slice(&data.to_le_bytes());
    let mut r = 1u32;
    let bits = 8 * bytes as u32;
    for i in 0..n {
        for c in 0..channels as u32 {
            r = r.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let x = match (i / 5000) % 4 {
                0 | 1 => {
                    (((i as f64 * (0.01 + 0.003 * c as f64)).sin() * 0.3) * (1u64 << (bits - 1)) as f64) as i64
                        + (r >> 26) as i64
                        - 32
                }
                2 if i % 5000 < 2500 => 0,
                2 => 1234,
                _ => (r as i64) >> (32 - bits),
            };
            let x = if bytes == 1 { (x + 128) as u64 } else { x as u64 };
            d.extend_from_slice(&x.to_le_bytes()[..bytes as usize]);
        }
    }
    d
}

/// pcm-recons lays its memory out as a stack, its limit its plan: the pipeline's first stage,
/// its error and LMS stages and its five workers each a task taking the same memory whatever
/// it is given, all at once, and the messages its channels may hold. Of stereo 16-bit, mono
/// 8-bit, 24-bit 6 channels, 32-bit and 8 channels, and of fewer samples than a block.
#[test]
fn pcm_plans_hold() {
    let queued = |u: &aqr_formula::Usage| {
        aqr_raw::module::pcm::plan(u.params, u.resources, u.inputs, u.outputs[0], u.initial()).unwrap().1
    };
    for (n, ch, bytes) in
        [(600_000, 2, 2), (40_000, 1, 1), (30_000, 6, 3), (25_000, 2, 4), (20_000, 8, 2), (1000, 2, 2)]
    {
        let d = wav_of(n, ch, bytes);
        let f = aqr_raw::formula::encode(&d, 1).unwrap_or_else(|| panic!("{n} {ch} {bytes}"));
        assert!(applications_hold(&format!("pcm {n}x{ch}x{bytes}"), &f, &d, queued) > 0, "{n} {ch} {bytes}");
    }
}

/// flac-recons lays its memory out as a stack, its limit its plan (as pcm-recons'), of
/// frames of the room its params give: of every fixture (junk between frames, 24 and 32 bits,
/// 6 channels, a cut file, mono 8-bit, silence).
#[test]
fn flac_plans_hold() {
    let queued = |u: &aqr_formula::Usage| {
        aqr_flac::module::plan(u.params, u.resources, u.inputs, u.outputs[0], u.initial()).unwrap().1
    };
    let dir = format!("{}/../../codecs/aqr-flac/tests/data", env!("CARGO_MANIFEST_DIR"));
    let mut seen = 0;
    for e in std::fs::read_dir(dir).unwrap() {
        let path = e.unwrap().path();
        let d = std::fs::read(&path).unwrap();
        let what = path.file_name().unwrap().to_string_lossy().into_owned();
        let f = aqr_flac::formula::encode(&d, 1).unwrap_or_else(|| panic!("{what}"));
        seen += applications_hold(&what, &f, &d, queued);
    }
    assert!(seen >= 12, "{seen}");
}

/// mp3-recons lays its memory out as a stack, its limit its plan: of every fixture, of a file
/// of 5 MB (well past its window, 12000 frames) and of the same with 4000 frames whose
/// main_data_begin is out of range (their payloads are one gap, many pieces long) and its
/// last frames cut.
#[test]
fn mp3_plans_hold() {
    use aqr_mp3::frame::Header;
    let dir = format!("{}/../../codecs/aqr-mp3/tests/data", env!("CARGO_MANIFEST_DIR"));
    let mut seen = 0;
    let mut big = Vec::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let path = e.unwrap().path();
        let d = std::fs::read(&path).unwrap();
        let what = path.file_name().unwrap().to_string_lossy().into_owned();
        let f = aqr_mp3::formula::encode(&d, 1).unwrap_or_else(|| panic!("{what}"));
        plan_holds(&what, f, &d);
        seen += 1;
        if what == "cbr128.mp3" {
            big = d.iter().copied().cycle().take(5 << 20).collect();
        }
    }
    assert!(seen >= 8, "{seen}");
    let f = aqr_mp3::formula::encode(&big, 1).unwrap();
    plan_holds("5 MB", f, &big);
    // (frames at their sync words: the fixture is CBR, with no CRC)
    let mut frames = Vec::new();
    let mut p = 0;
    while p + 4 <= big.len() && big[p] == 0xff {
        let h = Header::from_bytes(&big[p..]);
        if !h.valid_any() {
            break;
        }
        frames.push(p);
        p += h.frame_len_free(0);
    }
    assert!(frames.len() > 10000, "{}", frames.len());
    for &p in &frames[3000..7000] {
        // main_data_begin 511 (past the start of the payload)
        big[p + 4] = 0xff;
        big[p + 5] |= 0x80;
    }
    // (their payloads are gaps: past the window)
    let c = aqr_mp3::codec::encode(&big);
    assert!(
        c.streams[6].len() > 512 << 10,
        "{:?} {}",
        c.streams.iter().map(|s| s.len()).collect::<Vec<_>>(),
        frames.len()
    );
    let f = aqr_mp3::formula::encode(&big, 1).unwrap();
    plan_holds("gap", f, &big);
}

/// The applications of the module `name` in `n`, in order.
fn applications(n: &Node, name: &str, out: &mut Vec<Node>) {
    match n {
        Node::Apply(a) => {
            if a.module.name == name {
                out.push(n.clone());
            }
            a.inputs.iter().for_each(|i| applications(i, name, out));
        }
        Node::Concat(ns) => ns.iter().for_each(|n| applications(n, name, out)),
        _ => {}
    }
}

/// vorbis-recons lays its memory out as a stack, its limit its plan: the Vorbis streams of the
/// Ogg files (libvorbisenc's and ffmpeg's, chained and grouped), the same with packets cut short
/// and with bytes after them (odd packets, with tails), and a stream of far more packets than
/// 64 and of the setup's tables.
#[test]
fn vorbis_plans_hold() {
    let mut seen = 0;
    for name in ["venc-mono.ogg", "ffmpeg-stereo.ogg", "chained.ogg", "grouped.ogg"] {
        let d = fixture(&format!("codecs/aqr-ogg/tests/data/{name}"));
        let f = aqr_ogg::formula::encode(&d, 1).unwrap();
        let mut apps = Vec::new();
        applications(&f.root, "vorbis-recons", &mut apps);
        for (i, node) in apps.into_iter().enumerate() {
            let mut g = f.clone();
            g.root = node;
            let want = g.eval(&Native::default()).unwrap();
            plan_holds(&format!("vorbis {name} {i}"), g.clone(), &want);
            seen += 1;
            // the packets cut short and followed by bytes, coded again
            let Node::Apply(a) = &g.root else { unreachable!() };
            let (Node::Slice { stream, start, len }, true) = (&a.inputs[0], a.params.len() == 2) else { panic!() };
            let setup = g.streams[*stream].data[*start as usize..][..*len as usize].to_vec();
            let ident = aqr_vorbis::setup::Ident {
                channels: a.params[0] as usize,
                blocksizes: [1 << (a.params[1] & 15), 1 << (a.params[1] >> 4)],
            };
            let (mut odd, mut at, mut k) = (Vec::new(), 0, 0u32);
            while at + 2 <= want.len() {
                let n = u16::from_le_bytes([want[at], want[at + 1]]) as usize;
                let mut p = want[at + 2..at + 2 + n].to_vec();
                match k % 7 {
                    1 => p.truncate(1 + (k as usize * 7) % p.len().max(1)),
                    3 => p.extend([0x5a, 0, 0xff][..1 + k as usize % 3].iter()),
                    _ => {}
                }
                odd.extend_from_slice(&(p.len() as u16).to_le_bytes());
                odd.extend_from_slice(&p);
                at += 2 + n;
                k += 1;
            }
            assert!(k > 64, "{name} {i}: {k} packets");
            let mut h = Formula::default();
            let Some((node, used)) = aqr_vorbis::formula::packets(&mut h, &setup, ident, &odd) else { continue };
            h.root = node;
            plan_holds(&format!("vorbis odd {name} {i}"), h, &odd[..used]);
            seen += 1;
        }
    }
    assert!(seen >= 12, "{seen}");
}

/// ogg-recons lays its memory out as a stack, its limit its plan: the Ogg files (Vorbis, Opus,
/// FLAC, chained, grouped, with data after them), and files of far more than 64 streams and
/// pages (the same file chained 70 times).
#[test]
fn ogg_plans_hold() {
    let dir = format!("{}/../../codecs/aqr-ogg/tests/data", env!("CARGO_MANIFEST_DIR"));
    let mut seen = 0;
    let mut mono = Vec::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let path = e.unwrap().path();
        let d = std::fs::read(&path).unwrap();
        let what = path.file_name().unwrap().to_string_lossy().into_owned();
        let Some((f, _)) = aqr_ogg::formula::encode_any(&d, 1) else { panic!("{what}") };
        plan_holds(&format!("ogg {what}"), f.clone(), &d);
        seen += 1;
        if what == "venc-mono.ogg" {
            mono = d.clone();
        }
        // with bytes after the last page
        let mut tail = d.clone();
        tail.extend_from_slice(b"not a page");
        if let Some((f, _)) = aqr_ogg::formula::encode_any(&tail, 1) {
            let Node::Concat(ns) = &f.root else { panic!("{what}: {:?}", f.root) };
            let mut g = f.clone();
            g.root = ns[0].clone();
            plan_holds(&format!("ogg tail {what}"), g, &d);
            seen += 1;
        }
    }
    assert!(seen >= 6, "{seen}");
    let chained: Vec<u8> = (0..70).flat_map(|_| mono.iter().copied()).collect();
    let (f, _) = aqr_ogg::formula::encode_any(&chained, 1).unwrap();
    plan_holds("ogg x70", f, &chained);
}

/// snappy-recons lays its memory out as a stack, its limit its plan: each stream of the
/// fixtures rebuilt as WebAssembly within the limit of its plan alone, its working memory that
/// of the plan to the page.
#[test]
fn snappy_plan_holds() {
    let dir = format!("{}/../../codecs/aqr-snappy/tests/data", env!("CARGO_MANIFEST_DIR"));
    let mut seen = 0;
    for e in std::fs::read_dir(dir).unwrap() {
        let path = e.unwrap().path();
        let what = path.to_string_lossy();
        let d = std::fs::read(&path).unwrap();
        // (LevelDB tables, leveldb-recons)
        if path.extension().is_some_and(|x| x == "ldb") {
            plan_holds(&what, aqr_snappy::formula::encode(&d, 1).unwrap(), &d);
            seen += 1;
            continue;
        }
        let s = d;
        let d = aqr_snappy::snappy::decode(&s, 1 << 24).unwrap();
        let hash = aqr_snappy::formula::find_hash(&d, &s).unwrap();
        let mut f = Formula::default();
        let data = f.push("raw", d);
        f.root = aqr_snappy::formula::stream(hash, data, s.len() as u64);
        plan_holds(&what, f, &s);
        seen += 1;
    }
    assert!(seen >= 10, "{seen} streams");
}

/// wasm-recons, gnu-gettext-recons and ima-recons lay their memory out as stacks, their limits
/// their plans: of modules aqr-modules built; of `.mo` files with and without a hash table; of
/// IMA ADPCM blocks of noise (any header) of one channel and of six.
#[test]
fn more_plans_hold() {
    for m in ["flate-set", "lz4-recons", "bcj-x86", "png-set"] {
        let d = fixture(&format!("crates/aqr-modules/wasm/{m}.wasm"));
        plan_holds(m, aqr_wasm::formula::encode(&d, 1).unwrap(), &d);
    }
    let pairs: Vec<(Vec<u8>, Vec<u8>)> = (0..3000)
        .map(|i| (format!("message {i:05}").into_bytes(), format!("Nachricht {i} von vielen").into_bytes()))
        .collect();
    for (hash, n) in [(0, 3000), (4099, 3000), (7, 3)] {
        let d = mo_file(&pairs[..n], hash);
        plan_holds(&format!("mo {hash} {n}"), aqr_gnu_gettext::formula::encode(&d, 1).unwrap(), &d);
    }
    for (channels, per) in [(1, 1024), (6, 36)] {
        let l = aqr_ima::codec::Layout::new(channels, per).unwrap();
        let d = aqr_common::rng::noise(200 * l.block());
        let mut f = Formula::default();
        let (node, n) = aqr_ima::formula::blocks(&mut f, l, &d).unwrap();
        assert_eq!(n, d.len());
        f.root = node;
        plan_holds(&format!("ima {channels}"), f, &d);
    }
}

/// A `.mo` file of `pairs` (original, translation), sorted as msgfmt sorts them, little-endian, a
/// hash table of `hash` slots (none for 0).
fn mo_file(pairs: &[(Vec<u8>, Vec<u8>)], hash: usize) -> Vec<u8> {
    let mut pairs = pairs.to_vec();
    pairs.sort();
    let w = |v: usize| (v as u32).to_le_bytes();
    let n = pairs.len();
    let (o, t) = (28, 28 + 8 * n);
    let h = t + 8 * n;
    let mut d: Vec<u8> = [0x9504_12de, 0, n, o, t, hash, h].iter().flat_map(|&v| w(v)).collect();
    let mut at = h + 4 * hash;
    let (mut tables, mut strings) = ([Vec::new(), Vec::new()], Vec::new());
    for (k, table) in tables.iter_mut().enumerate() {
        for p in &pairs {
            let s = if k == 0 { &p.0 } else { &p.1 };
            table.extend_from_slice(&w(s.len()));
            table.extend_from_slice(&w(at));
            strings.extend_from_slice(s);
            strings.push(0);
            at += s.len() + 1;
        }
    }
    d.extend_from_slice(&tables[0]);
    d.extend_from_slice(&tables[1]);
    if hash > 0 {
        for v in aqr_gnu_gettext::mo::hash_table(pairs.iter().map(|p| &p.0), hash).unwrap() {
            d.extend_from_slice(&w(v as usize));
        }
    }
    d.extend_from_slice(&strings);
    d
}

/// pdb-msf lays its memory out as a stack, its limit its plan: of MSF files of streams of blocks
/// in order and shuffled, of 512 and 4096 bytes, more streams than the stack's holes.
#[test]
fn msf_plan_holds() {
    let streams: Vec<Option<Vec<u8>>> = (0..300)
        .map(|k| match k % 7 {
            3 => None,
            _ => Some((0..(k * 997) % 9000).map(|i| (i * 31 + k) as u8).collect()),
        })
        .collect();
    for (bs, shuffle) in [(512, false), (512, true), (4096, true)] {
        let d = msf_file(bs, &streams, shuffle, 0x5a);
        let inputs = aqr_pdb::msf::encode(&d).expect("taken");
        let mut f = Formula::default();
        let nodes = inputs.into_iter().map(|i| f.push("raw", i)).collect();
        f.root = Node::apply(aqr_decompress::PDB_MSF, Vec::new(), nodes, d.len() as u64);
        plan_holds(&format!("msf {bs} {shuffle}"), f, &d);
    }
}

/// raster-recons-16 lays its memory out as a stack, its limit its plan: of rasters of one channel
/// and of three, padded or not, of one chunk of rows and of more.
#[test]
fn raster16_plan_holds() {
    use aqr_raw::image::raster16::Format;
    for (width, height, channels, pad) in [(300, 90, 1, 0), (800, 1000, 3, 6), (2000, 1200, 1, 0)] {
        let fmt = Format { width, height, channels, stride: 2 * width * channels + pad, big_endian: true };
        let mut d = Vec::with_capacity(fmt.len().unwrap());
        for y in 0..height {
            for x in 0..width * channels {
                d.extend_from_slice(&(((x + 2 * y) * 37 + (x * y) % 11) as u16 * 13).to_be_bytes());
            }
            d.extend(std::iter::repeat_n(0x5a, pad));
        }
        let mut f = Formula::default();
        f.root = aqr_raw::formula::raster16::apply(&mut f, &d, fmt).expect("taken");
        plan_holds(&format!("raster16 {width}x{height}x{channels}"), f, &d);
    }
}

/// bvx-recons lays its memory out as a stack, its limit its plan: of libcompression's streams,
/// and of a stream of LZFSE blocks (more than the stack's holes), LZVN and stored blocks in turn.
#[test]
fn bvx_plans_hold() {
    use aqr_bvx::plan::{self, Match, Part, Plan, Side};
    // data made of its parse: literals, then a match back
    let mut rng = aqr_common::rng::Rng::new(5);
    let (mut data, mut p) = (Vec::new(), Plan::default());
    for k in 0..12 {
        let start = data.len();
        let lzvn = k % 3 == 1;
        let mut ms = Vec::new();
        while data.len() - start < 40_000 {
            let lits = rng.below(9) as u32;
            data.extend(rng.bytes(lits as usize));
            let window = if lzvn { 65535 } else { 1 << 18 };
            let dist = 1 + rng.below(data.len().clamp(1, window) as u64) as usize;
            let len = 4 + rng.below(40) as usize;
            for _ in 0..len {
                data.push(data[data.len() - dist]);
            }
            ms.push(Match { lits, len: len as u32, dist: dist as u32 });
        }
        let n = (data.len() - start) as u32;
        match k % 3 {
            0 => {
                p.push_seg(&ms, 0);
                let (ms, tail) = p.seg(p.segs.len() - 1).unwrap();
                let mut left = plan::triples(ms, tail).count() as u32;
                while left > 0 {
                    let n = left.min(20 + rng.below(80) as u32);
                    p.parts.push(Part::Lzfse(n, Side::USUAL));
                    left -= n;
                }
            }
            1 => {
                p.parts.push(Part::Lzvn(n));
                p.push_seg(&ms, 0);
            }
            _ => p.parts.push(Part::Stored(n)),
        }
    }
    let mut fill = Vec::new();
    plan::shapes(&p, &mut |i, lits, none| {
        fill.push((i, (4 - lits % 4) % 4, none));
        Ok(())
    })
    .unwrap();
    for (i, pad, none) in fill {
        let Part::Lzfse(_, side) = &mut p.parts[i] else { unreachable!() };
        side.npad = pad as u8;
        p.zd.extend(std::iter::repeat_n(0, none));
    }
    let mut mixed = Vec::new();
    plan::join(&data, &p, &p.counts().unwrap(), &mut mixed).unwrap();
    assert!(p.parts.len() > 100, "{}", p.parts.len());
    let mut streams: Vec<(String, Vec<u8>)> = ["text", "words", "icon", "small", "stored"]
        .map(|t| (t.to_owned(), fixture(&format!("codecs/aqr-bvx/tests/data/{t}.lzfse"))))
        .into();
    streams.push(("mixed".into(), mixed));
    for (t, d) in streams {
        let mut f = aqr_bvx::formula::encode(&d, 1).unwrap_or_else(|| panic!("{t}"));
        if let Node::Concat(nodes) = &f.root {
            f.root = nodes[0].clone();
        }
        let used = aqr_bvx::formula::extent(&d).unwrap();
        plan_holds(&format!("bvx {t}"), f, &d[..used]);
    }
}

mod lha_gen {
    use aqr_common::rng::Rng;
    use aqr_lha::lzh::{Method, Stream, Token};

    /// Data of runs copied from before, so that matches abound, some far.
    pub fn data(rng: &mut Rng, n: usize) -> Vec<u8> {
        let mut d = Vec::with_capacity(n);
        while d.len() < n {
            if d.len() > 16 && rng.below(3) != 0 {
                let back = 1 + rng.below(d.len().min(70000) as u64) as usize;
                let len = 3 + rng.below(40) as usize;
                for _ in 0..len {
                    d.push(d[d.len() - back]);
                }
            } else {
                d.push(b'a' + rng.below(6) as u8);
            }
        }
        d.truncate(n);
        d
    }

    /// A random parse of `d` within the window of `m`: literals, and matches of any lengths and
    /// distances that fit, not only the nearest nor the longest.
    pub fn parse(rng: &mut Rng, d: &[u8], m: Method) -> Vec<Token> {
        let mut out = Vec::new();
        let mut pos = 0;
        while pos < d.len() {
            let mut took = false;
            if rng.below(3) != 0 && pos > 0 {
                for _ in 0..8 {
                    let dist = 1 + rng.below(pos.min(m.window()) as u64) as usize;
                    let l = aqr_lha::recons::lcp(d, pos - dist, pos).min(256);
                    if l >= 3 {
                        let len = 3 + rng.below((l - 2) as u64) as usize;
                        out.push(Token::Match { len: len as u16, dist: dist as u32 });
                        pos += len;
                        took = true;
                        break;
                    }
                }
            }
            if !took {
                out.push(Token::Literal(d[pos]));
                pos += 1;
            }
        }
        out
    }

    /// A stream of `tokens` in blocks of random sizes, some headers written otherwise than predicted
    /// (runs of zeros as single zeros, the zeros at 3-5 not skipped), random padding, bytes after it.
    pub fn stream(rng: &mut Rng, d: &[u8], tokens: Vec<Token>, m: Method) -> Stream {
        let mut heads = Vec::new();
        let mut at = 0;
        while at < tokens.len() {
            let size = (1 + rng.below(3000) as usize).min(tokens.len() - at);
            let mut h = aqr_lha::huff::head(&tokens[at..at + size], m);
            if rng.below(3) == 0 && h.c.n != 0 {
                let lens = aqr_lha::lzh::c_lens(&h.c).unwrap();
                h.c.items = lens[..h.c.n as usize]
                    .iter()
                    .map(|&l| aqr_lha::lzh::Item { sym: if l == 0 { 0 } else { l + 2 }, extra: 0 })
                    .collect();
                let tl = aqr_lha::huff::lengths(&aqr_lha::huff::t_freqs(&h.c.items));
                h.t = aqr_lha::huff::pt_table(tl, true);
                if h.t.n > 3 && h.t.skip > 0 && rng.below(2) == 0 {
                    h.t.skip = 0;
                }
            }
            heads.push(h);
            at += size;
        }
        let mut st = Stream { data: d.to_vec(), heads, tokens, ..Stream::default() };
        // (the padding found by writing it: whatever bits there are to the byte's end)
        let probe = aqr_lha::lzh::encode(&Stream { pad_bits: 0, ..st.clone() }, m);
        let bits = match probe {
            Ok(_) => 0,
            Err(_) => (1..8).find(|&b| aqr_lha::lzh::encode(&Stream { pad_bits: b, ..st.clone() }, m).is_ok()).unwrap(),
        };
        st.pad_bits = bits;
        st.pad = rng.below(1 << bits) as u8;
        if rng.below(2) == 0 {
            st.tail = (0..rng.below(5)).map(|_| rng.below(256) as u8).collect();
        }
        st
    }
}

/// lha-recons lays its memory out as a stack, its limit its plan: of the fixtures' streams (one
/// and several in a call), and of random parses of every method, from a byte to 150 KB (past the
/// rings of the chains), headers written otherwise than predicted, bytes after the padding.
#[test]
fn lha_plans_hold() {
    let mut cases: Vec<(String, Vec<(Vec<u8>, usize, aqr_lha::lzh::Method)>)> = Vec::new();
    let mut fixtures = Vec::new();
    for name in ["amiga_lh4", "dos255e_lh5", "os2_h3_lh5", "unix_h0_lh5", "unix_h1_lh6", "unix_h2_lh7", "unix_lh6_long"]
    {
        let d = fixture(&format!("codecs/aqr-lha/tests/data/{name}.lzh"));
        for e in aqr_lha::lha::parse(&d).unwrap().entries {
            if let Some(m) = aqr_lha::lzh::Method::of(&e.method) {
                fixtures.push((d[e.data.clone()].to_vec(), e.size as usize, m));
            }
        }
    }
    cases.push(("fixtures".into(), fixtures.clone()));
    cases.push(("one".into(), fixtures[..1].to_vec()));
    let mut rng = aqr_common::rng::Rng::new(7);
    for k in 0..12 {
        let m = aqr_lha::lzh::Method::of_number(4 + (k % 4) as u8).unwrap();
        let n = [1, 100, 5000, 40000, 150000, 3][k % 6];
        let d = lha_gen::data(&mut rng, n);
        let tokens = lha_gen::parse(&mut rng, &d, m);
        let st = lha_gen::stream(&mut rng, &d, tokens, m);
        let s = aqr_lha::lzh::encode(&st, m).unwrap();
        cases.push((format!("random {k}"), vec![(s, d.len(), m)]));
    }
    for (what, streams) in cases {
        let (mut data, mut records, mut lens, mut all) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for (s, size, m) in &streams {
            let sp = aqr_lha::recons::split(s, *size, *m).unwrap();
            lens.push((sp.data.len(), sp.record.len()));
            data.extend(sp.data);
            records.extend(sp.record);
            all.extend_from_slice(s);
        }
        let mut f = Formula::default();
        let data = f.push("raw", data);
        let record = f.push("lha/record", records);
        let index = f.push("lha/index", aqr_lha::formula::index(&lens));
        f.root = Node::Apply(Box::new(aqr_lha::formula::apply_recons(data, record, index, all.len() as u64)));
        plan_holds(&format!("lha {what}"), f, &all);
    }
}

mod rar5_gen {
    use aqr_common::rng::Rng;
    use aqr_rar::huff;
    use aqr_rar::lz5::{self, Block, Encoder5, Filter, Stream, Tables, Token, bonus};

    fn min_bytes(v: u32) -> u8 {
        1 + (v > 0xff) as u8 + (v > 0xffff) as u8 + (v > 0xff_ffff) as u8
    }

    /// The tokens of the files of `sizes` (a solid group), of literals and copies near and far,
    /// repeated distances and the last copy again, and up to `filters` filters in each file.
    pub fn tokens(rng: &mut Rng, sizes: &[usize], filters: usize) -> Vec<Vec<Token>> {
        let (mut hist, mut cache, mut last) = (0usize, [0usize; 4], 0u32);
        let mut files = Vec::new();
        for &size in sizes {
            let (mut toks, mut pos, mut fend, mut want) = (Vec::new(), 0usize, 0usize, filters);
            while pos < size {
                let left = size - pos;
                if want > 0 && rng.below(10) == 0 {
                    let at = pos + rng.below(6) as usize;
                    if at >= fend && at < size {
                        let most = (size - at).min(if rng.below(200) == 0 { 20000 } else { 300 });
                        let (start, len) = ((at - pos) as u32, 1 + rng.below(most as u64) as u32);
                        let kind = rng.below(4) as u8;
                        let channels = if kind == 0 { 1 + rng.below(32) as u8 } else { 0 };
                        let (start_bytes, len_bytes) = (min_bytes(start), min_bytes(len));
                        toks.push(Token::Filter(Filter { start, len, kind, channels, start_bytes, len_bytes }));
                        (fend, want) = (at + len as usize, want - 1);
                        continue;
                    }
                }
                let made = match rng.below(10) {
                    4..=6 if hist > 0 => {
                        let near = if rng.below(3) == 0 { 100 } else { 1 << 20 };
                        let dist = 1 + rng.below(hist.min(near) as u64) as usize;
                        let b = bonus(dist) as usize;
                        if left < 2 + b {
                            None
                        } else {
                            let most = (left - b - 1).min(if rng.below(50) == 0 { 4096 } else { 10 });
                            let len = (2 + rng.below(most as u64) as usize + b) as u32;
                            cache.copy_within(0..3, 1);
                            cache[0] = dist;
                            last = len;
                            Some(Token::Copy { len, dist })
                        }
                    }
                    7 | 8 => {
                        let idx = rng.below(4) as usize;
                        if cache[idx] == 0 || left < 2 {
                            None
                        } else {
                            let len = 2 + rng.below((left - 1).min(12) as u64) as u32;
                            let d = cache[idx];
                            cache.copy_within(0..idx, 1);
                            cache[0] = d;
                            last = len;
                            Some(Token::Rep { idx: idx as u8, len })
                        }
                    }
                    9 if last != 0 && cache[0] != 0 && (last as usize) <= left.min(30) => Some(Token::Last),
                    _ => None,
                };
                let t = made.unwrap_or(Token::Lit(b'a' + rng.below(5) as u8));
                let n = match t {
                    Token::Lit(_) => 1,
                    Token::Copy { len, .. } | Token::Rep { len, .. } => len as usize,
                    _ => last as usize,
                };
                toks.push(t);
                pos += n;
                hist += n;
            }
            files.push(toks);
        }
        files
    }

    /// The tables over the tokens `serve`: Huffman's lengths, or those of every symbol present.
    fn tables(serve: &[Token], v1: bool, every: bool) -> Tables {
        let mut c = lz5::counts(serve, v1);
        if every {
            c.iter_mut().flatten().for_each(|x| *x += 1);
        }
        let lens: Vec<u8> = c.iter().flat_map(|f| huff::lengths(f, huff::MAX)).collect();
        let mut pc = vec![0u32; lz5::PRE];
        lz5::rle(&lens).iter().for_each(|r| pc[r.0 as usize] += 1);
        let pre = huff::lengths(&pc, huff::MAX).try_into().unwrap();
        Tables { pre, lens }
    }

    /// The tokens of each file cut into blocks of up to `most` tokens, some with tables (the
    /// first always), and written: the files' parts.
    pub fn parts(rng: &mut Rng, files: Vec<Vec<Token>>, v1: bool, most: usize, every: bool) -> Vec<Vec<u8>> {
        let mut shapes: Vec<Vec<(usize, bool)>> = Vec::new();
        for t in &files {
            let (mut at, mut v) = (0, Vec::new());
            while at < t.len() {
                let n = (1 + rng.below(most as u64) as usize).min(t.len() - at);
                v.push((n, shapes.is_empty() && v.is_empty() || rng.below(3) == 0));
                at += n;
            }
            shapes.push(v);
        }
        // (each table serves the tokens up to the next, across the files)
        let all: Vec<Token> = files.iter().flatten().copied().collect();
        let flat: Vec<(usize, bool)> = shapes.iter().flatten().copied().collect();
        let (mut starts, mut at) = (Vec::new(), 0);
        for &(n, _) in &flat {
            starts.push(at);
            at += n;
        }
        let mut tabs = Vec::new();
        for (i, &(_, has)) in flat.iter().enumerate() {
            if has {
                let next = (i + 1..flat.len()).find(|&j| flat[j].1).map_or(all.len(), |j| starts[j]);
                tabs.push(Some(tables(&all[starts[i]..next], v1, every && rng.below(2) == 0)));
            } else {
                tabs.push(None);
            }
        }
        let (mut enc, mut out, mut i) = (Encoder5::new(v1), Vec::new(), 0);
        let mut parts = Vec::new();
        for (t, shape) in files.into_iter().zip(shapes) {
            let blocks = (shape.iter().enumerate())
                .map(|(k, &(tokens, _))| {
                    i += 1;
                    Block { tables: tabs[i - 1].clone(), tokens, last: k + 1 == shape.len() }
                })
                .collect();
            let at = out.len();
            enc.file(&Stream { blocks, tokens: t }, &mut out).unwrap();
            parts.push(out[at..].to_vec());
        }
        parts
    }
}

/// What the module of a RAR application gives natively.
fn rar_run(a: &aqr_formula::Apply, i: &[&[u8]]) -> Vec<u8> {
    aqr_rar::formula::apply(a, i, 1).unwrap()
}

/// The applications of the module `name` in `f` (in its root, and its shared applications), each
/// as [`plan_holds`] checks it against what it gives natively. Returns their number.
fn module_applications_hold(
    what: &str,
    f: &Formula,
    name: &str,
    run: &dyn Fn(&aqr_formula::Apply, &[&[u8]]) -> Vec<u8>,
) -> usize {
    let mut found = Vec::new();
    f.root.visit(&mut |n| {
        if let Node::Apply(a) = n {
            found.push((**a).clone());
        }
    });
    found.extend(f.shared.iter().map(|s| s.apply.clone()));
    let mut count = 0;
    for a in found.into_iter().filter(|a| a.module.name == name) {
        // (the inputs as they come: each by itself, or where they hold outputs of other
        // applications, all at once)
        let mut outputs = false;
        a.inputs.iter().for_each(|n| n.visit(&mut |n| outputs |= matches!(n, Node::Output { .. })));
        let inputs: Vec<Vec<u8>> = if outputs {
            let mut g = f.clone();
            g.root = Node::Concat(a.inputs.clone());
            let all = g.eval(&Native::default()).unwrap();
            let mut at = 0;
            a.inputs
                .iter()
                .map(|n| {
                    at += n.len() as usize;
                    all[at - n.len() as usize..at].to_vec()
                })
                .collect()
        } else {
            (a.inputs.iter())
                .map(|n| {
                    let mut g = Formula { shared: Vec::new(), ..f.clone() };
                    g.root = n.clone();
                    g.eval(&Native::default()).unwrap()
                })
                .collect()
        };
        let refs: Vec<&[u8]> = inputs.iter().map(|i| &i[..]).collect();
        let want = run(&a, &refs);
        // (the application on inputs of their own)
        let mut g = Formula::default();
        let mut a = a;
        const NAMES: [&str; 12] = ["i0", "i1", "i2", "i3", "i4", "i5", "i6", "i7", "i8", "i9", "i10", "i11"];
        a.inputs = inputs.iter().zip(NAMES).map(|(i, name)| g.push(name, i.clone())).collect();
        g.root = Node::Apply(Box::new(a));
        plan_holds(&format!("{what} {count}"), g, &want);
        count += 1;
    }
    count
}

/// rar5-recons lays its memory out as a stack, its limit its plan: of the fixtures' archives and
/// of generated streams (RAR 6's and RAR 7's algorithm): a file with many blocks and filters,
/// tables written otherwise than predicted, a solid group of many files, a file past the rings of
/// the match finder.
#[test]
fn rar5_plans_hold() {
    for t in ["r5_m3", "r5_solid"] {
        let d = fixture(&format!("codecs/aqr-rar/tests/data/{t}.rar"));
        let f = aqr_rar::formula::encode(&d, 1).unwrap_or_else(|| panic!("{t}"));
        assert!(module_applications_hold(&format!("rar5 {t}"), &f, "rar5-recons", &rar_run) > 0, "{t}");
    }
    let cases: [(&str, bool, Vec<usize>, usize, usize, bool); 6] = [
        ("blocks and filters", false, vec![60000], 80, 100, false),
        ("version 1, tables otherwise", true, vec![100000], 300, 80, true),
        ("many files", false, vec![700; 80], 1, 100, true),
        ("past the rings", false, vec![300000], 10, 20000, false),
        ("small", false, vec![3], 0, 1, false),
        ("small files", true, vec![5, 1, 9], 1, 3, false),
    ];
    let mut rng = aqr_common::rng::Rng::new(5);
    for (what, v1, sizes, filters, most, every) in cases {
        let files = rar5_gen::tokens(&mut rng, &sizes, filters);
        let parts = rar5_gen::parts(&mut rng, files, v1, most, every);
        let refs: Vec<&[u8]> = parts.iter().map(|p| &p[..]).collect();
        let sp = aqr_rar::recons5::split(&refs, &sizes, v1, 1 << 40).unwrap_or_else(|e| panic!("{what}: {e}"));
        let all = parts.concat();
        // (more than the 64 holes that the stack would trap past: blocks, filters, files)
        let start = aqr_rar::recons5::start_of(&sp.record, sp.data.len()).unwrap();
        eprintln!("rar5 {what}: {} blocks, {} filters, {} files", start.blocks, start.filters, sizes.len());
        if what != "small" && what != "small files" && what != "past the rings" {
            assert!(start.blocks > 64 && (start.filters > 64 || sizes.len() > 64), "{what}");
        }
        let mut f = Formula::default();
        let data = f.push("raw", sp.data.clone());
        let record = f.push("rar/record", sp.record.clone());
        let index = f.push("rar/index", aqr_rar::formula::index(&[(sp.data.len(), sp.record.len())]));
        let a =
            aqr_rar::formula::apply_recons(aqr_rar::formula::MODULE5, vec![], data, record, index, all.len() as u64);
        f.root = Node::Apply(Box::new(a));
        plan_holds(&format!("rar5 {what}"), f, &all);
    }
}

mod rar3_gen {
    use aqr_common::rng::Rng;
    use aqr_rar::huff;
    use aqr_rar::lz3::{self, Block, Encoder3, Stream, Table, Token, bonus};

    /// What the streams so far have: the bytes, the four last distances, the last match.
    #[derive(Default)]
    pub struct State {
        hist: usize,
        rep: [usize; 4],
        last: (usize, usize),
    }

    /// A part of a file: LZ blocks of `n` bytes cut into blocks of up to `most` tokens, or PPMd
    /// blocks (their memories in MiB, less one) of `n` bytes in all.
    pub enum Seg {
        Lz { n: usize, most: usize },
        Ppmd { n: usize, mems: Vec<u8> },
    }

    pub struct File {
        pub segs: Vec<Seg>,
        pub next_header: bool,
        pub tail: Vec<u8>,
    }

    /// LZ tokens covering `n` bytes.
    fn lz_tokens(rng: &mut Rng, st: &mut State, n: usize) -> Vec<Token> {
        let (mut out, mut pos) = (Vec::new(), 0);
        while pos < n {
            let left = n - pos;
            let mut made = None;
            match rng.below(10) {
                3..=5 if st.hist > 0 => {
                    let near = if rng.below(3) == 0 { 300 } else { 1 << 20 };
                    let dist = 1 + rng.below(st.hist.min(near) as u64) as usize;
                    let b = bonus(dist as u32) as usize;
                    if left >= 3 + b {
                        let most = (left - 3 - b).min(if rng.below(30) == 0 { 255 } else { 12 });
                        let len = 3 + b + rng.below(most as u64 + 1) as usize;
                        st.rep.copy_within(0..3, 1);
                        st.rep[0] = dist;
                        st.last = (dist, len);
                        made = Some(Token::Copy { len: len as u32, dist: dist as u32, run: false });
                    }
                }
                6 | 7 => {
                    let idx = rng.below(4) as usize;
                    if st.rep[idx] != 0 && left >= 2 {
                        let len = 2 + rng.below((left - 1).min(12) as u64) as usize;
                        let d = st.rep[idx];
                        st.rep.copy_within(0..idx, 1);
                        st.rep[0] = d;
                        st.last = (d, len);
                        made = Some(Token::Rep { idx: idx as u8, len: len as u32 });
                    }
                }
                8 if st.hist > 0 && left >= 2 => {
                    let dist = 1 + rng.below(st.hist.min(256) as u64) as usize;
                    st.rep.copy_within(0..3, 1);
                    st.rep[0] = dist;
                    st.last = (dist, 2);
                    made = Some(Token::Short { dist: dist as u32 });
                }
                9 if st.last.0 != 0 && st.last.1 <= left.min(40) => made = Some(Token::Last),
                _ => {}
            }
            let t = made.unwrap_or(Token::Lit(b'a' + rng.below(5) as u8));
            let len = match t {
                Token::Lit(_) => 1,
                Token::Last => st.last.1,
                Token::Short { .. } => 2,
                Token::Copy { len, .. } | Token::Rep { len, .. } => len as usize,
                _ => unreachable!(),
            };
            out.push(t);
            pos += len;
            st.hist += len;
        }
        out
    }

    /// The symbols of a PPMd block of `n` bytes, `esc` the escape.
    fn ppmd_syms(rng: &mut Rng, st: &mut State, n: usize, esc: u8) -> Vec<u8> {
        let (mut syms, mut pos) = (Vec::new(), 0);
        while pos < n {
            let left = n - pos;
            let r = rng.below(60);
            if r == 0 && st.hist >= 2 && left >= 32 {
                let dist = 2 + rng.below((st.hist - 1).min(100000) as u64) as usize;
                let len = 32 + rng.below((left - 31).min(256) as u64) as usize;
                let d = dist - 2;
                syms.extend([esc, 4, (d >> 16) as u8, (d >> 8) as u8, d as u8, (len - 32) as u8]);
                pos += len;
                st.hist += len;
            } else if r == 1 && st.hist >= 1 && left >= 4 {
                let len = 4 + rng.below((left - 3).min(256) as u64) as usize;
                syms.extend([esc, 5, (len - 4) as u8]);
                pos += len;
                st.hist += len;
            } else if r == 2 {
                syms.extend([esc, 1]);
                pos += 1;
                st.hist += 1;
            } else {
                syms.push(b'a' + rng.below(6) as u8);
                pos += 1;
                st.hist += 1;
            }
        }
        syms
    }

    /// The streams of `files` (a solid group) written: their parts, their sizes.
    pub fn parts(rng: &mut Rng, files: Vec<File>) -> (Vec<Vec<u8>>, Vec<usize>) {
        let mut st = State::default();
        let mut esc_now = 2u8;
        // the blocks of each file, the LZ blocks' tokens first, their tables after
        let mut built: Vec<(Vec<Block>, bool, Vec<u8>)> = Vec::new();
        let mut sizes = Vec::new();
        for f in &files {
            let mut blocks = Vec::new();
            let mut size = 0;
            let nsegs = f.segs.len();
            for (si, seg) in f.segs.iter().enumerate() {
                match seg {
                    Seg::Lz { n, most } => {
                        let toks = lz_tokens(rng, &mut st, *n);
                        for c in toks.chunks((*most).max(1)) {
                            blocks.push(Block::Lz { table: None, tokens: c.to_vec() });
                        }
                        if toks.is_empty() {
                            blocks.push(Block::Lz { table: None, tokens: Vec::new() });
                        }
                        size += n;
                    }
                    Seg::Ppmd { n, mems } => {
                        let k = mems.len();
                        for (bi, &mem) in mems.iter().enumerate() {
                            let part = n / k + if bi == 0 { n % k } else { 0 };
                            let fresh = bi == 0 || rng.below(2) == 0;
                            let new_esc = bi == 0 || rng.below(3) == 0;
                            let e = [2u8, 0x7e, 0xff][rng.below(3) as usize];
                            if new_esc {
                                esc_now = e;
                            }
                            let flags = (fresh as u8) << 5 | (new_esc as u8) << 6 | 6;
                            let mut syms = ppmd_syms(rng, &mut st, part, esc_now);
                            // (the last block of the file ends the stream, the others their block)
                            let last = si + 1 == nsegs && bi + 1 == k;
                            syms.extend([esc_now, if last { 2 } else { 0 }]);
                            blocks.push(Block::Ppmd { flags, mem: if fresh { mem } else { 0 }, esc: e, syms });
                        }
                        size += n;
                    }
                }
            }
            let ends_ppmd = matches!(f.segs.last(), Some(Seg::Ppmd { .. }));
            built.push((blocks, ends_ppmd || f.next_header, f.tail.clone()));
            sizes.push(size);
        }
        // the tables: each LZ block has one but a file's first after a file that goes on with its codes
        let mut header = true;
        let mut flat = Vec::new();
        for (fi, (blocks, next, _)) in built.iter().enumerate() {
            for (bi, b) in blocks.iter().enumerate() {
                if matches!(b, Block::Lz { .. }) {
                    flat.push((fi, bi, !(bi == 0 && !header)));
                }
            }
            header = *next;
        }
        let bases = lz3::bases();
        let mut prev = [0u8; lz3::LENS];
        for k in 0..flat.len() {
            let (fi, bi, has) = flat[k];
            if !has {
                continue;
            }
            let mut c = [vec![0u32; lz3::MAIN], vec![0; lz3::DIST], vec![0; lz3::LOW], vec![0; lz3::LEN]];
            let mut low = (0, 0);
            let mut j = k;
            loop {
                let (f, b, _) = flat[j];
                let Block::Lz { tokens, .. } = &built[f].0[b] else { unreachable!() };
                for t in tokens {
                    lz3::symbols(t, &bases, &mut low, |a, s| c[a][s] += 1);
                }
                c[0][256] += 1;
                j += 1;
                if j == flat.len() || flat[j].2 {
                    break;
                }
            }
            let mut lens = [0u8; lz3::LENS];
            let mut at = 0;
            for f in &c {
                let l = huff::lengths(f, huff::MAX);
                lens[at..at + l.len()].copy_from_slice(&l);
                at += l.len();
            }
            let keep = rng.below(2) == 0;
            let from = if keep { prev } else { [0; lz3::LENS] };
            let mut pc = vec![0u32; lz3::PRE];
            lz3::rle(&lens, &from).iter().for_each(|r| pc[r.0 as usize] += 1);
            let pre: [u8; lz3::PRE] = huff::lengths(&pc, huff::MAX).try_into().unwrap();
            if let Block::Lz { table, .. } = &mut built[fi].0[bi] {
                *table = Some(Box::new(Table { keep, pre, lens }));
            }
            prev = lens;
        }
        let (mut enc, mut out, mut header) = (Encoder3::new(), Vec::new(), true);
        let mut parts = Vec::new();
        for (blocks, next, tail) in built {
            let at = out.len();
            let s = Stream { blocks, next_header: next, filters: Vec::new(), tail };
            enc.file(&s, header, &mut out).unwrap();
            header = next;
            parts.push(out[at..].to_vec());
        }
        (parts, sizes)
    }
}

/// rar3-recons lays its memory out as a stack, its limit its plan: of the fixtures' archives (LZ
/// and PPMd, with filters) and of generated streams: LZ files with many blocks, a solid group of
/// many files, PPMd blocks of models of several sizes among LZ blocks, tails other than RAR's.
#[test]
fn rar3_plans_hold() {
    for t in ["r4_m3", "r4_solid", "r4_ppmd", "r4_audio"] {
        let d = fixture(&format!("codecs/aqr-rar/tests/data/{t}.rar"));
        let f = aqr_rar::formula::encode(&d, 1).unwrap_or_else(|| panic!("{t}"));
        assert!(module_applications_hold(&format!("rar3 {t}"), &f, "rar3-recons", &rar_run) > 0, "{t}");
    }
    use rar3_gen::{File, Seg};
    let lz = |n, most| Seg::Lz { n, most };
    let pp = |n, mems: &[u8]| Seg::Ppmd { n, mems: mems.to_vec() };
    let trailer = vec![0x00, 0xbf, 0x88, 0x67, 0xf6, 0xa9, 0xff, 0xd4];
    let file = |segs, next_header, tail: &[u8]| File { segs, next_header, tail: tail.to_vec() };
    let cases: Vec<(&str, Vec<File>)> = vec![
        ("many blocks", vec![file(vec![lz(60000, 90)], true, &[])]),
        ("many files", (0..80).map(|i| file(vec![lz(500, 40)], i % 3 != 0, &[])).collect()),
        ("ppmd", vec![file(vec![pp(30000, &[1, 0, 2])], true, &trailer)]),
        (
            "mixed",
            vec![
                file(vec![lz(3000, 100), pp(8000, &[0, 1]), lz(2000, 50), pp(4000, &[2])], true, &trailer),
                file(vec![lz(1000, 30)], false, &[7, 8]),
                file(vec![lz(1500, 30), pp(5000, &[0])], true, &[1, 2, 3]),
            ],
        ),
        ("past the rings", vec![file(vec![lz(300000, 20000)], true, &[])]),
        ("small", vec![file(vec![lz(3, 1)], true, &[])]),
    ];
    let mut rng = aqr_common::rng::Rng::new(9);
    for (what, files) in cases {
        let (parts, sizes) = rar3_gen::parts(&mut rng, files);
        let refs: Vec<&[u8]> = parts.iter().map(|p| &p[..]).collect();
        let sp = aqr_rar::recons3::split(&refs, &sizes, 1 << 22).unwrap_or_else(|e| panic!("{what}: {e}"));
        let all = parts.concat();
        let start = aqr_rar::recons3::start_of(&sp.record, sp.data.len()).unwrap();
        eprintln!(
            "rar3 {what}: {} blocks, {} regions, {} files, {} elements, model {} MiB",
            start.blocks,
            start.regions,
            sizes.len(),
            start.tokens,
            start.model
        );
        if what == "many blocks" || what == "many files" {
            assert!(start.blocks > 64 || sizes.len() > 64, "{what}");
        }
        let mut f = Formula::default();
        let data = f.push("raw", sp.data.clone());
        let record = f.push("rar/record", sp.record.clone());
        let index = f.push("rar/index", aqr_rar::formula::index(&[(sp.data.len(), sp.record.len())]));
        let params = aqr_rar::formula::params3(sp.ppmd);
        let a =
            aqr_rar::formula::apply_recons(aqr_rar::formula::MODULE3, params, data, record, index, all.len() as u64);
        f.root = Node::Apply(Box::new(a));
        plan_holds(&format!("rar3 {what}"), f, &all);
    }
}

/// lzx-recons lays its memory out as a stack, its limit its plan: of WIM chunks (aqr-lzx's
/// fixture) in one stream and in two with bytes kept between and after, and of a CAB stream
/// (a window of 64 KiB, e8).
#[test]
fn lzx_plans_hold() {
    let d = fixture("codecs/aqr-lzx/tests/data/greedy.chunks");
    let (mut chunks, mut at) = (Vec::new(), 0);
    while at < d.len() {
        let n = u32::from_le_bytes(d[at..at + 4].try_into().unwrap()) as usize;
        let len = u32::from_le_bytes(d[at + 4..at + 8].try_into().unwrap()) as usize;
        chunks.push((&d[at + 8..at + 8 + len], n));
        at += 8 + len;
    }
    for (what, streams) in [
        ("one", vec![chunks.clone()]),
        ("two", vec![chunks[..1].to_vec(), chunks[1..].to_vec()]),
        ("a chunk", vec![chunks[..1].to_vec()]),
    ] {
        let mut f = Formula::default();
        f.push("raw", Vec::new());
        f.push("kept", b"<before><between><after>".to_vec());
        let mut parts = Vec::new();
        let mut want = Vec::new();
        for (i, (s, sp)) in streams.iter().zip(aqr_lzx::formula::split(&streams, 1)).enumerate() {
            let sp = sp.unwrap();
            let at = f.streams[0].data.len() as u64;
            f.streams[0].data.extend_from_slice(&sp.data);
            let kept = if i == 0 { Node::slice(1, 0, 8) } else { Node::slice(1, 8, 9) };
            want.extend_from_slice(if i == 0 { b"<before>" } else { b"<between>" });
            s.iter().for_each(|c| want.extend_from_slice(c.0));
            parts.push(aqr_lzx::formula::Stream {
                kept,
                data: Node::slice(0, at, sp.data.len() as u64),
                record: sp.record,
            });
        }
        want.extend_from_slice(b"<after>");
        f.root = aqr_lzx::formula::apply(&mut f, parts, Node::slice(1, 17, 7), want.len() as u64);
        plan_holds(&format!("lzx {what}"), f, &want);
    }
    let s = fixture("codecs/aqr-lzx/tests/data/greedy.cab-lzx");
    let v = aqr_lzx::lzx::Variant::cab(s[0]).unwrap();
    let n = u32::from_le_bytes(s[1..5].try_into().unwrap()) as usize;
    let (mut src, mut at) = (Vec::new(), 5);
    while at < s.len() {
        let len = u32::from_le_bytes(s[at..at + 4].try_into().unwrap()) as usize;
        src.extend_from_slice(&s[at + 4..at + 4 + len]);
        at += 4 + len;
    }
    let sp = aqr_lzx::recons::split_stream(&src, n, v).unwrap();
    let mut f = Formula::default();
    f.push("raw", sp.data.clone());
    let stream = aqr_lzx::formula::Stream {
        kept: Node::Concat(Vec::new()),
        data: Node::slice(0, 0, n as u64),
        record: sp.record,
    };
    let a = aqr_lzx::formula::apply_as(&mut f, v, vec![stream], Node::Concat(Vec::new()), src.len() as u64);
    f.root = Node::Apply(Box::new(a));
    plan_holds("lzx cab", f, &src);
}

mod pdf_gen {
    use aqr_flate::zlib::{Params, compress};

    /// `data` as a zlib stream at `level`.
    pub fn zlib(data: &[u8], level: u8) -> Vec<u8> {
        let raw = compress(data, Params { level, ..Params::default() }).unwrap();
        let (mut a, mut b) = (1u32, 0u32);
        for &x in data {
            a = (a + x as u32) % 65521;
            b = (b + a) % 65521;
        }
        let mut out = vec![0x78, [0x01, 0x5e, 0x9c, 0xda][(level as usize / 3).min(3)]];
        out.extend_from_slice(&raw);
        out.extend_from_slice(&(b << 16 | a).to_be_bytes());
        out
    }

    /// A PDF of the given objects (dictionary body, optional stream data), with an xref table.
    pub fn pdf(objs: &[(String, Option<Vec<u8>>)]) -> Vec<u8> {
        let mut f = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
        let mut offs = vec![];
        for (i, (d, s)) in objs.iter().enumerate() {
            offs.push(f.len());
            f.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            match s {
                Some(s) => {
                    f.extend_from_slice(format!("<< {d} /Length {} >>\nstream\n", s.len()).as_bytes());
                    f.extend_from_slice(s);
                    f.extend_from_slice(b"\nendstream\n");
                }
                None => f.extend_from_slice(format!("{d}\n").as_bytes()),
            }
            f.extend_from_slice(b"endobj\n");
        }
        let x = f.len();
        f.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
        for o in offs {
            f.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        f.extend_from_slice(
            format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n", objs.len() + 1).as_bytes(),
        );
        f
    }

    /// Page content: text operators and numbers, somewhat repetitive.
    pub fn content(seed: u32, n: usize) -> Vec<u8> {
        let mut s = String::from("BT /F1 12 Tf\n");
        let mut x = seed;
        for i in 0..n {
            x = x.wrapping_mul(1103515245).wrapping_add(12345);
            s += &format!(
                "{} {} Td ({}) Tj\n",
                (x >> 16) % 600,
                i % 800,
                ["Hello", "PDF", "world", "recompression"][(x >> 8) as usize % 4]
            );
        }
        s += "ET\n";
        s.into_bytes()
    }

    /// Pixels of an image of `w` by `h` of `bpp` bytes: a gradient with noise.
    pub fn pixels(w: usize, h: usize, bpp: usize, seed: u32) -> Vec<u8> {
        let mut x = seed;
        (0..w * h * bpp)
            .map(|i| {
                x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                ((i / bpp % w) * 255 / w + (i / bpp / w) * 3) as u8 ^ (x >> 29) as u8
            })
            .collect()
    }

    /// A document of `pages` pages (a content stream each, of text and of compression levels
    /// that vary) and some images, a stream of binary data, a stored stream.
    pub fn document(pages: usize, images: usize) -> Vec<u8> {
        let mut objs: Vec<(String, Option<Vec<u8>>)> = vec![("<< /Type /Catalog /Pages 2 0 R >>".into(), None)];
        objs.push(("<< /Type /Pages /Kids [] /Count 0 >>".into(), None));
        for p in 0..pages {
            let c = content(p as u32 + 1, 40 + p % 7 * 30);
            objs.push((format!("/Filter /FlateDecode /N {p}"), Some(zlib(&c, 1 + (p % 9) as u8))));
            objs.push((format!("<< /Type /Page /Parent 2 0 R /Contents {} 0 R >>", objs.len()), None));
        }
        for i in 0..images {
            let (w, h, bpp) = (30 + i * 7, 20 + i * 3, [1, 3][i % 2]);
            let cs = if bpp == 3 { "/DeviceRGB" } else { "/DeviceGray" };
            let px = pixels(w, h, bpp, i as u32 + 9);
            objs.push((
                format!("/Type /XObject /Subtype /Image /Width {w} /Height {h} /BitsPerComponent 8 /ColorSpace {cs} /Filter /FlateDecode"),
                Some(zlib(&px, 6)),
            ));
        }
        objs.push(("/Filter /FlateDecode".into(), Some(zlib(&[7u8; 3000], 9))));
        objs.push(("/Filter /FlateDecode".into(), Some(zlib(b"", 6))));
        objs.push(("/Type /Stored".into(), Some(b"plain bytes not compressed".to_vec())));
        pdf(&objs)
    }
}

/// pdf-recons lays its memory out as a stack, its limit its plan: of generated documents of
/// many objects and streams (pages of text at several levels, images of 8 bits a pixel and
/// of 24, empty and stored streams), and of a small one.
#[test]
fn pdf_plans_hold() {
    let run = |a: &aqr_formula::Apply, i: &[&[u8]]| aqr_pdf::formula::apply(a, i, 1).unwrap();
    // (the last two allow the token coder only, none through aqr-flate's records)
    let cases = [("small", 8, 2, true), ("pages", 90, 0, true), ("images", 5, 12, true), ("many", 140, 20, true)];
    for (what, pages, images, full) in cases.into_iter().chain([("tokens", 140, 20, false), ("pixels", 6, 90, false)]) {
        let d = pdf_gen::document(pages, images);
        let opts = aqr_pdf::encode::Options { full, ..Default::default() };
        let f = aqr_pdf::formula::encode_with(&d, &opts).unwrap_or_else(|| panic!("{what}: no formula"));
        let found = module_applications_hold(&format!("pdf {what}"), &f, "pdf-recons", &run);
        assert!(found > 0, "{what}");
        // (the table's items and token streams: more than the 64 holes that would trap)
        let mut g = Formula { shared: Vec::new(), ..f.clone() };
        let a = f.applies().into_iter().find(|a| a.module.name == "pdf-recons").unwrap();
        g.root = a.inputs[0].clone();
        let table = g.eval(&Native::default()).unwrap();
        let mut streams = 0;
        let items = aqr_pdf::module::scan_table(&table, d.len(), |_, _| streams += 1).unwrap();
        eprintln!("pdf {what}: {} bytes, {items} items, {streams} token streams", d.len());
        if pages > 80 {
            assert!(items > 64, "{what}");
        }
    }
}

/// Data of `n` bytes of 20 letters and repeats of them, from `seed`.
fn lzma_data(n: usize, seed: u64) -> Vec<u8> {
    let mut x = seed;
    let mut d = Vec::with_capacity(n);
    while d.len() < n {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        if d.len() > 64 && x >> 60 < 6 {
            let len = 2 + (x >> 8) as usize % 30;
            let src = (x >> 20) as usize % (d.len() - len.min(d.len()) + 1);
            for j in 0..len {
                d.push(d[src + j]);
            }
        } else {
            d.push(b'a' + (x >> 40) as u8 % 20);
        }
    }
    d.truncate(n);
    d
}

/// An LZMA stream of `d` (parsed greedily, matches within `dict`) and its split, the record in
/// parts of `part` bytes of data.
fn lzma_stream(
    d: &[u8],
    props: aqr_lzma::lzma::Props,
    dict: u32,
    marker: bool,
    part: usize,
) -> (Vec<u8>, aqr_lzma::recons::Split) {
    let packets = aqr_lzma::greedy::packets(d, dict as usize);
    let s = aqr_lzma::lzma::encode(d, props, &packets, marker);
    let size = (!marker).then_some(d.len() as u64);
    let sp = aqr_lzma::recons::split_with(&s, props, dict, size, 1 << 28, part, 1).unwrap();
    assert_eq!(sp.used, s.len());
    (s, sp)
}

/// lzma-recons lays its memory out as a stack, its limit its plan: of streams alone and in an
/// index (more than 64 of them), of every extreme of the properties (the literal model of 6 MiB
/// at lc + lp = 12) and the dictionary (4 KiB, and past the data), data of megabytes (the
/// chains' buckets of 16 positions from 1 MiB) and of bytes, records in parts (more than 64),
/// with the end marker or without.
#[test]
fn lzma_plans_hold() {
    use aqr_lzma::lzma::Props;
    let props = |lc, lp, pb| Props { lc, lp, pb };
    let mut cases: Vec<(String, Vec<(Vec<u8>, aqr_lzma::recons::Split)>)> = Vec::new();
    for t in ["text-p6", "binary-p9e", "binary-lc4pb4", "binary-lc0lp2pb0", "runs-dict4k", "empty-p6", "binary-dict4k"]
    {
        let d = fixture(&format!("codecs/aqr-lzma/tests/data/{t}.lzma"));
        let h = aqr_lzma::alone::parse(&d).unwrap();
        let sp = aqr_lzma::recons::split(&d[aqr_lzma::alone::HEADER..], h.props, h.dict, h.size, 1 << 28, 1).unwrap();
        cases.push((t.into(), vec![(d[aqr_lzma::alone::HEADER..].to_vec(), sp)]));
    }
    // (properties and dictionaries, data of 300 KB)
    for (k, &(lc, lp, pb, dict)) in
        [(0, 0, 0, 1 << 12), (8, 4, 4, 1 << 20), (4, 0, 2, 1 << 12), (0, 4, 0, 1 << 26), (3, 0, 2, 1 << 16)]
            .iter()
            .enumerate()
    {
        let d = lzma_data(300_000, k as u64);
        cases.push((
            format!("lc{lc} lp{lp} dict {dict}"),
            vec![lzma_stream(&d, props(lc, lp, pb), dict, k % 2 == 0, usize::MAX)],
        ));
    }
    // (megabytes, to the buckets of 16 positions: a small and a large dictionary, the extremes)
    for (k, &(lc, lp, dict)) in [(8, 4, 1 << 12), (3, 0, 1 << 26), (0, 0, 1 << 21)].iter().enumerate() {
        let d = lzma_data(3 << 20, 10 + k as u64);
        cases.push((
            format!("big lc{lc} lp{lp} dict {dict}"),
            vec![lzma_stream(&d, props(lc, lp, 2), dict, false, usize::MAX)],
        ));
    }
    // (parts: 80 of them)
    let d = lzma_data(400_000, 20);
    cases.push(("parts".into(), vec![lzma_stream(&d, props(3, 0, 2), 1 << 18, true, 5000)]));
    cases.push(("few parts".into(), vec![lzma_stream(&d, props(1, 1, 1), 1 << 14, false, 150_000)]));
    // (streams in an index: 100 of them, of 1 byte to 20 KB, of all the properties)
    let mut many = Vec::new();
    for k in 0..100usize {
        let d = lzma_data([1, 7, 300, 5000, 20000][k % 5], 100 + k as u64);
        many.push(lzma_stream(
            &d,
            props(k as u32 % 9, k as u32 / 9 % 5, k as u32 % 5),
            1 << (12 + k % 9),
            k % 3 == 0,
            usize::MAX,
        ));
    }
    cases.push(("100 streams".into(), many.clone()));
    cases.push(("5 streams".into(), many[95..].to_vec()));
    // (a big stream after small ones, and the reverse: the top comes back each time)
    let big = lzma_data(1 << 20, 30);
    let mut mixed = many[..70].to_vec();
    mixed.insert(30, lzma_stream(&big, props(8, 4, 0), 1 << 20, false, usize::MAX));
    mixed.push(lzma_stream(&big[..1000], props(0, 0, 0), 1 << 12, true, usize::MAX));
    cases.push(("mixed".into(), mixed));
    for (what, streams) in cases {
        let (mut data, mut records, mut lens, mut all) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for (s, sp) in &streams {
            lens.push((sp.data.len(), sp.record.len()));
            data.extend_from_slice(&sp.data);
            records.extend_from_slice(&sp.record);
            all.extend_from_slice(s);
        }
        let mut f = Formula::default();
        let data = f.push("raw", data);
        let record = f.push("lzma/record", records);
        let index = f.push("lzma/index", aqr_lzma::formula::index(&lens));
        f.root = Node::apply(aqr_lzma::formula::MODULE, Vec::new(), vec![data, record, index], all.len() as u64);
        plan_holds(&format!("lzma {what}"), f, &all);
    }
}

/// The chunks of at most `max` bytes of data over `packets`, never splitting one, the first
/// `first` and the others these controls in turn.
fn lzma2_chunks(
    packets: &[aqr_lzma::lzma::Packet],
    props: aqr_lzma::lzma::Props,
    max: usize,
    first: u8,
    controls: &[u8],
) -> Vec<aqr_lzma::lzma2::Chunk> {
    let mut out: Vec<aqr_lzma::lzma2::Chunk> = Vec::new();
    let mut len = 0;
    let push = |out: &mut Vec<_>, len| {
        let control = if out.is_empty() { first } else { controls[(out.len() - 1) % controls.len()] };
        out.push(aqr_lzma::lzma2::Chunk { control, len, props: (control >= 0xC0).then_some(props) });
    };
    for pk in packets {
        if len + pk.len as usize > max {
            push(&mut out, len);
            len = 0;
        }
        len += pk.len as usize;
    }
    push(&mut out, len);
    out
}

/// lzma2-recons lays its memory out as a stack, its limit its plan: of segments alone and in an
/// index (70 segments of one stream), of streams of more than 64 chunks of every kind (the model
/// reset in place), stored chunks, the properties at their extremes (lc + lp = 4) and the
/// dictionary (4 KiB, and past the data), data of megabytes, and `.xz` files of blocks.
#[test]
fn lzma2_plans_hold() {
    use aqr_lzma::lzma::Props;
    use aqr_lzma::lzma2::{self, Chunk};
    let props = |lc, lp, pb| Props { lc, lp, pb };
    // (a segment: data, properties, dictionary, bytes of data per chunk, controls after the
    // first, and bytes stored in a first chunk of its own)
    let segment = |seed: u64, n: usize, p: Props, dict: usize, max: usize, controls: &[u8], stored: usize| {
        let d = lzma_data(n, seed);
        let packets = aqr_lzma::greedy::packets(&d[stored..], dict);
        let mut chunks = lzma2_chunks(&packets, p, max, if stored > 0 { 0xC0 } else { 0xE0 }, controls);
        if stored > 0 {
            chunks.insert(0, Chunk { control: 1, len: stored, props: None });
        }
        lzma2::encode(&d, &chunks, &packets).expect("chunks of the packets")
    };
    let mut cases: Vec<(String, Vec<u8>, u32)> = Vec::new();
    for t in ["text-p6", "binary-p9e", "binary-lc4pb4", "binary-lc0lp2pb0", "runs-dict4k", "empty-p6", "binary-dict4k"]
    {
        cases.push((t.into(), fixture(&format!("codecs/aqr-lzma/tests/data/{t}.lzma2")), 8 << 20));
    }
    let end = |mut s: Vec<u8>| {
        s.push(0);
        s
    };
    // (more than 64 chunks, resetting the state and the properties in turn)
    cases.push((
        "chunks".into(),
        end(segment(1, 400_000, props(3, 0, 2), 1 << 16, 3000, &[0x80, 0xA0, 0xC0, 0x80, 0xC0], 0)),
        1 << 16,
    ));
    cases.push((
        "chunks lc0 lp4".into(),
        end(segment(2, 300_000, props(0, 4, 4), 1 << 12, 2500, &[0xC0, 0xA0], 0)),
        1 << 12,
    ));
    cases.push((
        "chunks lc4 lp0".into(),
        end(segment(3, 300_000, props(4, 0, 0), 1 << 26, 2500, &[0x80, 0xC0], 0)),
        1 << 26,
    ));
    // (stored data first)
    cases.push(("stored".into(), end(segment(4, 50_000, props(2, 2, 2), 1 << 16, 6000, &[0x80, 0xA0], 100)), 1 << 16));
    // (megabytes: the buckets of 16 positions)
    cases.push(("big".into(), end(segment(5, 3 << 20, props(3, 0, 2), 1 << 22, 40_000, &[0x80, 0xA0], 0)), 1 << 22));
    cases.push((
        "big small dict".into(),
        end(segment(6, 2 << 20, props(0, 0, 0), 1 << 12, 40_000, &[0x80, 0xC0], 0)),
        1 << 12,
    ));
    // (70 segments in a stream, each from the dictionary reset, of all sizes)
    let mut segs = Vec::new();
    for k in 0..70usize {
        let n = [1, 50, 700, 9000, 30_000][k % 5];
        let p = props(k as u32 % 5, (4 - k as u32 % 5).min(k as u32 / 5 % 5), k as u32 % 5);
        segs.extend(segment(
            50 + k as u64,
            n,
            p,
            1 << 14,
            4000,
            &[0x80, 0xA0, 0xC0],
            if k % 7 == 3 && n >= 700 { 80 } else { 0 },
        ));
    }
    cases.push(("70 segments".into(), end(segs), 1 << 14));
    for (what, stream, dict) in cases {
        let sp = lzma2::split(&stream, dict, 1 << 28, 1).unwrap_or_else(|e| panic!("{what}: {e:?}"));
        let (mut data, mut records, mut lens) = (Vec::new(), Vec::new(), Vec::new());
        for x in &sp {
            lens.push((x.data.len(), x.record.len()));
            data.extend_from_slice(&x.data);
            records.extend_from_slice(&x.record);
        }
        let mut f = Formula::default();
        let data = f.push("raw", data);
        let record = f.push("lzma2/record", records);
        let index = f.push("lzma2/index", aqr_lzma::formula::index(&lens));
        f.root = Node::apply(aqr_lzma::formula::LZMA2, Vec::new(), vec![data, record, index], stream.len() as u64);
        plan_holds(&format!("lzma2 {what}"), f, &stream);
    }
    // (an .xz file of a block; several by a split are in the real files of the corpus)
    for t in ["crc64"] {
        let d = fixture(&format!("codecs/aqr-lzma/tests/data/{t}.xz"));
        let f = aqr_lzma::formula::encode_xz(&d, 1).unwrap_or_else(|| panic!("{t}"));
        applications_hold(&format!("xz {t}"), &f, &d, |_| 0);
    }
}

/// wrt-recons and wrt-recons-hangul lay their memory out as stacks, their limits their plans: of
/// text of words and markup, of Hangul, and of a little more than a piece of 16 MiB (two
/// pieces).
#[test]
fn wrt_plans_hold() {
    let mut rng = aqr_common::rng::Rng::new(3);
    let words =
        ["The", "quick", "brown", "fox", "jumps", "over", "the", "lazy", "dog;", "<b>THE</b>", "&amp;", "dog.\n"];
    let mut text = Vec::new();
    while text.len() < 300_000 {
        text.extend_from_slice(words[rng.below(words.len() as u64) as usize].as_bytes());
        text.push(b' ');
    }
    let hangul_words = ["사람은", "나는", "한국어로", "글을", "쓴다.", "그리고", "(English)", "읽는다,", "말이다.\n"];
    let mut korean = Vec::new();
    while korean.len() < 400_000 {
        korean.extend_from_slice(hangul_words[rng.below(hangul_words.len() as u64) as usize].as_bytes());
        korean.push(b' ');
    }
    // pieces: text past 16 MiB, cut where a byte of no token is
    let line = "Some words, and <b>some</b> MORE words &amp; others\n";
    let big = line.repeat((17 << 20) / line.len()).into_bytes();
    for (what, d) in [("words", text), ("hangul", korean), ("pieces", big)] {
        let f = aqr_wrt::formula::encode(&d, 2).unwrap_or_else(|| panic!("{what}"));
        plan_holds(&format!("wrt {what}"), f, &d);
    }
}

/// azo-recons lays its memory out as a stack, its limit its plan: of the streams of the EGG
/// fixtures (one of several blocks), alone and one after another, and a stream cut short in its
/// last block.
#[test]
fn azo_plans_hold() {
    let stream = |name: &str| {
        let d = fixture(&format!("codecs/aqr-egg/tests/data/{name}.egg"));
        let at = d.windows(4).position(|w| w == 0x02B5_0C13u32.to_le_bytes()).unwrap() + 4;
        let len = u32::from_le_bytes(d[at + 6..at + 10].try_into().unwrap()) as usize;
        d[at + 18..at + 18 + len].to_vec()
    };
    let (numbers, short) = (stream("azonumbers"), stream("azoshort_txt"));
    let cases: [(&str, Vec<&[u8]>); 4] = [
        ("numbers", vec![&numbers]),
        ("short", vec![&short]),
        ("both", vec![&short, &numbers]),
        ("three", vec![&short, &numbers, &short]),
    ];
    for (what, streams) in cases {
        let (mut data, mut records, mut lens, mut all) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for s in streams {
            let sp = aqr_azo::recons::split(s, usize::MAX, 2).unwrap();
            lens.push((sp.data.len(), sp.record.len()));
            data.extend_from_slice(&sp.data);
            records.extend_from_slice(&sp.record);
            all.extend_from_slice(s);
        }
        let mut f = Formula::default();
        let data = f.push("raw", data);
        let record = f.push("azo/record", records);
        let index = f.push("azo/index", aqr_azo::formula::index(&lens));
        f.root = Node::Apply(Box::new(aqr_azo::formula::apply(data, record, index, all.len() as u64)));
        plan_holds(&format!("azo {what}"), f, &all);
    }
}

/// fasta-recons lays its memory out as a stack, its limit its plan: of files of several records,
/// lower case and runs of other letters, a header alone, lines of other widths, and one large
/// enough for the model and the file to pass a few pages.
#[test]
fn fasta_plans_hold() {
    let mut big = b">chr\n".to_vec();
    for i in 0..30_000 {
        let line: Vec<u8> =
            (0..60).map(|j| if i % 700 < 3 { b'N' } else { b"ACGTacgtGa"[(i * 7 + j * 3 + j / 7) % 10] }).collect();
        big.extend_from_slice(&line);
        big.push(b'\n');
    }
    let files: [(&str, Vec<u8>); 5] = [
        ("plain", b">a\nACGT\nAC\n".to_vec()),
        ("mixed", b">a b c\nACGTacgtNNNNnnnnACGT\nAC\n>b\nRYKMacgt\nAC".to_vec()),
        ("header", b">x".to_vec()),
        ("widths", b">a\nACG\nACGT\nA\n>b\nNNNN\nNNNN\nNA\n".to_vec()),
        ("big", big),
    ];
    for (what, d) in files {
        let f = aqr_fasta::formula::encode(&d, 1).unwrap_or_else(|| panic!("{what}"));
        plan_holds(&format!("fasta {what}"), f, &d);
    }
}

/// bz2-recons and bz2-alz-recons lay their memory out as stacks, their limits their plans: of
/// the fixtures (a periodic block given its rank among them), of streams one after another, of
/// several blocks (the last the smaller), and of ALZip's.
#[test]
fn bz2_plans_hold() {
    let mut rng = aqr_common::rng::Rng::new(9);
    let words: Vec<&[u8]> = vec![b"bzip2 ", b"block ", b"sort ", b"aaaaaaaaaa", b"the ", b"rotation "];
    let mut text = Vec::new();
    while text.len() < 330_000 {
        text.extend_from_slice(words[rng.below(words.len() as u64) as usize]);
        let n = rng.below(3) as usize;
        text.extend(rng.bytes(n));
    }
    let blocks = aqr_bz2::compress::stream(1, &aqr_bz2::compress::blocks(&text, 1, 1));
    let mut streams: Vec<(String, Vec<u8>)> = ["text-1", "periodic-1", "random-1", "empty-9", "runs-1", "skewed-2"]
        .map(|t| (t.to_owned(), fixture(&format!("codecs/aqr-bz2/tests/data/{t}.bz2"))))
        .into();
    let two =
        [fixture("codecs/aqr-bz2/tests/data/text-5.bz2"), fixture("codecs/aqr-bz2/tests/data/runs-1.bz2")].concat();
    streams.push(("two streams".into(), two));
    streams.push(("blocks".into(), blocks));
    for (t, d) in streams {
        let mut f = aqr_bz2::formula::encode(&d, 1).unwrap_or_else(|| panic!("{t}"));
        if let Node::Concat(nodes) = &f.root {
            f.root = nodes[0].clone();
        }
        plan_holds(&format!("bz2 {t}"), f, &d);
    }
    let (mut streams, mut data, mut all) = (Vec::new(), Vec::new(), Vec::new());
    for t in ["a", "b", "text"] {
        let d = fixture(&format!("codecs/aqr-bz2/tests/data/alz/{t}.dlz"));
        let (p, dd) = aqr_bz2::formula::take_alz(&d, 1, usize::MAX, &mut |_, _, _| true).expect(t);
        streams.push(p);
        data.extend_from_slice(&dd);
        all.extend_from_slice(&d);
    }
    let mut f = Formula::default();
    let data = f.push("raw", data);
    f.root = Node::Apply(Box::new(aqr_bz2::formula::apply_alz(&streams, data, all.len() as u64)));
    plan_holds("bz2 alz", f, &all);
}

/// jsonl-recons lays its memory out as a stack, its limit its plan: of objects of key lists
/// that vary (more at a path than it keeps), maps, tuples, separators not of the style, and lines
/// kept as they are; each segment of a file its own application.
#[test]
fn jsonl_plans_hold() {
    let mut lists = String::new();
    for i in 0..3000 {
        let k = (i * 7919) % 1224;
        lists += &format!("{{\"a\":{{\"x{}\":1,\"y{}\":2,\"z\":{}}},\"n\":[{i},{}.5]}}\n", k % 40, k / 40, i, i % 7);
    }
    let mut maps = String::new();
    for i in 0..3000u32 {
        let h = i.wrapping_mul(2654435761);
        let sp = if i % 50 == 0 { " " } else { "" };
        maps += &format!(
            "{{\"id\":{i},\"files\":{{\"{h:08x}\":{{\"size\":{}}}}},\"at\":[{}.{:02},{}.5]{sp},\"s\":\"w{}\"}}\n",
            i * 3,
            i % 90,
            i % 100,
            i % 180,
            i % 13
        );
        if i % 700 == 0 {
            maps += "not json\n";
        }
    }
    for (t, d) in [("lists", lists), ("maps", maps)] {
        let d = d.into_bytes();
        let f = aqr_jsonl::formula::encode(&d, 1).unwrap_or_else(|| panic!("{t}"));
        let parts = match &f.root {
            Node::Concat(nodes) => nodes.clone(),
            n => vec![n.clone()],
        };
        let mut at = 0;
        for (k, n) in parts.into_iter().enumerate() {
            let Node::Apply(a) = &n else { panic!("{t}: not an application") };
            let len = a.len as usize;
            let mut g = f.clone();
            g.root = n;
            plan_holds(&format!("jsonl {t} {k}"), g, &d[at..at + len]);
            at += len;
        }
        assert_eq!(at, d.len(), "{t}");
    }
}

/// obj-recons lays its memory out as a stack, its limit its plan: of a mesh in blocks (some
/// stored whole), of unit normals and equal lines, and of faces of indexes past the vertices.
#[test]
fn obj_plans_hold() {
    use aqr_obj::codec::Options;
    let mut r = 5u64;
    let mut rnd = || {
        r ^= r << 13;
        r ^= r >> 7;
        r ^= r << 17;
        r
    };
    let mut mesh = String::new();
    let (w, h) = (60, 60);
    for y in 0..h {
        for x in 0..w {
            mesh += &format!("v {x}.0 {y}.0 {}\nvt 0.{x:02} 0.{y:02}\nvn 0 0 1\n", (x * y) % 7);
        }
    }
    mesh += "# a comment\ng part\nusemtl stone\n";
    for y in 0..h - 1 {
        for x in 0..w - 1 {
            let a = y * w + x + 1;
            let (b, c, d) = (a + 1, a + w + 1, a + w);
            mesh += &format!("f {a}/{a}/{a} {b}/{b}/{b} {c}/{c}/{c}\nf {a}/{a}/{a} {c}/{c}/{c} {d}/{d}/{d}\n");
        }
    }
    let mut normals = String::new();
    for i in 0..20000 {
        let mut g = || (rnd() >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0;
        let (x, y, z) = (g(), g(), g());
        let l = (x * x + y * y + z * z).sqrt().max(1e-9);
        normals += &format!("vn {:.6} {:.6} {:.6}\n", x / l, y / l, z / l);
        normals += &format!("v {}.5 1.25 2.75{}\n", i % 50, if i % 3 == 0 { " 1.0" } else { "" });
    }
    let mut far = String::from("v 1 2 3\nv 4 5 6\nv 7 8 9\n");
    for i in 0..3000 {
        far += &format!("f 1 {} 99999{}\nv {i} 0 0\n", 2 + i % 2, i % 10);
    }
    for (t, d, opt) in [
        ("mesh", mesh, Options { block: 1 << 15, force_whole: 0x0101, ..Default::default() }),
        ("normals", normals, Options::default()),
        ("far", far, Options { block: 1 << 12, ..Default::default() }),
    ] {
        let d = d.into_bytes();
        let f = aqr_obj::formula::encode_with(&d, &opt).unwrap_or_else(|| panic!("{t}"));
        plan_holds(&format!("obj {t}"), f, &d);
    }
}

/// bcn-recons lays its memory out as a stack, its limit its plan of the instance and as many
/// tasks at once as their slots allow: of each format, mipmaps, layers, sides not of whole
/// blocks, and surfaces in stripes (16 of the largest, more than the slots).
#[test]
fn bcn_plans_hold() {
    use aqr_tex::layout::{Format, Layout};
    let mut rng = aqr_common::rng::Rng::new(11);
    for (format, width, height, mips, layers) in [
        (Format::Bc1, 1024, 1024, 3, 1),
        (Format::Bc5, 1024, 520, 2, 2),
        (Format::Bc3, 2048, 2048, 2, 1),
        (Format::Bc2, 64, 64, 7, 1),
        (Format::Bc4, 1030, 514, 4, 1),
        (Format::Bc1, 2048, 64, 1, 1),
    ] {
        let l = Layout { format, width, height, mips, layers };
        let blocks = rng.bytes(l.len() as usize);
        let (main, apart, _) = aqr_tex::texture::encode(&l, &blocks, 4);
        let mut f = Formula::default();
        let inputs = vec![f.push("bcn/coded", main), f.push("bcn/stripes", apart)];
        f.root = Node::apply(aqr_tex::formula::MODULE, l.to_bytes(), inputs, l.len());
        plan_holds(&format!("bcn {format:?} {width}x{height} {mips} {layers}"), f, &blocks);
    }
}

/// prores-recons lays its memory out as a stack, its limit its plan of the instance and as many
/// tasks at once as their slots allow: of 4:2:2, 4:4:4 with alpha, interlaced frames, and of a
/// chunk of 17 stripes (more than the slots); each application of a file on its own.
#[test]
fn prores_plans_hold() {
    for t in ["ks-422", "ks-4444-alpha", "ks-interlaced", "aw-422hq x80", "aw-422hq x600"] {
        let d = match t.split_once(" x") {
            Some((t, n)) => fixture(&format!("codecs/aqr-prores/tests/data/{t}.mov")).repeat(n.parse().unwrap()),
            None => fixture(&format!("codecs/aqr-prores/tests/data/{t}.mov")),
        };
        let f = aqr_prores::formula::encode(&d, 1).unwrap_or_else(|| panic!("{t}"));
        let parts = match &f.root {
            Node::Concat(nodes) => nodes.clone(),
            n => vec![n.clone()],
        };
        let (mut at, mut applied) = (0, 0);
        for n in parts {
            let len = match &n {
                Node::Apply(a) => a.len,
                Node::Slice { len, .. } => *len,
                _ => panic!("{t}: a node of another kind"),
            } as usize;
            if let Node::Apply(_) = n {
                let mut g = f.clone();
                g.root = n;
                plan_holds(&format!("prores {t} {applied}"), g, &d[at..at + len]);
                applied += 1;
            }
            at += len;
        }
        assert!(applied > 0, "{t}");
    }
}

/// sqlite-recons lays its memory out as a stack, its limit its plan of the instance and as many
/// tasks at once as their slots allow: of tables with indexes (more than the slots), a table of
/// many columns, records over pages (overflow), repeated texts, and a database of no index.
#[test]
fn sqlite_plans_hold() {
    let dir = std::env::temp_dir().join(format!("aqr-sqlite-plans-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut sql = String::from("PRAGMA page_size=1024;");
    for t in 0..10 {
        sql += &format!("CREATE TABLE t{t}(a INTEGER, b TEXT, c REAL, d BLOB);");
        sql += &format!("CREATE INDEX i{t}a ON t{t}(a); CREATE INDEX i{t}b ON t{t}(b, a DESC);");
        sql += &format!(
            "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n WHERE x < {}) \
             INSERT INTO t{t} SELECT x * 7 % 1000, 'w' || (x % 37), x / 3.0, \
             CASE WHEN x % 50 = 0 THEN randomblob(3000) ELSE randomblob(x % 20) END FROM n;",
            200 + 150 * t
        );
    }
    let wide: Vec<String> = (0..80).map(|c| format!("c{c} INTEGER")).collect();
    sql += &format!("CREATE TABLE w({}); CREATE INDEX iw ON w(c3, c70);", wide.join(", "));
    let vals: Vec<String> = (0..80).map(|c| format!("x * {} % 97", c + 1)).collect();
    sql += &format!(
        "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n WHERE x < 500) INSERT INTO w SELECT {} FROM n;",
        vals.join(", ")
    );
    let plain = "PRAGMA page_size=4096; CREATE TABLE p(a, b); WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL \
        SELECT x + 1 FROM n WHERE x < 3000) INSERT INTO p SELECT x, printf('%050d', x) FROM n;";
    for (t, sql) in [("indexed", sql.as_str()), ("plain", plain)] {
        let path = dir.join(format!("{t}.db"));
        let out = std::process::Command::new("sqlite3").arg(&path).arg(sql).output().expect("sqlite3");
        assert!(out.status.success(), "{t}: {}", String::from_utf8_lossy(&out.stderr));
        let d = std::fs::read(&path).unwrap();
        let f = aqr_sqlite::formula::encode(&d, 1).unwrap_or_else(|| panic!("{t}"));
        plan_holds(&format!("sqlite {t}"), f, &d);
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// jpeg-recons lays its memory out as a stack, its limit its plan of the instance and as many
/// tasks at once as their slots allow: of a sequential scan coded a stripe at a time (more
/// stripes than the slots; with and without restart intervals, and of intervals across the
/// stripes; cut short), and of scans coded whole (a progressive file, a sequential one).
#[test]
fn jpeg_plans_hold() {
    let mut files: Vec<(String, Vec<u8>)> = vec![
        ("gradient".into(), fixture("codecs/aqr-jpeg/tests/data/gradient.jpg")),
        ("progressive".into(), fixture("codecs/aqr-jpeg/tests/data/prog_script_recode.jpg")),
    ];
    for (w, h, ri) in [(2048, 2048, 0), (2048, 1536, 64), (1024, 768, 7)] {
        files.push((format!("{w}x{h} ri {ri}"), jpeg_synthetic(w, h, ri)));
    }
    let mut cut = jpeg_synthetic(2048, 1024, 0);
    cut.truncate(cut.len() * 2 / 3);
    files.push(("cut short".into(), cut));
    for (t, d) in files {
        let f = aqr_jpeg::formula::encode(&d, 1).unwrap_or_else(|| panic!("{t}"));
        plan_holds(&format!("jpeg {t}"), f, &d);
    }
}

/// git-pack-recons and git-pack-index-recons lay their memory out as stacks, their limits their
/// plans: of a pack of more entries than the stack's holes, alone and with its index and reverse
/// index after it.
#[test]
fn git_plans_hold() {
    struct Git;
    impl aqr_formula::Modules for Git {
        fn apply(&self, a: &aqr_formula::Apply, inputs: &[&[u8]]) -> aqr_formula::Result<Vec<u8>> {
            aqr_git::formula::apply(a, inputs, 1).map_err(|e| aqr_formula::Error::new(e.to_string()))
        }
    }
    let n = 300;
    let mut pack = b"PACK\0\0\0\x02".to_vec();
    pack.extend_from_slice(&(n as u32).to_be_bytes());
    for i in 0..n {
        let data: Vec<u8> =
            (0..500 + 37 * (i % 40)).map(|k| b"lorem ipsum dolor sit amet "[(k * 7 + i) % 27]).collect();
        aqr_git::pack::put_header(aqr_git::pack::BLOB, data.len() as u64, &mut pack);
        let (level, header) = [(1, [0x78, 0x01]), (6, [0x78, 0x9c]), (9, [0x78, 0xda])][i % 3];
        pack.extend_from_slice(&header);
        pack.extend(aqr_flate::zlib::compress(&data, aqr_flate::zlib::Params { level, ..Default::default() }).unwrap());
        pack.extend_from_slice(&aqr_common::adler32::hash(&data).to_be_bytes());
    }
    let sum = aqr_git::pack::sha1(&pack);
    pack.extend_from_slice(&sum);
    let f = aqr_git::formula::encode(&pack, 1).expect("taken");
    // (the rebuilt streams as they are given)
    let Node::Apply(a) = &f.root else { panic!("not an application") };
    let mut deflated = f.clone();
    deflated.root = a.inputs[0].clone();
    let deflated = deflated.eval(&Git).unwrap();
    let slice = |n: &Node| match *n {
        Node::Slice { stream, start, len } => f.streams[stream].data[start as usize..][..len as usize].to_vec(),
        _ => panic!("an input not a slice"),
    };
    let (layout, kept) = (slice(&a.inputs[1]), slice(&a.inputs[2]));
    let mut g = Formula::default();
    let inputs = vec![g.push("raw", deflated.clone()), g.push("raw", layout.clone()), g.push("raw", kept.clone())];
    g.root = Node::apply(aqr_decompress::GIT_PACK, Vec::new(), inputs, pack.len() as u64);
    plan_holds("pack", g, &pack);
    // with its index and reverse index, of ids at random
    let mut starts: Vec<usize> = aqr_git::pack::entries(&pack).unwrap().iter().map(|e| e.header.start).collect();
    starts.push(pack.len() - aqr_git::pack::ID);
    let ids = aqr_common::rng::noise(n * aqr_git::pack::ID);
    let mut indexes = vec![0; aqr_git::index::len(&starts, true)];
    aqr_git::index::build(&pack, &starts, &ids, true, &mut indexes).unwrap();
    let mut params = vec![aqr_git::module::WITH_REV];
    aqr_common::varint::put(&mut params, pack.len() as u64);
    let mut g = Formula::default();
    let inputs = [deflated, layout, kept, ids].into_iter().map(|i| g.push("raw", i)).collect();
    let want = [&pack[..], &indexes].concat();
    g.root = Node::apply(aqr_decompress::GIT_PACK_INDEX, params, inputs, want.len() as u64);
    plan_holds("pack and indexes", g, &want);
}

/// pdb-symbols lays its memory out as a stack, its limit its plan: of modules of blocks nested
/// shallow and deep, their lines inlinee lines of few inlinees and of many.
#[test]
fn pdb_symbols_plan_holds() {
    for (mods, n, depth, entries, ids) in [(3, 3000, 5, 20_000, 5000), (1, 400, 200, 0, 1), (40, 50, 3, 300, 300)] {
        let what = format!("symbols {mods} {n} {depth} {entries}");
        let whole: Vec<(Vec<u8>, usize, usize)> = (0..mods).map(|k| pdb_module(k, n, depth, entries, ids)).collect();
        let info =
            |&(_, symbols, c13): &(Vec<u8>, usize, usize)| aqr_pdb::dbi::Module { stream: 0, symbols, c11: 0, c13 };
        let parts: Vec<(&[u8], aqr_pdb::dbi::Module)> = whole.iter().map(|w| (&w.0[..], info(w))).collect();
        let (params, st) = aqr_pdb::symbols::encode(&parts).expect("taken");
        let mut f = Formula::default();
        let nodes = (aqr_pdb::symbols::INPUTS.iter())
            .map(|(_, which)| f.push("raw", aqr_pdb::pack(&which.iter().map(|&k| &st[k][..]).collect::<Vec<_>>())))
            .collect();
        let want: Vec<u8> = whole.iter().flat_map(|w| w.0.iter().copied()).collect();
        f.root = Node::apply(aqr_decompress::PDB_SYMBOLS, params, nodes, want.len() as u64);
        plan_holds(&what, f, &want);
    }
}

/// A module's stream: symbols of `n` blocks (S_BLOCK32, each holding a local) nested `depth`
/// deep in turn, then C13 lines of inlinee lines of `entries` entries of `ids` inlinees; with the
/// bytes of its symbols and of its C13 lines.
fn pdb_module(k: u32, n: u32, depth: u32, entries: u32, ids: u32) -> (Vec<u8>, usize, usize) {
    let record = |m: &mut Vec<u8>, kind: u16, payload: &[u8]| {
        let len = (payload.len() + 4).next_multiple_of(4);
        m.extend_from_slice(&((len - 2) as u16).to_le_bytes());
        m.extend_from_slice(&kind.to_le_bytes());
        m.extend_from_slice(payload);
        m.resize(m.len() + len - 4 - payload.len(), 0);
    };
    let mut m = 4u32.to_le_bytes().to_vec();
    let mut open: Vec<usize> = Vec::new();
    for i in 0..n {
        let parent = open.last().map_or(0, |&q| q as u32);
        let mut b: Vec<u8> = [parent, 0, 0x40 * i + k, 8 + i % 5].iter().flat_map(|v| v.to_le_bytes()).collect();
        b.extend_from_slice(&[1, 0, 0]);
        open.push(m.len());
        record(&mut m, 0x1103, &b);
        let mut local = (0x74 + i % 3).to_le_bytes().to_vec();
        local.extend_from_slice(&[1, 0]);
        local.extend_from_slice(format!("x{i}\0").as_bytes());
        record(&mut m, 0x113e, &local);
        if open.len() as u32 == depth || i + 1 == n {
            while let Some(q) = open.pop() {
                let end = m.len() as u32;
                m[q + 8..q + 12].copy_from_slice(&end.to_le_bytes());
                record(&mut m, 0x0006, &[]);
            }
        }
    }
    let symbols = m.len();
    if entries > 0 {
        m.extend_from_slice(&0xf6u32.to_le_bytes());
        m.extend_from_slice(&(4 + 12 * entries).to_le_bytes());
        m.extend_from_slice(&0u32.to_le_bytes());
        for e in 0..entries {
            let id = 0x1000 + (e * 7919 + k) % ids;
            for v in [id, 0x18 * (id % 9), 100 + e / 3 + id % 50] {
                m.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
    let c13 = m.len() - symbols;
    (m, symbols, c13)
}

/// An MSF file of block size `bs` holding `streams` (`None` a stream of none), their blocks in
/// order but where `shuffle`, free blocks of `stale` bytes among them (as aqr-pdb's tests make
/// them).
fn msf_file(bs: usize, streams: &[Option<Vec<u8>>], shuffle: bool, stale: u8) -> Vec<u8> {
    let u32s = |v: &[u32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
    let blocks_of = |n: usize| n.div_ceil(bs);
    // blocks: 0 superblock, 1-2 free page maps, 3 the directory's list, then the rest
    let mut next = 4u32;
    let mut lists = Vec::new();
    for s in streams {
        let n = s.as_ref().map_or(0, |s| blocks_of(s.len()));
        let mut l: Vec<u32> = (next..next + n as u32).collect();
        if shuffle {
            l.reverse();
        }
        next += n as u32 + 1;
        lists.push(l);
    }
    let mut dir = (streams.len() as u32).to_le_bytes().to_vec();
    for s in streams {
        dir.extend_from_slice(&s.as_ref().map_or(u32::MAX, |s| s.len() as u32).to_le_bytes());
    }
    for l in &lists {
        dir.extend(u32s(l));
    }
    let dir_blocks: Vec<u32> = (next..next + blocks_of(dir.len()) as u32).collect();
    let nb = next as usize + dir_blocks.len();
    let mut d = vec![stale; nb * bs];
    d[..bs].fill(0);
    d[..32].copy_from_slice(aqr_pdb::msf::MAGIC);
    for (k, v) in [bs as u32, 1, nb as u32, dir.len() as u32, 0, 3].into_iter().enumerate() {
        d[32 + 4 * k..36 + 4 * k].copy_from_slice(&v.to_le_bytes());
    }
    d[bs..3 * bs].fill(0xff);
    d[3 * bs..4 * bs].fill(0);
    d[3 * bs..3 * bs + 4 * dir_blocks.len()].copy_from_slice(&u32s(&dir_blocks));
    for (chunk, &b) in dir.chunks(bs).zip(&dir_blocks) {
        d[b as usize * bs..][..bs].fill(0);
        d[b as usize * bs..][..chunk.len()].copy_from_slice(chunk);
    }
    for (s, l) in streams.iter().zip(&lists) {
        let Some(s) = s else { continue };
        for (chunk, &b) in s.chunks(bs).zip(l) {
            d[b as usize * bs..][..bs].fill(0);
            d[b as usize * bs..][..chunk.len()].copy_from_slice(chunk);
        }
    }
    d
}

fn jpeg_segment(out: &mut Vec<u8>, marker: u8, body: &[u8]) {
    out.extend_from_slice(&[0xFF, marker]);
    out.extend_from_slice(&(body.len() as u16 + 2).to_be_bytes());
    out.extend_from_slice(body);
}

/// A baseline 4:4:4 JPEG of `w` x `h` pixels (restart interval `ri` MCUs, 0 for none):
/// smooth DC and a few small low-frequency AC coefficients per block.
fn jpeg_synthetic(w: usize, h: usize, ri: u16) -> Vec<u8> {
    let mut d = vec![0xFF, 0xD8];
    let mut dqt = vec![0u8];
    dqt.extend((0..64).map(|k| 2 + k as u8 / 4));
    jpeg_segment(&mut d, 0xDB, &dqt);
    let mut sof = vec![8];
    sof.extend_from_slice(&(h as u16).to_be_bytes());
    sof.extend_from_slice(&(w as u16).to_be_bytes());
    sof.push(3);
    for id in 1..=3 {
        sof.extend_from_slice(&[id, 0x11, 0]);
    }
    jpeg_segment(&mut d, 0xC0, &sof);
    // DC: the standard luminance table; AC: EOB, ZRL and the (run, 1) symbols
    let mut dht = vec![0x00, 0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
    dht.extend(0..12);
    dht.extend_from_slice(&[0x10, 0, 1, 2, 3, 6, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    dht.push(0x00);
    dht.extend((0..16).map(|r| r << 4 | 1));
    dht.push(0xF0);
    jpeg_segment(&mut d, 0xC4, &dht);
    if ri > 0 {
        jpeg_segment(&mut d, 0xDD, &ri.to_be_bytes());
    }
    jpeg_segment(&mut d, 0xDA, &[3, 1, 0x00, 2, 0x00, 3, 0x00, 0, 63, 0]);
    let at = d.len();
    d.extend_from_slice(&[0xFF, 0xD9]);
    let j = aqr_jpeg::jpeg::parse(&d).unwrap();
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    let coefs: Vec<aqr_jpeg::jpeg::Coefs> = j
        .comps
        .iter()
        .map(|c| {
            let n = c.bw * c.bh;
            let mut o =
                aqr_jpeg::jpeg::Coefs { bw: c.bw, bh: c.bh, y0: 0, blocks: vec![[0; 64]; n], masks: vec![0; n] };
            for i in 0..n {
                let (bx, by) = ((i % c.bw) as i32, (i / c.bw) as i32);
                let b = &mut o.blocks[i];
                b[0] = ((bx * 3 + by * 2) % 200 - 100) as i16;
                for _ in 0..3 {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    b[1 + (x % 9) as usize] = if x & 1024 != 0 { 1 } else { -1 };
                }
                o.masks[i] = b.iter().enumerate().fold(0, |m, (k, &v)| m | ((v != 0) as u64) << k);
            }
            o
        })
        .collect();
    let mut scan = Vec::new();
    let info = aqr_jpeg::jpeg::ScanInfo { standard: true, ..Default::default() };
    aqr_jpeg::jpeg::encode_scan(&j, &j.scans[0], &info, &coefs, &mut scan).unwrap();
    d.splice(at..at, scan);
    d
}
