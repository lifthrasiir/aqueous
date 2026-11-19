//! Synthetic HFS+ volumes of loops, cycles, wrapped volumes and impossible counts: no panic, no
//! hang, and no allocation the data cannot hold.

use std::time::Duration;

use aqr_disk::Plain;
use aqr_testkit::within;

const BS: usize = 4096;
const BLOCKS: usize = 40;
/// The catalog: blocks 1..5 (4 nodes of a block); the extents overflow file: blocks 5..7.
const CATALOG: usize = 1;
const OVERFLOW: usize = 5;
const LIMIT: Duration = Duration::from_secs(20);

fn put16(d: &mut [u8], at: usize, v: u16) {
    d[at..at + 2].copy_from_slice(&v.to_be_bytes());
}

fn put32(d: &mut [u8], at: usize, v: u32) {
    d[at..at + 4].copy_from_slice(&v.to_be_bytes());
}

fn put64(d: &mut [u8], at: usize, v: u64) {
    d[at..at + 8].copy_from_slice(&v.to_be_bytes());
}

/// A fork's data at `at`: size and the first extents.
fn fork(d: &mut [u8], at: usize, size: u64, ext: &[(u32, u32)]) {
    put64(d, at, size);
    for (i, &(s, c)) in ext.iter().enumerate() {
        put32(d, at + 16 + 8 * i, s);
        put32(d, at + 20 + 8 * i, c);
    }
}

/// A B-tree header node in node `n` of the file at block `at` (nodes of a block), `nodes` nodes
/// of it, the first leaf `first`.
fn header(d: &mut [u8], at: usize, nodes: u32, first: u32) {
    let b = &mut d[at * BS..][..BS];
    b[8] = 1;
    put16(b, 10, 3);
    put32(b, 24, first);
    put16(b, 32, BS as u16);
    put32(b, 36, nodes);
}

/// Leaf node `n` of the file at block `at`, of `records`, its forward link `next`.
fn leaf(d: &mut [u8], at: usize, n: usize, next: u32, records: &[Vec<u8>]) {
    let b = &mut d[(at + n) * BS..][..BS];
    b.fill(0);
    put32(b, 0, next);
    b[8] = 0xff;
    b[9] = 1;
    put16(b, 10, records.len() as u16);
    let mut off = 14;
    for (i, r) in records.iter().enumerate() {
        b[off..off + r.len()].copy_from_slice(r);
        put16(b, BS - 2 * (i + 1), off as u16);
        off += r.len();
    }
    put16(b, BS - 2 * (records.len() + 1), off as u16);
}

/// The catalog key of `name` in folder `parent`.
fn key(parent: u32, name: &str) -> Vec<u8> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let mut k = vec![0u8; 8 + 2 * units.len()];
    put16(&mut k, 0, 6 + 2 * units.len() as u16);
    put32(&mut k, 2, parent);
    put16(&mut k, 6, units.len() as u16);
    for (i, u) in units.iter().enumerate() {
        put16(&mut k, 8 + 2 * i, *u);
    }
    k
}

fn folder(parent: u32, name: &str, id: u32) -> Vec<u8> {
    let mut r = key(parent, name);
    if r.len() % 2 == 1 {
        r.push(0);
    }
    let mut d = vec![0u8; 88];
    put16(&mut d, 0, 1);
    put32(&mut d, 8, id);
    r.extend(d);
    r
}

fn file(parent: u32, name: &str, id: u32, size: u64, ext: &[(u32, u32)]) -> Vec<u8> {
    let mut r = key(parent, name);
    let mut d = vec![0u8; 248];
    put16(&mut d, 0, 2);
    put32(&mut d, 8, id);
    put16(&mut d, 42, 0o100644);
    fork(&mut d, 88, size, ext);
    r.extend(d);
    r
}

/// An overflow record of the data fork of file `id` from block `first`: 8 more extents.
fn overflow(id: u32, first: u32, ext: &[(u32, u32)]) -> Vec<u8> {
    let mut r = vec![0u8; 12 + 64];
    put16(&mut r, 0, 10);
    put32(&mut r, 4, id);
    put32(&mut r, 8, first);
    for (i, &(s, c)) in ext.iter().enumerate() {
        put32(&mut r, 12 + 8 * i, s);
        put32(&mut r, 16 + 8 * i, c);
    }
    r
}

/// A volume of 4 KiB blocks whose catalog (in four nodes: the header, leaves 1 to 3) holds
/// `records` in leaf 1.
fn volume(records: &[Vec<u8>]) -> Vec<u8> {
    let mut d = vec![0u8; BS * BLOCKS];
    let h = 1024;
    d[h..h + 2].copy_from_slice(b"H+");
    put16(&mut d, h + 2, 4);
    put32(&mut d, h + 40, BS as u32);
    put32(&mut d, h + 44, BLOCKS as u32);
    fork(&mut d, h + 192, 2 * BS as u64, &[(OVERFLOW as u32, 2)]);
    fork(&mut d, h + 272, 4 * BS as u64, &[(CATALOG as u32, 4)]);
    header(&mut d, CATALOG, 4, 1);
    leaf(&mut d, CATALOG, 1, 0, records);
    header(&mut d, OVERFLOW, 2, 1);
    leaf(&mut d, OVERFLOW, 1, 0, &[]);
    d
}

fn names(d: &[u8]) -> Vec<(String, usize)> {
    let r = within(LIMIT, || aqr_hfs::files(&Plain(d))).unwrap();
    r.into_iter().map(|f| (String::from_utf8(f.name).unwrap(), f.extents.iter().map(|e| e.len()).sum())).collect()
}

fn root() -> Vec<u8> {
    folder(1, "vol", 2)
}

#[test]
fn plain_file() {
    let d = volume(&[root(), file(2, "a", 16, 5000, &[(10, 2)])]);
    assert!(aqr_hfs::starts(&Plain(&d)));
    assert_eq!(names(&d), [("a".to_string(), 5000)]);
}

/// Leaves whose forward links make a cycle, and one that links to itself.
#[test]
fn leaf_cycles() {
    for (first, second) in [(1, 1), (2, 1), (2, 3)] {
        let mut d = volume(&[root(), file(2, "a", 16, 100, &[(10, 1)])]);
        leaf(&mut d, CATALOG, 1, first, &[root(), file(2, "a", 16, 100, &[(10, 1)])]);
        leaf(&mut d, CATALOG, 2, second, &[file(2, "b", 17, 100, &[(10, 1)])]);
        leaf(&mut d, CATALOG, 3, 1, &[file(2, "c", 18, 100, &[(10, 1)])]);
        let got = names(&d);
        assert!(got.iter().any(|g| g.0 == "a"), "{got:?}");
    }
}

/// Folders that are their own parent or parents of each other, off the root and on it.
#[test]
fn folder_cycles() {
    let d = volume(&[
        root(),
        file(2, "a", 16, 100, &[(10, 1)]),
        folder(2, "x", 20),
        folder(20, "y", 21),
        folder(21, "x", 20),
        folder(22, "self", 22),
        folder(30, "p", 31),
        folder(31, "q", 30),
        file(31, "deep", 40, 100, &[(10, 1)]),
        file(22, "inself", 41, 100, &[(10, 1)]),
        // a folder of the root's own id, below another
        folder(20, "again", 2),
    ]);
    let got = names(&d);
    assert!(got.iter().any(|g| g.0 == "a"), "{got:?}");
    assert!(got.len() <= 3, "{got:?}");
}

/// A file's extents overflow chain: a long one is followed, one that points where it came from
/// and records of the overflow file's own tree in a cycle are not looped on.
#[test]
fn overflow_chains() {
    let first: Vec<(u32, u32)> = (10..18).map(|b| (b, 1)).collect();
    let second: Vec<(u32, u32)> = (18..26).map(|b| (b, 1)).collect();
    let mut d = volume(&[root(), file(2, "a", 16, 20 * BS as u64, &first), file(2, "huge", 17, u64::MAX, &first)]);
    // a: 8 extents, then 8 from block 8, then 8 from block 16 (only four are needed)
    let rec = [overflow(16, 8, &second), overflow(16, 16, &first), overflow(17, 8, &first), overflow(17, 16, &first)];
    leaf(&mut d, OVERFLOW, 1, 1, &rec);
    let got = names(&d);
    assert_eq!(got.iter().find(|g| g.0 == "a").map(|g| g.1), Some(20 * BS));
    assert!(got.iter().any(|g| g.0 == "huge"));
}

/// The catalog's own extents continued by an overflow record that covers the catalog itself.
#[test]
fn catalog_overflow_loop() {
    let mut d = volume(&[root(), file(2, "a", 16, 100, &[(10, 1)])]);
    let h = 1024;
    // the catalog's fork: 4 blocks, then more (record of the catalog, id 4, from block 4, whose
    // extents are the catalog's own blocks again)
    fork(&mut d, h + 272, 8 * BS as u64, &[(CATALOG as u32, 4)]);
    let own: Vec<(u32, u32)> = vec![(CATALOG as u32, 4)];
    leaf(&mut d, OVERFLOW, 1, 0, &[overflow(4, 4, &own), overflow(4, 8, &own)]);
    let got = names(&d);
    assert!(got.len() <= 1, "{got:?}");
}

/// A wrapped volume (HFS "BD" with the HFS+ volume embedded) is not read, and says so.
#[test]
fn wrapped() {
    let inner = volume(&[root(), file(2, "a", 16, 100, &[(10, 1)])]);
    let mut d = vec![0u8; 8192];
    d.extend_from_slice(&inner);
    d[1024..1026].copy_from_slice(b"BD");
    // drAlBlkSiz, drAlBlSt, and the embedded signature "H+" with its extent
    d[1024 + 20..1024 + 24].copy_from_slice(&512u32.to_be_bytes());
    d[1024 + 124..1024 + 126].copy_from_slice(b"H+");
    assert!(!aqr_hfs::starts(&Plain(&d)));
    assert!(names(&d).is_empty());
}

/// Counts and sizes the image cannot hold.
#[test]
fn huge_counts() {
    let base = volume(&[root(), file(2, "a", 16, 100, &[(10, 1)])]);
    let h = 1024;
    let cases: [(&str, Vec<(usize, u32)>); 6] = [
        ("blocks", vec![(h + 44, u32::MAX)]),
        ("block size", vec![(h + 40, 1 << 31)]),
        ("catalog size", vec![(h + 272 + 4, u32::MAX), (h + 272, u32::MAX)]),
        ("catalog extent", vec![(h + 272 + 16 + 4, u32::MAX)]),
        ("nodes", vec![(CATALOG * BS + 36, u32::MAX)]),
        ("file", vec![(CATALOG * BS + 14 + 102 + 10 + 88, u32::MAX)]),
    ];
    for (name, patch) in cases {
        let mut d = base.clone();
        for (at, v) in patch {
            put32(&mut d, at, v);
        }
        within(LIMIT, || {
            let _ = aqr_hfs::starts(&Plain(&d));
            let _ = aqr_hfs::files(&Plain(&d));
        })
        .unwrap_or_else(|e| panic!("{name}: {e}"));
    }
}

/// Every way of damaging the synthetic volume.
#[test]
fn damaged_synthetic() {
    let first: Vec<(u32, u32)> = (10..18).map(|b| (b, 1)).collect();
    let mut d = volume(&[root(), folder(2, "sub", 20), file(20, "a", 16, 12 * BS as u64, &first)]);
    leaf(&mut d, OVERFLOW, 1, 0, &[overflow(16, 8, &first)]);
    aqr_testkit::damaged(&d[..OVERFLOW * BS + 400], 200, 1600, LIMIT, |v| {
        let _ = aqr_hfs::files(&Plain(v));
    });
}
