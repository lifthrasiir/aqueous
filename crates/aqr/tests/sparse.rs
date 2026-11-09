//! Files written sparse as they are extracted: runs of zeros long enough left as holes, in an
//! archive of a single file and in one of several files, the bytes given back the same.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("aqr-sparse-{name}-{}", std::process::id()));
    _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn aqr(d: &Path, args: &[&str]) {
    let o = Command::new(env!("CARGO_BIN_EXE_aqr")).current_dir(d).args(args).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

const MIB: usize = 1 << 20;

/// Data, 4 MiB of zeros (a hole), data, 128 KiB of zeros (too short for one), data, then 2 MiB
/// of zeros to the end (a hole).
fn image() -> Vec<u8> {
    let data = |seed: u32, n: usize| -> Vec<u8> {
        let mut x = seed;
        (0..n)
            .map(|_| {
                x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                (x >> 16) as u8 | 1
            })
            .collect()
    };
    let mut d = data(1, 100_000);
    d.resize(d.len() + 4 * MIB, 0);
    d.extend(data(2, 70_000));
    d.resize(d.len() + (128 << 10), 0);
    d.extend(data(3, 5000));
    d.resize(d.len() + 2 * MIB, 0);
    d
}

/// The bytes allocated to file `p`.
#[cfg(unix)]
fn allocated(p: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(p).unwrap().blocks() * 512
}

#[cfg(windows)]
fn allocated(p: &Path) -> u64 {
    use std::os::windows::ffi::OsStrExt;
    let w: Vec<u16> = p.as_os_str().encode_wide().chain([0]).collect();
    let mut hi = 0u32;
    let lo = unsafe { windows_sys::Win32::Storage::FileSystem::GetCompressedFileSizeW(w.as_ptr(), &mut hi) };
    ((hi as u64) << 32) | lo as u64
}

fn holed(p: &Path, d: &[u8]) {
    assert!(fs::read(p).unwrap() == d, "{}: other bytes", p.display());
    let a = allocated(p);
    // (the 6 MiB of holes; the file system may still give some of them blocks)
    assert!(a <= (d.len() - 4 * MIB) as u64, "{}: {a} bytes allocated of {}", p.display(), d.len());
}

#[test]
fn single_file() {
    let d = scratch("single");
    let img = image();
    fs::write(d.join("disk.img"), &img).unwrap();
    aqr(&d, &["-q", "disk.img"]);
    fs::remove_file(d.join("disk.img")).unwrap();
    aqr(&d, &["-dq", "disk.img.aqr"]);
    holed(&d.join("disk.img"), &img);
    _ = fs::remove_dir_all(&d);
}

#[test]
fn in_a_tree() {
    let d = scratch("tree");
    let img = image();
    fs::create_dir(d.join("t")).unwrap();
    fs::write(d.join("t/disk.img"), &img).unwrap();
    fs::write(d.join("t/zeros.bin"), vec![0; 3 * MIB]).unwrap();
    fs::write(d.join("t/a.txt"), b"text").unwrap();
    aqr(&d, &["-q", "t", "-o", "t.aqr"]);
    aqr(&d, &["-dq", "t.aqr", "-o", "out"]);
    holed(&d.join("out/disk.img"), &img);
    let z = d.join("out/zeros.bin");
    assert!(fs::read(&z).unwrap() == vec![0; 3 * MIB]);
    assert!(allocated(&z) < MIB as u64, "{}: {} bytes allocated", z.display(), allocated(&z));
    assert_eq!(fs::read(d.join("out/a.txt")).unwrap(), b"text");
    _ = fs::remove_dir_all(&d);
}
