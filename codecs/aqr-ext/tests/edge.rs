//! Synthetic ext volumes of loops, cross-links, huge sizes and counts: no panic, no hang, and no
//! allocation the data cannot hold.

use std::time::Duration;

use aqr_disk::Plain;
use aqr_testkit::within;

const BLOCK: usize = 1024;
const BLOCKS: usize = 256;
/// The first block of the inode table (inodes of 128 bytes, 32 of them).
const TABLE: usize = 3;
const LIMIT: Duration = Duration::from_secs(20);

fn put16(d: &mut [u8], at: usize, v: u16) {
    d[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

fn put32(d: &mut [u8], at: usize, v: u32) {
    d[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

/// An empty ext2 volume of 1 KiB blocks, one group, with the root directory (inode 2) in block 10.
fn volume() -> Vec<u8> {
    let mut d = vec![0u8; BLOCK * BLOCKS];
    let s = 1024;
    put32(&mut d, s, 32);
    put32(&mut d, s + 4, BLOCKS as u32);
    put32(&mut d, s + 20, 1);
    put32(&mut d, s + 32, 8192);
    put32(&mut d, s + 40, 32);
    put16(&mut d, s + 56, 0xef53);
    put32(&mut d, s + 76, 1);
    put16(&mut d, s + 88, 128);
    // the group descriptor, in block 2
    put32(&mut d, 2 * BLOCK + 8, TABLE as u32);
    inode(&mut d, 2, 0x41ed, BLOCK as u64, &[(0, 10, 1)]);
    dir(&mut d, 10, &[(2, "."), (2, "..")]);
    d
}

fn inode_at(n: u32) -> usize {
    TABLE * BLOCK + (n as usize - 1) * 128
}

/// Inode `n` of `mode` and `size`, its blocks as extents (logical, physical, count) in the
/// inode.
fn inode(d: &mut [u8], n: u32, mode: u16, size: u64, ext: &[(u32, u64, u16)]) {
    let at = inode_at(n);
    put16(d, at, mode);
    put32(d, at + 4, size as u32);
    put32(d, at + 108, (size >> 32) as u32);
    put32(d, at + 32, 0x80000);
    node(&mut d[at + 40..at + 100], 0, ext);
}

/// An extent tree node of `level` in `b`: leaf extents, or for the index (logical, child, _).
fn node(b: &mut [u8], level: u16, ext: &[(u32, u64, u16)]) {
    put16(b, 0, 0xf30a);
    put16(b, 2, ext.len() as u16);
    put16(b, 4, 4);
    put16(b, 6, level);
    for (k, &(logical, physical, len)) in ext.iter().enumerate() {
        let e = &mut b[12 + 12 * k..];
        put32(e, 0, logical);
        if level == 0 {
            put16(e, 4, len);
            put16(e, 6, (physical >> 32) as u16);
            put32(e, 8, physical as u32);
        } else {
            put32(e, 4, physical as u32);
            put16(e, 8, (physical >> 32) as u16);
        }
    }
}

/// Block `b` as a directory of entries (inode, name).
fn dir(d: &mut [u8], b: usize, entries: &[(u32, &str)]) {
    let blk = &mut d[b * BLOCK..][..BLOCK];
    blk.fill(0);
    let mut at = 0;
    for (k, &(ino, name)) in entries.iter().enumerate() {
        let rec = if k + 1 == entries.len() { BLOCK - at } else { (8 + name.len()).next_multiple_of(4) };
        put32(blk, at, ino);
        put16(blk, at + 4, rec as u16);
        blk[at + 6] = name.len() as u8;
        blk[at + 8..at + 8 + name.len()].copy_from_slice(name.as_bytes());
        at += rec;
    }
}

fn names(d: &[u8]) -> Vec<(String, usize)> {
    let r = within(LIMIT, || aqr_ext::files(&Plain(d))).unwrap();
    for f in &r {
        for e in &f.extents {
            assert!(e.start <= e.end);
        }
    }
    r.into_iter().map(|f| (String::from_utf8(f.name).unwrap(), f.extents.iter().map(|e| e.len()).sum())).collect()
}

#[test]
fn plain_file() {
    let mut d = volume();
    dir(&mut d, 10, &[(2, "."), (2, ".."), (12, "a")]);
    inode(&mut d, 12, 0x81a4, 1500, &[(0, 20, 2)]);
    assert_eq!(names(&d), [("a".to_string(), 1500)]);
}

/// An extent tree whose index points at its own node, and one of two nodes pointing at each other:
/// the walk ends, the file keeps what is read.
#[test]
fn extent_loops() {
    for pair in [false, true] {
        let mut d = volume();
        dir(&mut d, 10, &[(2, "."), (2, ".."), (12, "a")]);
        inode(&mut d, 12, 0x81a4, 1 << 30, &[]);
        let root = &mut d[inode_at(12) + 40..][..60];
        node(root, 1, &[(0, 30, 0), (1, 30, 0), (2, 30, 0), (3, 30, 0)]);
        let n = [(0, 30u64, 0u16), (10, if pair { 31 } else { 30 }, 0), (20, 30, 0), (30, 31, 0)];
        node(&mut d[30 * BLOCK..][..BLOCK], 1, &n);
        node(&mut d[31 * BLOCK..][..BLOCK], 1, &[(0, 30, 0), (5, 31, 0), (9, 30, 0)]);
        let _ = names(&d);
    }
}

/// Indirect blocks (single, double, triple) that point at themselves and at each other.
#[test]
fn indirect_loops() {
    let mut d = volume();
    dir(&mut d, 10, &[(2, "."), (2, ".."), (12, "a")]);
    let at = inode_at(12);
    put16(&mut d, at, 0x81a4);
    put32(&mut d, at + 4, u32::MAX);
    put32(&mut d, at + 108, 0xffff);
    for (k, b) in [(12, 30u32), (13, 31), (14, 32)] {
        put32(&mut d, at + 40 + 4 * k, b);
    }
    for b in 30..33usize {
        for k in 0..BLOCK / 4 {
            put32(&mut d, b * BLOCK + 4 * k, 30 + ((b + k) % 3) as u32);
        }
    }
    let _ = names(&d);
}

/// Two files with the same blocks, and a directory block of an entry for an ancestor, itself and
/// the files: each file once, as many as the blocks hold.
#[test]
fn cross_links() {
    let mut d = volume();
    dir(&mut d, 10, &[(2, "."), (2, ".."), (12, "a"), (13, "b"), (14, "sub"), (2, "up")]);
    inode(&mut d, 12, 0x81a4, 3000, &[(0, 20, 3)]);
    inode(&mut d, 13, 0x81a4, 3000, &[(0, 20, 3)]);
    // a directory whose only block is the root's
    inode(&mut d, 14, 0x41ed, BLOCK as u64, &[(0, 10, 1)]);
    let got = names(&d);
    assert!(got.contains(&("a".to_string(), 3000)) && got.contains(&("b".to_string(), 3000)));
    assert!(got.len() <= 8, "{got:?}");
}

/// Sizes of `i_size_high` far beyond the volume, and extents beyond it: the file holds only what
/// the blocks give.
#[test]
fn huge_sizes() {
    let mut d = volume();
    dir(&mut d, 10, &[(2, "."), (2, ".."), (12, "a"), (13, "b"), (14, "c")]);
    inode(&mut d, 12, 0x81a4, 1 << 40 | 5, &[(0, 20, 2), (1 << 20, 40, 1)]);
    inode(&mut d, 13, 0x81a4, u64::MAX, &[(0, 20, 2)]);
    // an extent of the most blocks the format allows, at a physical block of 2^48, and one at
    // the last logical blocks
    inode(&mut d, 14, 0x81a4, 1 << 33, &[(0, (1 << 48) - 1, 32768), (u32::MAX - 1, 20, 1)]);
    let got = names(&d);
    let size = |n: &str| got.iter().find(|g| g.0 == n).map(|g| g.1);
    assert_eq!(size("a"), Some(2 * BLOCK + BLOCK));
    assert_eq!(size("b"), Some(2 * BLOCK));
    // (the extent past the volume stays a range of the image's; whoever takes it checks it)
    assert!(size("c").is_some());
}

/// A directory of a size beyond 4 GiB by `i_size_high`.
#[test]
fn huge_directory() {
    let mut d = volume();
    dir(&mut d, 10, &[(2, "."), (2, ".."), (12, "a")]);
    inode(&mut d, 12, 0x81a4, 10, &[(0, 20, 1)]);
    inode(&mut d, 2, 0x41ed, 1 << 40, &[(0, 10, 1), (1, 10, 32768), (40000, 10, 32768)]);
    // (extents overlapping on the directory's block use up the volume's budget of blocks: the
    // file may come out without its blocks, never with more than its size)
    let got = names(&d);
    assert!(matches!(&got[..], [(n, 0 | 10)] if n == "a"), "{got:?}");
}

/// Counts of blocks and groups the image cannot hold: nothing hangs or allocates by them.
#[test]
fn huge_counts() {
    let base = volume();
    let cases: [(&str, Vec<(usize, u32)>); 5] = [
        ("blocks", vec![(1024 + 4, u32::MAX)]),
        ("blocks per group", vec![(1024 + 4, u32::MAX), (1024 + 32, 1)]),
        ("wide", vec![(1024 + 96, 0x80), (1024 + 0x150, u32::MAX), (1024 + 4, u32::MAX), (1024 + 32, 8)]),
        ("inodes", vec![(1024, u32::MAX), (1024 + 40, u32::MAX)]),
        ("inodes per group", vec![(1024 + 40, 1 << 31)]),
    ];
    for (name, patch) in cases {
        let mut d = base.clone();
        for (at, v) in patch {
            put32(&mut d, at, v);
        }
        within(LIMIT, || {
            let _ = aqr_ext::starts(&Plain(&d));
            let _ = aqr_ext::inode_tables(&Plain(&d));
            let _ = aqr_ext::files(&Plain(&d));
        })
        .unwrap_or_else(|e| panic!("{name}: {e}"));
    }
}

/// Every way of damaging the synthetic volume.
#[test]
fn damaged_synthetic() {
    let mut d = volume();
    dir(&mut d, 10, &[(2, "."), (2, ".."), (12, "a"), (14, "sub")]);
    inode(&mut d, 12, 0x81a4, 1500, &[(0, 20, 2)]);
    inode(&mut d, 14, 0x41ed, BLOCK as u64, &[(0, 11, 1)]);
    dir(&mut d, 11, &[(14, "."), (2, ".."), (12, "again")]);
    aqr_testkit::damaged(&d, 300, 1200, LIMIT, |v| {
        let _ = aqr_ext::files(&Plain(v));
    });
}
