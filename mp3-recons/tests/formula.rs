//! Formulas: `formula::encode` evaluated through `formula::decode` gives the input back, with
//! any number of threads; inputs that the container stores give no formula.

use mp3_recons::{compress, formula};
use recons_formula::{Apply, Formula, Node};

const THREADS: [usize; 3] = [1, 2, 8];

fn files() -> Vec<(String, Vec<u8>)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| (e.file_name().to_string_lossy().into_owned(), std::fs::read(e.path()).unwrap()))
        .collect();
    v.sort();
    assert!(!v.is_empty());
    v
}

/// `d` repeated up to at least 600 KB.
fn big(d: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    while v.len() < 600 << 10 {
        v.extend_from_slice(d);
    }
    v
}

fn eval(f: &Formula, threads: usize) -> recons_formula::Result<Vec<u8>> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| {
        assert_eq!(a.module, formula::MODULE);
        formula::decode(&a.params, inputs, a.len, threads).map_err(|e| recons_formula::Error::new(e.0))
    })
}

/// The formula, if the container codes `d`, and then its round trip.
fn check(name: &str, d: &[u8]) -> bool {
    let f = formula::encode(d, 1);
    let coded = compress(d)[8] == 1;
    assert_eq!(f.is_some(), coded, "{name}: formula iff coded");
    let Some(f) = f else { return false };
    assert_eq!(f.len(), d.len() as u64);
    let Node::Apply(a) = &f.root else { panic!("{name}: root") };
    assert!(a.params.is_empty() && a.inputs.len() == 10);
    for t in THREADS {
        assert!(eval(&f, t).unwrap() == d, "{name}: {t} threads");
        let g = formula::encode(d, t).unwrap();
        assert!(g == f, "{name}: formula built with {t} threads differs");
    }
    true
}

#[test]
fn small_inputs() {
    for (name, d) in files() {
        assert!(check(&name, &d), "{name}");
    }
}

#[test]
fn large_inputs() {
    let all = files();
    for (name, d) in &all {
        let b = big(d);
        assert!(check(name, &b));
        let mut j = b"some junk before".to_vec();
        j.extend_from_slice(&b);
        j.extend_from_slice(b"TAG and after");
        assert!(check(name, &j));
        for cut in [1, 417, b.len() / 3 + 5] {
            check(name, &b[..b.len() - cut]);
            check(name, &b[cut..]);
        }
        // Streams concatenated (sample rate and mode change).
        let mut two = b.clone();
        for (_, e) in &all {
            two.extend_from_slice(e);
        }
        assert!(check(name, &two));
    }
}

#[test]
fn not_coded() {
    assert!(formula::encode(b"", 1).is_none());
    assert!(formula::encode(b"not an mp3 file at all", 1).is_none());
    let mut x = 0x2545f4914f6cdd1du64;
    let junk: Vec<u8> = (0..5000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect();
    assert!(formula::encode(&junk, 1).is_none());
    assert!(formula::encode(&junk, 4).is_none());
    // Truncated to a few bytes of a frame.
    let (_, d) = &files()[0];
    for n in [1, 4, 10, 100] {
        check("short", &d[..n]);
    }
}

#[test]
fn corrupt_inputs() {
    let (name, d) = &files()[0];
    let f = formula::encode(&big(d), 1).unwrap();
    let Node::Apply(a) = &f.root else { panic!() };
    let slots: Vec<&[u8]> = f.streams.iter().map(|s| &s.data[..]).collect();
    let len = a.len;
    for t in [1, 4] {
        assert!(formula::decode(&[], &slots, len, t).unwrap() == big(d), "{name}");
        assert!(formula::decode(&[0], &slots, len, t).is_err());
        assert!(formula::decode(&[], &slots[..9], len, t).is_err());
        assert!(formula::decode(&[], &slots, len + 1, t).is_err());
        let mut words = slots[8].to_vec();
        words.push(0);
        let mut s = slots.clone();
        s[8] = &words;
        assert!(formula::decode(&[], &s, len, t).is_err());
        // Flips in each slot: an error or the same output, never a panic.
        for k in 0..10 {
            if slots[k].is_empty() {
                continue;
            }
            let mut m = slots[k].to_vec();
            let i = m.len() / 2;
            m[i] ^= 0x5a;
            let mut s = slots.clone();
            s[k] = &m;
            let one = formula::decode(&[], &s, len, 1).map_err(|e| e.0);
            assert_eq!(formula::decode(&[], &s, len, t).map_err(|e| e.0), one, "slot {k}, {t} threads");
        }
    }
}
