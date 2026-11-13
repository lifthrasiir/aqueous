//! The names that inputs are archived by (`aqr -o ARCHIVE INPUT [--rename TO]...`): a relative
//! path as it is given (`./` and `.` left out, the directories on the way made plainly), an
//! absolute one or one with `..` by its last name, as 7-Zip does; `--rename TO` the path in the
//! archive exactly; names taken twice told by the inputs that take them.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

fn scratch(name: &str) -> PathBuf {
    common::scratch("inputs", name)
}

/// `aqr` with `args`, run in directory `dir`: whether it succeeded, and its standard error.
fn aqr_in(dir: &Path, args: &[&str]) -> (bool, String) {
    let o: Output = std::process::Command::new(env!("CARGO_BIN_EXE_aqr")).current_dir(dir).args(args).output().unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stderr).into_owned())
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

/// A tree to archive from: `w/p/a/bar`, `w/p/b/bar`, `w/q/baz`; the work done in `w/p`.
fn tree(name: &str) -> (PathBuf, PathBuf) {
    let d = scratch(name);
    let p = d.join("w/p");
    fs::create_dir_all(p.join("a")).unwrap();
    fs::create_dir_all(p.join("b")).unwrap();
    fs::create_dir_all(d.join("w/q")).unwrap();
    fs::write(p.join("a/bar"), b"1").unwrap();
    fs::write(p.join("b/bar"), b"2").unwrap();
    fs::write(d.join("w/q/baz"), b"3").unwrap();
    (d, p)
}

/// `args` archived in `p` into `x.aqr`, then extracted: what it holds, or the error.
fn archived(p: &Path, args: &[&str]) -> Result<Vec<String>, String> {
    let _ = fs::remove_file(p.join("x.aqr"));
    let out = p.join("out");
    if out.exists() {
        fs::remove_dir_all(&out).unwrap();
    }
    let (ok, err) = aqr_in(p, &[&["-q", "-l", "0", "-o", "x.aqr"], args].concat());
    if !ok {
        return Err(err);
    }
    let (ok, err) = aqr_in(p, &["-q", "-d", "x.aqr", "-o", "out"]);
    assert!(ok, "{err}");
    Ok(listing(&out))
}

#[test]
fn relative_paths_kept() {
    let (_d, p) = tree("relative");
    let want = ["a/", "a/bar=1", "b/", "b/bar=2"];
    assert_eq!(archived(&p, &["a/bar", "b/bar"]).unwrap(), want);
    assert_eq!(archived(&p, &["./a/bar", "./b/./bar"]).unwrap(), want);
    // (a directory given as well as a file under another: each where its path has it)
    assert_eq!(archived(&p, &["a", "b/bar"]).unwrap(), want);
}

#[test]
fn absolute_and_up_by_name() {
    let (d, p) = tree("absolute");
    let abs = |s: &str| p.join(s).to_str().unwrap().to_owned();
    assert_eq!(archived(&p, &[&abs("a"), "../q/baz"]).unwrap(), ["a/", "a/bar=1", "baz=3"]);
    // (the same name twice: told by both inputs)
    let err = archived(&p, &[&abs("a/bar"), &abs("b/bar")]).unwrap_err();
    assert!(err.contains(&abs("a/bar")) && err.contains(&abs("b/bar")), "{err}");
    let err = archived(&d.join("w"), &["p/a", "p/a/bar"]).unwrap_err();
    assert!(err.contains("p/a/bar"), "{err}");
}

#[test]
fn renamed() {
    let (_d, p) = tree("renamed");
    assert_eq!(
        archived(&p, &["a/bar", "--rename", "x/y/z", "b/bar"]).unwrap(),
        ["b/", "b/bar=2", "x/", "x/y/", "x/y/z=1"]
    );
    // (a single file, or a directory, renamed: in a tree of that name)
    assert_eq!(archived(&p, &["a/bar", "--rename", "c"]).unwrap(), ["c=1"]);
    assert_eq!(archived(&p, &["a", "--rename", "k/l"]).unwrap(), ["k/", "k/l/", "k/l/bar=1"]);
    // (a directory made on the way to a name, then given itself)
    assert_eq!(archived(&p, &["b/bar", "--rename", "a/x", "a"]).unwrap(), ["a/", "a/bar=1", "a/x=2"]);
    // (into INPUT.aqr, without -o)
    let (ok, err) = aqr_in(&p, &["-q", "-l", "0", "a/bar", "--rename", "r/s"]);
    assert!(ok, "{err}");
    let (ok, err) = aqr_in(&p, &["-q", "-d", "a/bar.aqr", "-o", "out2"]);
    assert!(ok, "{err}");
    assert_eq!(listing(&p.join("out2")), ["r/", "r/s=1"]);

    for (args, told) in [
        (&["--rename", "x", "a/bar"][..], "not after an input"),
        (&["a/bar", "--rename", "x", "--rename", "y"], "given twice"),
        (&["a/bar", "--rename", "../x"], "not a path in an archive"),
        (&["a/bar", "--rename", "/x"], "not a path in an archive"),
        (&["a/bar", "--rename", "x//y"], "not a path in an archive"),
        (&["a/bar", "--rename", "x/./y"], "not a path in an archive"),
        (&["a/bar", "--rename", "b/bar", "b/bar"], "b/bar"),
    ] {
        let err = archived(&p, args).unwrap_err();
        assert!(err.contains(told), "{args:?}: {err}");
    }
}
