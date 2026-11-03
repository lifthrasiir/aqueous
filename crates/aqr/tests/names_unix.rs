//! Names of an archive taken as one by a file system through `aqr -d` (spec, "Extraction"):
//! entries of names taken as one are a conflict, the later not made; a link whose target names a
//! link by a name of another case is not made (it could lead out of the directory extracted
//! into), nor one going round in a loop of links; and the metadata of what was made is applied
//! to that alone, not to what a link made over it leads to.
//!
//! In the temporary directory (macOS's APFS takes names case and normalization aside), and in
//! each directory of `AQR_TEST_DIRS` (separated by `:`), say on Linux of vfat, exFAT, hfsplus,
//! ntfs3 of `nocase` or a casefolded directory of ext4. Each test runs where the file system
//! takes names so (as a probe finds), else tells so.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use aqr_compress::{Input, Options, compress_tree};
use aqr_formula::aqr::{Entry, EntryNode, Extra, Tree};
use unicode_normalization::UnicodeNormalization;

/// The directories to test in.
fn places() -> Vec<PathBuf> {
    let mut v = vec![std::env::temp_dir()];
    if let Some(dirs) = std::env::var_os("AQR_TEST_DIRS") {
        v.extend(std::env::split_paths(&dirs));
    }
    v
}

fn scratch(place: &Path, name: &str) -> PathBuf {
    let d = place.join(format!("aqr-names-{name}-{}", std::process::id()));
    if d.exists() {
        fs::remove_dir_all(&d).unwrap();
    }
    fs::create_dir_all(&d).unwrap();
    d
}

fn entry(name: &str, node: EntryNode, mode: Option<u32>) -> Entry {
    Entry { parent: None, name: name.into(), node, extra: Extra { mode, ..Extra::default() } }
}

/// Whether `d` takes `other` as file `made`'s name.
fn alike(d: &Path, made: &str, other: &str) -> bool {
    let Ok(()) = fs::write(d.join(made), b"") else { return false };
    let r = d.join(other).exists();
    fs::remove_file(d.join(made)).unwrap();
    r
}

/// Whether `d` takes links.
fn links(d: &Path) -> bool {
    let r = std::os::unix::fs::symlink("x", d.join("probe-link")).is_ok();
    let _ = fs::remove_file(d.join("probe-link"));
    r
}

fn extract(d: &Path, tree: &Tree, force: bool) -> (PathBuf, String) {
    let contents: Vec<Vec<u8>> = (0..4).map(|i| format!("content {i}").into_bytes()).collect();
    let inputs: Vec<Input> = contents.iter().map(|c| Input { name: b"c", data: c }).collect();
    let archive = d.join("a.aqr");
    fs::write(&archive, compress_tree(tree, &inputs, &Options::default()).archive).unwrap();
    let out = d.join("out");
    let mut c = Command::new(env!("CARGO_BIN_EXE_aqr"));
    c.args(["-d".as_ref(), "-o".as_ref(), out.as_os_str(), archive.as_os_str()]);
    if force {
        c.arg("-f");
    }
    let o = c.output().unwrap();
    (out, String::from_utf8_lossy(&o.stderr).into_owned())
}

/// The names in `d`, composed (as HFS+ keeps them decomposed).
fn names_in(d: &Path) -> Vec<String> {
    let mut names: Vec<String> =
        fs::read_dir(d).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().nfc().collect()).collect();
    names.sort();
    names
}

/// `A` a link to `.`, and `B` one to `a/../victim`: `a` is `A` here, so `B` reaches the
/// directory above the one extracted into. `C`, to `A` as it is, is made.
#[test]
fn link_through_other_case() {
    for place in places() {
        let d = scratch(&place, "case-link");
        if !alike(&d, "probe", "PROBE") || !links(&d) {
            eprintln!("{} takes names by case, or no links: not tested", d.display());
            fs::remove_dir_all(&d).unwrap();
            continue;
        }
        let tree = Tree {
            root: Extra::default(),
            entries: vec![
                entry("A", EntryNode::Link(".".into()), None),
                entry("B", EntryNode::Link("a/../victim".into()), None),
                entry("C", EntryNode::Link("A".into()), None),
            ],
        };
        fs::write(d.join("victim"), b"outside").unwrap();
        let (out, err) = extract(&d, &tree, false);
        let reached = fs::canonicalize(out.join("B")).ok();
        assert!(
            reached.as_ref().is_none_or(|r| r.starts_with(fs::canonicalize(&out).unwrap())),
            "{}: B reaches {reached:?}; {err}",
            d.display()
        );
        assert!(err.contains("a link through a, "), "{}: {err}", d.display());
        assert_eq!(fs::read_link(out.join("C")).unwrap(), Path::new("A"), "{}: {err}", d.display());
        fs::remove_dir_all(&d).unwrap();
    }
}

/// `x????` a link to `.`, and `B` one to `x😀/../victim`: Linux's hfsplus keeps `x😀` as
/// `x????`, so `B` reaches the directory above the one extracted into. Where the file system
/// takes them so alone.
#[test]
fn link_through_astral() {
    for place in places() {
        let d = scratch(&place, "astral-link");
        if !alike(&d, "x\u{1f600}", "x????") || !links(&d) {
            eprintln!("{} takes x\u{1f600} as other than x????, or no links: not tested", d.display());
            fs::remove_dir_all(&d).unwrap();
            continue;
        }
        let tree = Tree {
            root: Extra::default(),
            entries: vec![
                entry("x????", EntryNode::Link(".".into()), None),
                entry("B", EntryNode::Link("x\u{1f600}/../victim".into()), None),
            ],
        };
        fs::write(d.join("victim"), b"outside").unwrap();
        let (out, err) = extract(&d, &tree, false);
        let reached = fs::canonicalize(out.join("B")).ok();
        assert!(
            reached.as_ref().is_none_or(|r| r.starts_with(fs::canonicalize(&out).unwrap())),
            "{}: B reaches {reached:?}; {err}",
            d.display()
        );
        assert!(err.contains("a link through x\u{1f600}, "), "{}: {err}", d.display());
        fs::remove_dir_all(&d).unwrap();
    }
}

/// Entries of names taken as one: `X` after file `x`, `é` decomposed after `é` composed, and
/// directory `D` (with a file in it) after `d`, none of the later made (but told), with `-f` or
/// not; and nothing written over.
#[test]
fn names_taken_as_one() {
    for place in places() {
        for force in [false, true] {
            let d = scratch(&place, &format!("case-clash-{force}"));
            if !alike(&d, "probe", "PROBE") {
                eprintln!("{} takes names by case: not tested", d.display());
                fs::remove_dir_all(&d).unwrap();
                continue;
            }
            let dir = |name: &str| entry(name, EntryNode::Dir, None);
            let mut entries = vec![
                entry("x", EntryNode::File(0), None),
                entry("X", EntryNode::File(1), None),
                entry("\u{e9}", EntryNode::File(2), None),
                entry("e\u{301}", EntryNode::File(3), None),
                dir("d"),
                dir("D"),
            ];
            entries.push(Entry { parent: Some(5), ..entry("y", EntryNode::File(1), None) });
            let tree = Tree { root: Extra::default(), entries };
            let (out, err) = extract(&d, &tree, force);
            let at = d.display();
            assert_eq!(fs::read_to_string(out.join("x")).unwrap(), "content 0", "{at}: {err}");
            assert_eq!(fs::read_to_string(out.join("\u{e9}")).unwrap(), "content 2", "{at}: {err}");
            assert!(!out.join("d").join("y").exists(), "{at}: {err}");
            assert_eq!(names_in(&out), ["d", "x", "\u{e9}"], "{at}: {err}");
            for n in ["X", "e\u{301}", "D"] {
                assert!(err.contains(&format!("{n}: not extracted")), "{at}: {n}: {err}");
            }
            fs::remove_dir_all(&d).unwrap();
        }
    }
}

/// Names that file systems of short names (FAT) or of a `?` for each byte of characters beyond
/// the Basic Multilingual Plane (Linux's hfsplus) take as one, with `-f`: `LONGFI~1.TXT` after
/// `longfilename.txt`, `x😁` after `x😀`. Where the file system takes them so alone.
#[test]
fn short_and_astral_names() {
    let mut tested = 0;
    for place in places() {
        let d = scratch(&place, "short-astral");
        let cases = [("longfilename.txt", "LONGFI~1.TXT"), ("x\u{1f600}", "x\u{1f601}")];
        for (first, later) in cases {
            if !alike(&d, first, later) {
                eprintln!("{} takes {later} as other than {first}: not tested", d.display());
                continue;
            }
            tested += 1;
            let tree = Tree {
                root: Extra::default(),
                entries: vec![entry(first, EntryNode::File(0), None), entry(later, EntryNode::File(1), None)],
            };
            let (out, err) = extract(&d, &tree, true);
            let at = d.display();
            assert_eq!(fs::read_to_string(out.join(first)).unwrap(), "content 0", "{at}: {err}");
            assert!(err.contains(&format!("{later}: not extracted")), "{at}: {err}");
            fs::remove_dir_all(&out).unwrap();
        }
        fs::remove_dir_all(&d).unwrap();
    }
    eprintln!("{tested} cases tested");
}

/// With `-f`: file `b` written over by link `B` (`a/../victim`: `a` is `A` here, a link to `.`,
/// so out of the directory), and the mode of `b` then applied to what `B` leads to.
#[test]
fn mode_through_other_case() {
    for place in places() {
        let d = scratch(&place, "case-mode");
        if !alike(&d, "probe", "PROBE") || !links(&d) {
            eprintln!("{} takes names by case, or no links: not tested", d.display());
            fs::remove_dir_all(&d).unwrap();
            continue;
        }
        let tree = Tree {
            root: Extra::default(),
            entries: vec![
                {
                    // (of a fork: its metadata applied at the end)
                    let mut b = entry("b", EntryNode::File(0), Some(0o777));
                    b.extra.forks = vec![(b"user.x".to_vec(), 0)];
                    b
                },
                entry("A", EntryNode::Link(".".into()), None),
                entry("B", EntryNode::Link("a/../victim".into()), None),
            ],
        };
        fs::write(d.join("victim"), b"outside").unwrap();
        fs::set_permissions(d.join("victim"), fs::Permissions::from_mode(0o600)).unwrap();
        let (_, err) = extract(&d, &tree, true);
        let mode = fs::metadata(d.join("victim")).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o600, "{}: {err}", d.display());
        fs::remove_dir_all(&d).unwrap();
    }
}

/// `l1` a link to `l2`, `l2` one to `l1`, `s` one to `s/x`: links going round in a loop, told
/// so (not as reaching out); `d`, to `l1/..`, never resolving either. `ok`, to `.`, is made.
#[test]
fn link_loops() {
    let d = scratch(&std::env::temp_dir(), "link-loops");
    let tree = Tree {
        root: Extra::default(),
        entries: vec![
            entry("l1", EntryNode::Link("l2".into()), None),
            entry("l2", EntryNode::Link("l1".into()), None),
            entry("s", EntryNode::Link("s/x".into()), None),
            entry("d", EntryNode::Link("l1/..".into()), None),
            entry("ok", EntryNode::Link(".".into()), None),
        ],
    };
    let (out, err) = extract(&d, &tree, false);
    for name in ["l1", "l2", "s", "d"] {
        assert!(err.contains(&format!("{name}: a link going round in a loop of links, not created")), "{name}: {err}");
        assert!(fs::symlink_metadata(out.join(name)).is_err(), "{name} made");
    }
    assert!(!err.contains("reaching out"), "{err}");
    assert_eq!(fs::read_link(out.join("ok")).unwrap(), Path::new("."), "{err}");
    fs::remove_dir_all(&d).unwrap();
}
