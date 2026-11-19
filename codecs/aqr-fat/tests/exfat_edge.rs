//! exFAT volumes made here: loops of chains and directories, floods of NoFatChain files, a
//! ValidDataLength short of the DataLength.

use aqr_disk::Plain;
use aqr_testkit::within;
use std::time::Duration;

const CLUSTER: usize = 512;
const COUNT: usize = 1000;
/// The sector of the first FAT, its sectors, the cluster heap's sector.
const FAT: usize = 24;
const HEAP: usize = 64;

/// A file set: a File entry, its Stream Extension and File Name entries.
fn set(name: &str, directory: bool, contiguous: bool, valid: u64, first: u32, length: u64) -> Vec<u8> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let names = units.len().div_ceil(15);
    let mut out = vec![0u8; 64 + 32 * names];
    out[0] = 0x85;
    out[1] = 1 + names as u8;
    out[4..6].copy_from_slice(&(if directory { 0x10u16 } else { 0x20 }).to_le_bytes());
    out[32] = 0xc0;
    out[33] = 1 | (contiguous as u8) << 1;
    out[35] = units.len() as u8;
    out[40..48].copy_from_slice(&valid.to_le_bytes());
    out[52..56].copy_from_slice(&first.to_le_bytes());
    out[56..64].copy_from_slice(&length.to_le_bytes());
    for (i, chunk) in units.chunks(15).enumerate() {
        out[64 + 32 * i] = 0xc1;
        for (k, u) in chunk.iter().enumerate() {
            out[64 + 32 * i + 2 + 2 * k..][..2].copy_from_slice(&u.to_le_bytes());
        }
    }
    out
}

/// A volume whose root directory (cluster 2, then the chain of `root`: its sets in as many
/// clusters as they take) holds `sets`, `links` the FAT's entries (cluster, next), the clusters
/// from 100 on filled with their numbers.
fn volume(sets: &[Vec<u8>], links: &[(u32, u32)]) -> Vec<u8> {
    let mut d = vec![0u8; HEAP * 512 + COUNT * CLUSTER];
    for c in 100..COUNT {
        d[HEAP * 512 + (c - 2) * CLUSTER..][..CLUSTER].fill(c as u8);
    }
    d[3..11].copy_from_slice(b"EXFAT   ");
    d[80..84].copy_from_slice(&(FAT as u32).to_le_bytes());
    d[84..88].copy_from_slice(&8u32.to_le_bytes());
    d[88..92].copy_from_slice(&(HEAP as u32).to_le_bytes());
    d[92..96].copy_from_slice(&(COUNT as u32).to_le_bytes());
    d[96..100].copy_from_slice(&2u32.to_le_bytes());
    d[108] = 9;
    d[110] = 1;
    let dir: Vec<u8> = sets.concat();
    let clusters = dir.len().div_ceil(CLUSTER).max(1);
    assert!(clusters < 90);
    d[HEAP * 512..][..dir.len()].copy_from_slice(&dir);
    let mut fat = vec![0u32; 8 * 128];
    for c in 0..clusters {
        fat[2 + c] = if c + 1 < clusters { 3 + c as u32 } else { 0xffff_ffff };
    }
    for &(c, next) in links {
        fat[c as usize] = next;
    }
    for (i, e) in fat.iter().enumerate() {
        d[FAT * 512 + 4 * i..][..4].copy_from_slice(&e.to_le_bytes());
    }
    d
}

fn contents(d: &[u8], f: &aqr_fat::File) -> Vec<u8> {
    f.extents.iter().flat_map(|r| d[r.clone()].iter().copied()).collect()
}

/// Only the first ValidDataLength bytes are the contents, of either kind of chain.
#[test]
fn valid_data_length_short() {
    let d = volume(
        &[
            set("fat", false, false, 700, 100, 3000),
            set("contiguous", false, true, 100, 105, 3000),
            set("all", false, true, 3000, 110, 3000),
            set("over", false, true, 9000, 120, 3000),
        ],
        &[(100, 101), (101, 102), (102, 103), (103, 104), (104, 105), (105, 0xffff_ffff)],
    );
    let files = aqr_fat::exfat::files(&Plain(&d));
    let get = |n: &str| files.iter().find(|f| f.name == n.as_bytes()).unwrap();
    assert!(contents(&d, get("fat")) == [vec![100u8; 512], vec![101u8; 188]].concat());
    assert!(contents(&d, get("contiguous")) == vec![105u8; 100]);
    assert_eq!(contents(&d, get("all")).len(), 3000);
    // (a valid length past the data length is cut to it)
    assert_eq!(contents(&d, get("over")).len(), 3000);
}

/// Chains looping on themselves, shared by many files, are followed no more than the clusters'
/// number allows in all.
#[test]
fn chains_loop() {
    let sets: Vec<Vec<u8>> = (0..300).map(|i| set(&format!("f{i}"), false, false, u64::MAX, 100, 1 << 40)).collect();
    let d = volume(&sets, &[(100, 101), (101, 100)]);
    let files = within(Duration::from_secs(20), || aqr_fat::exfat::files(&Plain(&d))).unwrap();
    assert_eq!(files.len(), 300);
    let bytes: usize = files.iter().flat_map(|f| &f.extents).map(|r| r.len()).sum();
    assert!(bytes <= (2 * COUNT + 1024) * CLUSTER, "{bytes}");
}

/// Directories: one of the root's own cluster, one whose chain loops, one inside another that is
/// its own parent: each read once.
#[test]
fn directories_loop() {
    let d = volume(
        &[
            set("self", true, false, 512, 2, 512),
            set("loop", true, false, 1 << 40, 100, 1 << 40),
            set("sub", true, true, 512, 101, 512),
            set("file", false, true, 10, 102, 10),
        ],
        &[(100, 100)],
    );
    // (cluster 101: a directory holding "sub" again, and the loop)
    let mut d = d;
    let inner = [set("sub", true, true, 512, 101, 512), set("loop", true, false, 512, 100, 512)].concat();
    d[HEAP * 512 + 99 * CLUSTER..][..inner.len()].copy_from_slice(&inner);
    let files = within(Duration::from_secs(20), || aqr_fat::exfat::files(&Plain(&d))).unwrap();
    assert_eq!(files.len(), 1);
}

/// Many files of NoFatChain, each of the whole volume: found, and the volume still made again.
#[test]
fn contiguous_flood() {
    let sets: Vec<Vec<u8>> =
        (0..400).map(|i| set(&format!("f{i}"), false, true, u64::MAX >> 1, 100, u64::MAX >> 1)).collect();
    let d = volume(&sets, &[]);
    let files = within(Duration::from_secs(20), || aqr_fat::exfat::files(&Plain(&d))).unwrap();
    assert_eq!(files.len(), 400);
    for f in &files {
        assert!(f.extents.iter().all(|r| r.end <= d.len()), "{:?}", f.extents);
    }
    let opt = aqr_disk::formula::Options {
        file_systems: &[aqr_fat::EXFAT],
        kind: |_| aqr_formula::Kind::Unknown,
        ..Default::default()
    };
    let f = within(Duration::from_secs(20), || aqr_disk::formula::encode_with(&d, &opt)).unwrap();
    if let Some(f) = f {
        use aqr_archive::Evaluate;
        let none = |_: &aqr_formula::Apply, _: &[&[u8]]| -> aqr_formula::Result<Vec<u8>> { unreachable!() };
        assert!(f.eval(&none).unwrap() == d);
    }
}
