//! `-C DIR`, as tar has it: compressing, the inputs after it found in `DIR` (each `-C` from
//! the one before) and archived as if run there, `INPUT.aqr` beside them, `-o` where it is
//! given; decompressing, what is extracted goes into `DIR` (`-o` there too), the archives
//! where they are given.

use std::fs;
use std::path::{Path, PathBuf};

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("aqr-chdir-{name}-{}", std::process::id()));
    if d.exists() {
        fs::remove_dir_all(&d).unwrap();
    }
    fs::create_dir_all(&d).unwrap();
    d
}

/// `aqr -q` with `args`, run in directory `dir`: whether it succeeded, and its standard error.
fn aqr_in(dir: &Path, args: &[&str]) -> (bool, String) {
    let o = std::process::Command::new(env!("CARGO_BIN_EXE_aqr")).current_dir(dir).arg("-q").args(args).output();
    let o = o.unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stderr).into_owned())
}

fn ok(dir: &Path, args: &[&str]) {
    let (ok, err) = aqr_in(dir, args);
    assert!(ok, "{args:?}: {err}");
}

/// The paths under `d`, sorted, as `/`-separated relative paths, files with their contents.
fn listing(d: &Path) -> Vec<String> {
    fn walk(root: &Path, d: &Path, out: &mut Vec<String>) {
        for e in fs::read_dir(d).unwrap() {
            let p = e.unwrap().path();
            let name = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            if fs::symlink_metadata(&p).unwrap().is_dir() {
                out.push(format!("{name}/"));
                walk(root, &p, out);
            } else {
                out.push(format!("{name}={}", String::from_utf8_lossy(&fs::read(&p).unwrap())));
            }
        }
    }
    let mut out = Vec::new();
    walk(d, d, &mut out);
    out.sort();
    out
}

/// A tree: `w/p/a/bar`, `w/p/b/bar`, `w/q/baz`; returns `w`.
fn tree(name: &str) -> PathBuf {
    let w = scratch(name).join("w");
    fs::create_dir_all(w.join("p/a")).unwrap();
    fs::create_dir_all(w.join("p/b")).unwrap();
    fs::create_dir_all(w.join("q")).unwrap();
    fs::write(w.join("p/a/bar"), b"1").unwrap();
    fs::write(w.join("p/b/bar"), b"2").unwrap();
    fs::write(w.join("q/baz"), b"3").unwrap();
    w
}

/// `args` archived in `w` into `w/x.aqr`, then extracted: what it holds.
fn archived(w: &Path, args: &[&str]) -> Vec<String> {
    let _ = fs::remove_file(w.join("x.aqr"));
    let out = w.join("out");
    if out.exists() {
        fs::remove_dir_all(&out).unwrap();
    }
    ok(w, &[&["-l", "0", "-o", "x.aqr"], args].concat());
    ok(w, &["-d", "x.aqr", "-o", "out"]);
    listing(&out)
}

#[test]
fn compressing_in_dir() {
    let w = tree("compress");
    assert_eq!(archived(&w, &["-C", "p", "a/bar", "b/bar"]), ["a/", "a/bar=1", "b/", "b/bar=2"]);
    // (each -C from the one before; the inputs before it where they are)
    assert_eq!(
        archived(&w, &["q/baz", "-C", "p", "a", "-C", "../q", "baz"]),
        ["a/", "a/bar=1", "baz=3", "q/", "q/baz=3"]
    );
    // (a single directory: what it holds, as without -C)
    assert_eq!(archived(&w, &["-C", "p", "a"]), ["bar=1"]);
    // (without -o: INPUT.aqr beside the input)
    ok(&w, &["-l", "0", "-C", "p", "a/bar"]);
    assert!(w.join("p/a/bar.aqr").is_file());

    for (args, told) in [
        (&["p/a", "-C", "q"][..], "not before an input"),
        (&["-C", "nowhere", "p/a"], "nowhere"),
        (&["-C"], "takes a value"),
    ] {
        let (ok, err) = aqr_in(&w, &[&["-l", "0", "-o", "y.aqr"], args].concat());
        assert!(!ok && err.contains(told), "{args:?}: {err}");
    }
    let (ok, err) = aqr_in(&w, &["-L", "-C", "p", "x.aqr"]);
    assert!(!ok && err.contains("-L takes archives alone"), "{err}");
}

#[test]
fn decompressing_into_dir() {
    let w = tree("decompress");
    fs::create_dir(w.join("o")).unwrap();
    ok(&w, &["-l", "0", "-o", "x.aqr", "p/a"]);
    ok(&w, &["--format", "tar", "-o", "t.tar", "p/a", "p/b"]);
    // (an Aqueous archive into its name in DIR, not beside it; -C after the archive too)
    ok(&w, &["-d", "x.aqr", "-C", "o"]);
    assert_eq!(listing(&w.join("o")), ["x/", "x/bar=1"]);
    // (-o in DIR)
    ok(&w, &["-d", "-C", "o", "-o", "y", "x.aqr"]);
    assert_eq!(listing(&w.join("o/y")), ["bar=1"]);
    // (another format: into DIR as it is, a selection too)
    ok(&w, &["-d", "-C", "o", "t.tar"]);
    assert!(w.join("o/p/a/bar").is_file() && w.join("o/p/b/bar").is_file());
    fs::create_dir(w.join("o2")).unwrap();
    ok(&w, &["-d", "-C", "o2", "t.tar", "p/b"]);
    assert_eq!(listing(&w.join("o2")), ["p/", "p/b/", "p/b/bar=2"]);
    // (--auto-dir: DIR in place of beside the archive)
    fs::create_dir(w.join("o3")).unwrap();
    ok(&w, &["-d", "--each", "--auto-dir", "-C", "o3", "t.tar", "x.aqr"]);
    assert_eq!(listing(&w.join("o3")), ["bar=1", "p/", "p/a/", "p/a/bar=1", "p/b/", "p/b/bar=2"]);

    let (ok, err) = aqr_in(&w, &["-d", "-C", "nowhere", "x.aqr"]);
    assert!(!ok && err.contains("nowhere"), "{err}");
}
