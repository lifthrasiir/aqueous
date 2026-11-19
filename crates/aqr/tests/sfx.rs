//! Self-extracting programs made by `aqr -c --format sfx` (Windows x86-64): run, they write the
//! files out as they were; and the modules run as WebAssembly by the pack's interpreter blob.
//! The aqr built has the pack of `target/aqr-sfx/` (`AQR_SFX_PACK`'s where that is set;
//! `crates/aqr-sfx/pack.py` makes it) written into a copy of it, as the release does; the tests
//! fail where there is none.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn scratch(name: &str) -> PathBuf {
    common::scratch("sfx", name)
}

/// The aqr built, with the pack written into a copy of it where there is one.
fn aqr() -> &'static Path {
    static AQR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    AQR.get_or_init(|| {
        let exe = PathBuf::from(env!("CARGO_BIN_EXE_aqr"));
        #[cfg(windows)]
        {
            // (the target directory: the exe's is that of its profile)
            let target = exe.parent().and_then(Path::parent).unwrap();
            let dir = std::env::var_os("AQR_SFX_PACK").map_or_else(|| target.join("aqr-sfx"), PathBuf::from);
            if let (Ok(pack), Ok(id)) = (fs::read(dir.join("sfx.pack")), fs::read_to_string(dir.join("pack.id"))) {
                let copy = scratch("aqr").join("aqr.exe");
                fs::copy(&exe, &copy).unwrap();
                aqr_sfx::resource::embed(&copy, &pack, id.trim()).unwrap();
                return copy;
            }
        }
        exe
    })
}

fn aqr_in(dir: &Path, args: &[&str]) -> Output {
    Command::new(aqr()).current_dir(dir).args(args).output().unwrap()
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
/// Makes `out` (`--format sfx`) of `inputs` in `dir` (the aqr tested carrying the pack).
fn made(dir: &Path, inputs: &[&str], out: &str) {
    let mut args = vec!["-c", "--format", "sfx", "-o", out];
    args.extend_from_slice(inputs);
    let o = aqr_in(dir, &args);
    let err = String::from_utf8_lossy(&o.stderr);
    let pack = "(the pack: crates/aqr-sfx/pack.py, which `cargo aqr` runs, or AQR_SFX_PACK)";
    assert!(o.status.success(), "aqr {args:?}: {err} {pack}");
    assert!(err.contains("self-extracting"), "{err}");
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
    made(&d, &["in"], "in.exe");
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
    made(&d, &["a.txt"], "a.exe");
    run(&d, "a.exe", "out");
    assert!(fs::read(d.join("a.txt")).unwrap() == fs::read(d.join("out/a.txt")).unwrap());
    fs::remove_dir_all(&d).unwrap();
}

/// The modules run as WebAssembly (`-Z wasm`) by the interpreter blob of the pack, as aqr.exe
/// links no Wasmtime: an archive of a module (aqr-wrt's) tested and extracted as it was.
#[cfg(all(windows, target_arch = "x86_64"))]
#[test]
fn wasm_through_the_interpreter() {
    let d = scratch("interp");
    fs::create_dir_all(d.join("in")).unwrap();
    fs::write(d.join("in/a.txt"), prose(300_000)).unwrap();
    fs::write(d.join("in/b.txt"), prose(5_000)).unwrap();
    let o = aqr_in(&d, &["-c", "-q", "-o", "a.aqr", "in"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let pack = "(the pack: crates/aqr-sfx/pack.py, which `cargo aqr` runs, or AQR_SFX_PACK)";
    for args in [&["-K", "-Z", "wasm", "a.aqr"][..], &["-d", "-q", "-Z", "wasm", "-o", "out", "a.aqr"]] {
        let o = aqr_in(&d, args);
        assert!(o.status.success(), "aqr {args:?}: {} {pack}", String::from_utf8_lossy(&o.stderr));
    }
    for f in ["a.txt", "b.txt"] {
        assert!(fs::read(d.join("in").join(f)).unwrap() == fs::read(d.join("out").join(f)).unwrap(), "{f}");
    }
    // (and a module it ran: the archive of text has aqr-wrt's)
    let list = String::from_utf8_lossy(&aqr_in(&d, &["-t", "-v", "a.aqr"]).stdout).into_owned();
    assert!(list.contains("wrt"), "{list}");
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
