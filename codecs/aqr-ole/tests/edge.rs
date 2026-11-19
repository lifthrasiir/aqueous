//! Edge cases: version 4's sectors of 4096 bytes, a DIFAT of its own, directories that loop,
//! many siblings and deep storages, damaged files.

use std::time::Duration;

use aqr_disk::Plain;
use aqr_ole::{File, files, starts};
use aqr_testkit::{Refusing, case, damaged, most, within};

#[global_allocator]
static ALLOC: Refusing = Refusing(256 << 20);

const LIMIT: Duration = Duration::from_secs(60);
const NONE: u32 = 0xffff_ffff;

/// A directory entry to write: its links are indexes of the entries (`NONE` for none); `data`
/// the contents of a stream, laid out in the sectors.
struct Ent {
    name: String,
    kind: u8,
    left: u32,
    right: u32,
    child: u32,
    data: Vec<u8>,
}

fn stream(name: &str, data: Vec<u8>) -> Ent {
    Ent { name: name.into(), kind: 2, left: NONE, right: NONE, child: NONE, data }
}

fn storage(name: &str, child: u32) -> Ent {
    Ent { name: name.into(), kind: 1, left: NONE, right: NONE, child, data: Vec::new() }
}

fn root(child: u32) -> Ent {
    Ent { name: "Root Entry".into(), kind: 5, left: NONE, right: NONE, child, data: Vec::new() }
}

fn pattern(n: usize, k: u8) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8 ^ k).collect()
}

/// A compound file of `ents` (the first the root), of version `v` (3 or 4): FAT sectors, then
/// the DIFAT's, the directory's, and the streams' (each from the first sector it needs, one
/// after another). Returns the file and each stream's first sector.
fn build(v: u16, ents: &[Ent]) -> Vec<u8> {
    let sector = if v == 3 { 512 } else { 4096 };
    let per = sector / 4;
    let dir_n = (ents.len() * 128).div_ceil(sector);
    let data_n: usize = ents.iter().map(|e| e.data.len().div_ceil(sector)).sum();
    let (mut f, mut dif) = (1, 0);
    loop {
        let total = f + dif + dir_n + data_n;
        let nf = total.div_ceil(per);
        let nd = nf.saturating_sub(109).div_ceil(per - 1);
        if (nf, nd) == (f, dif) {
            break;
        }
        (f, dif) = (nf, nd);
    }
    let total = f + dif + dir_n + data_n;
    let mut fat = vec![NONE; f * per];
    let dir_at = f + dif;
    fat[..f].fill(0xffff_fffd);
    for i in 0..dif {
        fat[f + i] = 0xffff_fffc;
    }
    let chain = |fat: &mut Vec<u32>, from: usize, n: usize| {
        for i in 0..n {
            fat[from + i] = if i + 1 < n { (from + i + 1) as u32 } else { 0xffff_fffe };
        }
    };
    chain(&mut fat, dir_at, dir_n);
    let mut at = dir_at + dir_n;
    let mut starts = Vec::new();
    for e in ents {
        let n = e.data.len().div_ceil(sector);
        starts.push(if n == 0 { 0xffff_fffe } else { at as u32 });
        chain(&mut fat, at, n);
        at += n;
    }
    assert_eq!(at, total);
    let mut d = vec![0u8; sector];
    d[..8].copy_from_slice(&[0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1]);
    d[24..26].copy_from_slice(&0x3eu16.to_le_bytes());
    d[26..28].copy_from_slice(&v.to_le_bytes());
    d[28..30].copy_from_slice(&0xfffeu16.to_le_bytes());
    d[30..32].copy_from_slice(&(if v == 3 { 9u16 } else { 12 }).to_le_bytes());
    d[32..34].copy_from_slice(&6u16.to_le_bytes());
    d[44..48].copy_from_slice(&(f as u32).to_le_bytes());
    d[48..52].copy_from_slice(&(dir_at as u32).to_le_bytes());
    d[56..60].copy_from_slice(&4096u32.to_le_bytes());
    d[60..64].copy_from_slice(&0xffff_fffeu32.to_le_bytes());
    d[68..72].copy_from_slice(&(if dif > 0 { f as u32 } else { 0xffff_fffe }).to_le_bytes());
    d[72..76].copy_from_slice(&(dif as u32).to_le_bytes());
    for i in 0..109 {
        let n = if i < f { i as u32 } else { NONE };
        d[76 + 4 * i..80 + 4 * i].copy_from_slice(&n.to_le_bytes());
    }
    d.extend(fat.iter().flat_map(|v| v.to_le_bytes()));
    for k in 0..dif {
        let mut s = vec![0xffu8; sector];
        for j in 0..per - 1 {
            let i = 109 + k * (per - 1) + j;
            if i < f {
                s[4 * j..4 * j + 4].copy_from_slice(&(i as u32).to_le_bytes());
            }
        }
        let next = if k + 1 < dif { (f + k + 1) as u32 } else { 0xffff_fffe };
        s[sector - 4..].copy_from_slice(&next.to_le_bytes());
        d.extend(s);
    }
    let mut dir = vec![0u8; dir_n * sector];
    for (i, e) in ents.iter().enumerate() {
        let b = &mut dir[128 * i..128 * i + 128];
        let units: Vec<u16> = e.name.encode_utf16().collect();
        for (j, u) in units.iter().enumerate() {
            b[2 * j..2 * j + 2].copy_from_slice(&u.to_le_bytes());
        }
        b[64..66].copy_from_slice(&(2 * units.len() as u16 + 2).to_le_bytes());
        b[66] = e.kind;
        b[68..72].copy_from_slice(&e.left.to_le_bytes());
        b[72..76].copy_from_slice(&e.right.to_le_bytes());
        b[76..80].copy_from_slice(&e.child.to_le_bytes());
        b[116..120].copy_from_slice(&starts[i].to_le_bytes());
        b[120..128].copy_from_slice(&(e.data.len() as u64).to_le_bytes());
    }
    d.extend(dir);
    for e in ents {
        d.extend_from_slice(&e.data);
        d.resize(d.len().next_multiple_of(sector), 0);
    }
    d
}

fn gather(d: &[u8], f: &File) -> Vec<u8> {
    f.extents.iter().flat_map(|r| d[r.clone()].iter().copied()).collect()
}

fn of(d: &[u8]) -> Vec<File> {
    case("ole");
    let r = files(&Plain(d));
    assert!(most() < 256 << 20);
    r
}

/// Version 4: sectors of 4096 bytes (the header's own sector too).
#[test]
fn version_4() {
    let (a, b) = (pattern(10_000, 1), pattern(4096, 2));
    let mut s = stream("A", a.clone());
    s.right = 2;
    let d = build(4, &[root(1), s, stream("B", b.clone())]);
    assert!(starts(&Plain(&d)));
    let fs = of(&d);
    assert_eq!(fs.len(), 2);
    assert_eq!(gather(&d, &fs[0]), a);
    assert_eq!(gather(&d, &fs[1]), b);
    assert_eq!(fs[0].extents.len(), 1);
    assert_eq!(fs[0].extents[0], 3 * 4096..3 * 4096 + 10_000);
}

/// More than 109 FAT sectors: the rest listed by DIFAT sectors chained from the header's.
#[test]
fn real_difat() {
    for sectors in [14_000usize, 31_000] {
        let (a, b) = (pattern(sectors * 512 - 100, 3), pattern(8000, 4));
        let mut s = stream("A", a.clone());
        s.right = 2;
        let d = build(3, &[root(1), s, stream("B", b.clone())]);
        let difat = u32::from_le_bytes(d[72..76].try_into().unwrap());
        assert!(difat >= 1, "{sectors}: {difat} DIFAT sectors");
        let fs = of(&d);
        assert_eq!(fs.len(), 2, "{sectors}");
        assert_eq!(gather(&d, &fs[0]), a);
        assert_eq!(gather(&d, &fs[1]), b);
    }
}

/// Entries linking to themselves, their ancestors and each other: every stream found once,
/// no hang.
#[test]
fn directory_cycles() {
    let data = |k| pattern(5000, k);
    let cases: Vec<Vec<Ent>> = {
        let mut out = Vec::new();
        // a stream its own left and right
        let mut a = stream("A", data(1));
        (a.left, a.right) = (1, 1);
        out.push(vec![root(1), a]);
        // two siblings pointing at each other
        let (mut a, mut b) = (stream("A", data(1)), stream("B", data(2)));
        (a.right, b.right, b.left) = (2, 1, 1);
        out.push(vec![root(1), a, b]);
        // a storage whose child is the root, and one whose child is itself
        let mut s = storage("S", 0);
        s.right = 2;
        out.push(vec![root(1), s, stream("A", data(3))]);
        let mut s = storage("S", 1);
        s.right = 2;
        out.push(vec![root(1), s, stream("A", data(3))]);
        // the root's child the root
        out.push(vec![root(0), stream("A", data(4))]);
        // links past the directory
        let mut a = stream("A", data(5));
        (a.left, a.right, a.child) = (99, 0xffff_fffe, 1 << 31);
        out.push(vec![root(1), a]);
        out
    };
    for (k, ents) in cases.into_iter().enumerate() {
        let d = build(3, &ents);
        let found = within(LIMIT, || of(&d)).unwrap();
        let mut names: Vec<_> = found.iter().map(|f| f.name.clone()).collect();
        let n = names.len();
        names.dedup();
        assert_eq!(names.len(), n, "case {k}");
        assert!(found.len() <= ents.len(), "case {k}");
    }
}

/// A storage of many streams as a list (each the right of the one before, as a writer that
/// does not balance makes it): every stream is found, past 256.
#[test]
fn many_siblings() {
    for n in [200usize, 300, 1000] {
        let mut ents = vec![root(1)];
        for i in 0..n {
            let mut s = stream(&format!("s{i}"), pattern(4096, i as u8));
            s.right = if i + 1 < n { i as u32 + 2 } else { NONE };
            ents.push(s);
        }
        let d = build(3, &ents);
        let fs = of(&d);
        assert_eq!(fs.len(), n, "{n} siblings");
        // (and as a list of lefts)
        for e in &mut ents[1..] {
            (e.left, e.right) = (e.right, NONE);
        }
        let d = build(3, &ents);
        assert_eq!(of(&d).len(), n, "{n} siblings on the left");
    }
}

/// Storages nested deeper than the reader goes: those within are read, nothing else is harmed.
#[test]
fn deep_storages() {
    for depth in [10usize, 255, 300, 3000] {
        let mut ents = vec![root(1)];
        for i in 0..depth {
            ents.push(storage(&format!("d{i}"), i as u32 + 2));
        }
        ents.push(stream("leaf", pattern(5000, 9)));
        let d = build(3, &ents);
        let fs = within(LIMIT, || of(&d)).unwrap();
        assert_eq!(fs.len(), (depth < 256) as usize, "depth {depth}");
        if let Some(f) = fs.first() {
            assert!(f.name.starts_with(b"d0/d1/") && f.name.ends_with(b"/leaf"));
        }
    }
}

#[test]
fn damaged_files() {
    let mut s = stream("A", pattern(5000, 1));
    s.right = 2;
    let d = build(3, &[root(1), s, stream("B", pattern(4100, 2))]);
    damaged(&d, 100, 600, LIMIT, |d| {
        of(d);
    });
    let mut s = stream("A", pattern(5000, 1));
    s.right = 2;
    let d = build(4, &[root(1), s, stream("B", pattern(4100, 2))]);
    damaged(&d, 100, 600, LIMIT, |d| {
        of(d);
    });
}
