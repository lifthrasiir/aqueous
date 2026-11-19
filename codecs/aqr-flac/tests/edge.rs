//! Edge cases of FLAC files, made synthetically: header codes (variable block size, block size
//! and sample rate codes, sample sizes, channel assignments), subframe types, escaped
//! partitions, metadata, and damage. A file is either not coded or evaluated back as it was.

use aqr_archive::Evaluate;
use aqr_flac::flac;
use aqr_flac::formula::{self, MODULE};
use aqr_formula::{Apply, Formula};
use aqr_testkit::{Refusing, damaged};
use std::time::Duration;

#[global_allocator]
static ALLOC: Refusing = Refusing(1 << 30);

#[path = "common/synth.rs"]
mod synth;
use synth::*;

fn eval(f: &Formula, threads: usize) -> aqr_formula::Result<Vec<u8>> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| {
        assert_eq!(a.module, MODULE);
        let res: Vec<&[u8]> = a.resources.iter().map(|r| &r[..]).collect();
        formula::decode(&a.params, &res, inputs, a.len, threads).map_err(|e| aqr_formula::Error::new(e.0))
    })
}

/// Whether `d` is coded; evaluated back it is `d`.
fn check(name: &str, d: &[u8]) -> bool {
    let Some(f) = formula::encode(d, 1) else { return false };
    for t in [1, 3] {
        assert!(eval(&f, t).unwrap() == d, "{name}: {t} threads");
    }
    true
}

fn coded(name: &str, d: &[u8]) {
    assert!(check(name, d), "{name}: not coded");
}

#[test]
fn block_size_codes() {
    for code in [1u8, 2, 3, 4, 5, 8, 9, 10, 11, 12, 13, 14, 15] {
        let h = Hdr::new(code, 0, 4);
        coded(&format!("bs code {code}"), &file(&h, 16, 1, &[Sub::fixed(2, 0)], 3));
    }
    // code 6: 8 bits + 1, code 7: 16 bits + 1
    for (code, extra) in [(6u8, 0u32), (6, 1), (6, 255), (7, 0), (7, 4095), (7, 65535)] {
        let mut h = Hdr::new(code, 0, 4);
        h.bs_extra = extra;
        let d = file(&h, 16, 1, &[Sub::fixed(1.min(extra as usize + 1) - 1 + 1, 0)], 3);
        let ok = check(&format!("bs code {code} extra {extra}"), &d);
        if extra < 4096 {
            assert!(ok, "bs code {code} extra {extra}: not coded");
        }
    }
    // orders over a block of 1 sample: not coded, not a panic
    for order in 1..=4 {
        let mut h = Hdr::new(6, 0, 4);
        h.bs_extra = 0;
        check(&format!("order {order} over block 1"), &file(&h, 16, 1, &[Sub::fixed(order, 0)], 3));
    }
}

#[test]
fn variable_block_size() {
    let mut h = Hdr::new(8, 0, 4);
    h.variable = true;
    coded("variable 256", &file(&h, 16, 1, &[Sub::fixed(2, 0)], 40));
    h.number = 1 << 30;
    coded("variable big start", &file(&h, 16, 1, &[Sub::fixed(2, 0)], 5));
    h.number = (1 << 36) - 1000;
    coded("variable 36 bits", &file(&h, 16, 1, &[Sub::fixed(2, 0)], 5));
    // block size changing between frames
    let mut o = meta(&[(0, streaminfo(16, 44100, 1, 16, 0))]);
    let mut pos = 0u64;
    for (i, b) in [16usize, 200, 1, 100, 16].into_iter().enumerate() {
        let mut h = Hdr::new(6, 0, 4);
        h.variable = true;
        h.bs_extra = b as u32 - 1;
        h.number = pos;
        pos += b as u64;
        o.extend(frame(&h, 16, &[Sub::fixed(b.min(2), 0)], i as u32));
    }
    check("variable changing blocks", &o);
}

#[test]
fn verbatim_and_constant() {
    for ss in [0u8, 1, 2, 4, 5, 6, 7] {
        let h = Hdr::new(8, 0, ss);
        let bps = if ss == 0 { 16 } else { 8 };
        let bits = bps_of(&h, bps);
        let top = (1i64 << (bits - 1)) - 1;
        let vs: Vec<i64> = (0..256).map(|i| if i % 2 == 0 { top } else { -top - 1 }).collect();
        coded(&format!("verbatim ss {ss}"), &file(&h, bps, 1, &[Sub::Verbatim(vs)], 3));
        coded(&format!("constant ss {ss}"), &file(&h, bps, 1, &[Sub::Constant(-top - 1)], 3));
    }
    let h = Hdr::new(8, 1, 4);
    coded("verbatim+fixed", &file(&h, 16, 2, &[Sub::Verbatim(vec![5; 256]), Sub::fixed(4, 3)], 4));
    coded("constant+constant", &file(&h, 16, 2, &[Sub::Constant(0), Sub::Constant(1)], 4));
}

#[test]
fn channel_assignments() {
    for assign in 0..=10u8 {
        let h = Hdr::new(8, assign, 4);
        let ch = if assign < 8 { assign as usize + 1 } else { 2 };
        let subs = vec![Sub::fixed(2, 1); ch];
        coded(&format!("assign {assign}"), &file(&h, 16, ch as u32, &subs, 4));
        // The side channel is a bit wider: verbatim extremes.
        if assign >= 8 {
            let side = if assign == 9 { 0 } else { 1 };
            let mut subs = vec![Sub::fixed(1, 0); 2];
            subs[side] = Sub::Verbatim((0..256).map(|i| if i % 2 == 0 { 32768 } else { -32769 }).collect());
            coded(&format!("assign {assign} side extreme"), &file(&h, 16, 2, &subs, 3));
        }
    }
    // 32-bit samples take 33 bits of side
    let h = Hdr::new(8, 9, 7);
    let side: Vec<i64> = (0..256).map(|i| if i % 2 == 0 { (1i64 << 32) - 1 } else { -(1i64 << 32) }).collect();
    coded("33-bit side", &file(&h, 32, 2, &[Sub::Verbatim(side), Sub::fixed(1, 0)], 3));
    // Reserved assignments 11-15 are not frames.
    for assign in 11..16u8 {
        let h = Hdr::new(8, assign, 4);
        check(&format!("assign {assign}"), &file(&h, 16, 2, &[Sub::fixed(1, 0), Sub::fixed(1, 0)], 3));
    }
}

#[test]
fn sample_rate_codes() {
    for (code, extra) in [(0u8, 0u32), (1, 0), (11, 0), (12, 44), (12, 255), (13, 44100), (14, 65535), (14, 0), (15, 0)]
    {
        let mut h = Hdr::new(8, 0, 4);
        h.sr_code = code;
        h.sr_extra = extra;
        let ok = check(&format!("rate {code}/{extra}"), &file(&h, 16, 1, &[Sub::fixed(2, 0)], 3));
        assert!(ok || code == 15, "rate {code}/{extra}: not coded");
    }
}

#[test]
fn sample_sizes() {
    // Sample size codes 0, 2, 5 over STREAMINFO's sizes, and the reserved one.
    for (ss, si) in [(0u8, 4u32), (0, 8), (0, 12), (0, 20), (0, 24), (0, 32), (0, 1), (0, 3), (2, 16), (5, 16), (3, 16)]
    {
        let h = Hdr::new(9, 0, ss);
        let ok = check(&format!("ss {ss} si {si}"), &file(&h, si, 1, &[Sub::fixed(2, 2)], 3));
        if matches!(ss, 0 | 2 | 5) && (4..=32).contains(&si) {
            assert!(ok, "ss {ss} si {si}: not coded");
        }
    }
}

#[test]
fn partitions_and_escapes() {
    let h = Hdr::new(11, 0, 4); // 2048
    for porder in [0u32, 1, 5, 8, 10, 11] {
        coded(&format!("porder {porder}"), &file(&h, 16, 1, &[Sub::fixed(4, porder)], 3));
        let s = Sub::Pred { order: 4, lpc: None, method: 1, porder, escape: None, k: 20, wasted: 0 };
        coded(&format!("method 1 porder {porder}"), &file(&h, 16, 1, &[s], 3));
    }
    // partitions of fewer samples than the order, and more partitions than samples
    for porder in [12u32, 15] {
        check(&format!("porder {porder}"), &file(&h, 16, 1, &[Sub::fixed(0, porder)], 2));
    }
    check("order 4 in partitions of 2", &file(&h, 16, 1, &[Sub::fixed(4, 10)], 2));
    for nb in [0u32, 1, 2, 5, 16, 20, 24] {
        for porder in [0u32, 3] {
            let s = Sub::Pred { order: 2, lpc: None, method: 0, porder, escape: Some(nb), k: 0, wasted: 0 };
            coded(&format!("escape {nb} porder {porder}"), &file(&h, 16, 1, &[s], 3));
        }
    }
    let s = Sub::Pred { order: 2, lpc: None, method: 1, porder: 2, escape: Some(12), k: 0, wasted: 0 };
    coded("escape method 1", &file(&h, 16, 1, &[s], 3));
    // reserved methods 2 and 3
    for method in [2, 3] {
        let s = Sub::Pred { order: 2, lpc: None, method, porder: 0, escape: None, k: 3, wasted: 0 };
        check("reserved method", &file(&h, 16, 1, &[s], 3));
    }
}

#[test]
fn lpc_and_wasted() {
    let h = Hdr::new(9, 0, 4);
    for (order, p, sh) in [(1usize, 1u32, 0i32), (32, 15, 15), (12, 12, -1), (8, 5, -16), (3, 16, 31)] {
        let s = Sub::Pred { order, lpc: Some((p, sh)), method: 0, porder: 3, escape: None, k: 4, wasted: 0 };
        let ok = check(&format!("lpc {order} {p} {sh}"), &file(&h, 16, 1, &[s], 3));
        if p <= 15 {
            assert!(ok, "lpc {order} {p} {sh}: not coded");
        }
    }
    for wasted in [1u32, 3, 14, 15, 16, 17, 31] {
        let s = Sub::Pred { order: 2, lpc: None, method: 0, porder: 1, escape: None, k: 4, wasted };
        let ok = check(&format!("wasted {wasted}"), &file(&h, 16, 1, &[s], 3));
        assert!(ok || wasted >= 15, "wasted {wasted}: not coded");
        check(&format!("wasted {wasted} constant"), &file(&h, 16, 1, &[Sub::Constant(1)], 3));
    }
}

#[test]
fn metadata() {
    let h = Hdr::new(8, 0, 4);
    let body: Vec<u8> =
        (1..4).flat_map(|i| frame(&Hdr { number: i, ..h.clone() }, 16, &[Sub::fixed(2, 1)], 9)).collect();
    let si = streaminfo(256, 44100, 1, 16, 0);
    let mut small = si.clone();
    small[12] &= 0x0f; // 1 bit a sample
    let mut cases: Vec<(&str, Vec<u8>)> = Vec::new();
    let with = |m: Vec<u8>| [m, body.clone()].concat();
    cases.push(("duplicate streaminfo", with(meta(&[(0, si.clone()), (0, streaminfo(256, 44100, 2, 24, 0))]))));
    cases.push(("second streaminfo is right", with(meta(&[(0, streaminfo(256, 44100, 1, 24, 0)), (0, si.clone())]))));
    cases.push(("no streaminfo", with(meta(&[(4, vec![0; 8])]))));
    cases.push(("short streaminfo", with(meta(&[(0, si[..17].to_vec())]))));
    cases.push(("empty streaminfo", with(meta(&[(0, vec![])]))));
    cases.push(("streaminfo of 1 bps", with(meta(&[(0, small)]))));
    cases.push((
        "padding and picture",
        with(meta(&[(0, si.clone()), (1, vec![0; 3000]), (6, vec![1; 500]), (127, vec![])])),
    ));
    cases.push(("metadata cut", meta(&[(0, si.clone()), (4, vec![0; 100])])[..90].to_vec()));
    cases.push(("no frames", meta(&[(0, si.clone())])));
    cases.push(("one byte of frame", [meta(&[(0, si.clone())]), vec![0xff]].concat()));
    cases.push((
        "id3 before",
        with([b"ID3\x03\x00\x00\x00\x00\x00\x05abcde".to_vec(), meta(&[(0, si.clone())])].concat()),
    ));
    cases.push((
        "id3 with footer",
        with([b"ID3\x04\x00\x10\x00\x00\x00\x05abcde0123456789".to_vec(), meta(&[(0, si.clone())])].concat()),
    ));
    cases.push(("id3 truncated", b"ID3\x03\x00\x00\x7f\x7f\x7f\x7f".to_vec()));
    cases.push(("id3 header only", b"ID3\x03\x00\x00\x00\x00\x00\x00".to_vec()));
    cases.push(("id3 huge", b"ID3\x03\x00\x10\x7f\x7f\x7f\x7f".to_vec()));
    for (name, d) in &cases {
        check(name, d);
    }
    for name in ["padding and picture", "id3 before", "duplicate streaminfo"] {
        let (_, d) = cases.iter().find(|(n, _)| *n == name).unwrap();
        coded(name, d);
    }
}

#[test]
fn header_beyond_the_data() {
    let d = [0xffu8, 0xf8, 0x89, 0x08, 0x00];
    for p in [0usize, 1, 4, 5, 6, 100, usize::MAX - 3, usize::MAX] {
        assert!(flac::parse_header(&d, p, 16).is_none(), "p = {p}");
    }
}

fn sample() -> Vec<u8> {
    let h = Hdr::new(9, 8, 4);
    let lpc = Sub::Pred { order: 3, lpc: Some((10, 5)), method: 0, porder: 3, escape: Some(9), k: 0, wasted: 0 };
    file(&h, 16, 2, &[Sub::fixed(2, 2), lpc], 4)
}

#[test]
fn damaged_files() {
    let d = sample();
    coded("sample", &d);
    damaged(&d, 300, 80, Duration::from_secs(20), |v| {
        check("damaged", v);
        assert!(aqr_testkit::most() < 1 << 28, "allocated {}", aqr_testkit::most());
    });
}

#[test]
fn damaged_streams() {
    let d = sample();
    let f = formula::encode(&d, 1).unwrap();
    let a = f.applies()[0];
    let ins: Vec<Vec<u8>> = f.streams.iter().map(|s| s.data.to_vec()).collect();
    for k in 0..ins.len() {
        damaged(&ins[k], 100, 40, Duration::from_secs(20), |v| {
            let mut m = ins.clone();
            m[k] = v.to_vec();
            let r: Vec<&[u8]> = m.iter().map(|v| &v[..]).collect();
            let _ = formula::decode(&a.params, &[&a.resources[0][..]], &r, a.len, 1);
            assert!(aqr_testkit::most() < 1 << 28, "allocated {}", aqr_testkit::most());
        });
    }
}
