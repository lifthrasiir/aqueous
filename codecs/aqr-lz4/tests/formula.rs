use aqr_formula::{Apply, Formula, Node};
use aqr_lz4::block::{self, Compressor};
use aqr_lz4::formula::{self, ACCELERATIONS, MODULE, MOZLZ4};
use aqr_lz4::frame;

fn eval(f: &Formula) -> aqr_formula::Result<Vec<u8>> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| {
        assert_eq!(a.module, MODULE);
        formula::decode(&a.params, inputs, a.len, 1).map_err(|e| aqr_formula::Error::new(e.to_string()))
    })
}

/// The fixtures of liblz4 1.10.0 (`scripts/gen-fixtures.py`), by name.
fn fixtures(ext: &str) -> Vec<(String, Vec<u8>)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == ext))
        .map(|p| (p.file_stem().unwrap().to_str().unwrap().to_owned(), std::fs::read(&p).unwrap()))
        .collect();
    v.sort();
    assert!(!v.is_empty());
    v
}

/// The application of `f`'s root, and its acceleration.
fn application(f: &Formula) -> (&Apply, u8) {
    let mut found = None;
    f.root.visit(&mut |n| {
        if let Node::Apply(a) = n {
            found = Some(&**a);
        }
    });
    let a = found.expect("an application");
    (a, a.params[1])
}

#[test]
fn blocks_of_liblz4() {
    for (name, d) in fixtures("mozlz4") {
        let a: u32 = name.rsplit_once("-a").unwrap().1.parse().unwrap();
        let n = u32::from_le_bytes(d[8..12].try_into().unwrap()) as usize;
        let blk = &d[12..];
        assert_eq!(block::end(blk, n), Some(blk.len()), "{name}");
        let mut data = Vec::new();
        block::decode(blk, &mut data, 0, n).unwrap();
        assert_eq!(data.len(), n);
        assert!(Compressor::compress_fast(&data, a).unwrap() == blk, "{name}: compressed otherwise");
        assert!(Compressor::reproduces_fast(&data, a, blk), "{name}");
        assert_eq!(formula::extent(&d), Some(d.len()));
        // an acceleration not tried is found only where a smaller one compresses the same
        let Some(f) = formula::encode(&d, 1) else {
            assert!(!ACCELERATIONS.contains(&a), "{name}: no formula");
            continue;
        };
        assert!(eval(&f).unwrap() == d, "{name}");
        let (_, found) = application(&f);
        assert!(found as u32 <= a, "{name}: acceleration {found}");
    }
}

#[test]
fn frames_of_liblz4() {
    for (name, d) in fixtures("lz4") {
        let fr = frame::decode(&d).unwrap();
        assert_eq!(fr.len, d.len(), "{name}");
        assert_eq!(frame::end(&d), Some(d.len()), "{name}");
        let f = formula::encode(&d, 1);
        if name.ends_with("-linked") {
            assert!(f.is_none(), "{name}: linked blocks reproduced");
            continue;
        }
        let f = f.unwrap_or_else(|| panic!("{name}: no formula"));
        assert!(eval(&f).unwrap() == d, "{name}");
        let (a, acc) = application(&f);
        assert_eq!(a.len, d.len() as u64);
        // `--fast=3` is level -3: acceleration 4
        assert_eq!(acc, if name.ends_with("-level-3") { 4 } else { 1 }, "{name}");
    }
}

/// What follows a container is sliced as it is, and an empty mozlz4 is a block of a token.
#[test]
fn around_and_empty() {
    let data = b"session window tab session window tab session window tab".repeat(50);
    let blk = Compressor::compress_fast(&data, 1).unwrap();
    let moz = [&MOZLZ4[..], &(data.len() as u32).to_le_bytes(), &blk, b"trailing"].concat();
    let f = formula::encode(&moz, 1).unwrap();
    assert!(eval(&f).unwrap() == moz);
    assert_eq!(formula::extent(&moz), Some(moz.len() - 8));
    let empty = [&MOZLZ4[..], &[0; 4], &[0]].concat();
    assert!(eval(&formula::encode(&empty, 1).unwrap()).unwrap() == empty);

    let (_, fr) = fixtures("lz4").into_iter().find(|(n, _)| n == "f64k").unwrap();
    let two = [&fr[..], &fr[..], b"x"].concat();
    let f = formula::encode(&two, 1).unwrap();
    assert!(eval(&f).unwrap() == two);
    assert_eq!(formula::extent(&two), Some(fr.len()));
}

/// Streams no acceleration gives are turned down.
#[test]
fn not_reproduced() {
    let data = b"abcdefgh".repeat(100);
    // a valid block, but liblz4 extends a match as far as it goes, not cut in two: 8 literals,
    // a match of 100 bytes (4 + 15 + 81), one of 687 (4 + 15 + 255 + 255 + 158), 5 literals
    let blk = [&[0x8f][..], &data[..8], &[8, 0, 81, 0x0f, 8, 0, 255, 255, 158, 0x50], &data[795..]].concat();
    let mut out = Vec::new();
    block::decode(&blk, &mut out, 0, data.len()).unwrap();
    assert!(out == data);
    let moz = [&MOZLZ4[..], &(data.len() as u32).to_le_bytes(), &blk].concat();
    assert_eq!(formula::extent(&moz), Some(moz.len()));
    assert!(formula::encode(&moz, 1).is_none());
    assert!(formula::encode(b"mozLz40\0\x10\0\0\0\x10", 1).is_none());
    assert!(formula::encode(b"hello", 1).is_none());
}

fn xorshift(r: &mut u64) -> u64 {
    *r ^= *r << 13;
    *r ^= *r >> 7;
    *r ^= *r << 17;
    *r
}

/// Damaged containers and module inputs: errors or other output, never panics.
#[test]
fn corrupt_inputs() {
    let mut r = 99u64;
    for (name, d) in fixtures("mozlz4").into_iter().chain(fixtures("lz4")).filter(|(_, d)| d.len() < 8000) {
        for k in 0..100 {
            let mut d = d.clone();
            let i = xorshift(&mut r) as usize % d.len();
            if k % 4 == 0 {
                d.truncate(i);
            } else {
                d[i] ^= 1 << (xorshift(&mut r) % 8);
            }
            let _ = formula::extent(&d);
            if let Some(f) = formula::encode(&d, 1) {
                assert!(eval(&f).unwrap() == d, "{name}: mutation {k}");
            }
        }
    }
    let data = b"0123456789".repeat(100);
    for params in [&[][..], &[0], &[0, 0], &[0, 1, 0], &[1, 1], &[1, 1, 0x40, 0x40], &[1, 1, 0x60, 0x70, 0], &[2, 1]] {
        assert!(formula::decode(params, &[&data], 100, 1).is_err(), "{params:?}");
    }
    assert!(formula::decode(&[0, 1], &[&data, &data], 100, 1).is_err());
    assert!(formula::decode(&[0, 1], &[&data], 100, 1).is_err());
    assert!(formula::decode(&[0, 0x82, 0x80, 0x04], &[&data], 100, 1).is_err());
    for _ in 0..2000 {
        let n = xorshift(&mut r) as usize % 64;
        let blk: Vec<u8> = (0..n).map(|_| xorshift(&mut r) as u8).collect();
        let mut out = Vec::new();
        let _ = block::decode(&blk, &mut out, 0, 1 << 16);
        let _ = block::end(&blk, xorshift(&mut r) as usize % 300);
        let _ = frame::decode(&[&frame::MAGIC[..], &blk].concat());
    }
}

/// Round trips of the compressor through the decoder, over sizes around its thresholds and
/// accelerations.
#[test]
fn compressor_round_trips() {
    let mut r = 7u64;
    let mut src = Vec::new();
    while src.len() < 300_000 {
        let k = xorshift(&mut r);
        if k.is_multiple_of(5) && src.len() > 1000 {
            let from = src.len() - 1 - (k >> 8) as usize % 1000.min(src.len() - 1);
            for i in 0..(k >> 20) as usize % 300 {
                src.push(src[from + i]);
            }
        } else {
            src.push(b"aaaabbbcdef"[(k >> 30) as usize % 11]);
        }
    }
    for n in [0, 1, 12, 13, 14, 100, 4095, 4096, 65535, 65546, 65547, 65548, 200_000, 300_000] {
        for a in [1, 3, 100, 65537] {
            let blk = Compressor::compress_fast(&src[..n], a).unwrap();
            assert!(blk.len() <= block::bound(n));
            let mut out = Vec::new();
            block::decode(&blk, &mut out, 0, n).unwrap();
            assert!(out == src[..n], "{n} bytes, acceleration {a}");
            assert_eq!(block::end(&blk, n), Some(blk.len()));
        }
    }
}
