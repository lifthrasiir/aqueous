//! The parallel paths of Huffman decoding (restart intervals in chunks, speculative decoding of
//! scans without them, stripe-wise decoding, progressive scans decoded at once), made to run on
//! small images by small chunks, with the test oracles on: parallel decoding equals serial
//! decoding (`jpeg:verify-parallel`), stripe-wise equals whole (`jpeg:verify-striped`), and a
//! progressive scan judged reproducible re-encodes identically (`jpeg:verify-progressive`).
//! Every input either is not coded or comes back byte for byte, truncated and damaged ones too.

use aqr_archive::Evaluate;
use aqr_formula::Apply;
use aqr_jpeg::formula::{self, MODULE};

mod common;
use common::*;

const THREADS: usize = 4;

fn module_fn(a: &Apply, inputs: &[&[u8]]) -> aqr_formula::Result<Vec<u8>> {
    assert_eq!(a.module, MODULE);
    formula::decode(&a.params, inputs, a.len, THREADS).map_err(|e| aqr_formula::Error::new(e.0))
}

fn setup() {
    // once per process (`apply` refuses a second setting)
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        aqr_common::zopt::apply(&[
            "jpeg:verify-parallel",
            "jpeg:verify-striped",
            "jpeg:verify-progressive",
            "jpeg:chunk-blocks=16",
            "jpeg:stripe-blocks=256",
        ])
        .unwrap();
    });
}

/// Codes `d` with several threads: where it is coded, it comes back as it was.
fn roundtrip(name: &str, d: &[u8]) -> bool {
    setup();
    let Some(f) = formula::encode(d, THREADS) else { return false };
    let back = f.eval(&module_fn).unwrap_or_else(|e| panic!("{name}: decode: {e:?}"));
    assert!(back == d, "{name}: roundtrip");
    true
}

const YCC: [(u8, u8, u8, u8); 3] = [(1, 2, 2, 0), (2, 1, 1, 1), (3, 1, 1, 1)];
const GRAY: [(u8, u8, u8, u8); 1] = [(1, 1, 1, 0)];

/// The data of the only scan of `d` cut at `at` (a fraction of its length, in 1/64), as if
/// the file ended there (with EOI) or its bytes after were damaged.
fn cut(d: &[u8], at: usize) -> Vec<u8> {
    let j = aqr_jpeg::jpeg::parse(d).unwrap();
    let s = &j.scans[0];
    let n = s.start + (s.end - s.start) * at / 64;
    let mut v = d[..n].to_vec();
    v.extend_from_slice(&[0xFF, 0xD9]);
    v
}

fn damage(d: &[u8], at: usize, with: u8) -> Vec<u8> {
    let j = aqr_jpeg::jpeg::parse(d).unwrap();
    let s = &j.scans[0];
    let mut v = d.to_vec();
    let n = s.start + (s.end - s.start) * at / 64;
    v[n] = with;
    v
}

/// Sequential files of every shape the parallel paths split: with and without restart
/// intervals (interleaved, which is stripe-wise, and not, which is speculative), each whole,
/// cut and damaged.
#[test]
fn sequential_parallel() {
    let mut cases: Vec<(String, Spec)> = Vec::new();
    for ri in [0u16, 7, 64] {
        for (name, comps) in [("gray", &GRAY[..]), ("4:2:0", &YCC[..])] {
            let mut s = sequential(520, 400, comps);
            s.ri = ri;
            cases.push((format!("{name} ri={ri}"), s));
            let mut s = sequential(520, 400, comps);
            s.ri = ri;
            s.scans = (0..comps.len()).map(|c| scan(&[c], 0, 63, 0, 0)).collect();
            cases.push((format!("{name} ri={ri} one scan each"), s));
        }
    }
    for (name, s) in cases {
        let d = build(&s);
        assert!(roundtrip(&name, &d), "{name}: not coded");
        if s.scans.len() > 1 {
            // (the later scans' data cut or damaged is the first's job as well)
            continue;
        }
        for at in [1, 9, 33, 63] {
            roundtrip(&format!("{name} cut {at}"), &cut(&d, at));
            for with in [0x00, 0xFF, 0x5A] {
                roundtrip(&format!("{name} damaged {at} {with:#x}"), &damage(&d, at, with));
            }
        }
    }
}

/// A damaged first scan of several: the rest stays raw, the whole comes back.
#[test]
fn separate_scans_damaged() {
    let mut s = sequential(520, 400, &YCC);
    s.scans = (0..3).map(|c| scan(&[c], 0, 63, 0, 0)).collect();
    let d = build(&s);
    let j = aqr_jpeg::jpeg::parse(&d).unwrap();
    for sc in &j.scans {
        for at in [3, 20, 40] {
            let mut v = d.clone();
            let n = sc.start + (sc.end - sc.start) * at / 64;
            v[n] ^= 0x5A;
            roundtrip(&format!("damaged {n}"), &v);
            // cut inside this scan, the later ones left as they are
            let mut v = d.clone();
            v.drain(n..sc.end);
            roundtrip(&format!("cut {n}"), &v);
        }
    }
}

/// Progressive files whose scans are decoded at once, each by the rows of those before.
#[test]
fn progressive_concurrent() {
    for (name, comps) in [("gray", &GRAY[..]), ("4:2:0", &YCC[..])] {
        let all: Vec<usize> = (0..comps.len()).collect();
        let mut s = sequential(520, 400, comps);
        s.progressive = true;
        s.scans = vec![scan(&all, 0, 0, 0, 1)];
        for c in 0..comps.len() {
            s.scans.push(scan(&[c], 1, 63, 0, 2));
        }
        for c in 0..comps.len() {
            s.scans.push(scan(&[c], 1, 63, 2, 1));
            s.scans.push(scan(&[c], 1, 63, 1, 0));
        }
        s.scans.push(scan(&all, 0, 0, 1, 0));
        let d = build(&s);
        assert!(roundtrip(name, &d), "progressive {name}: not coded");
        // with restart intervals
        let mut s2 = sequential(520, 400, comps);
        s2.progressive = true;
        s2.ri = 5;
        s2.scans = vec![scan(&all, 0, 0, 0, 0)];
        for c in 0..comps.len() {
            s2.scans.push(scan(&[c], 1, 5, 0, 0));
            s2.scans.push(scan(&[c], 6, 63, 0, 0));
        }
        let d2 = build(&s2);
        assert!(roundtrip(&format!("{name} ri"), &d2), "progressive {name} ri: not coded");
        // a scan damaged or the file cut: the scans before it stay as they are
        let j = aqr_jpeg::jpeg::parse(&d).unwrap();
        for (k, sc) in j.scans.iter().enumerate() {
            let n = sc.start + (sc.end - sc.start) / 2;
            let mut v = d.clone();
            v[n] ^= 0xA5;
            roundtrip(&format!("{name} scan {k} damaged"), &v);
            let mut v = d[..n].to_vec();
            v.extend_from_slice(&[0xFF, 0xD9]);
            roundtrip(&format!("{name} cut in scan {k}"), &v);
        }
    }
}

/// The per-scan timing helper of the progressive decoder runs and tells each scan.
#[test]
fn scan_times_tell_every_scan() {
    let mut s = sequential(200, 120, &YCC);
    s.progressive = true;
    s.scans = vec![
        scan(&[0, 1, 2], 0, 0, 0, 1),
        scan(&[0], 1, 63, 0, 0),
        scan(&[1], 1, 63, 0, 0),
        scan(&[0, 1, 2], 0, 0, 1, 0),
    ];
    let d = build(&s);
    let j = aqr_jpeg::jpeg::parse(&d).unwrap();
    let t = aqr_jpeg::progressive::scan_times(&j, &d, 2).unwrap();
    assert_eq!(t.len(), j.scans.len());
    assert!(t.iter().all(|(name, dec, enc, bytes)| !name.is_empty() && *dec >= 0.0 && *enc >= 0.0 && *bytes > 0));
}
