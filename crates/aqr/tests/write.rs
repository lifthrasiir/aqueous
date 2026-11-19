//! Writing and appending archives at the edges (docs/edge-tests-todo.md, "Writing and appending").

mod common;
use common::{files, stderr, text};

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn scratch(name: &str) -> PathBuf {
    common::scratch("write", name)
}

fn aqr(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aqr")).current_dir(dir).args(args).stdin(Stdio::null()).output().unwrap()
}

fn ok(dir: &Path, args: &[&str]) -> Output {
    let o = aqr(dir, args);
    assert!(o.status.success(), "aqr {args:?}: {}", stderr(&o));
    o
}

/// A directory `src` of `n` text files under `d`.
fn source(d: &Path, n: u64) -> Vec<(String, Vec<u8>)> {
    let src = d.join("src");
    fs::create_dir_all(&src).unwrap();
    for i in 0..n {
        fs::write(src.join(format!("t{i}.txt")), text(10 + i, 3000 + 700 * i as usize)).unwrap();
    }
    files(&src)
}

fn extracted(d: &Path, name: &str) -> Vec<(String, Vec<u8>)> {
    let out = format!("out-{name}");
    let _ = fs::remove_dir_all(d.join(&out));
    ok(d, &["-q", "-d", "-o", &out, name]);
    files(&d.join(&out))
}

/// write/1: an archive of mirror blocks (`-A`) is not appended to, journalled or edited in
/// place: what was written after its mirror blocks would go unmirrored. It is left as it is.
#[test]
fn mirrored_archive_not_appended() {
    let d = scratch("mirrored");
    source(&d, 5);
    ok(&d, &["-q", "-A", "-o", "m.aqr", "src"]);
    let before = fs::read(d.join("m.aqr")).unwrap();
    fs::write(d.join("src/new.txt"), text(99, 5000)).unwrap();
    let cases: [&[&str]; 5] = [
        &["-a", "m.aqr", "src"],
        &["-a", "-j", "x", "m.aqr", "src"],
        &["--tag", "x", "m.aqr"],
        &["--delete", "m.aqr", "t1.txt"],
        &["--compact", "m.aqr"],
    ];
    for args in cases {
        let o = aqr(&d, args);
        assert!(!o.status.success() && stderr(&o).contains("mirror blocks"), "{args:?}: {}", stderr(&o));
        assert_eq!(fs::read(d.join("m.aqr")).unwrap(), before, "{args:?}");
    }
    assert_eq!(extracted(&d, "m.aqr").len(), 5);
    // (an archive without them is: the check is not of every archive)
    ok(&d, &["-q", "-o", "p.aqr", "src"]);
    ok(&d, &["-q", "-a", "p.aqr", "src"]);
    fs::remove_dir_all(&d).unwrap();
}

/// The length of a stub to put before archive `a` so that its footer's first bytes (those a
/// commit makes a spacer block) cross a sector of the file, or none.
fn stub_for_crossing(a: &[u8], crossing: bool) -> usize {
    let at = aqr_archive::locate(a).unwrap();
    let (footer, _) = aqr_archive::footer_of(&a[at.clone()]).unwrap();
    let n = aqr_archive::kill(footer.len()).len();
    (0..512).find(|p| aqr_archive::crossing((p + at.start + footer.start) as u64, n) == crossing).unwrap()
}

/// write/2: a footer across a sector of the file is written over (the file cut at it, the new
/// footer after) rather than made a spacer block; behind a stub too. A file cut as a crash
/// between the two leaves it, whole up to the footer, is still appended to.
#[test]
fn footer_across_a_sector() {
    let d = scratch("sector");
    source(&d, 6);
    ok(&d, &["-q", "-o", "a.aqr", "src"]);
    let a = fs::read(d.join("a.aqr")).unwrap();
    for crossing in [false, true] {
        let stub = vec![b'#'; stub_for_crossing(&a, crossing)];
        let name = format!("s{crossing}.aqr");
        fs::write(d.join(&name), [&stub[..], &a].concat()).unwrap();
        fs::write(d.join("src/new.txt"), text(99, 5000)).unwrap();
        let o = ok(&d, &["-v", "-a", &name, "src"]);
        assert_eq!(stderr(&o).contains("across a sector"), crossing, "{}", stderr(&o));
        let got = fs::read(d.join(&name)).unwrap();
        assert!(got.starts_with(&stub) && got.len() > a.len() + stub.len());
        assert_eq!(extracted(&d, &name), files(&d.join("src")));
        let t = ok(&d, &["-K", &name]);
        assert!(!stderr(&t).contains("incomplete"), "{}", stderr(&t));
        // and again, from the file as it is now
        fs::write(d.join("src/new.txt"), text(98, 7000)).unwrap();
        ok(&d, &["-q", "-a", &name, "src"]);
        assert_eq!(extracted(&d, &name), files(&d.join("src")));
        fs::remove_file(d.join("src/new.txt")).unwrap();
    }
    // the crash: the file cut at its footer (as `set_len` left it), before the new one is written
    let at = aqr_archive::locate(&a).unwrap();
    let (footer, _) = aqr_archive::footer_of(&a[at.clone()]).unwrap();
    fs::write(d.join("cut.aqr"), &a[..footer.start]).unwrap();
    let o = ok(&d, &["-d", "-o", "cut-out", "cut.aqr"]);
    assert!(stderr(&o).contains("footer"), "{}", stderr(&o));
    assert_eq!(files(&d.join("cut-out")), source(&d, 6));
    // (and to be appended to: told what it is, the file left alone)
    let o = aqr(&d, &["-a", "cut.aqr", "src"]);
    assert!(!o.status.success() && stderr(&o).contains("cut.aqr"), "{}", stderr(&o));
    assert_eq!(fs::read(d.join("cut.aqr")).unwrap(), &a[..footer.start]);
    fs::remove_dir_all(&d).unwrap();
}
