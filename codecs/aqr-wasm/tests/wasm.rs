//! Wasm modules through the formula and the module: aqr-modules' modules, every function coded;
//! modules of LEB128s not in the shortest form, unknown opcodes and sections of every kind, kept
//! as they are where they cannot be split; streams corrupted, which never panic.

use aqr_common::rng::Rng;
use aqr_formula::{Apply, Formula};
use aqr_wasm::formula;
use aqr_wasm::wasm::{self, STREAM_NAMES, STREAMS};

fn eval(f: &Formula) -> aqr_formula::Result<Vec<u8>> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| {
        assert_eq!(a.module, formula::MODULE);
        formula::decode(&a.params, inputs, a.len).map_err(|e| aqr_formula::Error::new(e.0))
    })
}

fn stream<'a>(st: &'a [Vec<u8>; STREAMS], name: &str) -> &'a [u8] {
    &st[STREAM_NAMES.iter().position(|&n| n == name).unwrap()]
}

fn join(st: &[Vec<u8>; STREAMS], len: usize) -> aqr_wasm::Result<Vec<u8>> {
    wasm::join(std::array::from_fn(|i| &st[i][..]), len as u64)
}

fn corpus() -> Vec<(String, Vec<u8>)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../crates/aqr-modules/wasm");
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "wasm"))
        .map(|p| (p.display().to_string(), std::fs::read(&p).unwrap()))
        .collect();
    v.sort();
    assert!(v.len() > 10);
    v
}

/// aqr-modules' modules come back, none of their functions kept as they are.
#[test]
fn modules() {
    for (name, d) in corpus() {
        let f = formula::encode(&d, 1).unwrap();
        assert!(eval(&f).unwrap() == d, "{name}");
        let st = wasm::split(&d).unwrap();
        // (the sections but the custom, code and data ones alone)
        assert_eq!(stream(&st, "sections").len(), others(&d), "{name}");
        assert_eq!(formula::extent(&[&d[..], b"\xffjunk"].concat()), Some(d.len()), "{name}");
    }
}

/// The bytes of the sections of module `d` but its custom, code and data ones, their ids and
/// sizes aside.
fn others(d: &[u8]) -> usize {
    let (mut at, mut n) = (8, 0);
    while at < d.len() {
        let id = d[at];
        at += 1;
        let size = aqr_common::varint::get(d, &mut at).unwrap() as usize;
        if ![0, 10, 11].contains(&id) {
            n += size;
        }
        at += size;
    }
    n
}

fn leb(v: u64) -> Vec<u8> {
    let mut o = Vec::new();
    aqr_common::varint::put(&mut o, v);
    o
}

fn section(id: u8, content: &[u8]) -> Vec<u8> {
    [&[id][..], &leb(content.len() as u64), content].concat()
}

fn vector(items: &[Vec<u8>]) -> Vec<u8> {
    [leb(items.len() as u64), items.concat()].concat()
}

/// A function body of locals `locals` (count, type) and instructions `code`, its size in the
/// shortest form.
fn func(locals: &[(u32, u8)], code: &[u8]) -> Vec<u8> {
    let decls: Vec<Vec<u8>> = locals.iter().map(|&(n, t)| [leb(n as u64), vec![t]].concat()).collect();
    let body = [vector(&decls), code.to_vec()].concat();
    [leb(body.len() as u64), body].concat()
}

/// A module of the code section of functions `funcs` (all of type `[i32] -> []`), a data
/// section of an active and a passive segment, and a custom section.
fn module(funcs: &[Vec<u8>], code_section: impl Fn(&[u8]) -> Vec<u8>) -> Vec<u8> {
    let types = section(1, &vector(&[vec![0x60, 1, 0x7f, 0]]));
    let functions = section(3, &vector(&vec![vec![0]; funcs.len()]));
    let memory = section(5, &vector(&[vec![0, 1]]));
    let data = section(
        11,
        &vector(&[
            [&[0, 0x41, 0x80, 0x08, 0x0b][..], &leb(5), b"hello"].concat(),
            [&[1][..], &leb(3), b"abc"].concat(),
        ]),
    );
    let custom = section(0, &[&leb(4)[..], b"name", b"\x01\x02\x03"].concat());
    [&wasm::HEADER[..], &types, &functions, &memory, &code_section(&vector(funcs)), &data, &custom].concat()
}

/// Instructions of every kind of immediate.
const CODE: &[u8] = &[
    0x02, 0x40, // block
    0x03, 0x7f, // loop (result i32)
    0x20, 0x00, 0x21, 0x01, 0x22, 0x01, 0x20, 0x00, // locals
    0x41, 0xff, 0x00, 0x42, 0x80, 0x80, 0x04, 0x6a, // i32.const, i64.const
    0x43, 1, 2, 3, 4, 0x44, 1, 2, 3, 4, 5, 6, 7, 8, 0x1a, 0x1a, // f32.const, f64.const
    0x28, 0x02, 0x10, 0x36, 0x42, 0x01, 0x90, 0x01, // loads and stores, of memory 1
    0x0d, 0x01, 0x0e, 0x02, 0x00, 0x01, 0x00, // br_if, br_table
    0x0b, 0x0b, // ends
    0x10, 0x00, 0x11, 0x00, 0x00, // call, call_indirect
    0xfc, 0x0a, 0x00, 0x00, 0xfc, 0x0b, 0x00, // memory.copy, memory.fill
    0xfd, 0x0c, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, // v128.const
    0xfd, 0x15, 0x03, 0xfd, 0x54, 0x00, 0x08, 0x01, // extract_lane, load8_lane
    0x1c, 0x01, 0x7f, 0x3f, 0x00, 0x23, 0x00, // select t, memory.size, global.get
    0x0b,
];

/// Functions of LEB128s not in the shortest form, or unknown opcodes, are kept as they are, and
/// so are code and data sections of sizes not in the shortest form; the others are split.
#[test]
fn fallbacks() {
    let good = func(&[(3, 0x7f), (1, 0x7e)], CODE);
    let padded_local = func(&[(1, 0x7f)], &[0x20, 0x80, 0x00, 0x1a, 0x0b]);
    let unknown = func(&[], &[0xff, 0x0b]);
    let trailing = func(&[], &[0x0b, 0x01]);
    let mut padded_size = func(&[], &[0x01, 0x0b]);
    padded_size.splice(0..1, [padded_size[0] | 0x80, 0]);
    let funcs = [good.clone(), padded_local.clone(), unknown.clone(), trailing.clone(), padded_size.clone()];
    let m = module(&funcs, |c| section(10, c));
    let st = wasm::split(&m).unwrap();
    assert_eq!(join(&st, m.len()).unwrap(), m);
    let raw: Vec<u8> = [&padded_local[..], &unknown, &trailing, &padded_size].concat();
    assert!(stream(&st, "sections").ends_with(&raw));
    assert!(!stream(&st, "sections").ends_with(&[&good[..], &raw].concat()));
    assert_eq!(stream(&st, "data"), b"helloabc");
    assert_eq!(stream(&st, "custom"), b"\x04name\x01\x02\x03");

    // (the code section's size padded)
    let m = module(std::slice::from_ref(&good), |c| {
        let s = section(10, c);
        [&[10, s[1] | 0x80, 0x80, 0][..], &s[2..]].concat()
    });
    let st = wasm::split(&m).unwrap();
    assert_eq!(join(&st, m.len()).unwrap(), m);
    assert!(stream(&st, "sections").ends_with(&vector(&[good])));
    assert!(stream(&st, "op").len() < 8);
}

/// What is not a module of version 1 of sections ending within it is not taken.
#[test]
fn not_taken() {
    let m = module(&[func(&[], &[0x0b])], |c| section(10, c));
    assert!(formula::encode(&m, 1).is_some());
    assert!(formula::encode(&m[..m.len() - 1], 1).is_none());
    assert!(formula::encode(b"\0asm\x02\0\0\0", 1).is_none());
    assert!(formula::encode(&[&m[..], &[0x01, 0x80]].concat(), 1).is_none());
    assert_eq!(formula::extent(&[&m[..], &[0x01, 0x80]].concat()), Some(m.len()));
    assert!(wasm::split(&wasm::HEADER).is_some());
}

/// Streams cut short, or of bytes changed, give an error or a module of the length declared,
/// never a panic.
#[test]
fn corrupt() {
    let (_, d) = corpus().into_iter().find(|(n, _)| n.ends_with("bcj-x86.wasm")).unwrap();
    let st = wasm::split(&d).unwrap();
    let mut rng = Rng::new(7);
    for _ in 0..3000 {
        let mut bad = st.clone();
        let s = rng.below(STREAMS as u64) as usize;
        if bad[s].is_empty() {
            continue;
        }
        let at = rng.below(bad[s].len() as u64) as usize;
        match rng.below(3) {
            0 => bad[s].truncate(at),
            1 => bad[s][at] ^= 1 << rng.below(8),
            _ => bad[s][at] = rng.u64() as u8,
        }
        if let Ok(m) = join(&bad, d.len()) {
            assert_eq!(m.len(), d.len());
        }
    }
}
