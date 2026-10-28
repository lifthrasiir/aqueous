//! The formula module: `formula::encode` evaluated through `formula::decode` gives the input back,
//! over the files in tests/data (also with junk, cut, on threads), and nothing for what is not
//! coded.

use aqr_flac::formula::{self, MODULE};
use aqr_formula::{Apply, Formula, aqr};

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

fn eval(f: &Formula, threads: usize) -> aqr_formula::Result<Vec<u8>> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| {
        assert_eq!(a.module, MODULE);
        let res: Vec<&[u8]> = a.resources.iter().map(|r| &r[..]).collect();
        formula::decode(&a.params, &res, inputs, a.len, threads).map_err(|e| aqr_formula::Error::new(e.0))
    })
}

/// The formula of `d`, if there is one, evaluated back (on 1 and several threads).
fn check(name: &str, d: &[u8], threads: usize) -> Option<Formula> {
    let f = formula::encode(d, threads)?;
    assert_eq!(f.len(), d.len() as u64, "{name}");
    assert_eq!(f.streams.len(), 9, "{name}");
    for t in [1, 3] {
        assert!(eval(&f, t).unwrap() == d, "{name}: {t} threads");
    }
    Some(f)
}

#[test]
fn data_files() {
    for (name, d) in files() {
        for threads in [1, 4] {
            let f = check(&name, &d, threads).expect(&name);
            let [a] = f.applies()[..] else { panic!("{name}") };
            assert!(a.params.is_empty(), "{name}");
            assert_eq!(a.inputs.len(), 9);
        }
        // These should all be coded, and smaller than the input as archives; an archive
        // also holds the tables (2306 bytes compressed, once).
        let f = formula::encode(&d, 1).unwrap();
        let n = aqr::encode(&f, Some(aqr::checksum(&d)), aqr::Level::MID).unwrap().len();
        assert!(n < d.len() + 2400, "{name}: {n} >= {} + 2400", d.len());
    }
}

#[test]
fn junk_and_cuts() {
    for (name, d) in files() {
        let opt = 1;
        let mut j = b"some junk before".to_vec();
        j.extend_from_slice(&d);
        j.extend_from_slice(b"TAG and after");
        check(&name, &j, opt);
        // Two files concatenated.
        let mut two = d.clone();
        two.extend_from_slice(&files()[0].1);
        check(&name, &two, opt);
        for cut in [1, 5, 417, d.len() / 3, d.len() / 2 + 7] {
            check(&name, &d[..d.len() - cut.min(d.len())], opt);
            check(&name, &d[cut.min(d.len())..], opt);
        }
    }
}

#[test]
fn not_coded() {
    assert!(formula::encode(b"not audio at all", 1).is_none());
    assert!(formula::encode(b"", 1).is_none());
    let tiny_flac = b"fLaC\x80\x00\x00\x22".iter().copied().chain([0u8; 34]).collect::<Vec<u8>>();
    // Whatever it does, it must not panic and must round-trip.
    check("tiny flac", &tiny_flac, 1);
    // WAV and AIFF are aqr-raw's.
    assert!(formula::encode(b"RIFF\0\0\0\0WAVEfmt ", 1).is_none());
}

#[test]
fn corrupt_inputs() {
    let (_, d) = files().into_iter().next().unwrap();
    let f = formula::encode(&d, 1).unwrap();
    let a = f.applies()[0];
    let ins: Vec<Vec<u8>> = f.streams.iter().map(|s| s.data.clone()).collect();
    let run = |p: &[u8], i: &[Vec<u8>], len: u64| {
        let r: Vec<&[u8]> = i.iter().map(|v| &v[..]).collect();
        let _ = formula::decode(p, &[&a.resources[0][..]], &r, len, 1);
    };
    run(&[0], &ins, a.len);
    run(&[1], &ins, a.len);
    run(&a.params, &ins[..8], a.len);
    run(&a.params, &ins, a.len + 1);
    run(&a.params, &ins, u64::MAX);
    for k in 0..ins.len() {
        let mut m = ins.clone();
        let n = m[k].len();
        m[k].truncate(n / 2);
        run(&a.params, &m, a.len);
        for i in (0..n).step_by(53) {
            let mut m = ins.clone();
            m[k][i] ^= 0x5a;
            run(&a.params, &m, a.len);
        }
    }
    let mut m = ins.clone();
    m[6].push(1);
    run(&a.params, &m, a.len);
}

/// The decoder codes with the resource it is given, not the built-in tables.
#[test]
fn resource() {
    let (_, d) = files().into_iter().next().unwrap();
    let f = formula::encode(&d, 1).unwrap();
    let a = f.applies()[0];
    assert_eq!(a.module, MODULE);
    assert_eq!(a.resources.len(), 1);
    assert!(a.resources[0][..] == formula::resource()[..]);
    let ins: Vec<&[u8]> = f.streams.iter().map(|s| &s.data[..]).collect();
    let run = |res: &[&[u8]]| formula::decode(&a.params, res, &ins, a.len, 1).map_err(|e| e.0);
    let good = &a.resources[0][..];
    assert!(run(&[good]).unwrap() == d);
    assert!(run(&[]).is_err());
    assert!(run(&[good, good]).is_err());
    let mut bad = good.to_vec();
    bad[0] ^= 1; // layout id
    assert!(run(&[&bad]).is_err());
    assert!(run(&[&good[..good.len() - 2]]).is_err());
    assert!(run(&[&[good, &[0, 0][..]].concat()]).is_err());
    // Valid tables, different from the built-in ones (two frequencies of each context
    // exchanged): the output is not `d`.
    let mut other = good.to_vec();
    for c in 0..124 {
        let o = 4 + c * 144;
        other.copy_within(o + 2..o + 4, o + 4);
        other[o + 2..o + 4].copy_from_slice(&good[o + 4..o + 6]);
    }
    assert!(other != good);
    assert!(run(&[&other]).map_or(true, |o| o != d));
    // Frequencies not summing to the scale are rejected, not a panic.
    let mut bad = good.to_vec();
    bad[4] ^= 1;
    assert!(run(&[&bad]).is_err());
}
