//! What is left out of the inputs (`-x`: by name anywhere, by path in the archive) and the
//! inputs listed in a file (`--files-from`, lines or NUL-ended with `--null`).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("aqr-exclude-{name}-{}", std::process::id()));
    _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// aqr run in `d` on `args`, `input` its standard input: whether it succeeded, its standard
/// error.
fn aqr(d: &Path, args: &[&str], input: &[u8]) -> (bool, String) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_aqr"))
        .current_dir(d)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(input).unwrap();
    let o = c.wait_with_output().unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stderr).into_owned())
}

/// The paths archive `a` (in `d`) lists.
fn listed(d: &Path, a: &str) -> Vec<String> {
    let o = Command::new(env!("CARGO_BIN_EXE_aqr")).current_dir(d).args(["-tq", a]).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let mut l: Vec<String> = String::from_utf8_lossy(&o.stdout).lines().map(str::to_owned).collect();
    l.sort();
    l
}

fn tree(d: &Path) {
    for p in ["t/a.txt", "t/.git/config", "t/sub/.git/x", "t/sub/b.txt", "t/sub/c.txt", "t/c.txt"] {
        let p = d.join(p);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, p.to_string_lossy().repeat(20)).unwrap();
    }
}

#[test]
fn exclude() {
    let d = scratch("x");
    tree(&d);
    let (ok, err) = aqr(&d, &["-q", "-x", ".git", "--exclude=sub/c.txt", "-o", "a.aqr", "t"], b"");
    assert!(ok, "{err}");
    assert_eq!(listed(&d, "a.aqr"), ["a.txt", "c.txt", "sub/", "sub/b.txt"]);
    // (paths as archived, several inputs; an input itself, by name)
    let (ok, err) = aqr(&d, &["-q", "-x/t/sub", "-xa.txt", "-o", "b.aqr", "t/a.txt", "t"], b"");
    assert!(ok, "{err}");
    assert_eq!(listed(&d, "b.aqr"), ["t/", "t/.git/", "t/.git/config", "t/c.txt"]);
    let (ok, err) = aqr(&d, &["-q", "-x", "a.txt", "t/a.txt"], b"");
    assert!(!ok && err.contains("nothing to archive"), "{err}");
    let (ok, err) = aqr(&d, &["-dq", "-x", "a.txt", "a.aqr"], b"");
    assert!(!ok && err.contains("-x is for compressing"), "{err}");
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn files_from() {
    let d = scratch("from");
    tree(&d);
    fs::write(d.join("list"), "t/a.txt\r\n\nt/sub\n").unwrap();
    let (ok, err) = aqr(&d, &["-q", "--files-from", "list", "-o", "a.aqr"], b"");
    assert!(ok, "{err}");
    assert_eq!(
        listed(&d, "a.aqr"),
        ["t/", "t/a.txt", "t/sub/", "t/sub/.git/", "t/sub/.git/x", "t/sub/b.txt", "t/sub/c.txt"]
    );
    // of standard input, NUL-ended; with -x
    let (ok, err) = aqr(&d, &["-q", "--null", "--files-from=-", "-x.git", "-o", "b.aqr"], b"t/c.txt\0t/sub\0");
    assert!(ok, "{err}");
    assert_eq!(listed(&d, "b.aqr"), ["t/", "t/c.txt", "t/sub/", "t/sub/b.txt", "t/sub/c.txt"]);
    // decompressing, the paths of the archive
    fs::write(d.join("paths"), "t/sub/b.txt\n").unwrap();
    let (ok, err) = aqr(&d, &["-dq", "b.aqr", "--files-from", "paths", "-o", "out"], b"");
    assert!(ok, "{err}");
    assert!(d.join("out/t/sub/b.txt").is_file() && !d.join("out/t/c.txt").exists());
    // (an empty list: no input, standard input not taken for one)
    fs::write(d.join("empty"), "").unwrap();
    let (ok, err) = aqr(&d, &["-q", "--files-from", "empty", "-o", "c.aqr"], b"");
    assert!(!ok && err.contains("no input"), "{err}");
    let (ok, err) = aqr(&d, &["-q", "--files-from=-", "-"], b"");
    assert!(!ok && err.contains("standard input taken otherwise"), "{err}");
    fs::remove_dir_all(&d).unwrap();
}
