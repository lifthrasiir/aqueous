//! Self-extracting programs made by `aqr -c --format sfx` (Windows x86-64): run, they write the
//! files out as they were. Skipped where this aqr carries no stub (built without
//! `target/aqr-sfx/`, which `crates/aqr-sfx/pack.sh` gathers).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("aqr-sfx-{name}-{}", std::process::id()));
    if d.exists() {
        fs::remove_dir_all(&d).unwrap();
    }
    fs::create_dir_all(&d).unwrap();
    d
}

fn aqr_in(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aqr")).current_dir(dir).args(args).output().unwrap()
}

#[cfg(all(windows, target_arch = "x86_64"))]
/// Text of words (which aqr-wrt recompresses, so that the archive has a module).
fn prose(n: usize) -> Vec<u8> {
    let words = ["the", "archive", "of", "files", "written", "out", "again", "when", "run", "and", "kept"];
    let mut t = Vec::new();
    let mut x = 7u32;
    while t.len() < n {
        x = x.wrapping_mul(1103515245).wrapping_add(12345);
        t.extend_from_slice(words[(x >> 16) as usize % words.len()].as_bytes());
        t.push(if x.is_multiple_of(13) { b'\n' } else { b' ' });
    }
    t
}

#[cfg(all(windows, target_arch = "x86_64"))]
/// Makes `out` (`--format sfx`) of `inputs` in `dir`; `false` where this aqr carries no stub.
fn made(dir: &Path, inputs: &[&str], out: &str) -> bool {
    let mut args = vec!["-c", "--format", "sfx", "-o", out];
    args.extend_from_slice(inputs);
    let o = aqr_in(dir, &args);
    let err = String::from_utf8_lossy(&o.stderr);
    if !o.status.success() && err.contains("carries no blobs") {
        eprintln!("skipped: {err}");
        return false;
    }
    assert!(o.status.success(), "aqr {args:?}: {err}");
    assert!(err.contains("self-extracting"), "{err}");
    true
}

#[cfg(all(windows, target_arch = "x86_64"))]
/// Runs self-extracting program `exe` in `dir`, writing the files under `to`.
fn run(dir: &Path, exe: &str, to: &str) {
    let o = Command::new(dir.join(exe)).current_dir(dir).arg(to).output().unwrap();
    assert!(o.status.success(), "{exe}: {}", String::from_utf8_lossy(&o.stderr));
}

#[cfg(all(windows, target_arch = "x86_64"))]
#[test]
fn sfx_of_a_directory() {
    let d = scratch("dir");
    fs::create_dir_all(d.join("in/sub")).unwrap();
    fs::write(d.join("in/a.txt"), prose(200_000)).unwrap();
    fs::write(d.join("in/sub/b.bin"), (0..5000u32).map(|i| (i * 31 % 251) as u8).collect::<Vec<_>>()).unwrap();
    fs::write(d.join("in/sub/empty"), b"").unwrap();
    if !made(&d, &["in"], "in.exe") {
        return;
    }
    run(&d, "in.exe", "out");
    for f in ["a.txt", "sub/b.bin", "sub/empty"] {
        assert!(fs::read(d.join("in").join(f)).unwrap() == fs::read(d.join("out").join(f)).unwrap(), "{f}");
    }
    fs::remove_dir_all(&d).unwrap();
}

/// A single file is in the program by its name (not as an archive of a single file, which has none).
#[cfg(all(windows, target_arch = "x86_64"))]
#[test]
fn sfx_of_a_file() {
    let d = scratch("file");
    fs::write(d.join("a.txt"), prose(100_000)).unwrap();
    if !made(&d, &["a.txt"], "a.exe") {
        return;
    }
    run(&d, "a.exe", "out");
    assert!(fs::read(d.join("a.txt")).unwrap() == fs::read(d.join("out/a.txt")).unwrap());
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn sfx_not_with() {
    let d = scratch("not");
    fs::write(d.join("a.txt"), b"text").unwrap();
    for (flag, what) in [("-P", "-P"), ("-A", "-A"), ("-a", "-a")] {
        let o = aqr_in(&d, &["-c", flag, "--format", "sfx", "-o", "a.exe", "a.txt"]);
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(!o.status.success() && err.contains(&format!("--format sfx: not with {what}")), "{flag}: {err}");
    }
    let o = aqr_in(&d, &["-c", "--format", "sfx", "-o", "-", "a.txt"]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("not with standard input or output"));
    fs::remove_dir_all(&d).unwrap();
}
