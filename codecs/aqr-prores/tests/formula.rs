//! The formulas of the fixtures (`scripts/gen-fixtures.sh`) rebuild them through the module,
//! and damaged ones fail without a panic.

use aqr_formula::{Formula, Node};
use aqr_prores::formula;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

const FIXTURES: [&str; 4] = ["ks-422.mov", "aw-422hq.mov", "ks-4444-alpha.mov", "ks-interlaced.mov"];

/// The bytes of `n`, a node of the formulas aqr-prores makes, its applications by the module.
fn eval(f: &Formula, n: &Node) -> Result<Vec<u8>, String> {
    Ok(match n {
        Node::Slice { stream, start, len } => {
            f.streams[*stream].data[*start as usize..(*start + *len) as usize].to_vec()
        }
        Node::Concat(ns) => ns.iter().map(|n| eval(f, n)).collect::<Result<Vec<_>, _>>()?.concat(),
        Node::Apply(a) => {
            assert_eq!(a.module.name, formula::MODULE.name);
            let inputs = a.inputs.iter().map(|n| eval(f, n)).collect::<Result<Vec<_>, _>>()?;
            let refs: Vec<&[u8]> = inputs.iter().map(|v| &v[..]).collect();
            formula::decode(&a.params, &refs, a.len).map_err(|e| e.to_string())?
        }
        other => panic!("not in these formulas: {other:?}"),
    })
}

#[test]
fn fixtures_round_trip() {
    for t in FIXTURES {
        let d = fixture(t);
        assert!(formula::starts(&d), "{t}");
        let f = formula::encode(&d, 4).unwrap_or_else(|| panic!("{t}"));
        assert!(eval(&f, &f.root).unwrap() == d, "{t}");
    }
}

#[test]
fn files_of_several_chunks() {
    // Two files one after the other: the frames apart by more than a chunk's gap start another.
    let (a, b) = (fixture("ks-422.mov"), fixture("aw-422hq.mov"));
    let d = [a.as_slice(), &vec![7u8; 5 << 20], &b].concat();
    let f = formula::encode(&d, 2).unwrap();
    let applies = |n: &Node| match n {
        Node::Concat(ns) => ns.iter().filter(|n| matches!(n, Node::Apply(_))).count(),
        _ => 0,
    };
    assert_eq!(applies(&f.root), 2);
    assert!(eval(&f, &f.root).unwrap() == d);
}

#[test]
fn not_prores() {
    let mut d = fixture("ks-422.mov");
    // the frames' tags gone
    for i in 0..d.len() - 4 {
        if &d[i..i + 4] == b"icpf" {
            d[i] = b'x';
        }
    }
    assert!(!formula::starts(&d));
    assert!(formula::encode(&d, 1).is_none());
    assert!(!formula::starts(b"\0\0\0\x14ftypqt  "));
}

#[test]
fn damaged_chunks() {
    let d = fixture("ks-4444-alpha.mov");
    let f = formula::encode(&d, 1).unwrap();
    for class in ["prores/coded", "prores/side"] {
        let coded = f.streams.iter().position(|s| s.class == class).unwrap();
        let n = f.streams[coded].data.len();
        for k in 0..64 {
            let mut bad = f.clone();
            let s = &mut bad.streams[coded].data;
            let at = n * k / 64;
            s[at] ^= 1 << (k % 8);
            // decoding gives an error or some bytes, but no panic
            let r = std::panic::catch_unwind(|| eval(&bad, &bad.root));
            assert!(r.is_ok(), "panic with byte {at} of {class} changed");
        }
    }
}
