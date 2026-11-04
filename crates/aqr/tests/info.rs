//! `aqr -I`: an archive's records told, of the archive itself and of its paths; of an archive
//! of several segments (joined here: no encoder writes segments updating the paths of those
//! before them), the entries before the update too; of an archive of a single file, its
//! formula at once.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("aqr-info-{name}-{}", std::process::id()));
    if d.exists() {
        fs::remove_dir_all(&d).unwrap();
    }
    fs::create_dir_all(&d).unwrap();
    d
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
    fs::write(d.join("two.aqr"), aqr_formula::aqr::inspect::join_segments(&[&a, &b])).unwrap();

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
    for key in ["mtime: ", "atime: ", "unix-mode: ", "formula:"] {
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
