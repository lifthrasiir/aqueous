//! The modules as WebAssembly: those aqr-modules built, run natively and by Wasmtime alike, and
//! the profile and the host functions of the spec. Every module run by Wasmtime runs in the
//! sandbox too (with the feature `sandbox`), and gives the same, failures included.

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
    std::fs::read(format!("{}/../{path}", env!("CARGO_MANIFEST_DIR"))).unwrap()
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
        assert_eq!(m.map(|m| m.version), Some(k.version), "{}: run aqr-modules/build.sh", k.name);
        assert_eq!(known(k.wasm), m.copied());
        let w = wasm_of(*m.unwrap()).unwrap();
        assert_eq!((&w.bytes[..], w.block.as_deref()), (k.wasm, Some(k.block)));
        assert!(aqr::module_block_data(k.wasm).unwrap() == k.block, "{}: run aqr-modules/build.sh", k.name);
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
        let d = fixture(&format!("aqr-flac/tests/data/{t}"));
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

#[path = "../../aqr-psd/tests/common/mod.rs"]
mod psd;

/// PSD files of the shapes of aqr-psd's tests, the same natively and as WebAssembly (whose
/// decoding of a group once called `Instant::now`, which WebAssembly has not).
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
        let d = fixture(&format!("aqr-snappy/tests/data/{t}.ldb"));
        formulas.push((t.into(), aqr_snappy::formula::encode(&d, 1).unwrap(), d));
    }
    for t in ["fragments-crc", "mixed-crc"] {
        let s = fixture(&format!("aqr-snappy/tests/data/{t}.snappy"));
        let d = aqr_snappy::snappy::decode(&s, 1 << 24).unwrap();
        let hash = aqr_snappy::formula::find_hash(&d, &s).unwrap();
        let mut f = Formula::default();
        let data = f.push("raw", d);
        f.root = aqr_snappy::formula::stream(hash, data, s.len() as u64);
        formulas.push((t.into(), f, s));
    }
    for t in ["f64k.lz4", "f256k-checks.lz4", "small-a5.mozlz4", "fempty.lz4"] {
        let d = fixture(&format!("aqr-lz4/tests/data/{t}"));
        let f = aqr_lz4::formula::encode(&d, 1).unwrap_or_else(|| panic!("{t}"));
        formulas.push((t.into(), f, d));
    }
    // FLAC and PCM of more batches of their pipelines than tasks (a batch is 2^16 samples)
    for t in ["s16_junk.flac", "s24_f8.flac", "c6.flac"] {
        let d = fixture(&format!("aqr-flac/tests/data/{t}"));
        formulas.push((t.into(), aqr_flac::formula::encode(&d, 1).unwrap_or_else(|| panic!("{t}")), d));
    }
    formulas.push(("pcm".into(), aqr_raw::formula::encode(&wav(), 1).expect("pcm"), wav()));
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
        let d = fixture(&format!("aqr-lzma/tests/data/{t}.lzma"));
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
    let r = load(wat)?.apply(b"pp", &[], inputs, sizes, limit);
    #[cfg(feature = "sandbox")]
    {
        let s = SANDBOX.load(&module(wat))?.apply(b"pp", &[], inputs, sizes, limit);
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
    const REV: Module = Module { name: "rev", version: 1 };
    static WASM: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    WASM.set(rev).unwrap();
    let mut f = Formula::default();
    let a = f.push("a", b"hello".to_vec());
    f.root = Node::apply(REV, b"pp".to_vec(), vec![a], 5);
    let enc = Encoder::new(Level::Zstd(3))
        .with_modules(|m| (m == REV).then(|| aqr::Wasm { bytes: WASM.get().unwrap().into(), block: None }));
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
