//! Edge cases of Snappy streams and LevelDB tables: lengths claimed that the data cannot
//! hold, tables mixing the kinds of blocks, footers that are not what the writer makes.

use std::time::Duration;

use aqr_archive::Evaluate;
use aqr_common::varint;
use aqr_formula::{Apply, Formula};
use aqr_snappy::formula::{self, TABLE_MODULE};
use aqr_snappy::leveldb::{self, CODE_KEPT, CODE_RAW, CODE_SNAPPY, FOOTER, SNAPPY};
use aqr_snappy::snappy::{self, Hash};
use aqr_testkit::{Refusing, case, damaged, most};

#[global_allocator]
static ALLOC: Refusing = Refusing(256 << 20);

const LIMIT: Duration = Duration::from_secs(60);

fn eval(f: &Formula) -> aqr_formula::Result<Vec<u8>> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| {
        assert_eq!(a.module, TABLE_MODULE);
        formula::decode_table(&a.params, inputs, a.len, 1).map_err(|e| aqr_formula::Error::new(e.to_string()))
    })
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// Some text, different for each `seed`, that the hashes compress differently.
fn text(seed: u32, n: usize) -> Vec<u8> {
    let words = ["alpha", "beta", "gamma", "delta", "key", "value", "user", "session", "0123", "zz"];
    let mut r = seed.wrapping_mul(2654435761) | 1;
    let mut v = Vec::new();
    while v.len() < n {
        r ^= r << 13;
        r ^= r >> 17;
        r ^= r << 5;
        v.extend_from_slice(words[r as usize % words.len()].as_bytes());
        v.push(b' ' + (r >> 8) as u8 % 40);
    }
    v.truncate(n);
    v
}

/// What a data block is stored as.
#[derive(Clone, Copy, Debug)]
enum Kind {
    Raw,
    /// Snappy by the hash of the table.
    Rebuilt,
    /// Snappy by the other hash (kept).
    Other,
    /// Snappy of a stream the compressors do not give (kept): a literal for all of it.
    Odd,
}

/// A table of data blocks of the kinds given (and an empty metaindex and an index of raw
/// blocks), compressed by `Mul`.
fn table(kinds: &[Kind]) -> Vec<u8> {
    let (mut data, mut layout, mut kept) = (Vec::new(), Vec::new(), Vec::new());
    let mut handles = Vec::new();
    let mut at = 0u64;
    for (i, &k) in kinds.iter().enumerate() {
        let d = text(i as u32 + 1, 3000 + i * 100);
        let stored = match k {
            Kind::Raw => d.clone(),
            Kind::Rebuilt => snappy::compress(&d, Hash::Mul).unwrap(),
            Kind::Other => {
                let c = snappy::compress(&d, Hash::Crc).unwrap();
                assert_ne!(c, snappy::compress(&d, Hash::Mul).unwrap(), "block {i}");
                c
            }
            Kind::Odd => {
                let mut s = Vec::new();
                varint::put(&mut s, d.len() as u64);
                // (a literal of all of it: the tag's length is of 2 bytes, 61 << 2)
                s.push(61 << 2);
                s.extend_from_slice(&((d.len() - 1) as u16).to_le_bytes());
                s.extend_from_slice(&d);
                s
            }
        };
        handles.push((at, stored.len() as u64));
        at += stored.len() as u64 + 5;
        match k {
            Kind::Raw => {
                varint::put(&mut layout, d.len() as u64);
                layout.push(CODE_RAW);
                data.extend_from_slice(&d);
            }
            Kind::Rebuilt => {
                varint::put(&mut layout, d.len() as u64);
                layout.push(CODE_SNAPPY);
                data.extend_from_slice(&d);
            }
            _ => {
                varint::put(&mut layout, stored.len() as u64);
                layout.extend_from_slice(&[CODE_KEPT, SNAPPY]);
                kept.extend_from_slice(&stored);
            }
        }
    }
    // the metaindex: no entries, one restart; the index: a key and a handle for each block
    let meta = [0, 0, 0, 0, 1, 0, 0, 0];
    let mut index = Vec::new();
    for (i, &(off, size)) in handles.iter().enumerate() {
        let mut v = Vec::new();
        varint::put(&mut v, off);
        varint::put(&mut v, size);
        let key = [b'k', i as u8];
        varint::put(&mut index, 0);
        varint::put(&mut index, key.len() as u64);
        varint::put(&mut index, v.len() as u64);
        index.extend_from_slice(&key);
        index.extend_from_slice(&v);
    }
    index.extend_from_slice(&[0, 0, 0, 0, 1, 0, 0, 0]);
    for raw in [&meta[..], &index[..]] {
        varint::put(&mut layout, raw.len() as u64);
        layout.push(CODE_RAW);
        data.extend_from_slice(raw);
    }
    leveldb::build(&data, &layout, &kept, Hash::Mul).unwrap()
}

fn assert_rebuilt(t: &[u8], expect_kept: bool) {
    let f = formula::encode(t, 1).expect("a formula");
    assert!(eval(&f).unwrap() == t);
    assert_eq!(!f.streams[2].data.is_empty(), expect_kept);
}

#[test]
fn claimed_lengths_the_data_cannot_hold() {
    for claim in [u32::MAX as u64, 1 << 30, (1 << 30) - 1, 1 << 28] {
        let mut s = Vec::new();
        varint::put(&mut s, claim);
        s.push(0); // a literal of 1 byte
        s.push(b'a');
        case("claim");
        assert!(snappy::decode(&s, 1 << 30).is_err(), "{claim}");
        assert!(most() < 1 << 20, "{claim}: {} bytes asked", most());
    }
    // a table's blocks claiming the same: not a table, and no allocation for it
    let mut bomb = Vec::new();
    varint::put(&mut bomb, 1 << 30);
    bomb.extend_from_slice(&[0, b'a']);
    let (mut layout, mut kept) = (Vec::new(), Vec::new());
    for _ in 0..2 {
        varint::put(&mut layout, bomb.len() as u64);
        layout.extend_from_slice(&[CODE_KEPT, SNAPPY]);
        kept.extend_from_slice(&bomb);
    }
    let t = leveldb::build(&[], &layout, &kept, Hash::Mul).unwrap();
    case("table claim");
    assert!(leveldb::is_table(&t));
    assert!(leveldb::blocks(&t).is_err());
    assert!(formula::encode(&t, 1).is_none());
    assert!(most() < 1 << 20, "{} bytes asked", most());
}

#[test]
fn high_ratio_streams() {
    // 16 MiB in about 800 KB: copies of 64 bytes from one byte back
    let mut s = Vec::new();
    let n = 16u64 << 20;
    varint::put(&mut s, n);
    s.extend_from_slice(&[0, b'a']);
    let rest = n - 1;
    for _ in 0..rest / 64 {
        s.extend_from_slice(&[(63 << 2) | 2, 1, 0]);
    }
    assert_eq!(rest % 64, 63);
    s.extend_from_slice(&[(62 << 2) | 2, 1, 0]);
    let d = snappy::decode(&s, 1 << 30).unwrap();
    assert!(d.len() == n as usize && d.iter().all(|&b| b == b'a'));
    assert!(snappy::decode(&s, 1 << 20).is_err());
}

#[test]
fn tables_mixing_the_kinds_of_blocks() {
    use Kind::*;
    assert_rebuilt(&table(&[Rebuilt, Raw, Rebuilt]), false);
    assert_rebuilt(&table(&[Raw, Rebuilt, Other, Rebuilt]), true);
    assert_rebuilt(&table(&[Odd, Rebuilt, Raw, Other, Odd]), true);
    assert_rebuilt(&table(&[Other, Rebuilt]), true);
    assert_rebuilt(&table(&[Rebuilt]), false);
    // nothing rebuilt: no formula
    assert!(formula::encode(&table(&[Raw, Raw]), 1).is_none());
    assert!(formula::encode(&table(&[Odd, Raw, Odd]), 1).is_none());
    // a table by the other hash is rebuilt by it
    assert_rebuilt(&table(&[Other]), false);
    assert!(formula::encode(&table(&[]), 1).is_none());
}

#[test]
fn footers_the_writer_does_not_make() {
    let t = table(&[Kind::Rebuilt, Kind::Raw, Kind::Rebuilt]);
    assert_rebuilt(&t, false);
    // padding of the footer that is not zero: whatever is made, it is the table
    for i in 0..40 {
        let mut u = t.clone();
        let at = u.len() - FOOTER + i;
        if u[at] != 0 {
            continue;
        }
        u[at] = 1;
        case("padding");
        if let Some(f) = formula::encode(&u, 1) {
            assert!(eval(&f).unwrap() == u, "padding at {i}");
        }
    }
    // the last byte of the padding, certainly past the handles
    let mut u = t.clone();
    let at = u.len() - 9;
    assert_eq!(u[at], 0);
    u[at] = 1;
    assert!(leveldb::is_table(&u));
    assert!(formula::encode(&u, 1).is_none());
    // junk before the table, and the table cut at every place of its footer
    let mut u = b"junk".to_vec();
    u.extend_from_slice(&t);
    assert!(formula::encode(&u, 1).is_none());
    for cut in 1..=FOOTER + 6 {
        let u = &t[..t.len() - cut];
        assert!(formula::encode(u, 1).is_none(), "cut {cut}");
    }
}

#[test]
fn damaged_inputs() {
    let t = table(&[Kind::Raw, Kind::Rebuilt, Kind::Other, Kind::Odd]);
    for d in [t, fixture("mixed-mul.ldb"), fixture("odd-crc.ldb")] {
        damaged(&d, 120, FOOTER + 8, LIMIT, |v| {
            let _ = leveldb::is_table(v);
            let _ = leveldb::blocks(v);
            if let Some(f) = formula::encode(v, 1) {
                assert!(eval(&f).unwrap() == v);
            }
            assert!(most() < 128 << 20, "{} bytes asked", most());
        });
    }
    for name in ["mixed-mul.snappy", "tiny300-crc.snappy", "tiny0-mul.snappy", "runs-crc.snappy"] {
        let d = fixture(name);
        damaged(&d, 150, 16, LIMIT, |v| {
            if let Ok(x) = snappy::decode(v, 1 << 24) {
                assert!(x.len() <= 1 << 24);
            }
            assert!(most() < 128 << 20, "{} bytes asked", most());
        });
    }
}
