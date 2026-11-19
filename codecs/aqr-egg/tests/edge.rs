//! Edge cases of the reading and the formula: damaged archives, solid archives of many files,
//! mixed methods, sizes disagreeing with the data, LZMA with and without an end marker, split
//! volumes.

use std::time::Duration;

use aqr_archive::Evaluate;
use aqr_egg::{egg, formula};
use aqr_formula::{Apply, Formula};
use aqr_lzma::lzma::{self, Props};

#[global_allocator]
static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(1 << 30);

const FILE: u32 = 0x0A85_90E3;
const FILENAME: u32 = 0x0A85_91AC;
const END: u32 = 0x08E2_8222;
const SPLIT: u32 = 0x24F5_A262;
const SKIP: u32 = 0xFFFF_0000;

fn eval(f: &Formula) -> Result<Vec<u8>, String> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| formula::apply(a, inputs, 1).map_err(aqr_formula::Error::new))
        .map_err(|e| e.to_string())
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}.egg", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// Whatever `d` is, reading it and making its formula neither panic nor hang nor ask for what it
/// cannot hold, and a formula made rebuilds `d`.
fn check(d: &[u8]) {
    let _ = egg::parse(d);
    let _ = formula::extent(d);
    let _ = egg::entry_sums(&mut |r| d.get(r.start as usize..r.end as usize).map(<[u8]>::to_vec));
    if let Some(f) = formula::encode(d, 1) {
        assert!(eval(&f).as_deref() == Ok(d), "the formula does not rebuild");
    }
    // (what a header's size reserves is within the decoding budget, nothing past it)
    let budget = formula::limit(d.len()) as usize + (1 << 20);
    assert!(aqr_testkit::most() <= budget, "an allocation of {} bytes", aqr_testkit::most());
}

fn put(d: &mut Vec<u8>, s: u32) {
    d.extend_from_slice(&s.to_le_bytes());
}

const SMALL: [&str; 9] = [
    "azoshort_txt",
    "directories",
    "extattr",
    "globalcomment",
    "lorem_short_readonlyhidden",
    "small_kor",
    "solid",
    "lorem_long_zipEnc",
    "lzma_simple",
];

#[test]
fn truncated() {
    for name in SMALL {
        let d = fixture(name);
        for n in (0..d.len()).filter(|&n| n < 200 || n % 53 == 0 || n + 40 > d.len()) {
            aqr_testkit::case(&format!("{name} cut at {n}"));
            check(&d[..n]);
        }
    }
}

#[test]
fn damaged_fixtures() {
    for name in ["azoshort_txt", "solid", "lzma_simple", "small_kor", "extattr", "globalcomment"] {
        let d = fixture(name);
        aqr_testkit::damaged(&d, 100, 220, Duration::from_secs(30), check);
    }
}

/// Damage in the middle of a big AZO stream (data and index of its blocks).
#[test]
fn damaged_azo_stream() {
    let d = fixture("azonumbers");
    let a = egg::parse(&d).unwrap();
    let b = a.entries.iter().flat_map(|e| &e.blocks).find(|b| b.method == egg::AZO).unwrap();
    let body = b.body.clone();
    let mut x = d.clone();
    for (k, at) in (body.start..body.end).step_by((body.len() / 12).max(1)).enumerate() {
        x[at] ^= 1 << (k % 8);
        aqr_testkit::case(&format!("azonumbers flipped at {at}"));
        check(&x);
        x[at] ^= 1 << (k % 8);
    }
    for n in [body.start + 1, body.start + 9, body.start + body.len() / 2, body.end - 1] {
        check(&d[..n]);
    }
}

fn text(n: usize) -> Vec<u8> {
    (0..n as u32).flat_map(|i| format!("{} ", i * i % 1013).into_bytes()).collect()
}

fn bzip2(data: &[u8]) -> Vec<u8> {
    aqr_bz2::compress::stream(9, &aqr_bz2::compress::blocks(data, 9, 1))
}

/// A solid archive of five files: the first four have no block (their sizes add up to the part
/// of the last block's data before the last file), the last holds the one block of all.
#[test]
fn solid_of_many_files() {
    let data = text(3000);
    let sizes = [100u64, 0, 900, 500];
    let before: u64 = sizes.iter().sum();
    let last = egg::write(true, &[("e", egg::STORED, &data, data.len() as u32)]);
    // the prefix: header, the solid record, its end marker
    let head = egg::HEADER + 4 + 3 + 4;
    let mut d = last[..head].to_vec();
    for (k, (name, n)) in ["a", "b", "c", "d"].iter().zip(sizes).enumerate() {
        put(&mut d, FILE);
        put(&mut d, k as u32 + 1);
        d.extend_from_slice(&n.to_le_bytes());
        put(&mut d, FILENAME);
        d.extend_from_slice(&[0, 1, 0]);
        d.extend_from_slice(name.as_bytes());
        put(&mut d, END);
    }
    d.extend_from_slice(&last[head..]);
    let a = egg::parse(&d).unwrap();
    assert_eq!((a.entries.len(), a.solid, a.ended), (5, true, true));
    assert_eq!(a.entries.iter().filter(|e| e.blocks.is_empty()).count(), 4);
    let kind = |n: &[u8]| if n == b"e" { aqr_formula::Kind::Compressed } else { aqr_formula::Kind::Unknown };
    let opt = formula::Options { kind, ..Default::default() };
    // (the file e is its own size: the block holds all the files' contents, so its size is the
    // sum of the sizes; here the header of e says its own)
    let f = formula::encode_with(&d, &opt, &mut |_, _, _| true).unwrap().formula;
    assert!(eval(&f).as_deref() == Ok(&d[..]));
    check(&d);
    // sizes lying: the files before claim more than the block holds
    let mut y = d.clone();
    y[a.entries[0].pos + 8..a.entries[0].pos + 16].copy_from_slice(&u64::MAX.to_le_bytes());
    check(&y);
    let f = formula::encode_with(&y, &opt, &mut |_, _, _| true).unwrap().formula;
    assert!(eval(&f).as_deref() == Ok(&y[..]));
    assert!(before < data.len() as u64);
}

/// A LZMA block's data: 4 bytes the decoders skip, the properties and the dictionary, the stream.
fn lzma_block(data: &[u8], props: Props, marker: bool) -> Vec<u8> {
    let packets = aqr_lzma::greedy::packets(data, 1 << 16);
    let mut b = vec![0x10, 0x02, 0x05, 0x00, props.byte()];
    b.extend_from_slice(&(1u32 << 16).to_le_bytes());
    b.extend_from_slice(&lzma::encode(data, props, &packets, marker));
    b
}

/// Blocks of every method side by side, rebuilt or kept each by its own.
#[test]
fn mixed_methods() {
    let t = text(2000);
    let b = bzip2(&t);
    let props = Props { lc: 3, lp: 0, pb: 2 };
    let lz = lzma_block(&t, props, true);
    let files: [(&str, u8, &[u8], u32); 6] = [
        ("a", egg::STORED, b"stored", 6),
        ("b", egg::BZIP2, &b, t.len() as u32),
        ("c", egg::LZMA, &lz, t.len() as u32),
        ("d", egg::DEFLATE, b"\x03\x00", 0),
        ("e", egg::STORED, b"again", 5),
        ("f", egg::AZO, b"nope", 7),
    ];
    for solid in [false, true] {
        let d = egg::write(solid, &files);
        let f = formula::encode(&d, 2).unwrap();
        assert!(eval(&f).as_deref() == Ok(&d[..]));
        check(&d);
    }
    // a method unknown is kept
    let d = egg::write(false, &[("a", 9, b"???", 3), ("b", egg::STORED, b"x", 1)]);
    let f = formula::encode(&d, 1).unwrap();
    assert!(eval(&f).as_deref() == Ok(&d[..]));
}

/// LZMA with the end marker and without: both rebuilt, the sizes of the header right or wrong.
#[test]
fn lzma_markers_and_sizes() {
    let t = text(1500);
    let props = Props { lc: 3, lp: 0, pb: 2 };
    for marker in [true, false] {
        let lz = lzma_block(&t, props, marker);
        let d = egg::write(false, &[("a", egg::LZMA, &lz, t.len() as u32)]);
        let f = formula::encode(&d, 1);
        assert!(f.as_ref().is_some_and(|f| eval(f).as_deref() == Ok(&d[..])), "marker {marker}");
        for size in [0, 1, t.len() as u32 - 1, t.len() as u32 + 1, u32::MAX, 0x8000_0000] {
            let d = egg::write(false, &[("a", egg::LZMA, &lz, size)]);
            aqr_testkit::case(&format!("lzma marker {marker} size {size}"));
            check(&d);
            assert!(formula::encode(&d, 1).is_none(), "marker {marker}, size {size}");
        }
        // properties and dictionary lying
        for (at, v) in [(4, 225u8), (4, 0xff), (5, 0), (8, 0xff)] {
            let mut lz = lz.clone();
            lz[at] = v;
            let d = egg::write(false, &[("a", egg::LZMA, &lz, t.len() as u32)]);
            check(&d);
        }
        // a block shorter than its header
        for n in 0..12 {
            let d = egg::write(false, &[("a", egg::LZMA, &lz[..n], t.len() as u32)]);
            check(&d);
        }
    }
}

/// Sizes of DEFLATE, bzip2 and AZO blocks that disagree with their data, or are huge.
#[test]
fn size_mismatches() {
    let t = text(1200);
    let b = bzip2(&t);
    // stored DEFLATE blocks, 3 bytes of data
    let stored_deflate: Vec<u8> = vec![0x01, 0x03, 0x00, 0xfc, 0xff, b'a', b'b', b'c'];
    for size in [0, 1, 2, 4, 1 << 20, u32::MAX] {
        for (method, body) in [(egg::DEFLATE, &stored_deflate[..]), (egg::BZIP2, &b[..])] {
            let want = if method == egg::DEFLATE { 3 } else { t.len() as u32 };
            let d = egg::write(false, &[("a", method, body, size)]);
            aqr_testkit::case(&format!("method {method} size {size}"));
            check(&d);
            if size != want {
                assert!(formula::encode(&d, 1).is_none(), "method {method}, size {size}");
            }
        }
        let d = egg::write(false, &[("a", egg::AZO, b"AZO\x00 not a stream", size)]);
        check(&d);
    }
    let d = egg::write(false, &[("a", egg::STORED, b"abc", 4)]);
    assert!(formula::encode(&d, 1).is_none());
    check(&d);
}

/// The first volume of a split archive: its records run out at its end; the second starts with
/// the split record and no file's start.
#[test]
fn split_volumes() {
    let t = text(800);
    let whole = egg::write(false, &[("a", egg::STORED, &t, t.len() as u32), ("b", egg::STORED, b"xyz", 3)]);
    let cut = whole.len() - 40;
    let first = &whole[..cut];
    let a = egg::parse(first).unwrap();
    assert!(!a.ended && a.end == first.len(), "{:?}", (a.ended, a.end));
    check(first);
    let mut vol = whole[..egg::HEADER].to_vec();
    put(&mut vol, SPLIT);
    vol.extend_from_slice(&[0, 8, 0, 1, 2, 3, 4, 5, 6, 7, 8]);
    put(&mut vol, END);
    vol.extend_from_slice(&whole[egg::HEADER + 4..cut]);
    let a = egg::parse(&vol).unwrap();
    assert!(a.split && !a.ended);
    check(&vol);
    // the second volume's records: bytes of no record where a file would start
    let mut second = whole[..egg::HEADER].to_vec();
    put(&mut second, SPLIT);
    second.extend_from_slice(&[0, 8, 0, 1, 2, 3, 4, 5, 6, 7, 8]);
    put(&mut second, END);
    second.extend_from_slice(&whole[cut..]);
    let a = egg::parse(&second).unwrap();
    assert!(a.split && a.entries.is_empty());
    check(&second);
}

/// A skip record with lengths that run past the data or overflow.
#[test]
fn extra_records_lying() {
    let whole = egg::write(false, &[("a", egg::STORED, b"abc", 3)]);
    for flags in [0u8, 1] {
        for len in [0u32, 1, 0xffff, 0xffff_ffff] {
            let mut d = whole[..egg::HEADER].to_vec();
            put(&mut d, SKIP);
            d.push(flags);
            if flags & 1 != 0 {
                d.extend_from_slice(&len.to_le_bytes());
            } else {
                d.extend_from_slice(&(len as u16).to_le_bytes());
            }
            put(&mut d, END);
            d.extend_from_slice(&whole[egg::HEADER + 4..]);
            check(&d);
        }
    }
}
