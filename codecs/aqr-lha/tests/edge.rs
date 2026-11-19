//! Edge cases: damaged archives, a bad header checksum between good entries, the extended header
//! of 64-bit sizes, directories and symbolic links, lying sizes.

use std::time::Duration;

use aqr_archive::Evaluate;
use aqr_formula::{Apply, Formula};
use aqr_lha::{formula, lha};
use aqr_testkit::{Refusing, damaged};

#[global_allocator]
static ALLOC: Refusing = Refusing(256 << 20);

const LIMIT: Duration = Duration::from_secs(60);

fn eval(f: &Formula) -> Vec<u8> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| formula::apply(a, inputs, 2).map_err(aqr_formula::Error::new)).unwrap()
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}.lzh", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// What an archive makes of `d`: no panic, and a formula, if any, rebuilds it.
fn check(d: &[u8]) {
    let _ = lha::parse(d);
    let _ = formula::extent(d);
    if let Some(f) = formula::encode(d, 2) {
        assert!(eval(&f) == d, "formula does not rebuild");
    }
}

fn sum(h: &[u8]) -> u8 {
    h.iter().fold(0u8, |s, &b| s.wrapping_add(b))
}

/// A header of level 0 or 1 (level 1: no extended headers) for `data`, stored (`method`).
fn header01(level: u8, method: &[u8; 5], name: &[u8], size: u32, data: &[u8]) -> Vec<u8> {
    let mut h = vec![0, 0];
    h.extend_from_slice(method);
    h.extend_from_slice(&(data.len() as u32).to_le_bytes());
    h.extend_from_slice(&size.to_le_bytes());
    h.extend_from_slice(&[0; 4]);
    h.extend_from_slice(&[0x20, level, name.len() as u8]);
    h.extend_from_slice(name);
    h.extend_from_slice(&lha::crc16(data).to_le_bytes());
    if level == 1 {
        h.extend_from_slice(&[b'U', 0, 0]);
    }
    h[0] = (h.len() - 2) as u8;
    h[1] = sum(&h[2..]);
    [h, data.to_vec()].concat()
}

/// A header of level 2 whose extended headers are `ext` (each type and data), the sizes of
/// the base header being `packed` and `size`.
fn header2(method: &[u8; 5], packed: u32, size: u32, crc: u16, ext: &[(u8, Vec<u8>)], data: &[u8]) -> Vec<u8> {
    let mut h = vec![0, 0];
    h.extend_from_slice(method);
    h.extend_from_slice(&packed.to_le_bytes());
    h.extend_from_slice(&size.to_le_bytes());
    h.extend_from_slice(&[0; 4]);
    h.extend_from_slice(&[0, 2]);
    h.extend_from_slice(&crc.to_le_bytes());
    h.push(b'U');
    let mut tail = Vec::new();
    for (i, (t, x)) in ext.iter().enumerate() {
        let n = 1 + x.len() + 2;
        if i == 0 {
            tail.extend_from_slice(&(n as u16).to_le_bytes());
        }
        tail.push(*t);
        tail.extend_from_slice(x);
        let next = ext.get(i + 1).map_or(0, |(_, y)| 1 + y.len() + 2);
        tail.extend_from_slice(&(next as u16).to_le_bytes());
    }
    if ext.is_empty() {
        tail.extend_from_slice(&[0, 0]);
    }
    h.extend_from_slice(&tail);
    let len = h.len() as u16;
    h[..2].copy_from_slice(&len.to_le_bytes());
    [h, data.to_vec()].concat()
}

fn text(n: usize) -> Vec<u8> {
    (0..n).map(|i| b"hello, lha world\n"[i % 17 % 16]).collect()
}

#[test]
fn damaged_fixtures() {
    for name in ["unix_h0_lh5", "dos255e_subdir", "os2_h3_lh5", "amiga_lh4"] {
        let d = fixture(name);
        damaged(&d, 40, 120, LIMIT, check);
    }
}

#[test]
fn damaged_synthetic_levels() {
    let t = text(200);
    let mut a = header01(0, b"-lh0-", b"a.txt", 200, &t);
    a.extend(header01(1, b"-lh0-", b"b.txt", 200, &t));
    a.extend(header2(b"-lh0-", 200, 200, lha::crc16(&t), &[(1, b"c.txt".to_vec())], &t));
    a.push(0);
    assert_eq!(lha::parse(&a).unwrap().entries.len(), 3);
    check(&a);
    damaged(&a, 40, 120, LIMIT, check);
}

#[test]
fn bad_checksum_between_good_entries() {
    let t = text(300);
    let first = header01(0, b"-lh0-", b"a", 300, &t);
    let mut mid = header01(0, b"-lh0-", b"b", 300, &t);
    let last = header01(0, b"-lh0-", b"c", 300, &t);
    mid[1] ^= 0x55;
    let mut d = [first.clone(), mid.clone(), last.clone()].concat();
    d.push(0);
    let a = lha::parse(&d).unwrap();
    // the archive ends where the header is bad; what follows is kept as it is
    assert_eq!(a.entries.len(), 1);
    assert_eq!(a.end, first.len());
    assert_eq!(formula::extent(&d), Some(first.len()));
    check(&d);
    // the damage of the first entry's header: nothing at all
    let mut e = d.clone();
    e[1] ^= 1;
    assert!(lha::parse(&e).is_none());
    assert!(formula::encode(&e, 2).is_none());
    // the good ones still rebuild
    let mut ok = [first, last].concat();
    ok.push(0);
    let f = formula::encode(&ok, 2).unwrap();
    assert!(eval(&f) == ok);
}

#[test]
fn extended_header_of_64_bit_sizes() {
    let t = text(500);
    let mut sizes = Vec::new();
    sizes.extend_from_slice(&500u64.to_le_bytes());
    sizes.extend_from_slice(&500u64.to_le_bytes());
    // the base header's sizes are ignored where the extended header gives them
    let d = header2(b"-lh0-", 7, 9, lha::crc16(&t), &[(1, b"big".to_vec()), (0x42, sizes.clone())], &t);
    let a = lha::parse(&d).unwrap();
    assert_eq!((a.entries[0].size, a.entries[0].data.len()), (500, 500));
    check(&d);
    let f = formula::encode(&d, 2).unwrap();
    assert!(eval(&f) == d);
    // sizes past the file, and past usize/2^64: no entry, no allocation
    for big in [u64::MAX, 1 << 40, d.len() as u64] {
        let mut s = Vec::new();
        s.extend_from_slice(&big.to_le_bytes());
        s.extend_from_slice(&big.to_le_bytes());
        let x = header2(b"-lh5-", 5, 5, 0, &[(0x42, s)], &t);
        assert!(lha::parse(&x).is_none(), "{big}");
        assert!(formula::encode(&x, 2).is_none(), "{big}");
    }
    // a 0x42 header too short for its sizes is skipped
    let x = header2(b"-lh0-", 500, 500, 0, &[(0x42, vec![1; 8])], &t);
    assert_eq!(lha::parse(&x).unwrap().entries[0].data.len(), 500);
}

#[test]
fn directories_and_symbolic_links() {
    let t = text(120);
    let mut d = header01(0, b"-lhd-", b"dir/", 0, &[]);
    d.extend(header01(1, b"-lhd-", b"dir/sub/", 0, &[]));
    d.extend(header01(0, b"-lh0-", b"dir/f", 120, &t));
    // a link: method -lhs-, its target as the data (size = packed), unknown to the recompressor
    d.extend(header01(1, b"-lhs-", b"dir/link", 5, b"dir/f"));
    d.extend(header2(b"-lhs-", 5, 5, 0, &[(1, b"l2".to_vec())], b"dir/f"));
    d.push(0);
    let a = lha::parse(&d).unwrap();
    assert_eq!(a.entries.len(), 5);
    assert!(a.entries[0].data.is_empty() && a.entries[1].data.is_empty());
    assert_eq!(a.end, d.len());
    let f = formula::encode(&d, 2).unwrap();
    assert!(eval(&f) == d);
    // directories alone: nothing to rebuild
    let mut only = header01(0, b"-lhd-", b"x/", 0, &[]);
    only.push(0);
    assert!(lha::parse(&only).is_some());
    assert!(formula::encode(&only, 2).is_none());
    damaged(&d, 40, 140, LIMIT, check);
}

#[test]
fn lying_sizes_of_streams() {
    // a -lh5- header claiming sizes that its data cannot hold, or no data
    let mut d = header01(0, b"-lh5-", b"a", u32::MAX, &[0x55; 40]);
    d.push(0);
    check(&d);
    assert!(formula::encode(&d, 2).is_none());
    let mut z = header01(0, b"-lh5-", b"a", 1 << 28, &[]);
    z.push(0);
    check(&z);
    let mut e = header01(0, b"-lh7-", b"a", 100, &[0; 3]);
    e.push(0);
    check(&e);
}
