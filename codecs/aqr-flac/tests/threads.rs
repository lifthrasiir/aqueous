//! The threaded paths: the same formula whatever the number of threads, and evaluating
//! back to the input on any. Over the files in tests/data, with junk and cuts, and over
//! inputs large enough to fill the pipelines (many batches).

use aqr_flac::formula::{self, MODULE};
use aqr_formula::{Apply, Formula};

const THREADS: [usize; 3] = [2, 3, 8];

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

fn file(name: &str) -> Vec<u8> {
    files().into_iter().find(|f| f.0 == name).unwrap().1
}

fn eval(f: &Formula, threads: usize) -> aqr_formula::Result<Vec<u8>> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| {
        assert_eq!(a.module, MODULE);
        let res: Vec<&[u8]> = a.resources.iter().map(|r| &r[..]).collect();
        formula::decode(&a.params, &res, inputs, a.len, threads).map_err(|e| aqr_formula::Error::new(e.0))
    })
}

/// Same formula for all thread counts, and every evaluation round-trips (the formula, if
/// the input is coded at all).
fn check(name: &str, d: &[u8]) -> Option<Formula> {
    let f1 = formula::encode(d, 1);
    for t in THREADS {
        let f = formula::encode(d, t);
        assert!(f == f1, "{name}: {t} threads, other formula");
        if let Some(f) = &f {
            assert!(eval(f, t).as_deref() == Ok(d), "{name}: {t} threads, evaluate");
        }
    }
    f1
}

#[test]
fn data_files() {
    for (name, d) in files() {
        check(&name, &d).expect(&name);
        let mut j = b"some junk before".to_vec();
        j.extend_from_slice(&d);
        j.extend_from_slice(b"TAG and after");
        check(&format!("{name} with junk"), &j);
        for cut in [1, 417, d.len() / 3] {
            check(&format!("{name} cut {cut}"), &d[..d.len() - cut.min(d.len())]);
            check(&format!("{name} from {cut}"), &d[cut.min(d.len())..]);
        }
    }
}

#[test]
fn small_and_empty() {
    for d in [&b""[..], b"x", b"fLaC", b"RIFF\0\0\0\0WAVE"] {
        check(&format!("{d:?}"), d);
    }
}

/// FLAC files one after the other (the later headers become junk), with junk and damaged
/// frames: hundreds of frames, many batches.
#[test]
fn long_flac() {
    let mut d = Vec::new();
    for (i, name) in ["s16_f5.flac", "s16_junk.flac", "s16_lax.flac", "s24_f8.flac"].iter().cycle().take(40).enumerate()
    {
        let mut f = file(name);
        if i % 7 == 3 {
            // A damaged frame (kept as junk).
            let p = f.len() / 2;
            f[p] ^= 0x40;
        }
        d.extend_from_slice(&f);
        if i % 5 == 0 {
            d.extend_from_slice(b"junk between");
        }
    }
    check("long FLAC", &d).unwrap();
    // Only one kind of stream (the same sample size): no junk but the headers.
    let one = file("s16_f5.flac").repeat(30);
    check("s16_f5 x30", &one);
}
