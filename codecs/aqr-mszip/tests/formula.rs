//! CAB files of `scripts/gen-fixtures.py` (MSZIP folders by Python's zlib, stored folders,
//! reserved fields) taken apart and given back.

use aqr_decompress::Native;
use aqr_formula::{Formula, Node};
use aqr_mszip::{cab, formula};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}.cab", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn eval(f: &Formula) -> Vec<u8> {
    f.eval(&Native::default()).unwrap()
}

/// The bytes of the folders of `d` (their data).
fn folder_data(d: &[u8]) -> usize {
    cab::parse(d).unwrap().folders.iter().map(|f| f.size()).sum()
}

#[test]
fn taken_apart_and_given_back() {
    for name in ["mszip", "mixed", "fixed", "small"] {
        let d = fixture(name);
        let f = formula::encode(&d, 2).expect(name);
        assert!(eval(&f) == d, "{name}");
        // the files' contents are all there, apart from the rest
        let contents: usize = f.streams.iter().filter(|s| s.class == "raw").map(|s| s.data.len()).sum();
        assert_eq!(contents, folder_data(&d), "{name}");
        // what is kept: the header, folders and files, not the folders' spans
        let c = cab::parse(&d).unwrap();
        let spans: usize = c.folders.iter().map(|f| f.span().unwrap().len()).sum();
        assert_eq!(f.streams[0].data.len(), d.len() - spans, "{name}");
    }
}

/// Folders of several spans: one application, split into them, in place among the bytes kept
/// (none after the last).
#[test]
fn folders_in_place() {
    let d = fixture("mixed");
    let f = formula::encode(&d, 1).unwrap();
    let Node::Concat(ns) = &f.root else { panic!("not a concatenation") };
    let kinds: Vec<&str> = ns
        .iter()
        .map(|n| match n {
            Node::Output { .. } => "folder",
            Node::Slice { stream: 0, .. } => "kept",
            n => panic!("{n:?}"),
        })
        .collect();
    assert_eq!(kinds, ["kept", "folder", "folder", "folder"]);
    assert_eq!(f.shared.len(), 1);
}

/// Contents by the kinds of their names, as `Options::kind` tells them.
#[test]
fn contents_by_kind() {
    let d = fixture("mszip");
    let kind = |n: &[u8]| if n.ends_with(b".txt") { aqr_formula::Kind::Text } else { aqr_formula::Kind::Unknown };
    let e = formula::encode_with(&d, &formula::Options { kind, ..Default::default() }).unwrap();
    assert!(eval(&e.formula) == d);
    let text: usize = e.formula.streams.iter().filter(|s| s.class == "cab/text").map(|s| s.data.len()).sum();
    assert_eq!(text, 105_000);
    assert_eq!(e.inflated, 145_000);
}

/// MSZIP folders past the limit are kept as they are; stored folders still taken.
#[test]
fn limit() {
    let d = fixture("mixed");
    let e = formula::encode_with(&d, &formula::Options { limit: 80_000, ..Default::default() }).unwrap();
    assert!(eval(&e.formula) == d);
    assert_eq!(e.inflated, 70_000);
    let e = formula::encode_with(&d, &formula::Options { limit: 0, ..Default::default() }).unwrap();
    assert!(eval(&e.formula) == d);
    assert_eq!(e.inflated, 0);
    assert!(formula::encode_with(&fixture("mszip"), &formula::Options { limit: 0, ..Default::default() }).is_none());
}

#[test]
fn not_a_cab() {
    assert!(formula::encode(b"MSCF\0\0\0\0", 1).is_none());
    assert!(formula::encode(&[0; 300], 1).is_none());
    // a folder of LZX is kept
    let mut d = fixture("small");
    let c = cab::parse(&d).unwrap();
    assert_eq!(c.folders[0].method(), cab::MSZIP);
    d[36 + 6] = cab::LZX as u8;
    assert!(formula::encode(&d, 1).is_none());
}

/// Files cut short or with bytes changed: no panic, and a formula, where there is one, gives
/// the input back.
#[test]
fn damaged() {
    let d = fixture("mixed");
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for cut in [10, 36, 50, 200, d.len() / 3, d.len() / 2, d.len() - 10] {
        if let Some(f) = formula::encode(&d[..cut], 1) {
            assert!(eval(&f) == d[..cut], "cut at {cut}");
        }
    }
    let first = cab::parse(&d).unwrap().folders[0].blocks[0].start;
    for _ in 0..200 {
        let mut e = d.clone();
        for _ in 0..1 + next() % 3 {
            // the header, folders and files, or the data blocks
            let at = match next() % 2 {
                0 => (next() % first as u64) as usize,
                _ => first + (next() % (d.len() - first) as u64) as usize,
            };
            e[at] = next() as u8;
        }
        if let Some(f) = formula::encode(&e, 1) {
            assert!(eval(&f) == e);
        }
    }
}
