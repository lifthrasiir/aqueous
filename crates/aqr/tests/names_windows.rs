//! Names of an archive kept within their places on Windows through `aqr -d`: an entry name, a
//! fork name or a link target reaches neither out of the directory extracted into nor into a
//! stream of another file (spec, "Extraction").
#![cfg(windows)]

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use aqr_compress::{Input, Options, compress_tree};
use aqr_formula::aqr::{Entry, EntryNode, Extra, Tree};

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("aqr-names-{name}-{}", std::process::id()));
    if d.exists() {
        fs::remove_dir_all(&d).unwrap();
    }
    fs::create_dir_all(&d).unwrap();
    d
}

/// A lone high surrogate in WTF-8.
const SURROGATE: &[u8] = b"\xed\xa0\x80";

fn file(name: &[u8], content: usize, forks: Vec<(Vec<u8>, usize)>) -> Entry {
    Entry {
        parent: None,
        name: name.to_vec(),
        node: EntryNode::File(content),
        extra: Extra { forks, ..Extra::default() },
    }
}

fn link(name: &str, target: &str) -> Entry {
    Entry { parent: None, name: name.into(), node: EntryNode::Link(target.into()), extra: Extra::default() }
}

#[test]
fn names_kept_within() {
    let d = scratch("within");
    let (deep, out) = (d.join("x").join("y"), d.join("x").join("y").join("out"));
    fs::create_dir_all(&deep).unwrap();
    let contents: Vec<Vec<u8>> = (0..8).map(|i| format!("content {i}").into_bytes()).collect();
    let forks = ["x\\..\\..\\pwn-fork", "s:t", "..", "dot.", "a/b"];
    let tree = Tree {
        root: Extra::default(),
        entries: vec![
            file(b"f", 0, forks.iter().enumerate().map(|(k, n)| (n.as_bytes().to_vec(), 1 + k % 2)).collect()),
            file(&[SURROGATE, b"\\..\\..\\pwn-name"].concat(), 3, Vec::new()),
            file(&[b"x", SURROGATE, b":ads"].concat(), 4, Vec::new()),
            file(b"12:30.txt", 5, Vec::new()),
            link("l", "..\\..\\pwn-link"),
            link("r", "\\Windows"),
            link("c", "12:30.txt"),
        ],
    };
    let inputs: Vec<Input> = contents.iter().map(|c| Input { name: b"c", data: c }).collect();
    let archive = d.join("a.aqr");
    fs::write(&archive, compress_tree(&tree, &inputs, &Options::default()).archive).unwrap();

    let o = Command::new(env!("CARGO_BIN_EXE_aqr"))
        .args(["-d".as_ref(), "-o".as_ref(), out.as_os_str(), archive.as_os_str()])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&o.stderr);

    // nothing out of the directory extracted into
    let beside: Vec<_> = fs::read_dir(&deep).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(beside, ["out"], "{err}");
    // the names with a surrogate written as names (`\` and `:` as `_`)
    let names: Vec<Vec<u16>> = {
        use std::os::windows::ffi::OsStrExt;
        fs::read_dir(&out).unwrap().map(|e| e.unwrap().file_name().encode_wide().collect()).collect()
    };
    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().collect() };
    let lone = |a: &str, b: &str| [wide(a), vec![0xd800], wide(b)].concat();
    assert!(names.contains(&lone("", "_.._.._pwn-name")), "{err}");
    assert!(names.contains(&lone("x", "_ads")), "{err}");
    assert!(!names.contains(&lone("x", "")), "{err}");
    // the forks that can be streams of their own names alone
    assert!(err.contains("fork x\\..\\..\\pwn-fork not written"), "{err}");
    assert!(err.contains("fork s:t not written"), "{err}");
    assert!(err.contains("fork .. not written"), "{err}");
    assert!(err.contains("fork dot. not written"), "{err}");
    assert!(err.contains("fork a/b not written"), "{err}");
    assert_eq!(fs::read_to_string(out.join("f")).unwrap(), "content 0");
    // links within the directory, to the names the entries were written as
    for l in ["l", "r", "c"] {
        if let Ok(t) = fs::read_link(out.join(l)) {
            assert!(t.components().all(|c| matches!(c, Component::Normal(_))), "{l} -> {}", t.display());
        }
    }
    if fs::symlink_metadata(out.join("c")).is_ok() {
        assert_eq!(fs::read_link(out.join("c")).unwrap(), Path::new("12_30.txt"));
        assert_eq!(fs::read_to_string(out.join("c")).unwrap(), "content 5");
    }
    fs::remove_dir_all(&d).unwrap();
}
