//! Tests of `disk::fat`: the free bytes of FAT12/16/32 and exFAT volumes, zeroed in place.

use super::*;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A temporary directory of its own, removed at the end.
struct Dir(PathBuf);

impl Dir {
    fn new() -> Dir {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("aqr-lossy-fat-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        Dir(p)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fixture(name: &str) -> Vec<u8> {
    let p =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../codecs/aqr-fat/tests/data").join(format!("{name}.img.zst"));
    zstd::decode_all(std::fs::read(p).unwrap().as_slice()).unwrap()
}

fn zeros(d: &[u8]) -> usize {
    d.iter().filter(|&&b| b == 0).count()
}

fn rewritten(x: &[u8]) -> Option<(Rewrite, Option<Vec<u8>>)> {
    rewrite(x, &parse_any("slack").unwrap(), &Options::default())
}

/// `x` with its free bytes filled with a pattern, as the stale data of deleted files.
fn stale(x: &[u8]) -> Vec<u8> {
    let mut y = x.to_vec();
    for r in disk::free(x).unwrap() {
        for (i, b) in y[r.clone()].iter_mut().enumerate() {
            *b = 0xa5 ^ (i as u8).wrapping_mul(7) | 1;
        }
    }
    y
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).stdin(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The device of `img` attached as a raw disk, without mounting.
fn attach_raw(img: &Path) -> Option<String> {
    let out = run("hdiutil", &["attach", "-imagekey", "diskimage-class=CRawDiskImage", "-nomount", img.to_str()?])?;
    out.split_whitespace().next().map(str::to_string)
}

/// Whether `fsck_msdos -n` (`fsck_exfat`) finds the volume of `d` clean; None where the tools are missing.
fn fsck(dir: &Dir, d: &[u8]) -> Option<bool> {
    let img = dir.0.join("fsck.img");
    std::fs::write(&img, d).unwrap();
    let dev = attach_raw(&img)?;
    let ok = Command::new(if d[3..11] == *b"EXFAT   " { "fsck_exfat" } else { "fsck_msdos" })
        .args(["-n", &dev])
        .stdin(Stdio::null())
        .output()
        .ok()
        .map(|o| {
            if !o.status.success() {
                eprintln!("{}", String::from_utf8_lossy(&o.stdout));
            }
            o.status.success()
        });
    run("hdiutil", &["detach", &dev]);
    let _ = std::fs::remove_file(img);
    ok
}

/// A FAT volume of `width` (12, 16, 32) of `size` megabytes made with the system's tools: files
/// and directories written, half of them deleted again.
fn make(dir: &Dir, width: u32, size: u32) -> Option<Vec<u8>> {
    let img = dir.0.join(format!("make{width}.img"));
    let mnt = dir.0.join(format!("mnt{width}"));
    std::fs::create_dir_all(&mnt).unwrap();
    let f = std::fs::File::create(&img).unwrap();
    f.set_len(size as u64 * 1024 * 1024).unwrap();
    drop(f);
    let Some(dev) = attach_raw(&img) else {
        eprintln!("no hdiutil: skipped");
        return None;
    };
    let formatted = run("newfs_msdos", &["-F", &width.to_string(), "-v", "T", &dev]);
    run("hdiutil", &["detach", &dev]);
    formatted?;
    run(
        "hdiutil",
        &[
            "attach",
            "-imagekey",
            "diskimage-class=CRawDiskImage",
            "-mountpoint",
            mnt.to_str().unwrap(),
            img.to_str().unwrap(),
        ],
    )?;
    let content = |n: usize, k: u8| -> Vec<u8> { (0..n).map(|i| (i as u8).wrapping_mul(31) ^ k).collect() };
    for d in ["Docs", "Docs/Sub Dir With A Long Name", "pics"] {
        std::fs::create_dir_all(mnt.join(d)).unwrap();
    }
    for i in 0..40u32 {
        let sizes = [0usize, 1, 511, 512, 4000, 33_333, 70_000, 200_000];
        let n = sizes[i as usize % sizes.len()] + (i as usize * 13) % 300;
        let place = ["", "Docs/", "Docs/Sub Dir With A Long Name/", "pics/"][i as usize % 4];
        std::fs::write(mnt.join(format!("{place}file number {i}.bin")), content(n, i as u8)).unwrap();
    }
    for i in (0..40u32).filter(|i| i % 3 == 1) {
        let place = ["", "Docs/", "Docs/Sub Dir With A Long Name/", "pics/"][i as usize % 4];
        std::fs::remove_file(mnt.join(format!("{place}file number {i}.bin"))).unwrap();
    }
    std::fs::write(mnt.join("big.bin"), content(900_000, 9)).unwrap();
    std::fs::remove_file(mnt.join("big.bin")).unwrap();
    std::fs::write(mnt.join("kept.bin"), content(100_000, 5)).unwrap();
    let detached = run("hdiutil", &["detach", mnt.to_str().unwrap()]);
    detached?;
    Some(std::fs::read(img).unwrap())
}

/// The checks of an X that has free bytes with stale data: made, checked, the files the same,
/// more zeros; the zeros as many as the stale bytes; where fsck is.
fn roundtrip(dir: &Dir, x: &[u8], expect_free: bool) -> Vec<u8> {
    let (w, r) = rewritten(x).expect("a rewriter takes it");
    assert!(matches!(w, Rewrite::Made { .. }), "{w:?}");
    let r = r.unwrap();
    check::disk(x, &r).unwrap();
    let fss = disk::file_systems();
    let (a, b) = (aqr_disk::files(x, fss), aqr_disk::files(&r, fss));
    assert!(!a.is_empty());
    assert_eq!(a.len(), b.len());
    for (f, g) in a.iter().zip(&b) {
        assert_eq!(f.name, g.name);
        let (p, q): (Vec<u8>, Vec<u8>) = (
            f.extents.iter().flat_map(|e| x[e.clone()].iter().copied()).collect(),
            g.extents.iter().flat_map(|e| r[e.clone()].iter().copied()).collect(),
        );
        assert_eq!(p, q);
    }
    let free: usize = disk::free(x).unwrap().iter().map(|r| r.len()).sum();
    assert!(!expect_free || free > 0);
    assert!(zeros(&r) > zeros(x));
    eprintln!("{} bytes free; zeros {} -> {}", free, zeros(x), zeros(&r));
    // (zeroed again: nothing more to do)
    assert!(rewritten(&r).is_none_or(|(w, r2)| r2.is_none() && !matches!(w, Rewrite::Failed { .. })));
    match fsck(dir, &r) {
        Some(ok) => assert!(ok, "fsck_msdos finds R unclean"),
        None => eprintln!("no fsck_msdos or hdiutil: skipped"),
    }
    r
}

#[test]
fn made_volumes() {
    let dir = Dir::new();
    for (width, size) in [(12, 4), (16, 16), (32, 64)] {
        let Some(x) = make(&dir, width, size) else { return };
        eprintln!("FAT{width}:");
        roundtrip(&dir, &x, true);
    }
}

#[test]
fn fixtures_with_stale_data() {
    let dir = Dir::new();
    for name in ["fat12", "fat16", "fat32", "exfat"] {
        let x = fixture(name);
        assert!(!disk::free(&x).unwrap().is_empty(), "{name}");
        let y = stale(&x);
        assert!(zeros(&y) < zeros(&x));
        eprintln!("{name}:");
        let r = roundtrip(&dir, &y, true);
        // (the stale bytes are gone, and more where X had other bytes in its free ones)
        assert!(zeros(&r) >= zeros(&x), "{name}");
    }
}

/// The BPB's geometry of FAT12/16 volume `d`: the FATs' start, count and size in bytes, the root's start.
fn geometry(d: &[u8]) -> (usize, usize, usize, usize) {
    let bps = u16::from_le_bytes([d[11], d[12]]) as usize;
    let reserved = u16::from_le_bytes([d[14], d[15]]) as usize;
    let fats = d[16] as usize;
    let fat_sectors = u16::from_le_bytes([d[22], d[23]]) as usize;
    (reserved * bps, fats, fat_sectors * bps, (reserved + fats * fat_sectors) * bps)
}

fn refused(d: &[u8], what: &str) {
    let e = disk::free(d).expect_err(what);
    eprintln!("{what}: {e}");
    assert!(rewritten(d).is_none_or(|(w, r)| r.is_none() && matches!(w, Rewrite::Kept { .. })), "{what}");
}

#[test]
fn fats_differing() {
    let mut d = fixture("fat16");
    let (fat, fats, len, _) = geometry(&d);
    assert!(fats >= 2);
    d[fat + len + 100] ^= 0x55;
    refused(&d, "FATs differing");
}

#[test]
fn chain_loop_and_crossing() {
    let d = fixture("fat16");
    let (fat, fats, len, _) = geometry(&d);
    let entry = |d: &[u8], c: usize| u16::from_le_bytes([d[fat + 2 * c], d[fat + 2 * c + 1]]);
    let set = |d: &mut [u8], c: usize, v: u16| {
        for k in 0..fats {
            d[fat + k * len + 2 * c..][..2].copy_from_slice(&v.to_le_bytes());
        }
    };
    let ends: Vec<usize> = (2..2000).filter(|&c| entry(&d, c) >= 0xfff8).collect();
    assert!(ends.len() >= 2);
    let mut x = d.clone();
    set(&mut x, ends[0], ends[0] as u16);
    refused(&x, "a loop");
    // (a chain joining another file's)
    let mut x = d.clone();
    set(&mut x, ends[0], ends[1] as u16);
    refused(&x, "a crossing");
    // (a chain past the clusters)
    let mut x = d.clone();
    set(&mut x, ends[0], 0xfff0);
    refused(&x, "past the clusters");
    // (a chain into a free cluster)
    let free = (2..2000).find(|&c| entry(&d, c) == 0).unwrap();
    let mut x = d.clone();
    set(&mut x, ends[0], free as u16);
    refused(&x, "into a free cluster");
}

#[test]
fn size_disagreeing_with_chain() {
    let d = fixture("fat16");
    let (_, _, _, root) = geometry(&d);
    let e = (0..512).map(|i| root + i * 32).find(|&e| d[e] != 0 && d[e] != 0xe5 && d[e + 11] & 0x3f == 0x20).unwrap();
    let size = u32::from_le_bytes(d[e + 28..e + 32].try_into().unwrap());
    let mut x = d.clone();
    x[e + 28..e + 32].copy_from_slice(&(size + 100_000).to_le_bytes());
    refused(&x, "a file longer than its chain");
    let mut x = d.clone();
    x[e + 28..e + 32].copy_from_slice(&0u32.to_le_bytes());
    refused(&x, "an empty file with a chain");
}

#[test]
fn invalid_boot_sector() {
    let mut d = fixture("fat16");
    d[13] = 3;
    assert!(disk::free(&d).is_err() || rewritten(&d).is_none_or(|(w, _)| !matches!(w, Rewrite::Made { .. })));
}

#[test]
fn exfat_bitmap_and_chains() {
    let d = fixture("exfat");
    let sector = d[108] as u32;
    let per = d[109] as u32;
    let le32 = |at: usize| u32::from_le_bytes(d[at..at + 4].try_into().unwrap()) as usize;
    let heap = le32(88) << sector;
    let cluster = 1usize << (sector + per);
    let root = heap + (le32(96) - 2) * cluster;
    let e = (0..cluster / 32).map(|i| root + i * 32).find(|&e| d[e] == 0x81).expect("a bitmap");
    let bitmap = heap + (u32::from_le_bytes(d[e + 20..e + 24].try_into().unwrap()) as usize - 2) * cluster;
    // (the bitmap saying that the root is free)
    let mut x = d.clone();
    x[bitmap] = 0;
    refused(&x, "a bitmap freeing the root");
    // (a file's length disagreeing with its chain)
    let s = (0..cluster / 32).map(|i| root + i * 32).find(|&s| d[s] == 0xc0 && d[s - 32] == 0x85).expect("a file");
    let len = u64::from_le_bytes(d[s + 24..s + 32].try_into().unwrap());
    if len > 0 {
        let mut x = d.clone();
        x[s + 24..s + 32].copy_from_slice(&(len + 10 * cluster as u64).to_le_bytes());
        refused(&x, "a length past its chain");
    }
}
