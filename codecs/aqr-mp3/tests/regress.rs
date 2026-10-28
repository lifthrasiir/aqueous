//! Round trips of the files in tests/data (small LAME encodings of a synthetic signal),
//! through the coded path and the formula, also with junk around and cut short.

use aqr_formula::Apply;
use aqr_mp3::{codec, formula};

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

/// The coded path and the formula (if any) give `d` back; whether there is a formula.
fn roundtrip(name: &str, d: &[u8]) -> bool {
    let c = codec::encode(d);
    assert_eq!(codec::decode(&c, codec::tables()).unwrap(), d, "{name}: coded path");
    let Some(f) = formula::encode(d, 1) else { return false };
    let out = f
        .eval(&|a: &Apply, inputs: &[&[u8]]| {
            let res: Vec<&[u8]> = a.resources.iter().map(|r| &r[..]).collect();
            formula::decode(&a.params, &res, inputs, a.len, 1).map_err(|e| aqr_formula::Error::new(e.0))
        })
        .unwrap();
    assert_eq!(out, d, "{name}: formula");
    true
}

#[test]
fn data_files() {
    for (name, d) in files() {
        // These should all have a formula, with streams smaller than the input.
        assert!(roundtrip(&name, &d), "{name}: no formula");
        let f = formula::encode(&d, 1).unwrap();
        let n: usize = f.streams.iter().map(|s| s.data.len()).sum();
        assert!(n < d.len(), "{name}: {n} >= {}", d.len());
    }
}

#[test]
fn junk_and_cuts() {
    for (name, d) in files() {
        let mut j = b"some junk before".to_vec();
        j.extend_from_slice(&d);
        j.extend_from_slice(b"TAG and after");
        roundtrip(&name, &j);
        for cut in [1, 5, 417, d.len() / 3, d.len() / 2 + 7] {
            roundtrip(&name, &d[..d.len() - cut.min(d.len())]);
            roundtrip(&name, &d[cut.min(d.len())..]);
        }
        // Two streams concatenated (sample rate and mode may change).
        let mut two = d.clone();
        two.extend_from_slice(&files()[0].1);
        roundtrip(&name, &two);
    }
}
