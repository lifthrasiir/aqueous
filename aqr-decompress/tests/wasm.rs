//! The modules as WebAssembly: those aqr-modules built, run natively and by Wasmtime alike, and
//! the profile and the host functions of the spec.

use aqr_decompress::{MODULES, Native, Wasm, known, wasm_of};
use aqr_formula::aqr::{self, Encoder, Failure, Level, Loaded, Runner};
use aqr_formula::{Formula, Module, Node};

fn fixture(path: &str) -> Vec<u8> {
    std::fs::read(format!("{}/../{path}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// Every module built is one of this crate, of its version (else it is stale), and known by
/// its bytes.
#[test]
fn built_modules_known() {
    for k in aqr_modules::KNOWN {
        let m = MODULES.iter().find(|m| m.name == k.name);
        assert_eq!(m.map(|m| m.version), Some(k.version), "{}: run aqr-modules/build.sh", k.name);
        assert_eq!(known(k.wasm), m.copied());
        assert_eq!(wasm_of(*m.unwrap()).as_deref(), Some(k.wasm));
    }
    assert_eq!(known(b"\0asm\x01\0\0\0"), None);
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
    let enc = Encoder::new(Level::Zstd(3)).with_modules(wasm_of);
    for (name, f, d) in formulas {
        let c = enc.encode(&f, None).unwrap();
        assert!(aqr::decode(&c, &Native::default()).unwrap() == d, "{name}");
        assert!(aqr::decode(&c, &Wasm).unwrap() == d, "{name}");
        // damaged inputs fail alike (or give the same)
        let Node::Apply(a) = &f.root else { continue };
        let mut bad = f.clone();
        let s = &mut bad.streams[0].data;
        if !s.is_empty() {
            let n = s.len() / 2;
            s[n] ^= 1;
            let c = enc.encode(&bad, None).unwrap();
            let (n, w) = (aqr::decode(&c, &Native::default()), aqr::decode(&c, &Wasm));
            assert_eq!(n.is_ok(), w.is_ok(), "{name} ({}): {n:?} {w:?}", a.module);
            if let (Ok(n), Ok(w)) = (n, w) {
                assert!(n == w, "{name}");
            }
        }
    }
}

/// A module written by hand, of `wat`.
fn module(wat: &str) -> Vec<u8> {
    wat::parse_str(wat).unwrap()
}

fn load(wat: &str) -> aqr::Decoded<Box<dyn Loaded>> {
    Wasm.load(&module(wat))
}

fn apply(wat: &str, inputs: &[&[u8]], sizes: &[u64], limit: u64) -> aqr::Decoded<Vec<Vec<u8>>> {
    load(wat)?.apply(b"pp", &[], inputs, sizes, limit)
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
    let enc = Encoder::new(Level::Zstd(3)).with_modules(|m| (m == REV).then(|| WASM.get().unwrap().into()));
    let c = enc.encode(&f, None).unwrap();
    assert_eq!(aqr::decode(&c, &Native::default()).unwrap(), b"olleh");
    assert!(Encoder::new(Level::Zstd(3)).with_modules(wasm_of).encode(&f, None).is_err());
}
