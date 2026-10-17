//! The formula module: `formula::encode` evaluated through `formula::decode` gives the input back,
//! over the files in tests/data (also with junk, cut, as raw PCM, on threads), and nothing for
//! what the container stores.

use flac_recons::container::Options;
use flac_recons::formula::{self, MODULE};
use flac_recons::pcm::Format;
use recons_formula::{Apply, Formula};

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

fn eval(f: &Formula, threads: usize) -> recons_formula::Result<Vec<u8>> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| {
        assert_eq!(a.module, MODULE);
        formula::decode(&a.params, inputs, a.len, threads).map_err(|e| recons_formula::Error::new(e.0))
    })
}

/// The formula of `d`, if there is one, evaluated back (on 1 and several threads).
fn check(name: &str, d: &[u8], opt: &Options) -> Option<Formula> {
    let f = formula::encode_with(d, opt)?;
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
            let f = check(&name, &d, &Options { threads, ..Options::default() }).expect(&name);
            let [a] = f.applies()[..] else { panic!("{name}") };
            assert_eq!(a.params, [name.ends_with(".flac") as u8 ^ 1], "{name}");
            assert_eq!(a.inputs.len(), 9);
        }
        assert!(formula::encode(&d, 1).is_some());
    }
}

#[test]
fn junk_and_cuts() {
    for (name, d) in files() {
        let opt = Options::default();
        let mut j = b"some junk before".to_vec();
        j.extend_from_slice(&d);
        j.extend_from_slice(b"TAG and after");
        check(&name, &j, &opt);
        for cut in [1, 5, 417, d.len() / 3, d.len() / 2 + 7] {
            check(&name, &d[..d.len() - cut.min(d.len())], &opt);
            check(&name, &d[cut.min(d.len())..], &opt);
        }
    }
}

#[test]
fn raw_pcm() {
    for (name, d) in files() {
        for f in ["s16le:2", "s24be:2", "u8:1", "s32le:6", "s16be:3"] {
            let opt = Options { raw: Format::parse(f), threads: 1 };
            let fm = check(&format!("{name} as {f}"), &d, &opt).expect(&name);
            assert_eq!(fm.applies()[0].params, [1]);
        }
    }
}

#[test]
fn not_coded() {
    assert!(formula::encode(b"not audio at all", 1).is_none());
    assert!(formula::encode(b"", 1).is_none());
    let tiny_flac = b"fLaC\x80\x00\x00\x22".iter().copied().chain([0u8; 34]).collect::<Vec<u8>>();
    // Whatever it does, it must not panic and must round-trip.
    check("tiny flac", &tiny_flac, &Options::default());
    let raw = Options { raw: Format::parse("s16le:1"), threads: 1 };
    check("two bytes", b"ab", &raw);
}

#[test]
fn corrupt_inputs() {
    let (_, d) = files().into_iter().next().unwrap();
    let f = formula::encode(&d, 1).unwrap();
    let a = f.applies()[0];
    let ins: Vec<Vec<u8>> = f.streams.iter().map(|s| s.data.clone()).collect();
    let run = |p: &[u8], i: &[Vec<u8>], len: u64| {
        let r: Vec<&[u8]> = i.iter().map(|v| &v[..]).collect();
        let _ = formula::decode(p, &r, len, 1);
    };
    run(&[2], &ins, a.len);
    run(&[], &ins, a.len);
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
