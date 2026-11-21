use super::*;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A temporary directory of its own, removed at the end.
struct Dir(PathBuf);

impl Dir {
    fn new() -> Dir {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("aqr-lossy-hfs-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        Dir(p)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The standard output of a command; None where it is not there or fails.
fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    if !out.status.success() {
        eprintln!("{cmd} {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A raw image attached without mounting, detached at the end.
struct Attached(String);

impl Attached {
    fn new(file: &Path) -> Option<Attached> {
        let out =
            run("hdiutil", &["attach", "-nomount", "-imagekey", "diskimage-class=CRawDiskImage", file.to_str()?])?;
        let dev = out.split_whitespace().next()?.to_string();
        dev.starts_with("/dev/disk").then_some(Attached(dev))
    }
}

impl Drop for Attached {
    fn drop(&mut self) {
        let _ = Command::new("hdiutil").args(["detach", "-force", &self.0]).output();
    }
}

fn pseudo(seed: u64, n: usize) -> Vec<u8> {
    let mut s = seed | 1;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 24) as u8 | 1
        })
        .collect()
}

/// A 32 MiB HFS+ volume made by macOS (journaled with `journal`), with files written and half of
/// them deleted, unmounted cleanly. None where the tools fail.
fn make(journal: bool) -> Option<Vec<u8>> {
    let dir = Dir::new();
    let img = dir.0.join("t.img");
    std::fs::File::create(&img).unwrap().set_len(32 << 20).unwrap();
    let a = Attached::new(&img)?;
    let name = format!("AQR{}{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
    let mut args = vec!["-v", &name];
    if journal {
        args.push("-J");
    }
    args.push(&a.0);
    run("newfs_hfs", &args)?;
    run("diskutil", &["mount", &a.0])?;
    let mount = PathBuf::from("/Volumes").join(&name);
    for i in 0..40 {
        std::fs::write(mount.join(format!("f{i}.bin")), pseudo(i + 1, 5000 + 40_000 * (i as usize % 7))).ok()?;
    }
    for i in (0..40).step_by(2) {
        std::fs::remove_file(mount.join(format!("f{i}.bin"))).ok()?;
    }
    run("sync", &[]);
    run("diskutil", &["unmount", &a.0])?;
    drop(a);
    Some(std::fs::read(&img).unwrap())
}

/// Whether `fsck_hfs -n` finds `d` clean; None where the tools fail.
fn fsck(d: &[u8]) -> Option<bool> {
    let dir = Dir::new();
    let img = dir.0.join("r.img");
    std::fs::write(&img, d).unwrap();
    let a = Attached::new(&img)?;
    let out = Command::new("fsck_hfs").args(["-n", &a.0]).output().ok()?;
    if !out.status.success() {
        eprintln!("{}", String::from_utf8_lossy(&out.stdout));
    }
    Some(out.status.success())
}

fn fixture(name: &str) -> Vec<u8> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../codecs/aqr-hfs/tests/data").join(name);
    zstd::stream::decode_all(&std::fs::read(p).unwrap()[..]).unwrap()
}

/// The (first) HFS+ or HFSX volume of `d`.
fn volume_range(d: &[u8]) -> std::ops::Range<usize> {
    aqr_disk::volumes(&aqr_disk::Plain(d))
        .into_iter()
        .find(|v| d.get(v.start + 1024..v.start + 1026).is_some_and(|s| s == b"H+" || s == b"HX"))
        .unwrap()
}

fn volume(d: &[u8]) -> usize {
    volume_range(d).start
}

fn be32(d: &[u8], p: usize) -> usize {
    u32::from_be_bytes(d[p..p + 4].try_into().unwrap()) as usize
}

fn put32(d: &mut [u8], p: usize, v: usize) {
    d[p..p + 4].copy_from_slice(&(v as u32).to_be_bytes());
}

fn rewritten(x: &[u8]) -> Option<(Rewrite, Option<Vec<u8>>)> {
    rewrite(x, &Lossy::parse("slack").unwrap(), &Options::default())
}

/// Fills the free ranges of `x` with a pattern; their total.
fn stale(x: &mut [u8]) -> usize {
    let free = super::disk::free(x).unwrap();
    for r in &free {
        for (i, b) in x[r.clone()].iter_mut().enumerate() {
            *b = 0xa0 | (i % 7) as u8;
        }
    }
    free.iter().map(|r| r.len()).sum()
}

/// What a good image comes to: made again with its free bytes zeroed, checked, the files the
/// same, nothing more to take out of R. Returns R and the free bytes.
fn good(x: &[u8], with_fsck: bool) -> (Vec<u8>, usize) {
    let free = super::disk::free(x).unwrap();
    let bytes: usize = free.iter().map(|r| r.len()).sum();
    assert!(bytes > 0);
    let (rw, r) = rewritten(x).unwrap();
    assert!(matches!(rw, Rewrite::Made { .. }), "{rw:?}");
    let r = r.unwrap();
    assert_eq!(r.len(), x.len());
    check::disk(x, &r).unwrap();
    assert!(free.iter().all(|f| r[f.clone()].iter().all(|&b| b == 0)));
    let fss = super::disk::file_systems();
    let (a, b) = (aqr_disk::files(x, fss), aqr_disk::files(&r, fss));
    assert!(!a.is_empty());
    assert_eq!(a.len(), b.len());
    for (f, g) in a.iter().zip(&b) {
        assert_eq!(f.name, g.name);
        assert_eq!(f.extents, g.extents);
        assert!(f.extents.iter().all(|e| x[e.clone()] == r[e.clone()]));
    }
    assert!(rewritten(&r).is_none(), "R of R");
    if let Some(ok) = with_fsck.then(|| fsck(&r[volume_range(&r)])).flatten() {
        assert!(ok, "fsck_hfs -n on R");
    }
    (r, bytes)
}

fn refused(x: &[u8], why: &str) {
    let e = super::disk::free(x).unwrap_err();
    assert!(e.contains(why), "{e}");
    match rewritten(x) {
        Some((Rewrite::Kept { reason, .. }, None)) => assert!(reason.contains(why), "{reason}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn fixtures() {
    for name in ["hfs.img.zst", "hfsx.img.zst"] {
        let mut x = fixture(name);
        let n = stale(&mut x);
        eprintln!("{name}: {n} free bytes");
        good(&x, true);
    }
}

#[test]
fn real_volumes() {
    for journal in [false, true] {
        let Some(x) = make(journal) else {
            eprintln!("no hdiutil/newfs_hfs: skipped");
            return;
        };
        let (_, bytes) = good(&x, true);
        eprintln!("journal {journal}: {bytes} free bytes of {}", x.len());
        // (the deleted files' bytes were stale in free blocks: more than the empty volume's)
        assert!(x[1024..1026] == *b"H+");
        assert!(bytes > 1 << 20);
    }
}

#[test]
fn bitmap_bit_of_a_catalog_block_cleared() {
    let mut x = fixture("hfs.img.zst");
    let o = volume(&x);
    let (bs, free_blocks) = (be32(&x, o + 1024 + 40), be32(&x, o + 1024 + 48));
    let cat = be32(&x, o + 1024 + 272 + 16);
    let bitmap = o + be32(&x, o + 1024 + 112 + 16) * bs;
    x[bitmap + cat / 8] &= !(0x80 >> (cat % 8));
    // (the count made right, so that it is the catalog that is found out)
    put32(&mut x, o + 1024 + 48, free_blocks + 1);
    refused(&x, "special file");
}

#[test]
fn not_unmounted() {
    let mut x = fixture("hfsx.img.zst");
    let o = volume(&x);
    let attrs = be32(&x, o + 1024 + 4);
    put32(&mut x, o + 1024 + 4, attrs & !(1 << 8));
    refused(&x, "unmounted");
}

#[test]
fn free_count_not_the_bitmaps() {
    let mut x = fixture("hfs.img.zst");
    let o = volume(&x);
    let n = be32(&x, o + 1024 + 48);
    put32(&mut x, o + 1024 + 48, n + 1);
    refused(&x, "free blocks");
}

#[test]
fn journal_not_empty() {
    let Some(mut x) = make(true) else {
        eprintln!("no hdiutil/newfs_hfs: skipped");
        return;
    };
    let info = be32(&x, 1024 + 12) * be32(&x, 1024 + 40);
    let off = u64::from_be_bytes(x[info + 36..info + 44].try_into().unwrap()) as usize;
    // (the machine's byte order: little-endian where the journal says so)
    let le = x[off + 4..off + 8] == 0x12345678u32.to_le_bytes();
    let end: u64 = 512 + 4096;
    x[off + 16..off + 24].copy_from_slice(&if le { end.to_le_bytes() } else { end.to_be_bytes() });
    refused(&x, "journal");
}
