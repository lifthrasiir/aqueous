//! DEX files written here (no Android tools needed) through the formula and the module: strings
//! of every UTF-16 length, prototypes, fields, methods, classes with their data and code (try
//! blocks among it); code and classes' data laid out in other orders than the classes', LEB128s
//! not in the shortest form, wrong checksums and sections the module does not know, kept as
//! they are where they cannot be taken; streams corrupted, which never panic.

use aqr_archive::Evaluate;
use aqr_common::rng::Rng;
use aqr_dex::dex::{self, mode, s};
use aqr_dex::{encode, formula};
use aqr_formula::{Apply, Formula};
use std::time::Duration;

#[global_allocator]
static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(1 << 30);

fn eval(f: &Formula) -> aqr_formula::Result<Vec<u8>> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| {
        assert_eq!(a.module, formula::MODULE);
        formula::decode(&a.params, inputs, a.len).map_err(|e| aqr_formula::Error::new(e.0))
    })
}

#[path = "common/synth.rs"]
mod synth;
use synth::*;

/// The modes of the map's items, by type, from the head stream.
fn modes(st: &[Vec<u8>]) -> Vec<(u16, u8)> {
    let head = streams(st)[s::HEAD];
    let count = dex::le32(head, dex::HEADER + 1).unwrap() as usize;
    let items = dex::items(&head[dex::HEADER + 5..][..12 * count]);
    items.iter().map(|it| it.ty).zip(head[dex::HEADER + 5 + 12 * count..].iter().copied()).collect()
}

fn check(d: &[u8]) -> Vec<Vec<u8>> {
    let f = formula::encode(d, 1).expect("taken");
    assert_eq!(eval(&f).unwrap(), d);
    encode::split(d).unwrap()
}

/// The streams of the module's inputs `st`.
fn streams(st: &[Vec<u8>]) -> Vec<&[u8]> {
    dex::unpack(&st.iter().map(|v| v.as_slice()).collect::<Vec<_>>()).unwrap()
}

/// Files of every shape come back; what can be taken is.
#[test]
fn files() {
    for seed in 0..200 {
        let d = write(seed, Shape::default());
        let st = check(&d);
        for (t, m) in modes(&st) {
            assert_eq!(m == mode::TAKEN, dex::takes(t), "seed {seed}: {t:#x} {m}");
        }
        assert_eq!(streams(&st)[s::HEAD][dex::HEADER], 3, "checksum and signature derived");
        assert!(streams(&st)[s::REST].iter().all(|&b| b == 0), "only padding in the rest");
        for (k, shape) in [
            Shape { code_order: true, ..Shape::default() },
            Shape { data_order: true, ..Shape::default() },
            Shape { code_order: true, data_order: true, unknown: true, ..Shape::default() },
        ]
        .into_iter()
        .enumerate()
        {
            let st = check(&write(seed, shape));
            for (t, m) in modes(&st) {
                assert_eq!(m == mode::TAKEN, dex::takes(t), "seed {seed}, shape {k}: {t:#x} {m}");
            }
        }
    }
}

/// What the module would not rebuild as it is is kept: a class's data with a LEB128 longer than
/// it needs, and checksums that are wrong.
#[test]
fn kept() {
    for seed in 0..50 {
        let st = check(&write(seed, Shape { long_leb: true, ..Shape::default() }));
        let m = modes(&st);
        assert!(m.contains(&(0x2000, mode::RAW)) && m.contains(&(0x0006, mode::TAKEN)), "seed {seed}");
        let st = check(&write(seed, Shape { bad_sums: true, ..Shape::default() }));
        assert_eq!(streams(&st)[s::HEAD][dex::HEADER], 0);
    }
}

/// Not DEX files, or ones the module does not take.
#[test]
fn not_taken() {
    let d = write(1, Shape::default());
    assert!(encode::split(&d[..d.len() - 1]).is_none(), "cut short");
    let mut v41 = d.clone();
    v41[4..7].copy_from_slice(b"041");
    assert!(encode::split(&v41).is_none(), "a container");
    assert!(encode::split(b"dex\n035\0").is_none());
    assert!(encode::split(&[0; 200]).is_none());
}

/// Corrupted streams make the module fail or give a file of the length, never panic.
#[test]
fn corrupt() {
    let d = write(3, Shape { code_order: true, data_order: true, ..Shape::default() });
    let st = encode::split(&d).unwrap();
    let mut rng = Rng::new(7);
    for _ in 0..20000 {
        let mut bad = st.clone();
        let k = rng.below(st.len() as u64) as usize;
        if bad[k].is_empty() {
            continue;
        }
        let at = rng.below(bad[k].len() as u64) as usize;
        match rng.below(3) {
            0 => bad[k].truncate(at),
            1 => bad[k][at] ^= 1 << rng.below(8),
            _ => bad[k][at] = rng.u64() as u8,
        }
        let refs: Vec<&[u8]> = bad.iter().map(|v| v.as_slice()).collect();
        if let Ok(m) = formula::decode(&[], &refs, d.len() as u64) {
            assert_eq!(m.len(), d.len());
        }
    }
}

/// The files are some kilobytes: what damaged input reserves stays far below this.
const SMALL: usize = 1 << 24;

/// A length no file has (the archive declares it, the module must not trust it): refused
/// before the file's room is reserved.
#[test]
fn huge_lengths() {
    let d = write(3, Shape::default());
    let st = check(&d);
    let refs: Vec<&[u8]> = st.iter().map(|v| v.as_slice()).collect();
    for len in [u64::MAX, 1 << 62, 1 << 40, 1 << 33, d.len() as u64 * 1000, d.len() as u64 + 1, d.len() as u64 - 1, 0] {
        aqr_testkit::case(&format!("length {len}"));
        let r = formula::decode(&[], &refs, len);
        assert!(r.is_err(), "length {len}");
        assert!(aqr_testkit::most() < SMALL, "length {len}: {} bytes reserved", aqr_testkit::most());
    }
    assert_eq!(formula::decode(&[], &refs, d.len() as u64).unwrap(), d);
    assert!(formula::decode(&[0], &refs, d.len() as u64).is_err(), "params");
}

/// Every damage of the module's inputs: an error, or a file of the length, and no large
/// reservation (counts of the map and the headers are not trusted before they are checked).
#[test]
fn damaged_streams() {
    for seed in [3, 8] {
        let d = write(seed, Shape { code_order: true, data_order: true, ..Shape::default() });
        let st = encode::split(&d).unwrap();
        for k in 0..st.len() {
            aqr_testkit::damaged(&st[k], 150, 64, Duration::from_secs(30), |v| {
                let mut bad = st.clone();
                bad[k] = v.to_vec();
                let refs: Vec<&[u8]> = bad.iter().map(|v| v.as_slice()).collect();
                if let Ok(m) = formula::decode(&[], &refs, d.len() as u64) {
                    assert_eq!(m.len(), d.len());
                }
                assert!(aqr_testkit::most() < SMALL, "{} bytes reserved", aqr_testkit::most());
            });
        }
    }
}

/// Every damage of a DEX file: not taken, or taken and rebuilt bit for bit.
#[test]
fn damaged_files() {
    for seed in [2, 5] {
        let d = write(seed, Shape { code_order: true, ..Shape::default() });
        aqr_testkit::damaged(&d, 400, 0x80, Duration::from_secs(30), |v| {
            if let Some(f) = formula::encode(v, 1) {
                assert!(eval(&f).unwrap() == v, "not the file");
            }
            assert!(aqr_testkit::most() < SMALL, "{} bytes reserved", aqr_testkit::most());
        });
    }
}

/// Map items made debug info or encoded array items of counts the file cannot hold (the counts
/// size the lists of offsets): not taken, or rebuilt bit for bit, reserving little.
#[test]
fn lying_item_counts() {
    for seed in [2, 5] {
        let d = write(seed, Shape { code_order: true, ..Shape::default() });
        let map = dex::le32(&d, 0x34).unwrap() as usize;
        let n = dex::le32(&d, map).unwrap() as usize;
        for i in 0..n {
            let at = map + 4 + 12 * i;
            for ty in [dex::ty::DEBUG_INFO, dex::ty::ENCODED_ARRAY, dex::ty::CODE, dex::ty::TYPE_LIST] {
                for count in [u32::MAX, 1 << 31, 1 << 24, d.len() as u32 + 1, 0] {
                    let mut v = d.clone();
                    v[at..at + 2].copy_from_slice(&ty.to_le_bytes());
                    v[at + 4..at + 8].copy_from_slice(&count.to_le_bytes());
                    aqr_testkit::case(&format!("seed {seed} item {i} type {ty:#x} count {count:#x}"));
                    if let Some(f) = formula::encode(&v, 1) {
                        assert!(eval(&f).unwrap() == v, "not the file");
                    }
                    assert!(aqr_testkit::most() < SMALL, "{} bytes reserved", aqr_testkit::most());
                }
            }
        }
    }
}

/// The same counts in the head stream the module decodes from (the items' counts as the encoder
/// wrote them): an error or a file of the length, reserving little.
#[test]
fn lying_item_counts_decoded() {
    let d = write(3, Shape { code_order: true, data_order: true, ..Shape::default() });
    let st = encode::split(&d).unwrap();
    let head = streams(&st)[s::HEAD];
    let count = dex::le32(head, dex::HEADER + 1).unwrap() as usize;
    for i in 0..count {
        let at = dex::HEADER + 5 + 12 * i;
        for ty in [dex::ty::DEBUG_INFO, dex::ty::ENCODED_ARRAY] {
            for c in [u32::MAX, 1 << 31, 1 << 24] {
                let mut h = head.to_vec();
                h[at..at + 2].copy_from_slice(&ty.to_le_bytes());
                h[at + 4..at + 8].copy_from_slice(&c.to_le_bytes());
                let mut ins = streams(&st).iter().map(|x| x.to_vec()).collect::<Vec<_>>();
                ins[s::HEAD] = h;
                let packed = dex::pack(&dex::St(ins));
                let refs: Vec<&[u8]> = packed.iter().map(|v| v.as_slice()).collect();
                aqr_testkit::case(&format!("head item {i} type {ty:#x} count {c:#x}"));
                if let Ok(m) = formula::decode(&[], &refs, d.len() as u64) {
                    assert_eq!(m.len(), d.len());
                }
                assert!(aqr_testkit::most() < SMALL, "{} bytes reserved", aqr_testkit::most());
            }
        }
    }
}
