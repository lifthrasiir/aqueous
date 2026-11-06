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
            let lens: Vec<(usize, usize)> = set.iter().map(|&i| (parts[i].0.len(), parts[i].1.len())).collect();
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

/// [`apply`] with task limits (the spec's ApplyV2).
fn apply_v2(wat: &str, inputs: &[&[u8]], sizes: &[u64], limit: u64, tasks: &[u64]) -> aqr::Decoded<Vec<Vec<u8>>> {
    let r = load(wat)?.apply(b"pp", &[], inputs, sizes, limit, tasks);
    #[cfg(feature = "sandbox")]
    {
        let s = SANDBOX.load(&module(wat))?.apply(b"pp", &[], inputs, sizes, limit, tasks);
        assert!(s == r, "in the sandbox: {:?}, in this process: {:?}", s.as_ref().err(), r.as_ref().err());
    }
    r
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

/// The imports of tasks.
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
    assert_eq!(apply(&ok, &[b"abc"], &[6], u64::MAX).unwrap(), [b"bcdabc"]);
    assert_eq!(apply(&ok, &[b""], &[0], u64::MAX).unwrap(), [b""]);
    let many: Vec<u8> = (0..200u8).map(|b| b % 32 + 64).collect();
    let want: Vec<u8> = many.iter().map(|b| b + 1).chain(many.iter().copied()).collect();
    assert_eq!(apply(&ok, &[&many], &[400], u64::MAX).unwrap(), [want]);
    // a task trapping ('!'), as the application does
    invalid(apply(&ok, &[b"a!c"], &[6], u64::MAX), "trap");
    // the memory of the tasks counts: 4 pages for the module's memories, and one per task
    assert!(apply(&ok, &[b"a"], &[2], 5 << 16).is_ok());
    assert!(matches!(apply(&ok, &[b"a"], &[2], 4 << 16), Err(Failure::Resource(_))));

    // results taken but for one, a result outside a task, a take before the join
    let skip = incr("")
        .replace("(i32.lt_u (local.get $i) (i32.wrap_i64 (local.get $n)))", "(i32.lt_u (local.get $i) (i32.const 1))");
    invalid(apply(&skip, &[b"ab"], &[4], u64::MAX), "tasks not taken");
    invalid(apply(&incr("(call $result (i32.const 0) (i32.const 1))"), &[b"a"], &[2], u64::MAX), "outside a task");
    let early = incr("").replace(
        "(if (i32.ne (call $join",
        "(call $take (local.get $h) (i32.const 0) (i32.const 0) (i32.const 0)) (if (i32.ne (call $join",
    );
    invalid(apply(&early, &[b"a"], &[2], u64::MAX), "not joined");

    // the profile: tasks need aqr_task, and nothing starts
    invalid(load(&incr("").replace(r#"(export "aqr_task")"#, "")), "without aqr_task");
    invalid(
        load(
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
    assert_eq!(apply(&m, &[b""], &[2], u64::MAX).unwrap(), [[0, 7]]);
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
    assert_eq!(apply(&ok, &[b"abc"], &[3], u64::MAX).unwrap(), [b"cde"]);
    assert_eq!(apply(&ok, &[b""], &[0], u64::MAX).unwrap(), [b""]);
    let many: Vec<u8> = (0..3000u32).map(|i| (i % 200) as u8).collect();
    let want: Vec<u8> = many.iter().map(|b| b + 2).collect();
    for _ in 0..20 {
        assert_eq!(apply(&ok, &[&many], &[3000], u64::MAX).unwrap(), std::slice::from_ref(&want));
    }
    // a message longer than the buffer stays, its length told
    let small = piped(
        "",
        "(if (i32.ne (call $recv (i32.load (i32.const 0x104)) (i32.const 0x4000) (i32.const 0)) (i32.const 1)) (then unreachable))",
    );
    assert_eq!(apply(&small, &[b"ab"], &[2], u64::MAX).unwrap(), [b"cd"]);
    // the module sending to the channel the task 0 sends to
    let two = piped("(call $send (i32.load (i32.const 0x100)) (i32.const 0x10c) (i32.const 1))", "");
    invalid(apply(&two, &[b"ab"], &[2], u64::MAX), "two instances");
    // the module receiving from the channel the task 1 receives from
    let two = piped("", "(drop (call $recv (i32.load (i32.const 0x100)) (i32.const 0x4000) (i32.const 1)))");
    invalid(apply(&two, &[b"ab"], &[2], u64::MAX), "two instances");
    // waiting for a channel nobody sends to
    let dead = format!(
        r#"(module {MEMORIES} {TASKS} {CHANS}
            (func (export "aqr_task") (param i32 i32))
            (func (export "aqr_apply_whole") (param i64 i64 i64)
                (drop (call $recv (call $chan (i32.const 1)) (i32.const 0) (i32.const 0)))))"#
    );
    invalid(apply(&dead, &[b""], &[0], u64::MAX), "deadlock");
    // a task waiting so, joined
    let dead = piped("(i32.store (i32.const 0x108) (i32.const 0))", "").replace(
        "(call $close (local.get $c1))",
        "(drop (call $recv (call $chan (i32.const 1)) (i32.const 0) (i32.const 0)))",
    );
    invalid(apply(&dead, &[b"ab"], &[0], u64::MAX), "deadlock");
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
    assert_eq!(apply(&all, &[&input], &[16], limit).unwrap(), std::slice::from_ref(&input));
    assert_eq!(apply(&slotted(Some(7)), &[&input[..8]], &[8], u64::MAX).unwrap(), [&input[..8]]);
    invalid(apply(&slotted(Some(8)), &[&input[..9]], &[9], u64::MAX), "deadlock");
    let two = |w| slotted(w).replace(r#""aqr_task_slots" "\08""#, r#""aqr_task_slots" "\02""#);
    assert_eq!(apply(&two(Some(1)), &[&input[..2]], &[2], u64::MAX).unwrap(), [&input[..2]]);
    invalid(apply(&two(Some(2)), &[&input[..3]], &[3], u64::MAX), "deadlock");
    assert_eq!(apply(&two(None), &[&input], &[16], u64::MAX).unwrap(), std::slice::from_ref(&input));
}

/// Tasks of `aqr_spawn_v2`: a task of class 0 per byte of the input, started from one region (the
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
            (import "aqr" "aqr_spawn_v2" (func $spawn2 (param i32 i32 i32 i32 i32) (result i32)))
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

/// `aqr_spawn_v2`: tasks of the regions named, under their class's limit each with task limits
/// (and not in the instance's), spawning only classes above their own.
#[test]
fn tasks_of_regions() {
    let m = regions(None);
    assert_eq!(apply(&m, &[b"abc"], &[3], u64::MAX).unwrap(), [b"bcd"]);
    // a task is 4 pages, the instance 4 (its working memory, and the parameters, input and output a
    // page each)
    let pages = |n: u64| n << 16;
    assert_eq!(apply_v2(&m, &[b"abc"], &[3], pages(4), &[pages(4), 1]).unwrap(), [b"bcd"]);
    assert!(matches!(apply_v2(&m, &[b"abc"], &[3], pages(4), &[pages(3), 1]), Err(Failure::Resource(_))));
    // without task limits, the tasks' memories are in the memory limit (short of one task here)
    assert!(matches!(apply(&m, &[b"abc"], &[3], pages(4 + 3)), Err(Failure::Resource(_))));
    assert_eq!(apply(&m, &[b"a"], &[1], pages(4 + 4)).unwrap(), [b"b"]);
    // as many task limits as classes
    invalid(apply_v2(&m, &[b"abc"], &[3], u64::MAX, &[u64::MAX]), "task limits");
    // a task of class 0 spawning one of class 1, not of class 0 (with task limits; without, either)
    let up = regions(Some(1));
    assert_eq!(apply_v2(&up, &[b"ab"], &[2], u64::MAX, &[u64::MAX, u64::MAX]).unwrap(), [b"bc"]);
    let same = regions(Some(0));
    invalid(apply_v2(&same, &[b"ab"], &[2], u64::MAX, &[u64::MAX, u64::MAX]), "spawning one of class 0");
    // a class past the module's
    invalid(apply(&regions(Some(2)), &[b"ab"], &[2], u64::MAX), "class 2");
}

/// The task slots of a module: one custom section of one byte from 1 to 64 where it spawns
/// tasks, none where it does not.
#[test]
fn task_slots_declared() {
    let ok = slotted(None);
    let with = |data: &str| ok.replace(r#""aqr_task_slots" "\08""#, &format!(r#""aqr_task_slots" "{data}""#));
    invalid(load(&ok.replace(r#"(@custom "aqr_task_slots" "\08")"#, "")), "aqr_task_slots");
    invalid(load(&with("\\00")), "aqr_task_slots");
    invalid(load(&with("\\41")), "aqr_task_slots");
    invalid(load(&with("\\08\\00")), "aqr_task_slots");
    invalid(load(&with("\\88")), "aqr_task_slots");
    invalid(
        load(&ok.to_string().replacen("(module", r#"(module (@custom "aqr_task_slots" "\08")"#, 1)),
        "aqr_task_slots",
    );
    invalid(load(&with("\\c0\\00")), "aqr_task_slots");
    assert!(load(&with("\\40")).is_ok());
    let plain = r#"(module (import "aqr" "aqr_memory" (memory 1)) (@custom "aqr_task_slots" "\08")
        (func (export "aqr_apply_whole") (param i64)))"#;
    invalid(load(plain), "aqr_task_slots");
}

/// snappy-recons lays its memory out as a stack, its limit its plan: each stream of the
/// fixtures rebuilt as WebAssembly within the limit of its plan alone (no room), its working
/// memory that of the plan to the page.
#[test]
fn snappy_plan_holds() {
    let dir = format!("{}/../../codecs/aqr-snappy/tests/data", env!("CARGO_MANIFEST_DIR"));
    let mut seen = 0;
    for e in std::fs::read_dir(dir).unwrap() {
        let path = e.unwrap().path();
        if path.extension().is_none_or(|x| x != "snappy") {
            continue;
        }
        let s = std::fs::read(&path).unwrap();
        let d = aqr_snappy::snappy::decode(&s, 1 << 24).unwrap();
        let hash = aqr_snappy::formula::find_hash(&d, &s).unwrap();
        let mut f = Formula::default();
        let data = f.push("raw", d.clone());
        let mut root = aqr_snappy::formula::stream(hash, data, s.len() as u64);
        let Node::Apply(a) = &mut root else { panic!("an application") };
        let pages = |n: u64| n.div_ceil(65536) * 65536;
        let none: Vec<&[u8]> = Vec::new();
        let usage = aqr_formula::Usage {
            params: &a.params,
            resources: &[],
            inputs: &[d.len() as u64],
            outputs: &[s.len() as u64],
            contents: &none,
            initial_pages: aqr_decompress::initial_pages_of(a.module).unwrap(),
        };
        let plan = a.module.memory_usage_in_pages(&usage).unwrap() * 65536;
        let limit = pages(a.params.len() as u64) + pages(d.len() as u64) + pages(s.len() as u64) + plan;
        a.memory_limit = limit;
        f.root = root;
        let c = Encoder::new(Level::Zstd(3)).with_modules(wasm_of).encode(&f, None).unwrap();
        aqr_decompress::record_applications(true);
        let out = aqr::decode(&c, &Wasm);
        let applied = aqr_decompress::record_applications(false);
        assert!(out.as_ref().is_ok_and(|o| *o == s), "{path:?}: {:?}", out.err());
        // (those of this application: other tests' run meanwhile, without limits)
        for a in applied.iter().filter(|a| a.limit == limit) {
            assert!(a.working <= plan && plan - a.working < 65536, "{path:?}: plan {plan}, working {}", a.working);
            seen += 1;
        }
    }
    assert!(seen >= 10, "{seen} streams");
}
