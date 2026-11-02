//! Names of an archive written as this system takes them (`docs/names-design.md`): names of no
//! declared encoding (`raw`) decoded (`-E`) or kept as bytes, lone surrogates, normalization
//! (`--normalize`), and names this system cannot write (`--bad-names`).
//!
//! Where the temporary directory is: macOS's APFS (names UTF-8 alone, normalization kept as
//! written) or a file system of Linux taking names as bytes.
#![cfg(unix)]

use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use aqr_compress::{Input, Options, compress_tree};
use aqr_formula::aqr::{Entry, EntryNode, Extra, Tree};

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("aqr-transcode-{name}-{}", std::process::id()));
    if d.exists() {
        fs::remove_dir_all(&d).unwrap();
    }
    fs::create_dir_all(&d).unwrap();
    d
}

/// A tree of a directory `dir` holding a file `file` (both as encoded: `C1` before bytes of no
/// declared encoding).
fn tree(dir: &[u8], file: &[u8]) -> Tree {
    let e = |parent, name: &[u8], node| Entry { parent, name: name.to_vec(), node, extra: Extra::default() };
    Tree { entries: vec![e(None, dir, EntryNode::Dir), e(Some(0), file, EntryNode::File(0))], ..Tree::default() }
}

/// `tree` archived in `d` and extracted into `d/out` with `args`: whether it succeeded, what was
/// told, and the names made (bytes), as paths below `out`.
fn extract(d: &Path, tree: &Tree, args: &[&str]) -> (bool, String, Vec<Vec<u8>>) {
    let data = b"content".to_vec();
    let archive = d.join("a.aqr");
    let inputs = [Input { name: b"c", data: &data }];
    fs::write(&archive, compress_tree(tree, &inputs, &Options::default()).archive).unwrap();
    let out = d.join("out");
    if out.exists() {
        fs::remove_dir_all(&out).unwrap();
    }
    let o = Command::new(env!("CARGO_BIN_EXE_aqr"))
        .args(["-d".as_ref(), "-o".as_ref(), out.as_os_str(), archive.as_os_str()])
        .args(args)
        .output()
        .unwrap();
    let mut names = Vec::new();
    if out.is_dir() {
        walk(&out, &out, &mut names);
    }
    names.sort();
    (o.status.success(), String::from_utf8_lossy(&o.stderr).into_owned(), names)
}

fn walk(root: &Path, d: &Path, out: &mut Vec<Vec<u8>>) {
    for e in fs::read_dir(d).unwrap() {
        let p = e.unwrap().path();
        out.push(p.strip_prefix(root).unwrap().as_os_str().as_bytes().to_vec());
        if p.is_dir() {
            walk(root, &p, out);
        }
    }
}

fn raw(b: &[u8]) -> Vec<u8> {
    [&[0xc1][..], b].concat()
}

fn euc_kr(s: &str) -> Vec<u8> {
    encoding_rs::EUC_KR.encode(s).0.into_owned()
}

fn paths(v: &[&str]) -> Vec<Vec<u8>> {
    v.iter().map(|s| s.as_bytes().to_vec()).collect()
}

/// Whether the temporary directory takes names of bytes that are not UTF-8 (as Linux's file
/// systems do, not APFS).
fn takes_bytes() -> bool {
    let d = scratch("probe");
    let ok = fs::write(d.join(OsStr::from_bytes(b"\xb0\xa1")), b"").is_ok();
    fs::remove_dir_all(&d).unwrap();
    ok
}

/// Names of EUC-KR kept as bytes (as aqr archives names on Linux not of the locale's UTF-8):
/// decoded where this file system takes no such bytes, or where `-E` says so; kept as they are
/// with `-E raw` where it does.
#[test]
fn raw_names() {
    let d = scratch("raw");
    let t = tree(&raw(&euc_kr("사진")), &raw(&euc_kr("보고서.hwp")));
    let decoded = paths(&["사진", "사진/보고서.hwp"]);
    let bytes = vec![euc_kr("사진"), [euc_kr("사진"), b"/".to_vec(), euc_kr("보고서.hwp")].concat()];
    let (ok, told, names) = extract(&d, &t, &["-E", "euc-kr"]);
    assert!(ok, "{told}");
    assert_eq!(names, decoded, "{told}");
    let (ok, told, names) = extract(&d, &t, &[]);
    assert!(ok, "{told}");
    match takes_bytes() {
        true => assert_eq!(names, bytes, "{told}"),
        false => assert_eq!(names, decoded, "{told}"),
    }
    let (ok, told, names) = extract(&d, &t, &["-E", "raw"]);
    assert!(ok, "{told}");
    match takes_bytes() {
        true => assert_eq!(names, bytes, "{told}"),
        // (not to be written as bytes: percent-escaped, told)
        false => {
            assert_eq!(names, paths(&["%BB%E7%C1%F8", "%BB%E7%C1%F8/%BA%B8%B0%ED%BC%AD.hwp"]), "{told}");
            assert!(told.contains("written as"), "{told}");
        }
    }
    fs::remove_dir_all(&d).unwrap();
}

/// A name of a lone surrogate (from Windows): kept as WTF-8 where names of bytes are taken,
/// else U+FFFD, told; failing with `--bad-names fail`.
#[test]
fn lone_surrogate() {
    let d = scratch("surrogate");
    let t = tree(b"dir", b"a\xed\xa0\x80b.txt");
    let (ok, told, names) = extract(&d, &t, &[]);
    assert!(ok, "{told}");
    match takes_bytes() {
        true => assert_eq!(names, vec![b"dir".to_vec(), b"dir/a\xed\xa0\x80b.txt".to_vec()], "{told}"),
        false => {
            assert_eq!(names, paths(&["dir", "dir/a\u{fffd}b.txt"]), "{told}");
            assert!(told.contains("written as"), "{told}");
        }
    }
    let (ok, told, names) = extract(&d, &t, &["--bad-names", "replace"]);
    assert!(ok, "{told}");
    assert_eq!(names, paths(&["dir", "dir/a\u{fffd}b.txt"]), "{told}");
    let (ok, told, names) = extract(&d, &t, &["--bad-names", "fail"]);
    if !takes_bytes() {
        assert!(!ok && names.iter().all(|n| !n.starts_with(b"dir/")), "{told}");
    }
    let _ = (ok, names);
    fs::remove_dir_all(&d).unwrap();
}

/// Decomposed names (as macOS writes them) composed on Linux, kept on macOS; CJK
/// compatibility ideographs (singletons of NFC) kept but with `--normalize nfc`.
#[test]
fn normalization() {
    let d = scratch("nfc");
    // (가 decomposed, é decomposed after 豈 U+F900, which NFC makes U+8C48)
    let t = tree("\u{1100}\u{1161}".as_bytes(), "\u{f900}e\u{301}.txt".as_bytes());
    let (ok, told, names) = extract(&d, &t, &[]);
    assert!(ok, "{told}");
    let auto = match cfg!(target_os = "macos") {
        true => paths(&["\u{1100}\u{1161}", "\u{1100}\u{1161}/\u{f900}e\u{301}.txt"]),
        false => paths(&["가", "가/\u{f900}é.txt"]),
    };
    assert_eq!(names, auto, "{told}");
    let (ok, told, names) = extract(&d, &t, &["--normalize", "nfc"]);
    assert!(ok, "{told}");
    assert_eq!(names, paths(&["가", "가/\u{8c48}é.txt"]), "{told}");
    let (ok, told, names) = extract(&d, &t, &["--normalize", "keep"]);
    assert!(ok, "{told}");
    assert_eq!(names, paths(&["\u{1100}\u{1161}", "\u{1100}\u{1161}/\u{f900}e\u{301}.txt"]), "{told}");
    fs::remove_dir_all(&d).unwrap();
}

/// A link to a name of no declared encoding, made to the name its target's is decoded as.
#[test]
fn link_decoded() {
    let d = scratch("link");
    let e = |parent, name: Vec<u8>, node| Entry { parent, name, node, extra: Extra::default() };
    let t = Tree {
        entries: vec![
            e(None, raw(&euc_kr("사진")), EntryNode::Dir),
            e(Some(0), raw(&euc_kr("보고서")), EntryNode::File(0)),
            e(Some(0), b"link".to_vec(), EntryNode::Link(raw(&euc_kr("보고서")))),
        ],
        ..Tree::default()
    };
    let (ok, told, names) = extract(&d, &t, &["-E", "euc-kr"]);
    assert!(ok, "{told}");
    assert_eq!(names, paths(&["사진", "사진/link", "사진/보고서"]), "{told}");
    let target = fs::read_link(d.join("out/사진/link")).unwrap();
    assert_eq!(target.as_os_str().as_bytes(), "보고서".as_bytes());
    assert_eq!(fs::read(d.join("out/사진/link")).unwrap(), b"content");
    fs::remove_dir_all(&d).unwrap();
}

/// A ZIP archive of `entries` (names and data, stored) made on MS-DOS, its names of no declared
/// encoding.
fn dos_zip(entries: &[(&[u8], &[u8])]) -> Vec<u8> {
    let (mut out, mut cd) = (Vec::new(), Vec::new());
    for (name, data) in entries {
        let crc = aqr_common::crc32::hash(data);
        let at = out.len() as u32;
        let common = |v: &mut Vec<u8>| {
            v.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0x21, 0]);
            v.extend_from_slice(&crc.to_le_bytes());
            v.extend_from_slice(&(data.len() as u32).to_le_bytes());
            v.extend_from_slice(&(data.len() as u32).to_le_bytes());
            v.extend_from_slice(&(name.len() as u16).to_le_bytes());
            v.extend_from_slice(&[0, 0]);
        };
        out.extend_from_slice(b"PK\x03\x04");
        common(&mut out);
        out.extend_from_slice(name);
        out.extend_from_slice(data);
        cd.extend_from_slice(b"PK\x01\x02\x14\x00");
        common(&mut cd);
        cd.extend_from_slice(&[0; 10]);
        cd.extend_from_slice(&at.to_le_bytes());
        cd.extend_from_slice(name);
    }
    let (start, n) = (out.len() as u32, entries.len() as u16);
    out.extend_from_slice(&cd);
    out.extend_from_slice(b"PK\x05\x06\0\0\0\0");
    out.extend_from_slice(&n.to_le_bytes());
    out.extend_from_slice(&n.to_le_bytes());
    out.extend_from_slice(&(cd.len() as u32).to_le_bytes());
    out.extend_from_slice(&start.to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out
}

/// A ZIP archive of Japanese Windows (Shift_JIS, `\` between names, 表 and ソ of 5C as their
/// second byte) extracted: its names decoded and only then split, on any system; `-E` taking
/// another set.
#[test]
fn zip_of_shift_jis() {
    let d = scratch("zip");
    let sjis = |s: &str| encoding_rs::SHIFT_JIS.encode(s).0.into_owned();
    let (a, b) = (sjis("表示\\ソフト.txt"), sjis("予定表"));
    let zip = d.join("a.zip");
    fs::write(&zip, dos_zip(&[(&a, b"x"), (&b, b"y")])).unwrap();
    let run = |args: &[&str]| {
        let out = d.join("out");
        if out.exists() {
            fs::remove_dir_all(&out).unwrap();
        }
        let o = Command::new(env!("CARGO_BIN_EXE_aqr"))
            .args(["-d".as_ref(), "-o".as_ref(), out.as_os_str(), zip.as_os_str()])
            .args(args)
            .output()
            .unwrap();
        let mut names = Vec::new();
        if out.is_dir() {
            walk(&out, &out, &mut names);
        }
        names.sort();
        (o.status.success(), String::from_utf8_lossy(&o.stderr).into_owned(), names)
    };
    let (ok, told, names) = run(&[]);
    assert!(ok, "{told}");
    assert_eq!(names, paths(&["予定表", "表示", "表示/ソフト.txt"]), "{told}");
    assert_eq!(fs::read(d.join("out/予定表")).unwrap(), b"y");
    // (as Big5, which has 5C as a second byte too: not Shift_JIS's names, and not split as its)
    let (ok, told, names) = run(&["-E", "shift_jis,big5"]);
    assert!(ok, "{told}");
    assert_eq!(names, paths(&["予定表", "表示", "表示/ソフト.txt"]), "{told}");
    fs::remove_dir_all(&d).unwrap();
}
