//! Round trips of the files in tests/data (small LAME encodings of a synthetic signal),
//! through the coded path and the formula, also with junk around and cut short.

use aqr_archive::Evaluate;
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

/// A coder recording the symbols and raw bits it is given; with `ROLLBACK` it takes the
/// one-pass path of the encoder (`encode_granule`), without it the two-pass path that the
/// training's counter takes (`code_granule`).
struct Recorder<const ROLLBACK: bool> {
    syms: Vec<(usize, u32)>,
    raw: Vec<(u32, u32)>,
}

struct RecorderLocal(*mut Vec<(usize, u32)>, *mut Vec<(u32, u32)>);

impl aqr_mp3::model::Sym for RecorderLocal {
    fn sym(&mut self, ctx: usize, s: u32) -> u32 {
        // SAFETY: the pointers are of the recorder's vectors, which outlive the local.
        unsafe { (*self.0).push((ctx, s)) };
        s
    }
    fn raw(&mut self, n: u32, v: u32) -> u32 {
        // SAFETY: as above.
        unsafe { (*self.1).push((n, if n >= 32 { v } else { v & ((1 << n) - 1) })) };
        v
    }
}

impl<const ROLLBACK: bool> aqr_mp3::model::Coder for Recorder<ROLLBACK> {
    type L = RecorderLocal;
    fn local(&mut self) -> RecorderLocal {
        RecorderLocal(&mut self.syms, &mut self.raw)
    }
    fn put_back(&mut self, _l: RecorderLocal) {}
    const ROLLBACK: bool = ROLLBACK;
    fn save(&self) -> [u64; 4] {
        [self.syms.len() as u64, self.raw.len() as u64, 0, 0]
    }
    fn restore(&mut self, s: [u64; 4]) {
        self.syms.truncate(s[0] as usize);
        self.raw.truncate(s[1] as usize);
    }
}

/// The counting coder of the training (a coder without rollback: the two-pass path) sees the
/// same symbols in the same contexts as the encoder's one-pass path.
#[test]
fn counting_coder_agrees_with_one_pass() {
    use aqr_mp3::model::{Counter, N_CTX};
    for (name, d) in files() {
        let mut counter = Counter::new();
        codec::count(&d, &mut counter);
        let mut two = Recorder::<false> { syms: Vec::new(), raw: Vec::new() };
        codec::count(&d, &mut two);
        let mut one = Recorder::<true> { syms: Vec::new(), raw: Vec::new() };
        codec::count(&d, &mut one);
        assert!(!two.syms.is_empty(), "{name}: nothing counted");
        assert_eq!(one.syms, two.syms, "{name}: symbols");
        // (the raw bits are grouped differently by the two paths, but are as many)
        let bits = |r: &[(u32, u32)]| r.iter().map(|&(n, _)| n as u64).sum::<u64>();
        assert_eq!(bits(&one.raw), bits(&two.raw), "{name}: raw bits");
        let mut by_ctx = vec![Vec::new(); N_CTX];
        for &(c, s) in &two.syms {
            let v: &mut Vec<u64> = &mut by_ctx[c];
            if v.len() <= s as usize {
                v.resize(s as usize + 1, 0);
            }
            v[s as usize] += 1;
        }
        for (c, (got, want)) in counter.counts.iter().zip(&by_ctx).enumerate() {
            assert_eq!(&got[..want.len()], &want[..], "{name}: counts of context {c}");
            assert!(got[want.len()..].iter().all(|&n| n == 0), "{name}: context {c}");
        }
    }
}
