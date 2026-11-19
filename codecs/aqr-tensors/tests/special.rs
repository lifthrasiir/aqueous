//! Special values and lying headers: floats of every type with every bit pattern, the escaped tiles
//! of grids, tensors that overlap or are unsorted, GGUF and safetensors headers that lie, the
//! parameters of an application damaged, and a tensor over the 16 MiB segment's boundary: never a
//! panic, a hang or an allocation the data cannot hold; a file taken is rebuilt as it is.

use aqr_formula::{Formula, Node};
use aqr_tensors::formula::{self, MODULE};
use aqr_tensors::layout::Run;
use aqr_tensors::module::decode;
use aqr_testkit::Refusing;
use std::time::Duration;

#[global_allocator]
static ALLOC: Refusing = Refusing(1 << 30);

fn inputs<'a>(f: &'a Formula, a: &aqr_formula::Apply) -> Vec<&'a [u8]> {
    (a.inputs.iter())
        .map(|n| match n {
            &Node::Slice { stream, start, len } => &f.streams[stream].data[start as usize..][..len as usize],
            _ => panic!("not a slice"),
        })
        .collect()
}

fn eval(f: &Formula) -> Vec<u8> {
    let Node::Concat(nodes) = &f.root else { panic!("not a concatenation") };
    let mut out = Vec::new();
    for n in nodes {
        match n {
            Node::Apply(a) => {
                assert_eq!(a.module, MODULE);
                out.extend(decode(&a.params, &inputs(f, a), a.len, 1).unwrap());
            }
            &Node::Slice { stream, start, len } => {
                out.extend_from_slice(&f.streams[stream].data[start as usize..][..len as usize])
            }
            _ => panic!("a node of another kind"),
        }
    }
    out
}

/// `d` is not taken, or rebuilt as it is; whether it was taken.
fn holds(d: &[u8]) -> bool {
    match formula::encode(d, 1) {
        Some(f) => {
            assert!(eval(&f) == d, "rebuilt otherwise");
            true
        }
        None => false,
    }
}

fn apps(f: &Formula) -> Vec<&aqr_formula::Apply> {
    let Node::Concat(nodes) = &f.root else { panic!() };
    nodes.iter().filter_map(|n| if let Node::Apply(a) = n { Some(&**a) } else { None }).collect()
}

fn rng(seed: u64) -> aqr_common::rng::Rng {
    aqr_common::rng::Rng::new(seed)
}

/// A safetensors file of tensors `(dtype, row, data, offsets)`, in the order given (the offsets
/// where `Some`, else just after the one before).
type T<'a> = (&'a str, u64, Vec<u8>, Option<(usize, usize)>);

fn st(ts: &[T]) -> Vec<u8> {
    let mut h = String::from("{");
    let (mut at, mut body) = (0usize, Vec::new());
    for (i, (ty, row, d, off)) in ts.iter().enumerate() {
        let (a, b) = off.unwrap_or((at, at + d.len()));
        let n = d.len() as u64 / (*row).max(1);
        let sep = if i > 0 { "," } else { "" };
        h += &format!(r#"{sep}"t{i}":{{"dtype":"{ty}","shape":[{n},{row}],"data_offsets":[{a},{b}]}}"#);
        at += d.len();
        body.extend_from_slice(d);
    }
    h.push('}');
    while !h.len().is_multiple_of(8) {
        h.push(' ');
    }
    let mut out = (h.len() as u64).to_le_bytes().to_vec();
    out.extend_from_slice(h.as_bytes());
    out.extend_from_slice(&body);
    out
}

/// Special floats of every type (every bit pattern of the 8- and 16-bit ones, NaNs of payloads,
/// infinities, negative zeros, denormals, runs of one value): rebuilt through `Run::Float`, in
/// counts that end every SIMD loop differently.
#[test]
fn special_floats() {
    let mut r = rng(11);
    for (ty, w) in [("F8_E4M3", 1usize), ("F8_E5M2", 1), ("F16", 2), ("BF16", 2), ("F32", 4), ("F64", 8), ("C64", 8)] {
        let n = if w == 1 { 256 } else { 65536 };
        let all: Vec<Vec<u8>> = (0..n as u64)
            .map(|v| {
                let mut b = v.to_le_bytes()[..w.min(2)].to_vec();
                b.resize(w, 0);
                b
            })
            .collect();
        // (the big types': the patterns that matter, in the low and the high bytes)
        let mut specials: Vec<Vec<u8>> = Vec::new();
        for hi in [0x00u8, 0x7f, 0x80, 0xff, 0x7e, 0xfe, 0x01, 0x3f, 0x40] {
            for lo in [0x00u8, 0x01, 0x80, 0xff, 0x7f, 0x55] {
                for fill in [0x00u8, 0xff, 0xaa, 0x01] {
                    let mut b = vec![fill; w];
                    b[0] = lo;
                    b[w - 1] = hi;
                    specials.push(b);
                }
            }
        }
        let mixed: Vec<u8> =
            (0..30000).flat_map(|_| specials[r.below(specials.len() as u64) as usize].clone()).collect();
        let mut shuffled = all.clone();
        for i in (1..shuffled.len()).rev() {
            shuffled.swap(i, r.below(i as u64 + 1) as usize);
        }
        let minus_zero: Vec<u8> =
            (0..30000).flat_map(|_| (0..w).map(|k| if k == w - 1 { 0x80 } else { 0 }).collect::<Vec<u8>>()).collect();
        let noise: Vec<u8> = (0..30000 * w).map(|_| r.below(256) as u8).collect();
        let sets: Vec<(&str, Vec<u8>)> = vec![
            ("all", all.concat()),
            ("all shuffled", shuffled.concat()),
            ("mixed", mixed),
            ("zero", vec![0; 30000 * w]),
            ("minus zero", minus_zero),
            ("ff", vec![0xff; 30000 * w]),
            ("7f", vec![0x7f; 30000 * w]),
            ("noise", noise),
        ];
        for (_, d) in sets {
            for cut in [0, 1, 3, 7, 15, 17, 31, 33, 255, 4099] {
                let len = (d.len() / w).saturating_sub(cut).max(1) * w;
                let v = st(&[(ty, 64, d[..len.min(d.len())].to_vec(), None)]);
                holds(&v);
            }
        }
    }
}

/// Tiles of grids with escapes (almost-grids, NaN, Inf, -0.0, zeros, scales of the largest
/// exponents): the converted weights of `synth::converted` with spans of them overwritten, every
/// case rebuilt.
#[test]
fn grid_escapes() {
    let d = aqr_tensors::synth::converted(9);
    let t = aqr_tensors::tensors::parse(&d).unwrap();
    let mut r = rng(5);
    let specials: [u16; 12] =
        [0x0000, 0x8000, 0x7f80, 0xff80, 0x7fc0, 0xffff, 0x7fff, 0x0001, 0x8001, 0xffc1, 0x7f81, 0x3f80];
    let mut kept = 0;
    for (i, tensor) in t.tensors.iter().take(6).enumerate() {
        let (start, len) = (tensor.start, tensor.data.len() as usize);
        // (the converted weights are BF16 and FP16: of two bytes)
        for how in 0..8 {
            let mut v = d.clone();
            let body = &mut v[start..start + len];
            let n = body.len() / 2;
            let mut put = |k: usize, x: u16| {
                if k < n {
                    body[k * 2..k * 2 + 2].copy_from_slice(&x.to_le_bytes())
                }
            };
            match how {
                0 => (0..200).for_each(|_| put(r.below(n as u64) as usize, specials[r.below(12) as usize])),
                1 => {
                    let s = r.below(n as u64 - 600) as usize;
                    let x = specials[r.below(12) as usize];
                    (s..s + 600).for_each(|k| put(k, x));
                }
                2 => (0..n.min(2000)).for_each(|k| put(k, specials[k / 130 % 12])),
                3 => {
                    let ks: Vec<(usize, u64)> = (0..300).map(|_| (r.below(n as u64) as usize, r.below(16))).collect();
                    for (k, b) in ks {
                        let x = u16::from_le_bytes([body[k * 2], body[k * 2 + 1]]) ^ (1 << b);
                        body[k * 2..k * 2 + 2].copy_from_slice(&x.to_le_bytes());
                    }
                }
                4 => {
                    for _ in 0..2000 {
                        let k = r.below(n as u64) as usize;
                        body[k * 2] ^= 1;
                    }
                }
                5 => {
                    let s = r.below(n as u64 - 1300) as usize;
                    (s..s + 600).for_each(|k| put(k, 0));
                    (s + 600..s + 1200).for_each(|k| put(k, 0x3f80));
                }
                6 => {
                    let s = r.below(n as u64 - 600) as usize;
                    (s..s + 600).for_each(|k| put(k, if k % 2 == 0 { 0x7f7f } else { 0xff7f }));
                }
                _ => {
                    put(0, 0x7fc0);
                    put(n - 1, 0xffff);
                    put(n / 2, 0x8000);
                }
            }
            let f = formula::encode_in(&v, 2, 1 << 17).unwrap_or_else(|| panic!("tensor {i} case {how}: not taken"));
            assert!(eval(&f) == v, "tensor {i} case {how}");
            kept += grid_runs(&f);
        }
    }
    // (spans of damage escape their tiles only: most of the grids are kept)
    assert!(kept >= 40, "{kept} grid runs kept");
}

/// How many runs of grids the applications of `f` have.
fn grid_runs(f: &Formula) -> usize {
    let mut n = 0;
    for a in apps(f) {
        let (mut at, mut k) = (0, 0);
        while at < a.params.len() {
            let r = Run::read(&a.params, &mut at, k).unwrap();
            if let Run::Grid(g) = &r {
                k = g.k;
                n += 1;
            }
        }
    }
    n
}

/// Tiles whose values are NaN with every payload, as a whole tensor and among the grid's: all of
/// them rebuilt, whatever the scale a fit would make of them.
#[test]
fn grid_of_nans() {
    for ty in ["BF16", "F16", "F32"] {
        let w = if ty == "F32" { 4 } else { 2 };
        for pat in [0xffu8, 0x7f, 0x80, 0x00] {
            let d: Vec<u8> = (0..64 * 512)
                .flat_map(|k| (0..w).map(move |j| if j == w - 1 { pat } else { (k % 251) as u8 | 0x80 }))
                .collect();
            holds(&st(&[(ty, 512, d, None)]));
        }
    }
}

/// Tensors that overlap, tie, come unsorted or lie about their offsets: not taken, or rebuilt as
/// they are.
#[test]
fn tensors_lying() {
    let mut r = rng(3);
    let mk = |r: &mut aqr_common::rng::Rng, n: usize| -> Vec<u8> {
        (0..n).flat_map(|_| ((r.below(2000) as f32 - 1000.0) * 1e-3).to_le_bytes()).collect()
    };
    let (a, b, c) = (mk(&mut r, 6000), mk(&mut r, 5000), mk(&mut r, 4000));
    let (la, lb, lc) = (a.len(), b.len(), c.len());
    let body = [a.clone(), b.clone(), c.clone()].concat();
    let big = |ts: &[T]| -> Vec<u8> {
        // (the data of ts in `body` as given, the header's offsets alone differing)
        let mut v = st(ts);
        let hl = u64::from_le_bytes(v[..8].try_into().unwrap()) as usize;
        v.truncate(8 + hl);
        v.extend_from_slice(&body);
        v
    };
    let f32 = |o: Option<(usize, usize)>| ("F32", 100u64, vec![], o);
    let cases: Vec<(&str, Vec<_>)> = vec![
        ("ordinary", vec![f32(Some((0, la))), f32(Some((la, la + lb))), f32(Some((la + lb, la + lb + lc)))]),
        ("unsorted", vec![f32(Some((la, la + lb))), f32(Some((0, la))), f32(Some((la + lb, la + lb + lc)))]),
        ("reversed", vec![f32(Some((la + lb, la + lb + lc))), f32(Some((la, la + lb))), f32(Some((0, la)))]),
        ("tied", vec![f32(Some((0, la))), f32(Some((0, la))), f32(Some((la, la + lb)))]),
        ("overlapping", vec![f32(Some((0, la))), f32(Some((la / 2, la / 2 + lb))), f32(Some((la + lb, la + lb + lc)))]),
        ("nested", vec![f32(Some((0, la + lb))), f32(Some((8, 8 + 4000))), f32(Some((la + lb, la + lb + lc)))]),
        ("empty", vec![f32(Some((0, 0))), f32(Some((0, la))), f32(Some((la, la)))]),
        ("inverted", vec![f32(Some((la, 0))), f32(Some((0, la)))]),
        ("past the end", vec![f32(Some((0, la))), f32(Some((la, la + lb + lc + 4)))]),
        ("far past", vec![f32(Some((0, la))), f32(Some((la, usize::MAX)))]),
        ("at the top", vec![f32(Some((usize::MAX - 8, usize::MAX))), f32(Some((0, la)))]),
        ("odd length", vec![f32(Some((0, la - 1))), f32(Some((la, la + lb - 2)))]),
        ("one byte", vec![f32(Some((0, 1))), f32(Some((1, 2)))]),
    ];
    for (name, ts) in &cases {
        let v = big(ts);
        aqr_testkit::within(Duration::from_secs(20), || {
            aqr_testkit::case(name);
            holds(&v);
        })
        .unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    // (the ordinary one is taken: the others' coverage is the point of the rest)
    assert!(holds(&big(&cases[0].1)));
}

/// A JSON header's lies: sizes, nesting, duplicated keys, numbers past 2^64, a header length at
/// every extreme.
#[test]
fn safetensors_headers() {
    let d = aqr_tensors::synth::safetensors(2, 1);
    let hl = u64::from_le_bytes(d[..8].try_into().unwrap()) as usize;
    let head = &d[8..8 + hl];
    let text = std::str::from_utf8(head).unwrap().to_string();
    let with = |h: &str| {
        let mut h = h.to_string();
        while !h.len().is_multiple_of(8) {
            h.push(' ');
        }
        let mut v = (h.len() as u64).to_le_bytes().to_vec();
        v.extend_from_slice(h.as_bytes());
        v.extend_from_slice(&d[8 + hl..]);
        v
    };
    let mut cases: Vec<(String, Vec<u8>)> = Vec::new();
    for len in [
        0u64,
        1,
        2,
        7,
        hl as u64 - 1,
        hl as u64 + 1,
        d.len() as u64,
        d.len() as u64 - 8,
        u64::MAX,
        1 << 63,
        100_000_001,
    ] {
        let mut v = d.clone();
        v[..8].copy_from_slice(&len.to_le_bytes());
        cases.push((format!("header length {len}"), v));
    }
    let deep = format!(
        r#"{{"__metadata__":{}"x"{},"a":{{"dtype":"F32","shape":[1],"data_offsets":[0,4]}}}}"#,
        "[".repeat(100000),
        "]".repeat(100000)
    );
    cases.push(("deep".into(), with(&deep)));
    cases.push(("deep object".into(), with(&format!(r#"{{"a":{}1{}}}"#, r#"{"b":"#.repeat(50000), "}".repeat(50000)))));
    for (name, h) in [
        ("empty", "{}".to_string()),
        ("big numbers", text.replace("\"shape\":[", "\"shape\":[99999999999999999999999,")),
        ("offset past 2^64", text.replacen("\"data_offsets\":[0,", "\"data_offsets\":[18446744073709551616,", 1)),
        ("offset 2^64-1", text.replacen("\"data_offsets\":[0,", "\"data_offsets\":[18446744073709551615,", 1)),
        ("negative", text.replacen("\"data_offsets\":[0,", "\"data_offsets\":[-1,", 1)),
        ("float", text.replacen("\"data_offsets\":[0,", "\"data_offsets\":[0.5,", 1)),
        ("duplicate key", text.replacen("layer.1.weight", "layer.0.weight", 1)),
        ("unknown dtype", text.replace("\"F32\"", "\"F33\"")),
        ("dtype twice", text.replacen("\"dtype\":\"F32\"", "\"dtype\":\"F32\",\"dtype\":\"I8\"", 1)),
        ("no offsets", text.replace("data_offsets", "offsets")),
        ("one offset", text.replacen("\"data_offsets\":[0,", "\"data_offsets\":[", 1)),
        ("three offsets", text.replacen("\"data_offsets\":[0,", "\"data_offsets\":[0,0,", 1)),
        ("scalar shape", text.replacen("\"shape\":[", "\"shape\":[],\"x\":[", 1)),
        ("trailing comma", text.replacen("}}", "},}", 1)),
        ("escape at the end", "{\"a\\".to_string()),
        ("unterminated", text[..text.len() - 3].to_string()),
        ("nul", text.replacen("layer", "la\u{0}yer", 1)),
    ] {
        cases.push((name.to_string(), with(&h)));
    }
    for (name, v) in &cases {
        aqr_testkit::within(Duration::from_secs(20), || {
            aqr_testkit::case(name);
            holds(v);
        })
        .unwrap_or_else(|e| panic!("{name}: {e}"));
    }
}

/// GGUF's header lies: counts, dimensions, offsets, alignments, strings and arrays past the file.
#[test]
fn gguf_headers() {
    let d = aqr_tensors::synth::file(2, 1);
    let mut cases: Vec<(String, Vec<u8>)> = Vec::new();
    let mut set = |name: &str, at: usize, bytes: &[u8]| {
        let mut v = d.clone();
        v[at..at + bytes.len()].copy_from_slice(bytes);
        cases.push((name.to_string(), v));
    };
    // (the tensor count, the key-value count)
    for x in [0u64, 1, 1 << 20, 1 << 40, u64::MAX, 1 << 63] {
        set(&format!("tensors {x}"), 8, &x.to_le_bytes());
        set(&format!("key-values {x}"), 16, &x.to_le_bytes());
    }
    // (the first key: its length)
    for x in [0u64, 1 << 30, u64::MAX] {
        set(&format!("first key length {x}"), 24, &x.to_le_bytes());
    }
    // (the alignment's value, and of the key's type)
    let at = d.windows(17).position(|w| w == b"general.alignment").unwrap() + 17;
    for x in [0u32, 1, 3, 31, 33, 1 << 31, u32::MAX] {
        set(&format!("alignment {x}"), at + 4, &x.to_le_bytes());
    }
    for t in [0u32, 1, 5, 8, 9, 10, 99] {
        set(&format!("alignment of type {t}"), at, &t.to_le_bytes());
    }
    // (the tokens' array: its element type and count)
    let tk = d.windows(16).position(|w| w == b"tokenizer.tokens").unwrap() + 16;
    for t in [0u32, 4, 8, 9, 12, 13, 99] {
        set(&format!("array of type {t}"), tk + 4, &t.to_le_bytes());
    }
    for x in [0u64, 4, 1 << 40, u64::MAX] {
        set(&format!("array count {x}"), tk + 8, &x.to_le_bytes());
    }
    // (the first tensor's info, after the key-values: its name's length, dims, type and offset)
    let ti = d.windows(12).position(|w| w == b"blk.0.weight").unwrap() - 8;
    for x in [0u64, 1 << 40, u64::MAX] {
        set(&format!("tensor name length {x}"), ti, &x.to_le_bytes());
    }
    let after = ti + 8 + 12;
    for x in [0u32, 2, 8, 9, u32::MAX] {
        set(&format!("dims {x}"), after, &x.to_le_bytes());
    }
    for x in [0u64, 1, u64::MAX, 1 << 63, (1 << 32) + 3] {
        set(&format!("dim {x}"), after + 4, &x.to_le_bytes());
        set(&format!("offset {x}"), after + 4 + 8 + 4, &x.to_le_bytes());
    }
    for x in [0u32, 2, 3, 30, 31, 34, 35, 38, 39, 40, 1000, u32::MAX] {
        set(&format!("type {x}"), after + 4 + 8, &x.to_le_bytes());
    }
    for (name, v) in &cases {
        aqr_testkit::within(Duration::from_secs(20), || {
            aqr_testkit::case(name);
            holds(v);
        })
        .unwrap_or_else(|e| panic!("{name}: {e}"));
    }
}

/// Pickles of `torch.save` damaged where they are (a persistent id's count or key, a memo's index, a
/// length): the sweep over the pickle's bytes, every way.
#[test]
fn pickles_damaged() {
    let d = aqr_tensors::synth::pth(6, 1);
    let at = d.windows(8).position(|w| w == [0x80, 2, b'}', b'q', 0, b'(', b'X', 0x0c]).unwrap_or(0);
    let at = if at == 0 { d.windows(2).position(|w| w == [0x80, 2]).unwrap() } else { at };
    // (the whole front of the file: headers and the pickle)
    aqr_testkit::damaged(&d, 20, at + 2000, Duration::from_secs(120), |v| {
        holds(v);
    });
}

/// Parameters of an application damaged, every way (`Run::read`'s fields: tags, counts, widths,
/// grids' shapes): an error or other bytes of the length asked, never a panic, a hang or a huge
/// allocation.
#[test]
fn params_damaged() {
    for (name, d) in [("gguf", aqr_tensors::synth::file(7, 1)), ("converted", aqr_tensors::synth::converted(7))] {
        let f = formula::encode_in(&d, 2, 1 << 16).unwrap();
        // (every application's params, up to a few of each kind of run)
        for (k, a) in apps(&f).into_iter().take(6).enumerate() {
            let ins = inputs(&f, a);
            let n = a.params.len();
            aqr_testkit::damaged(&a.params, 400, 80.min(n), Duration::from_secs(30), |p| {
                if let Ok(out) = decode(p, &ins, a.len, 1) {
                    assert_eq!(out.len() as u64, a.len, "{name} app {k}");
                }
            });
            // (also with an output length lying)
            for len in [0u64, 1, a.len / 2, a.len + 1, a.len * 3, 1 << 33, u64::MAX / 2] {
                aqr_testkit::case("length");
                let _ = decode(&a.params, &ins, len, 1);
            }
        }
    }
}

/// A tensor over the real 16 MiB boundary of a segment (default segment size): rebuilt, in more than
/// one application, the tensor's elements cut where the segment is full.
#[test]
fn segment_boundary() {
    let mut r = rng(21);
    // (BF16 of 20 MiB: 10 Mi elements, with a second small tensor after it)
    let n = 10 << 20;
    let big: Vec<u8> = (0..n)
        .flat_map(|_| {
            let x = (r.below(2000) as f32 - 1000.0) * 1e-3;
            ((x.to_bits() >> 16) as u16).to_le_bytes()
        })
        .collect();
    let small: Vec<u8> = (0..5000).flat_map(|_| (r.below(100) as f32 * 0.01).to_le_bytes()).collect();
    let v = st(&[("BF16", 4096, big, None), ("F32", 100, small, None)]);
    let f = formula::encode(&v, 4).expect("taken");
    assert!(apps(&f).len() >= 2, "{} applications", apps(&f).len());
    assert!(eval(&f) == v);
    // (a file of exactly the segment's size of data, and one byte over)
    for extra in [0usize, 1, 2] {
        let len = (formula::SEGMENT / 2 + extra - 1) * 2;
        let d: Vec<u8> = (0..len).map(|_| r.below(256) as u8 & 0x7f).collect();
        let v = st(&[("BF16", 64, d, None)]);
        holds(&v);
    }
}
