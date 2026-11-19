//! out/6 and write/5 on Windows: destination files in use (opened by the test without sharing) or
//! read-only, and an input unreadable as it is locked.
#![cfg(windows)]

mod common;
use common::{files, stderr, text};

use std::fs;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const ERROR_SHARING_VIOLATION: &str = "os error 32";
const ERROR_ACCESS_DENIED: &str = "os error 5";

fn scratch(name: &str) -> PathBuf {
    common::scratch("fileswin", name)
}

fn aqr(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aqr")).current_dir(dir).args(args).stdin(Stdio::null()).output().unwrap()
}

fn ok(dir: &Path, args: &[&str]) -> Output {
    let o = aqr(dir, args);
    assert!(o.status.success(), "aqr {args:?}: {}", stderr(&o));
    o
}

/// `aqr args` failing with exit code 1 and a message containing each of `told`.
fn fails(dir: &Path, args: &[&str], told: &[&str]) -> String {
    let o = aqr(dir, args);
    let err = stderr(&o);
    assert_eq!(o.status.code(), Some(1), "aqr {args:?}: {err}");
    assert!(!err.contains("panicked"), "{err}");
    for t in told {
        assert!(err.contains(t), "aqr {args:?}: {t:?} not in {err}");
    }
    err
}

/// `path` opened for reading and writing, shared with no one.
fn hold(path: &Path) -> fs::File {
    fs::OpenOptions::new().read(true).write(true).share_mode(0).open(path).unwrap()
}

fn readonly(path: &Path, on: bool) {
    let mut perm = fs::metadata(path).unwrap().permissions();
    perm.set_readonly(on);
    fs::set_permissions(path, perm).unwrap();
}

/// A directory `src` of three files, archived as `a.aqr`.
fn archive(d: &Path) -> Vec<(String, Vec<u8>)> {
    let src = d.join("src");
    fs::create_dir_all(&src).unwrap();
    for (i, n) in ["a.txt", "b.txt", "c.txt"].into_iter().enumerate() {
        fs::write(src.join(n), text(i as u64, 5000)).unwrap();
    }
    ok(d, &["-q", "-o", "a.aqr", "src"]);
    files(&src)
}

fn names(d: &Path) -> Vec<String> {
    let mut v: Vec<String> =
        fs::read_dir(d).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into()).collect();
    v.sort();
    v
}

/// out/6: a file of the destination held open by another process: without `-f` the destination
/// is there already, told; with it the file is told (the system's reason), the others written
/// whole, the one held left as it was and nothing beside it; extracted again once it is free.
#[test]
fn extracting_over_a_file_in_use() {
    let d = scratch("in-use");
    let want = archive(&d);
    fs::create_dir_all(d.join("x")).unwrap();
    fs::write(d.join("x/b.txt"), b"old").unwrap();
    let held = hold(&d.join("x/b.txt"));
    fails(&d, &["-d", "-o", "x", "a.aqr"], &["x exists (-f to overwrite)"]);
    assert_eq!(names(&d.join("x")), ["b.txt"]);
    fails(&d, &["-d", "-f", "-o", "x", "a.aqr"], &["x\\b.txt", ERROR_SHARING_VIOLATION]);
    drop(held);
    assert_eq!(names(&d.join("x")), ["a.txt", "b.txt", "c.txt"]);
    assert_eq!(fs::read(d.join("x/b.txt")).unwrap(), b"old");
    for (n, data) in want.iter().filter(|(n, _)| n != "b.txt") {
        assert_eq!(&fs::read(d.join("x").join(n)).unwrap(), data, "{n}");
    }
    ok(&d, &["-q", "-d", "-f", "-o", "x", "a.aqr"]);
    assert_eq!(files(&d.join("x")), want);
    fs::remove_dir_all(&d).unwrap();
}

/// out/6: the destination itself a file held open (no directory there to extract into): told,
/// whatever `-f`, the file as it was.
#[test]
fn extracting_into_a_file_in_use() {
    let d = scratch("into-file");
    archive(&d);
    fs::write(d.join("x"), b"old").unwrap();
    let held = hold(&d.join("x"));
    fails(&d, &["-d", "-o", "x", "a.aqr"], &["x exists (-f to overwrite)"]);
    let o = aqr(&d, &["-d", "-f", "-o", "x", "a.aqr"]);
    assert!(!o.status.success() && !stderr(&o).contains("panicked"), "{}", stderr(&o));
    drop(held);
    assert_eq!(fs::read(d.join("x")).unwrap(), b"old");
    assert_eq!(names(&d), ["a.aqr", "src", "x"]);
    fs::remove_dir_all(&d).unwrap();
}

/// out/6: a read-only file of the destination is written over with `-f` (replaced, as the
/// attribute stops a write to it, not its replacement), and not without.
#[test]
fn extracting_over_a_read_only_file() {
    let d = scratch("read-only");
    let want = archive(&d);
    fs::create_dir_all(d.join("x")).unwrap();
    fs::write(d.join("x/b.txt"), b"old").unwrap();
    readonly(&d.join("x/b.txt"), true);
    fails(&d, &["-d", "-o", "x", "a.aqr"], &["x exists (-f to overwrite)"]);
    assert_eq!(fs::read(d.join("x/b.txt")).unwrap(), b"old");
    ok(&d, &["-q", "-d", "-f", "-o", "x", "a.aqr"]);
    assert_eq!(files(&d.join("x")), want);
    fs::remove_dir_all(&d).unwrap();
}

/// out/6: the archive being made held open by another process: without `-f` it is there already;
/// with it the failure is told once with its path, the archive as it was and no part of the new
/// one left. Appending to it is told too.
#[test]
fn archive_in_use() {
    let d = scratch("archive-in-use");
    archive(&d);
    let before = fs::read(d.join("a.aqr")).unwrap();
    fs::write(d.join("src/new.txt"), text(9, 4000)).unwrap();
    let held = hold(&d.join("a.aqr"));
    fails(&d, &["-o", "a.aqr", "src"], &["a.aqr exists (-f to overwrite)"]);
    let err = fails(&d, &["-f", "-o", "a.aqr", "src"], &["a.aqr", ERROR_ACCESS_DENIED]);
    assert_eq!(err.matches("a.aqr:").count(), 1, "{err}");
    fails(&d, &["-a", "a.aqr", "src"], &["a.aqr", ERROR_SHARING_VIOLATION]);
    drop(held);
    assert_eq!(names(&d), ["a.aqr", "src"]);
    assert_eq!(fs::read(d.join("a.aqr")).unwrap(), before);
    ok(&d, &["-q", "-a", "a.aqr", "src"]);
    fs::remove_dir_all(&d).unwrap();
}

/// write/5: a read-only archive is replaced with `-f` (the new one, not read-only), and appended
/// to not at all (even with nothing to add: it is opened to write): told, the file as it was.
#[test]
fn read_only_archive() {
    let d = scratch("read-only-archive");
    archive(&d);
    let a = d.join("a.aqr");
    let before = fs::read(&a).unwrap();
    readonly(&a, true);
    fails(&d, &["-a", "a.aqr", "src"], &["a.aqr", ERROR_ACCESS_DENIED]);
    fs::write(d.join("src/new.txt"), text(9, 4000)).unwrap();
    for args in [&["-a", "a.aqr", "src"][..], &["--atomic", "-a", "a.aqr", "src"]] {
        fails(&d, args, &["a.aqr", ERROR_ACCESS_DENIED]);
        assert_eq!(fs::read(&a).unwrap(), before, "{args:?}");
        assert!(fs::metadata(&a).unwrap().permissions().readonly());
    }
    assert_eq!(names(&d), ["a.aqr", "src"]);
    ok(&d, &["-q", "-f", "-o", "a.aqr", "src"]);
    assert!(!fs::metadata(&a).unwrap().permissions().readonly());
    ok(&d, &["-d", "-o", "x", "a.aqr"]);
    assert_eq!(files(&d.join("x")), files(&d.join("src")));
    fs::remove_dir_all(&d).unwrap();
}

/// write/5: an input locked by another process (opened without sharing) cannot be read: told by
/// its path, no archive made nor part of one left, nothing removed with `--rm`, an archive being
/// appended to as it was; all well once it is free.
#[test]
fn locked_input() {
    let d = scratch("locked-input");
    archive(&d);
    let a = fs::read(d.join("a.aqr")).unwrap();
    let held = hold(&d.join("src/b.txt"));
    fails(&d, &["-o", "n.aqr", "src"], &["src\\b.txt", ERROR_SHARING_VIOLATION]);
    fails(&d, &["-o", "n.aqr", "src/b.txt"], &["src/b.txt", ERROR_SHARING_VIOLATION]);
    fails(&d, &["--rm", "-o", "n.aqr", "src"], &["src\\b.txt", ERROR_SHARING_VIOLATION]);
    // (appending: b.txt is in the archive already, and not read again; a new file locked is)
    ok(&d, &["-q", "-a", "a.aqr", "src"]);
    assert_eq!(fs::read(d.join("a.aqr")).unwrap(), a);
    fs::write(d.join("src/new.txt"), text(9, 4000)).unwrap();
    let held_new = hold(&d.join("src/new.txt"));
    fails(&d, &["-a", "a.aqr", "src"], &["new.txt", ERROR_SHARING_VIOLATION]);
    assert_eq!(fs::read(d.join("a.aqr")).unwrap(), a);
    drop(held_new);
    drop(held);
    assert_eq!(names(&d), ["a.aqr", "src"]);
    assert_eq!(names(&d.join("src")), ["a.txt", "b.txt", "c.txt", "new.txt"]);
    ok(&d, &["-q", "-o", "n.aqr", "src"]);
    ok(&d, &["-q", "-a", "a.aqr", "src"]);
    fs::remove_dir_all(&d).unwrap();
}
