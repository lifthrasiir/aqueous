//! Names of an archive taken as one by a file system taking names case and normalization aside
//! (macOS's APFS by default) through `aqr -d` (spec, "Extraction"): entries of names taken as
//! one are a conflict, the later not made; a link whose target names a link by a name of another
//! case is not made (it could lead out of the directory extracted into); and the metadata of what
//! was made is applied to that alone, not to what a link made over it leads to.
#![cfg(target_os = "macos")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
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

fn entry(name: &str, node: EntryNode, mode: Option<u32>) -> Entry {
    Entry { parent: None, name: name.into(), node, extra: Extra { mode, ..Extra::default() } }
}

/// Whether `d` takes names case aside.
fn case_insensitive(d: &std::path::Path) -> bool {
    fs::write(d.join("probe"), b"").unwrap();
    let r = d.join("PROBE").exists();
    fs::remove_file(d.join("probe")).unwrap();
    r
}

fn extract(d: &std::path::Path, tree: &Tree, force: bool) -> (PathBuf, String) {
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

/// `A` a link to `.`, and `B` one to `a/../victim`: `a` is `A` here, so `B` reaches the
/// directory above the one extracted into. `C`, to `A` as it is, is made.
#[test]
fn link_through_other_case() {
    let d = scratch("case-link");
    if !case_insensitive(&d) {
        eprintln!("{} takes names by case: not tested", d.display());
        return;
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
        "B reaches {reached:?}; {err}"
    );
    assert!(err.contains("a link through a, "), "{err}");
    assert_eq!(fs::read_link(out.join("C")).unwrap(), std::path::Path::new("A"), "{err}");
    fs::remove_dir_all(&d).unwrap();
}

/// Entries of names taken as one: `X` after file `x`, `é` decomposed after `é` composed, and
/// directory `D` (with a file in it) after `d`, none of the later made (but told), with `-f` or
/// not; and nothing written over.
#[test]
fn names_taken_as_one() {
    for force in [false, true] {
        let d = scratch(&format!("case-clash-{force}"));
        if !case_insensitive(&d) {
            eprintln!("{} takes names by case: not tested", d.display());
            return;
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
        assert_eq!(fs::read_to_string(out.join("x")).unwrap(), "content 0", "{err}");
        assert_eq!(fs::read_to_string(out.join("\u{e9}")).unwrap(), "content 2", "{err}");
        assert!(!out.join("d").join("y").exists(), "{err}");
        let mut names: Vec<_> = fs::read_dir(&out).unwrap().map(|e| e.unwrap().file_name()).collect();
        names.sort();
        assert_eq!(names, ["d", "x", "\u{e9}"], "{err}");
        for n in ["X", "e\u{301}", "D"] {
            assert!(err.contains(&format!("{n}: not extracted")), "{n}: {err}");
        }
        fs::remove_dir_all(&d).unwrap();
    }
}

/// With `-f`: file `b` written over by link `B` (`a/../victim`, out of the directory), and the
/// mode of `b` then applied to what `B` leads to.
#[test]
fn mode_through_other_case() {
    let d = scratch("case-mode");
    if !case_insensitive(&d) {
        eprintln!("{} takes names by case: not tested", d.display());
        return;
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
    assert_eq!(mode, 0o600, "{err}");
    fs::remove_dir_all(&d).unwrap();
}
