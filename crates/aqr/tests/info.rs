//! `aqr -I`: an archive's records told, of the archive itself and of its paths; of an archive
//! of several segments (joined here: no encoder writes segments updating the paths of those
//! before them), the entries before the update too; of an archive of a single file, its
//! formula at once.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(name: &str) -> PathBuf {
    common::scratch("info", name)
}

/// `aqr` with `args`, run in `dir`: whether it succeeded, its standard output and error.
fn aqr_in(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_aqr")).current_dir(dir).args(args).output().unwrap();
    (
        o.status.success(),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

#[test]
fn segments_and_paths() {
    let d = scratch("seg");
    for (v, body) in [("v1", &b"one"[..]), ("v2", &b"two, longer"[..])] {
        let src = d.join(v);
        fs::create_dir_all(src.join("dir")).unwrap();
        fs::write(src.join("dir/f.txt"), body).unwrap();
        fs::write(src.join(format!("only-{v}")), vec![7u8; 400]).unwrap();
        let (ok, _, err) =
            aqr_in(&d, &["-q", "-p", "-l", "0", "-C", v, "-o", &format!("{v}.aqr"), "dir", &format!("only-{v}")]);
        assert!(ok, "{err}");
    }
    let (a, b) = (fs::read(d.join("v1.aqr")).unwrap(), fs::read(d.join("v2.aqr")).unwrap());
    fs::write(d.join("two.aqr"), aqr_archive::inspect::join_segments(&[&a, &b])).unwrap();

    // the archive itself: its blocks and segments, the last current
    let (ok, out, err) = aqr_in(&d, &["-I", "two.aqr"]);
    assert!(ok, "{err}");
    assert!(out.contains("segments: 2"), "{out}");
    assert!(out.contains("segment #1:") && out.contains("segment #2 [current]:"), "{out}");
    assert!(out.contains("to the metadata block at"), "{out}");
    assert!(out.contains("kind: standalone archive"), "{out}");
    assert!(!out.contains("problems:"), "{out}");

    // a path in both: each segment's entry, the second current, its times all told
    let (ok, out, err) = aqr_in(&d, &["-I", "two.aqr", "dir/f.txt"]);
    assert!(ok, "{err}");
    let (one, two) = out.split_once("\n  segment #2").expect(&out);
    assert!(one.contains("segment #1 [replaced in segment #2]:") && one.contains("a file of 3 B"), "{out}");
    assert!(two.starts_with(" [current]:") && two.contains("a file of 11 B"), "{out}");
    // (the mode a system keeps: Unix's, or Windows' attributes)
    let mode = if cfg!(windows) { "win-attrs: " } else { "unix-mode: " };
    for key in ["mtime: ", "atime: ", mode, "formula:"] {
        assert!(one.contains(key) && two.contains(key), "{key}: {out}");
    }
    // a path of the first alone, still current; of neither, an error
    let (ok, out, _) = aqr_in(&d, &["-I", "two.aqr", "only-v1"]);
    assert!(ok && out.contains("segment #1 [current]:") && !out.contains("segment #2"), "{out}");
    // the root directory, in each (merged: the latest current)
    let (ok, out, _) = aqr_in(&d, &["-I", "two.aqr", "/"]);
    assert!(
        ok && out.contains("segment #1 [merged into segment #2]:") && out.contains("segment #2 [current]:"),
        "{out}"
    );
    assert!(out.contains("the root directory"), "{out}");
    let (ok, out, _) = aqr_in(&d, &["-I", "two.aqr", "dir"]);
    assert!(
        ok && out.contains("segment #1 [merged into segment #2]:") && out.contains("segment #2 [current]:"),
        "{out}"
    );
    let (ok, _, err) = aqr_in(&d, &["-I", "two.aqr", "nothing"]);
    assert!(!ok && err.contains("not in the archive"), "{err}");
}

#[test]
fn single_file() {
    let d = scratch("one");
    fs::write(d.join("f.bin"), (0..=255u8).cycle().take(50_000).collect::<Vec<u8>>()).unwrap();
    // (in blocks of 16 KiB: leaves of several)
    let (ok, _, err) = aqr_in(&d, &["-q", "-b", "16K", "f.bin"]);
    assert!(ok, "{err}");
    let (ok, out, err) = aqr_in(&d, &["-I", "f.bin.aqr"]);
    assert!(ok, "{err}");
    assert!(out.contains("file (unnamed)"), "{out}");
    assert!(out.contains("leaf #1 ") && out.contains("no metadata block"), "{out}");
    let (ok, _, err) = aqr_in(&d, &["-I", "f.bin.aqr", "f.bin"]);
    assert!(!ok && err.contains("single file"), "{err}");
}

/// `-I -v` lists each application's memory limits and the most the archive needs.
#[test]
fn applications_limits() {
    let d = scratch("apps");
    let mut x = 12345u32;
    let text: Vec<u8> = (0..60000u32)
        .flat_map(|i| {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            format!("line {} of text {} {}\n", i, x >> 20, i % 97).into_bytes()
        })
        .collect();
    fs::write(d.join("t.txt"), &text).unwrap();
    assert!(Command::new("gzip").args(["-k", "-9", "t.txt"]).current_dir(&d).status().unwrap().success());
    fs::remove_file(d.join("t.txt")).unwrap();
    let (ok, _, err) = aqr_in(&d, &["-q", "-9", "-o", "t.aqr", "t.txt.gz"]);
    assert!(ok, "{err}");

    let (ok, out, err) = aqr_in(&d, &["-I", "t.aqr"]);
    assert!(ok, "{err}");
    assert!(!out.contains("applications:"), "{out}");
    let (ok, out, err) = aqr_in(&d, &["-I", "-v", "t.aqr"]);
    assert!(ok, "{err}");
    let line = out.lines().find(|l| l.contains("minimal")).expect(&out);
    assert!(line.contains("instance") && line.contains("full") && line.contains("chain"), "{out}");
    assert!(out.contains("most memory needed:"), "{out}");
}

/// `-I` of an encrypted archive tells its encryption (with no paths), then the payload as
/// decrypted; a foreign archive is refused.
#[test]
fn encrypted_and_foreign() {
    let d = scratch("enc");
    fs::create_dir_all(d.join("src")).unwrap();
    fs::write(d.join("src/f.txt"), b"hello hello hello").unwrap();
    fs::write(d.join("pass"), "secret\n").unwrap();
    let (ok, _, err) = aqr_in(&d, &["-q", "--passphrase-from", "pass", "-o", "e.aqr", "src"]);
    assert!(ok, "{err}");
    let (ok, out, err) = aqr_in(&d, &["-I", "--passphrase-from", "pass", "e.aqr"]);
    assert!(ok, "{err}");
    assert!(out.contains("encrypted: scrypt of work factor"), "{out}");
    assert!(out.contains("salt ") && out.contains("nonce ") && out.contains("chunk"), "{out}");
    assert!(out.contains("payload: ") && out.contains("kind: standalone archive"), "{out}");
    // a path: the entry, with no encryption told
    let (ok, out, err) = aqr_in(&d, &["-I", "--passphrase-from", "pass", "e.aqr", "f.txt"]);
    assert!(ok, "{err}");
    assert!(out.contains("a file of 17 B") && !out.contains("encrypted:"), "{out}");
    // a wrong passphrase is an error
    fs::write(d.join("wrong"), "nope\n").unwrap();
    let (ok, _, _) = aqr_in(&d, &["-I", "--passphrase-from", "wrong", "e.aqr"]);
    assert!(!ok);
    // another format's archive is not told of
    fs::write(d.join("x.zip"), b"PK\x05\x06\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0").unwrap();
    let (ok, _, err) = aqr_in(&d, &["-I", "x.zip"]);
    assert!(!ok && err.contains("only Aqueous archives are told of"), "{err}");
}

/// `-I` of the entries of a tree: a symbolic link, a FIFO, a directory and a file, each
/// told by what it is.
#[cfg(unix)]
#[test]
fn entry_kinds() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let d = scratch("kinds");
    let src = d.join("src");
    fs::create_dir_all(src.join("sub")).unwrap();
    fs::write(src.join("a"), b"content of a").unwrap();
    std::os::unix::fs::symlink("a", src.join("link")).unwrap();
    let fifo = CString::new(src.join("fifo").as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o640) }, 0);
    let (ok, _, err) = aqr_in(&d, &["-q", "-p", "-o", "k.aqr", "src"]);
    assert!(ok, "{err}");
    let told = |p: &str| {
        let (ok, out, err) = aqr_in(&d, &["-I", "k.aqr", p]);
        assert!(ok, "{p}: {err}");
        out
    };
    let out = told("link");
    assert!(out.contains("a symbolic link to \"a\""), "{out}");
    let out = told("fifo");
    assert!(out.contains("a FIFO"), "{out}");
    assert!(told("sub").contains("a directory"));
    let out = told("a");
    assert!(out.contains("a file of 12 B") && out.contains("unix-mode: "), "{out}");
}
