//! `-L` (`--dereference`): links given and found followed, archived as what they lead to at
//! their own names; those leading nowhere or to a directory they are in kept as links and told.
//! Without it, links archived as links, one given alone too.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("aqr-deref-{name}-{}", std::process::id()));
    if d.exists() {
        fs::remove_dir_all(&d).unwrap();
    }
    fs::create_dir_all(&d).unwrap();
    d
}

/// `aqr` with `args`, run in directory `dir`: whether it succeeded, its standard output and
/// its standard error.
fn aqr_in(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_aqr")).current_dir(dir).args(args).output().unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (o.status.success(), text(&o.stdout), text(&o.stderr))
}

/// The paths of archive `a` in `dir` (`aqr -t -q`).
fn paths(dir: &Path, a: &str) -> Vec<String> {
    let (ok, out, err) = aqr_in(dir, &["-t", "-q", a]);
    assert!(ok, "{err}");
    out.lines().map(String::from).collect()
}

#[test]
fn followed() {
    let d = scratch("followed");
    // elsewhere/ (outside the tree), tree/ with links into it, to a file in the tree, nowhere,
    // and to a directory the link is in
    fs::create_dir_all(d.join("elsewhere/sub")).unwrap();
    fs::write(d.join("elsewhere/x.txt"), b"x").unwrap();
    fs::write(d.join("elsewhere/sub/y.txt"), b"y").unwrap();
    fs::create_dir_all(d.join("tree/in")).unwrap();
    fs::write(d.join("tree/a.txt"), b"a").unwrap();
    symlink("../elsewhere", d.join("tree/dir")).unwrap();
    symlink("../elsewhere/x.txt", d.join("tree/file")).unwrap();
    symlink("a.txt", d.join("tree/same")).unwrap();
    symlink("nothing", d.join("tree/none")).unwrap();
    symlink("..", d.join("tree/in/up")).unwrap();

    // not followed: links as links
    let (ok, _, err) = aqr_in(&d, &["-q", "-o", "plain.aqr", "tree"]);
    assert!(ok, "{err}");
    assert_eq!(paths(&d, "plain.aqr"), ["a.txt", "dir", "file", "in/", "in/up", "none", "same"]);

    // followed: what they lead to at their names, those that cannot be kept as links and told
    let (ok, _, err) = aqr_in(&d, &["-L", "-o", "deref.aqr", "tree"]);
    assert!(ok, "{err}");
    assert!(err.contains("tree/none: a link leading nowhere, kept as a link"), "{err}");
    assert!(err.contains("tree/in/up: a link to a directory it is in, kept as a link"), "{err}");
    assert_eq!(
        paths(&d, "deref.aqr"),
        ["a.txt", "dir/", "dir/sub/", "dir/sub/y.txt", "dir/x.txt", "file", "in/", "in/up", "none", "same"]
    );
    let (ok, _, err) = aqr_in(&d, &["-q", "-d", "-o", "out", "deref.aqr"]);
    assert!(ok, "{err}");
    let out = d.join("out");
    assert_eq!(fs::read(out.join("dir/sub/y.txt")).unwrap(), b"y");
    assert_eq!(fs::read(out.join("file")).unwrap(), b"x");
    assert!(!fs::symlink_metadata(out.join("same")).unwrap().is_symlink());
    assert_eq!(fs::read(out.join("same")).unwrap(), b"a");
    assert_eq!(fs::read_link(out.join("none")).unwrap(), Path::new("nothing"));

    // given: a link alone (to a directory, to a file) or among others
    symlink("tree", d.join("tl")).unwrap();
    symlink("elsewhere/x.txt", d.join("xl")).unwrap();
    let (ok, _, err) = aqr_in(&d, &["-q", "-o", "tl.aqr", "tl"]);
    assert!(ok, "{err}");
    assert_eq!(paths(&d, "tl.aqr"), ["tl"]);
    let (ok, _, err) = aqr_in(&d, &["-q", "-L", "-o", "tl-L.aqr", "tl"]);
    assert!(ok, "{err}");
    assert_eq!(paths(&d, "tl-L.aqr")[..2], ["a.txt", "dir/"]);
    let (ok, _, err) = aqr_in(&d, &["-q", "xl"]);
    assert!(ok, "{err}");
    assert_eq!(paths(&d, "xl.aqr"), ["xl"]);
    let (ok, _, err) = aqr_in(&d, &["-q", "-L", "-f", "xl"]);
    assert!(ok, "{err}");
    assert_eq!(paths(&d, "xl.aqr"), ["<Unnamed>"]);
    let (ok, _, err) = aqr_in(&d, &["-q", "-L", "-o", "two.aqr", "xl", "tl"]);
    assert!(ok, "{err}");
    assert_eq!(paths(&d, "two.aqr")[..3], ["tl/", "tl/a.txt", "tl/dir/"]);
    assert_eq!(paths(&d, "two.aqr").last().unwrap(), "xl");

    // where it is not taken
    for (args, why) in [
        (&["-L", "--rm", "tl"][..], "--rm with -L"),
        (&["-L", "-d", "tl.aqr"], "-L is for compressing"),
        (&["-L", "-t", "tl.aqr"], "-L is for compressing"),
    ] {
        let (ok, _, err) = aqr_in(&d, args);
        assert!(!ok && err.contains(why), "{args:?}: {err}");
    }
    assert!(d.join("tl").exists());
    fs::remove_dir_all(&d).unwrap();
}
