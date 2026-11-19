//! Edge cases: damaged files (the example's mutation fuzz, as a test), header bits of every
//! mode (dual channel, plain and intensity stereo, private/copyright/emphasis), dense false
//! syncs.

use std::time::Duration;

use aqr_archive::Evaluate;
use aqr_formula::Apply;
use aqr_mp3::{codec, formula, stream};

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

/// `d` through the coded path and the formula (with `threads`): the input back or no formula.
fn check(d: &[u8], threads: usize) {
    let back = codec::decode(&codec::encode(d), codec::tables()).expect("coded path");
    assert!(back == d, "coded path differs");
    let Some(f) = formula::encode(d, threads) else { return };
    let out = f
        .eval(&|a: &Apply, inputs: &[&[u8]]| {
            let res: Vec<&[u8]> = a.resources.iter().map(|r| &r[..]).collect();
            formula::decode(&a.params, &res, inputs, a.len, threads).map_err(|e| aqr_formula::Error::new(e.0))
        })
        .expect("formula");
    assert!(out == d, "formula differs");
}

#[test]
fn damaged_files() {
    for (_, d) in files() {
        // (about 50 places for each way; the headers of the first frames at every byte)
        aqr_testkit::damaged(&d, 50, 64, Duration::from_secs(60), |m| check(m, 1));
    }
}

/// Every frame's header (and side information, where it has no CRC) patched by `f`.
fn patched(d: &[u8], f: impl Fn(&mut [u8])) -> Vec<u8> {
    let mut v = d.to_vec();
    let (_, _, frames) = stream::segment(d);
    assert!(!frames.is_empty());
    for (p, _) in frames {
        f(&mut v[p..p + 4]);
    }
    v
}

#[test]
fn header_bits() {
    for (name, d) in files() {
        let mut variants: Vec<(String, Vec<u8>)> = Vec::new();
        for ext in 0..4u8 {
            // joint stereo with each extension (plain, intensity, MS, both)
            variants.push((format!("joint ext {ext}"), patched(&d, |h| h[3] = h[3] & 0x0f | 1 << 6 | ext << 4)));
        }
        // dual channel and plain stereo
        variants.push(("dual".into(), patched(&d, |h| h[3] = h[3] & 0x0f | 2 << 6)));
        variants.push(("stereo".into(), patched(&d, |h| h[3] &= 0x0f)));
        // private (header byte 2 bit 0), copyright, original, emphasis (byte 3)
        variants.push(("private".into(), patched(&d, |h| h[2] ^= 1)));
        variants.push(("copyright".into(), patched(&d, |h| h[3] ^= 8)));
        variants.push(("original".into(), patched(&d, |h| h[3] ^= 4)));
        for em in 1..4u8 {
            variants.push((format!("emphasis {em}"), patched(&d, |h| h[3] = h[3] & !3 | em)));
        }
        for (v, m) in variants {
            std::panic::catch_unwind(|| check(&m, 1)).unwrap_or_else(|_| panic!("{name}, {v}"));
            check(&m, 4);
        }
    }
}

/// Dense false syncs (`FF FB` over and over, and valid headers with nothing behind them) take
/// time linear in their length.
#[test]
fn dense_false_syncs() {
    for (label, unit) in
        [("ff fb", &[0xffu8, 0xfb][..]), ("header 128k", &[0xff, 0xfb, 0x90, 0x00][..]), ("ff only", &[0xff][..])]
    {
        let time = |n: usize| {
            let d: Vec<u8> = unit.iter().copied().cycle().take(n).collect();
            let t = std::time::Instant::now();
            aqr_testkit::within(Duration::from_secs(60), || check(&d, 1)).unwrap();
            t.elapsed()
        };
        let small = time(1 << 18).as_secs_f64().max(0.005);
        let big = time(1 << 21).as_secs_f64();
        assert!(big < small * 8.0 * 4.0 + 0.5, "{label}: {small:.3}s for 256 KiB, {big:.3}s for 2 MiB");
    }
}
