//! IMA ADPCM WAV files (Microsoft's layout, format tag 0x11) through the formula: their blocks
//! by aqr-ima's `ima-recons`, the header and a partial block at the end kept as they are.
//! The files of `tests/ima` were written by ffmpeg (`adpcm_ima_wav`) and sox (`-e ima-adpcm`)
//! from `tests/data/s16.wav`.

use aqr_formula::{Apply, Formula, aqr};
use aqr_raw::container::{self, Ima, Layout};
use aqr_raw::formula;

fn files() -> Vec<(String, Vec<u8>)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/ima");
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| (e.file_name().to_string_lossy().into_owned(), std::fs::read(e.path()).unwrap()))
        .collect();
    v.sort();
    assert_eq!(v.len(), 3);
    v
}

fn eval(f: &Formula) -> aqr_formula::Result<Vec<u8>> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| {
        assert_eq!(a.module, aqr_ima::formula::MODULE);
        aqr_ima::formula::decode(&a.params, inputs, a.len).map_err(|e| aqr_formula::Error::new(e.0))
    })
}

/// The formula of `d`, the same on every number of threads, evaluated back.
fn check(name: &str, d: &[u8]) -> Option<Formula> {
    let f = formula::encode(d, 1);
    assert!(formula::encode(d, 4) == f, "{name}: other formula on 4 threads");
    let f = f?;
    assert_eq!(f.len(), d.len() as u64, "{name}");
    assert!(eval(&f).unwrap() == d, "{name}");
    Some(f)
}

#[test]
fn files_coded() {
    for (name, d) in files() {
        let c = container::detect(&d).expect(&name);
        let channels = if name.contains("mono") { 1 } else { 2 };
        let per_channel = if name.starts_with("ffmpeg") { 512 } else { 256 };
        assert_eq!((c.name, c.layout), ("wav", Layout::Ima(Ima { channels, per_channel })), "{name}");
        // The partial block at the end is the trailer.
        assert!(c.len > 0 && c.len.is_multiple_of(channels * per_channel) && c.start + c.len <= d.len(), "{name}");
        let f = check(&name, &d).expect(&name);
        let [a] = f.applies()[..] else { panic!("{name}") };
        assert_eq!(a.len, c.len as u64, "{name}");
        let n = aqr::encode(&f, Some(aqr::checksum(&d)), aqr::Level::MID).unwrap().len();
        assert!(n * 10 < d.len() * 8, "{name}: {n} of {}", d.len());
    }
}

#[test]
fn junk_and_cuts() {
    for (name, d) in files() {
        let mut j = d.clone();
        j.extend_from_slice(b"LIST and after");
        check(&name, &j).expect(&name);
        for cut in [1, 5, 417, d.len() / 3, d.len() / 2 + 7] {
            check(&name, &d[..d.len() - cut.min(d.len())]);
            check(&name, &d[cut.min(d.len())..]);
        }
    }
}

/// Layouts aqr-ima does not take, and blocks not all in the file, are not coded.
#[test]
fn not_coded() {
    let (_, d) = files().into_iter().next().unwrap();
    let set = |at: usize, v: u16| {
        let mut e = d.clone();
        e[at..at + 2].copy_from_slice(&v.to_le_bytes());
        e
    };
    // block align 1022 (per channel 511), 6 (3), and bits per sample 3
    for e in [set(32, 1022), set(32, 6), set(34, 3)] {
        assert!(container::detect(&e).is_none());
    }
    // less than a block of samples
    let c = container::detect(&d).unwrap();
    assert!(container::detect(&d[..c.start + 1000]).is_none());
}
