//! Edge cases of the JPEG module: frame shapes (sampling, component counts, scan splits, tables
//! and intervals redefined, 16-bit quantisation tables, progressive refinement), what surrounds
//! the stream, damaged inputs, the extremes of the dimensions, and the variants of
//! `variant::reencode` (fixed seeds). Every input either is not coded or comes back byte for byte.

use std::time::Duration;

use aqr_archive::Evaluate;
use aqr_formula::Apply;
use aqr_jpeg::formula::{self, MODULE};
use aqr_jpeg::jpeg;
use aqr_jpeg::{module, variant};
use aqr_testkit::{Refusing, damaged, most};

#[global_allocator]
static ALLOC: Refusing = Refusing(1 << 30);

mod common;
use common::*;

const GRADIENT: &[u8] = include_bytes!("data/gradient.jpg");
const PROGRESSIVE: &[u8] = include_bytes!("data/prog_script_recode.jpg");

fn module_fn(a: &Apply, inputs: &[&[u8]]) -> aqr_formula::Result<Vec<u8>> {
    assert_eq!(a.module, MODULE);
    formula::decode(&a.params, inputs, a.len, 1).map_err(|e| aqr_formula::Error::new(e.0))
}

/// Codes `d`: whether it is, and where it is, it comes back as it was.
fn roundtrip(name: &str, d: &[u8]) -> bool {
    let Some(f) = formula::encode(d, 1) else { return false };
    assert_eq!(f.eval(&module_fn).unwrap(), d, "{name}: roundtrip");
    true
}

#[test]
fn shapes() {
    let gray = [(1, 1, 1, 0)];
    let cmyk = [(1, 1, 1, 0), (2, 1, 1, 1), (3, 1, 1, 2), (4, 1, 1, 3)];
    let cmyk2 = [(1, 2, 2, 0), (2, 1, 1, 1), (3, 1, 1, 2), (4, 2, 2, 3)];
    let mut cases: Vec<(&str, Spec)> = vec![
        ("grayscale", sequential(237, 129, &gray)),
        ("gray 1 row", sequential(1500, 1, &gray)),
        ("gray 1 column", sequential(1, 1500, &gray)),
        ("4:4:4", sequential(233, 117, &sampling((1, 1)))),
        ("4:2:0", sequential(240, 130, &YCC)),
        ("4:2:2", sequential(240, 130, &sampling((2, 1)))),
        ("4:4:0", sequential(240, 130, &sampling((1, 2)))),
        ("4:1:1", sequential(270, 119, &sampling((4, 1)))),
        ("4 components", sequential(233, 121, &cmyk)),
        ("4 components, sampled", sequential(233, 121, &cmyk2)),
    ];
    let mut s = sequential(240, 130, &YCC);
    s.dqt16 = true;
    cases.push(("16-bit DQT", s));
    let mut s = sequential(240, 130, &YCC);
    s.ri = 3;
    cases.push(("restart interval", s));
    // one scan for each component, the planes' own blocks (not those of the MCU's padding)
    let mut s = sequential(240, 130, &YCC);
    s.scans = (0..3).map(|c| scan(&[c], 0, 63, 0, 0)).collect();
    cases.push(("non-interleaved", s));
    let mut s = sequential(240, 130, &YCC);
    s.scans = vec![scan(&[0], 0, 63, 0, 0), scan(&[1, 2], 0, 63, 0, 0)];
    cases.push(("luma, then chroma", s));
    // the tables and the interval redefined between scans
    let mut s = sequential(240, 130, &YCC);
    s.scans = vec![scan(&[0], 0, 63, 0, 0), scan(&[1, 2], 0, 63, 0, 0)];
    s.scans[1].redefine = Some(2);
    cases.push(("DHT and DRI redefined", s));
    let mut s = sequential(240, 130, &YCC);
    s.scans = vec![scan(&[0], 0, 63, 0, 0), scan(&[1], 0, 63, 0, 0), scan(&[2], 0, 63, 0, 0)];
    s.ri = 2;
    s.scans[1].redefine = Some(0);
    s.scans[2].redefine = Some(5);
    cases.push(("DRI redefined twice", s));
    // progressive: spectral selection, then successive approximation
    for (name, comps) in [("progressive gray", &gray[..]), ("progressive 4:2:0", &YCC[..])] {
        let all: Vec<usize> = (0..comps.len()).collect();
        let mut s = sequential(240, 130, comps);
        s.progressive = true;
        s.scans = vec![scan(&all, 0, 0, 0, 0)];
        for c in 0..comps.len() {
            s.scans.push(scan(&[c], 1, 5, 0, 0));
            s.scans.push(scan(&[c], 6, 63, 0, 0));
        }
        cases.push((name, s));
        let mut s = sequential(240, 130, comps);
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
        cases.push(("successive approximation", s));
    }
    for (name, s) in cases {
        let d = build(&s);
        assert!(jpeg::parse(&d).is_ok(), "{name}: the file does not parse");
        assert!(roundtrip(name, &d), "{name}: not coded");
    }
}

#[test]
fn around_the_stream() {
    let d = build(&sequential(240, 130, &YCC));
    assert!(roundtrip("plain", &d));
    let eoi = d.len() - 2;
    assert_eq!(&d[eoi..], &[0xFF, 0xD9]);
    // bytes after EOI (also a whole second JPEG's start, and a lone FF)
    for tail in [&b"trailing text"[..], &[0xFF], &[0xFF, 0xD8], &[0, 0, 0, 0], &d[..40]] {
        let mut v = d.clone();
        v.extend_from_slice(tail);
        assert!(roundtrip("tail", &v), "after EOI: {tail:?}");
    }
    // no EOI, or half of it
    assert!(roundtrip("no EOI", &d[..eoi]));
    assert!(roundtrip("half EOI", &d[..eoi + 1]));
    // fill bytes before the markers
    let j = jpeg::parse(&d).unwrap();
    let mut v = d.clone();
    let s = j.scans[0].start;
    let sos = d[..s].windows(2).rposition(|w| w == [0xFF, 0xDA]).unwrap();
    v.splice(sos..sos, [0xFF, 0xFF, 0xFF]);
    assert!(roundtrip("FF fill before SOS", &v));
    let end = v.len() - 2;
    v.splice(end..end, [0xFF, 0xFF]);
    assert!(roundtrip("FF fill before EOI", &v));
    // a comment between the scan and EOI
    let mut v = d[..eoi].to_vec();
    segment(&mut v, 0xFE, b"comment");
    v.extend_from_slice(&[0xFF, 0xD9]);
    assert!(roundtrip("comment after the scan", &v));
}

/// A truncated or damaged stream never panics, and what is coded comes back.
#[test]
fn damaged_streams() {
    for d in [build(&sequential(240, 130, &YCC)), PROGRESSIVE.to_vec()] {
        damaged(&d, 120, 160, Duration::from_secs(20), |v| {
            if let Some(f) = formula::encode(v, 1) {
                assert_eq!(f.eval(&module_fn).unwrap(), v);
            }
        });
    }
}

/// The inputs of the module's application damaged: an error, not a panic, a hang or a huge
/// allocation.
#[test]
fn damaged_application() {
    for data in [build(&sequential(240, 130, &YCC)), GRADIENT.to_vec()] {
        let f = formula::encode(&data, 1).expect("coded");
        let a = f.applies()[0].clone();
        let ins: Vec<Vec<u8>> = a
            .inputs
            .iter()
            .map(|n| {
                aqr_formula::Formula { streams: f.streams.clone(), root: n.clone(), shared: vec![] }
                    .eval(&module_fn)
                    .unwrap()
            })
            .collect();
        let run = |params: &[u8], ins: &[Vec<u8>]| {
            let refs: Vec<&[u8]> = ins.iter().map(|v| &v[..]).collect();
            let _ = formula::decode(params, &refs, a.len, 1);
        };
        damaged(&a.params, 64, 64, Duration::from_secs(20), |p| run(p, &ins));
        for k in 0..ins.len() {
            if ins[k].is_empty() {
                continue;
            }
            damaged(&ins[k], 40, 48, Duration::from_secs(20), |v| {
                let mut c = ins.clone();
                c[k] = v.to_vec();
                run(&a.params, &c)
            });
        }
    }
}

/// A header with no data to speak of: `comps` of `w` x `h`, a scan of 24 zero bytes.
fn headers(w: u16, h: u16, comps: &[(u8, u8, u8, u8)], progressive: bool) -> Vec<u8> {
    let mut d = vec![0xFF, 0xD8];
    let mut dqt = vec![0u8];
    dqt.extend((0..64).map(|k| 2 + k as u8 / 4));
    segment(&mut d, 0xDB, &dqt);
    let mut sof = vec![8];
    sof.extend_from_slice(&h.to_be_bytes());
    sof.extend_from_slice(&w.to_be_bytes());
    sof.push(comps.len() as u8);
    for &(id, hh, v, q) in comps {
        sof.extend_from_slice(&[id, hh << 4 | v, q]);
    }
    segment(&mut d, if progressive { 0xC2 } else { 0xC0 }, &sof);
    dht(&mut d, false, progressive);
    let mut sos = vec![comps.len() as u8];
    for &(id, ..) in comps {
        sos.extend_from_slice(&[id, 0]);
    }
    sos.extend_from_slice(&if progressive { [0, 0, 0] } else { [0, 63, 0] });
    segment(&mut d, 0xDA, &sos);
    d.extend_from_slice(&[0u8; 24]);
    d.extend_from_slice(&[0xFF, 0xD9]);
    d
}

/// Dimensions at the limits: the blocks of all components in all are limited to 2^24, over a
/// few bytes of data the file is not coded and no plane of the size is made.
#[test]
fn extreme_dimensions() {
    let gray = [(1, 1, 1, 0)];
    for progressive in [false, true] {
        for (w, h) in [(65535, 1), (1, 65535), (65535, 65535), (32768, 32768), (32768, 32776), (32776, 32768)] {
            let d = headers(w, h, &gray, progressive);
            let ok = jpeg::parse(&d).is_ok();
            assert_eq!(ok, (w as usize).div_ceil(8) * (h as usize).div_ceil(8) <= 1 << 24, "{w}x{h}: parse");
            aqr_testkit::case(&format!("{w}x{h} progressive {progressive}"));
            let f = formula::encode(&d, 1);
            assert!(f.is_none(), "{w}x{h}: coded");
            assert!(most() < (64 << 20), "{w}x{h}: {} bytes reserved", most());
        }
    }
    // the sum over components: three at 2^23 blocks each go over
    let ycc = [(1, 1, 1, 0), (2, 1, 1, 0), (3, 1, 1, 0)];
    assert!(jpeg::parse(&headers(32768, 16384, &ycc, false)).is_err());
    assert!(jpeg::parse(&headers(16384, 8192, &ycc, false)).is_ok());
    // the extremes with data that can hold them
    assert!(roundtrip("65535x1", &build(&sequential(65535, 1, &gray))));
    assert!(roundtrip("1x4000", &build(&sequential(1, 4000, &gray))));
}

/// The variants of `variant::reencode` (a valid scan coded otherwise, drawn from fixed seeds):
/// the same coefficients, and the formula either leaves the file or restores it.
#[test]
fn reencoded_variants() {
    let files = [GRADIENT.to_vec(), PROGRESSIVE.to_vec(), build(&sequential(240, 130, &YCC)), {
        let mut s = sequential(240, 130, &YCC);
        s.ri = 2;
        build(&s)
    }];
    let mut made = 0;
    for (i, d) in files.iter().enumerate() {
        let want = variant::coefficients(d).expect("decodes");
        for seed in 1..=24u64 {
            let mut rng = variant::Rng(seed * 0x9E37_79B9 + i as u64);
            let Some((v, desc)) = variant::reencode(d, &mut rng) else { continue };
            made += 1;
            let got = variant::coefficients(&v).unwrap_or_else(|| panic!("file {i} seed {seed}: no decode: {desc}"));
            assert_eq!(got.len(), want.len());
            for (g, w) in got.iter().zip(&want) {
                assert!(g.blocks == w.blocks, "file {i} seed {seed}: coefficients differ: {desc}");
            }
            roundtrip(&format!("file {i} seed {seed}: {desc}"), &v);
        }
    }
    assert!(made > 40, "only {made} variants made");
}

#[test]
fn params_of_nothing() {
    assert!(module::params(&[]).is_err());
    assert!(module::params(&[2]).is_err());
}

/// The variants of progressive scans (EOB runs cut or joined, correction bits, restart
/// intervals): the same coefficients, and the formula either leaves the file or restores it.
#[test]
fn reencoded_progressive_variants() {
    let all = [0usize, 1, 2];
    let mut files = Vec::new();
    for ri in [0u16, 3] {
        // spectral selection
        let mut s = sequential(240, 130, &YCC);
        s.progressive = true;
        s.ri = ri;
        s.scans = vec![scan(&all, 0, 0, 0, 0)];
        for c in 0..3 {
            s.scans.push(scan(&[c], 1, 5, 0, 0));
            s.scans.push(scan(&[c], 6, 63, 0, 0));
        }
        files.push(build(&s));
        // successive approximation: first scans, then refinements of the DC and AC bits
        let mut s = sequential(240, 130, &YCC);
        s.progressive = true;
        s.ri = ri;
        s.scans = vec![scan(&all, 0, 0, 0, 1)];
        for c in 0..3 {
            s.scans.push(scan(&[c], 1, 63, 0, 2));
        }
        for c in 0..3 {
            s.scans.push(scan(&[c], 1, 63, 2, 1));
            s.scans.push(scan(&[c], 1, 63, 1, 0));
        }
        s.scans.push(scan(&all, 0, 0, 1, 0));
        files.push(build(&s));
    }
    let mut made = 0;
    for (i, d) in files.iter().enumerate() {
        let want = variant::coefficients(d).expect("decodes");
        for seed in 1..=16u64 {
            let mut rng = variant::Rng(seed * 0x9E37_79B9 + i as u64);
            let Some((v, desc)) = variant::reencode(d, &mut rng) else { continue };
            made += 1;
            let got = variant::coefficients(&v).unwrap_or_else(|| panic!("file {i} seed {seed}: no decode: {desc}"));
            assert_eq!(got.len(), want.len());
            for (g, w) in got.iter().zip(&want) {
                assert!(g.blocks == w.blocks, "file {i} seed {seed}: coefficients differ: {desc}");
            }
            roundtrip(&format!("file {i} seed {seed}: {desc}"), &v);
        }
    }
    assert!(made > 40, "only {made} variants made");
}
