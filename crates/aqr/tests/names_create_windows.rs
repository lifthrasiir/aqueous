//! write/7 on Windows: names at creation. A tree of files Win32 would take for others by their
//! names (trailing dots and spaces, device names, a lone surrogate, a stream, a path past 260
//! characters), made through extended-length paths, is archived, appended to and extracted;
//! names of an archive that this file system takes as one (case) are told, none written over.
//! (`names_windows.rs` has the extraction of names no file system takes.)
#![cfg(windows)]

mod common;
use common::{stderr, text};

use std::ffi::OsString;
use std::fs;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use aqr_archive::{Entry, EntryNode, Extra, Tree};
use aqr_compress::{Input, Options, compress_tree};

fn scratch(name: &str) -> PathBuf {
    common::scratch("namescreate", name)
}

fn aqr(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aqr")).current_dir(dir).args(args).stdin(Stdio::null()).output().unwrap()
}

fn ok(dir: &Path, args: &[&str]) -> Output {
    let o = aqr(dir, args);
    assert!(o.status.success(), "aqr {args:?}: {}", stderr(&o));
    o
}

/// A name with a lone high surrogate (not Unicode: an `OsString` of wide units).
fn lone(prefix: &str) -> OsString {
    let units: Vec<u16> = prefix.encode_utf16().chain([0xd800, b'.' as u16, b't' as u16]).collect();
    OsString::from_wide(&units)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// Files of `names` made in `dir` (an extended-length path), the content of each
/// `text(seed + its index, 3000)`.
fn make(dir: &Path, names: &[OsString], seed: u64) {
    for (i, n) in names.iter().enumerate() {
        fs::write(dir.join(n), text(seed + i as u64, 3000)).unwrap();
    }
}

/// The files of `dir` (an extended-length path), by name in wide units, with their contents.
fn listing(dir: &Path) -> Vec<(Vec<u16>, Vec<u8>)> {
    let mut v: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap())
        .filter(|e| e.file_type().unwrap().is_file())
        .map(|e| (e.file_name().encode_wide().collect::<Vec<u16>>(), fs::read(e.path()).unwrap()))
        .collect();
    v.sort();
    v
}

/// What each of `names` is extracted as (`renamed` where this system does not take it), with the
/// content `make` gave it.
fn expected(names: &[OsString], renamed: &[(&str, &str)], seed: u64) -> Vec<(Vec<u16>, Vec<u8>)> {
    names
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let name: Vec<u16> = n.encode_wide().collect();
            let to = renamed.iter().find(|(f, _)| wide(f) == name).map_or(name, |(_, t)| wide(t));
            (to, text(seed + i as u64, 3000))
        })
        .collect()
}

#[test]
fn names_at_creation() {
    let d = fs::canonicalize(scratch("create")).unwrap();
    let src = d.join("src");
    fs::create_dir_all(&src).unwrap();
    let first: Vec<OsString> = ["trail.", "space ", "CON", "NUL.txt", "aux.tar.gz", "plain"]
        .iter()
        .map(OsString::from)
        .chain([lone("l")])
        .collect();
    make(&src, &first, 0);
    // a stream of `plain`, and a path of over 260 characters
    fs::write(src.join("plain:ads"), b"stream").unwrap();
    let mut deep = src.clone();
    for _ in 0..4 {
        deep.push("d".repeat(80));
    }
    fs::create_dir_all(&deep).unwrap();
    fs::write(deep.join("deep.txt"), text(99, 2000)).unwrap();
    assert!(deep.as_os_str().len() > 330);

    ok(&d, &["-o", "a.aqr", "src"]);
    ok(&d, &["-K", "a.aqr"]);
    let list = String::from_utf8_lossy(&ok(&d, &["-t", "-v", "a.aqr"]).stdout).into_owned();
    // (the table trims the line, a trailing space too)
    for n in ["trail.", "space ", "CON", "NUL.txt", "aux.tar.gz", "deep.txt"] {
        assert!(list.lines().any(|l| l.trim_end().ends_with(n.trim_end())), "{n} not in {list}");
    }

    // the names this system takes written as they are, the others as `_`s (told)
    let renamed = [
        ("CON", "_CON"),
        ("NUL.txt", "_NUL.txt"),
        ("aux.tar.gz", "_aux.tar.gz"),
        ("space ", "space_"),
        ("trail.", "trail_"),
    ];
    let o = ok(&d, &["-d", "-o", "out", "a.aqr"]);
    for (from, to) in &renamed {
        let told = format!("out\\{from}: not a name this system takes, written as {to}");
        assert!(stderr(&o).contains(&told), "{told}: {}", stderr(&o));
    }
    let mut want = expected(&first, &renamed, 0);
    want.sort();
    assert_eq!(listing(&d.join("out")), want);
    assert_eq!(fs::read(d.join("out").join("plain:ads")).unwrap(), b"stream");
    let mut got_deep = d.join("out");
    for _ in 0..4 {
        got_deep.push("d".repeat(80));
    }
    assert_eq!(fs::read(got_deep.join("deep.txt")).unwrap(), text(99, 2000));

    // appended to: the entries there are the same (nothing to append), the new ones added
    let o = ok(&d, &["-a", "a.aqr", "src"]);
    assert!(stderr(&o).contains("nothing to append"), "{}", stderr(&o));
    let second: Vec<OsString> =
        ["new.", "PRN", "lpt1.log", "end "].iter().map(OsString::from).chain([lone("m")]).collect();
    make(&src, &second, 50);
    ok(&d, &["-q", "-a", "a.aqr", "src"]);
    let o = ok(&d, &["-a", "a.aqr", "src"]);
    assert!(stderr(&o).contains("nothing to append"), "{}", stderr(&o));
    ok(&d, &["-K", "a.aqr"]);
    let renamed2 = [("new.", "new_"), ("PRN", "_PRN"), ("lpt1.log", "_lpt1.log"), ("end ", "end_")];
    let o = ok(&d, &["-d", "-o", "out2", "a.aqr"]);
    for (from, to) in &renamed2 {
        let told = format!("out2\\{from}: not a name this system takes, written as {to}");
        assert!(stderr(&o).contains(&told), "{told}: {}", stderr(&o));
    }
    let mut want2 = expected(&first, &renamed, 0);
    want2.extend(expected(&second, &renamed2, 50));
    want2.sort();
    assert_eq!(listing(&d.join("out2")), want2);
    assert_eq!(fs::read(d.join("out2").join("plain:ads")).unwrap(), b"stream");
    fs::remove_dir_all(&d).unwrap();
}

fn file(name: &[u8], content: usize) -> Entry {
    Entry { parent: None, name: name.to_vec(), node: EntryNode::File(content), extra: Extra::default() }
}

/// Names that differ by case alone (made by a tree: this file system holds one of them): the
/// first extracted, the others told and nothing written over, with and without `-f`; appending a
/// directory with one of them keeps the three apart in the archive.
#[test]
fn case_colliding_names() {
    let d = scratch("case");
    let tree = Tree { root: Extra::default(), entries: vec![file(b"Case", 0), file(b"case", 1), file(b"CASE", 2)] };
    let contents: Vec<Vec<u8>> = (0..3).map(|i| format!("content {i}").into_bytes()).collect();
    let inputs: Vec<Input> = contents.iter().map(|c| Input { name: b"c", data: c }).collect();
    fs::write(d.join("a.aqr"), compress_tree(&tree, &inputs, &Options::default()).archive).unwrap();
    for (out, args) in [("o1", &["-d", "-o", "o1", "a.aqr"][..]), ("o2", &["-d", "-f", "-o", "o2", "a.aqr"])] {
        let o = ok(&d, args);
        for n in ["case", "CASE"] {
            let told =
                format!("{out}\\{n}: not extracted, as this file system may take its name as that of {out}\\Case");
            assert!(stderr(&o).contains(&told), "{told}: {}", stderr(&o));
        }
        let got: Vec<_> = fs::read_dir(d.join(out)).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(got, ["Case"]);
        assert_eq!(fs::read_to_string(d.join(out).join("Case")).unwrap(), "content 0");
    }
    fs::create_dir_all(d.join("src")).unwrap();
    fs::write(d.join("src/Case"), "content 0").unwrap();
    fs::write(d.join("src/zzz"), "new").unwrap();
    ok(&d, &["-q", "-a", "a.aqr", "src"]);
    let o = ok(&d, &["-a", "a.aqr", "src"]);
    assert!(stderr(&o).contains("nothing to append"), "{}", stderr(&o));
    let list = String::from_utf8_lossy(&ok(&d, &["-t", "-v", "a.aqr"]).stdout).into_owned();
    for n in ["Case", "case", "CASE", "zzz"] {
        assert_eq!(list.lines().filter(|l| l.ends_with(&format!("  {n}"))).count(), 1, "{n} in {list}");
    }
    ok(&d, &["-K", "a.aqr"]);
    fs::remove_dir_all(&d).unwrap();
}
