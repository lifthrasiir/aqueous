//! Names of an archive taken as one by a file system through `aqr -d` (spec, "Extraction"):
//! entries of names taken as one are a conflict, the later not made; a link whose target names a
//! link by a name of another case is not made (it could lead out of the directory extracted
//! into), nor one going round in a loop of links (both made with `--links all`); and the
//! metadata of what was made is applied to that alone, not to what a link made over it leads
//! to.
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
    extract_with(d, tree, force, &[])
}

fn extract_with(d: &Path, tree: &Tree, force: bool, args: &[&str]) -> (PathBuf, String) {
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
    c.args(args);
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

/// With `--links all`, the links refused otherwise made as they are, and nothing told of them:
/// those going round in a loop, an absolute one and one going above the directory.
#[test]
fn links_all() {
    let d = scratch(&std::env::temp_dir(), "links-all");
    let tree = Tree {
        root: Extra::default(),
        entries: vec![
            entry("l1", EntryNode::Link("l2".into()), None),
            entry("l2", EntryNode::Link("l1".into()), None),
            entry("abs", EntryNode::Link("/etc".into()), None),
            entry("up", EntryNode::Link("../victim".into()), None),
        ],
    };
    let (out, err) = extract_with(&d, &tree, false, &["--links", "all"]);
    for (name, to) in [("l1", "l2"), ("l2", "l1"), ("abs", "/etc"), ("up", "../victim")] {
        assert_eq!(fs::read_link(out.join(name)).unwrap(), Path::new(to), "{name}: {err}");
    }
    assert!(!err.contains("a link"), "{err}");
    // (inside, the default, as given)
    fs::remove_dir_all(&out).unwrap();
    let (out, err) = extract_with(&d, &tree, false, &["--links", "inside"]);
    assert!(err.contains("abs: a link reaching out of") && fs::symlink_metadata(out.join("abs")).is_err(), "{err}");
    fs::remove_dir_all(&d).unwrap();
}

/// With `-f`, a link (or a file) where a directory of the archive goes is taken away and the
/// directory made, never followed: the archive's own directory (by `-o`, by its name, and with
/// `--auto-dir`) and a directory of the tree. Without `-f`, it is told as there (where a
/// directory there is taken as it is: of a path selected).
#[test]
fn force_over_links_to_directories() {
    let d = scratch(&std::env::temp_dir(), "force-links");
    let victim = d.join("victim");
    let tree = Tree {
        root: Extra::default(),
        entries: vec![
            entry("sub", EntryNode::Dir, None),
            Entry { parent: Some(0), ..entry("x", EntryNode::File(0), None) },
            entry("plain", EntryNode::Dir, None),
            entry("y", EntryNode::File(1), None),
        ],
    };
    let check = |out: &Path, err: &str| {
        assert_eq!(fs::read_dir(&victim).unwrap().count(), 0, "written through a link: {err}");
        for p in [out, &out.join("sub"), &out.join("plain")] {
            assert!(fs::symlink_metadata(p).unwrap().is_dir(), "{}: {err}", p.display());
        }
        assert_eq!(fs::read(out.join("sub/x")).unwrap(), b"content 0", "{err}");
    };
    let fresh = |at: &Path| {
        let _ = fs::remove_dir_all(&victim);
        fs::create_dir(&victim).unwrap();
        let _ = fs::remove_file(at).or_else(|_| fs::remove_dir_all(at));
    };

    // directories of the tree: a link to one, a file
    let out = d.join("out");
    fresh(&out);
    fs::create_dir(&out).unwrap();
    std::os::unix::fs::symlink(&victim, out.join("sub")).unwrap();
    fs::write(out.join("plain"), b"a file").unwrap();
    let (_, err) = extract_with(&d, &tree, true, &[]);
    check(&out, &err);
    fs::remove_dir_all(out.join("sub")).unwrap();
    std::os::unix::fs::symlink(&victim, out.join("sub")).unwrap();
    // (without -f: of a path selected, into a directory there already)
    let (_, err) = extract_with(&d, &tree, false, &["sub"]);
    assert!(err.contains("sub exists (-f to overwrite)"), "{err}");
    assert_eq!(fs::read_dir(&victim).unwrap().count(), 0, "{err}");

    // the archive's own directory, by -o
    fresh(&out);
    std::os::unix::fs::symlink(&victim, &out).unwrap();
    let (_, err) = extract_with(&d, &tree, true, &[]);
    check(&out, &err);

    // by its name, and with --auto-dir (two entries at the top: the archive's directory)
    for args in [&[][..], &["--auto-dir"][..]] {
        let own = d.join("a");
        fresh(&own);
        std::os::unix::fs::symlink(&victim, &own).unwrap();
        let o = Command::new(env!("CARGO_BIN_EXE_aqr"))
            .args(["-d", "-f"])
            .args(args)
            .arg(d.join("a.aqr"))
            .output()
            .unwrap();
        check(&own, &String::from_utf8_lossy(&o.stderr));
    }
    fs::remove_dir_all(&d).unwrap();
}

/// With `-f`, a directory where something else of the archive goes (a file, a hard link, a
/// link) is taken away with what is under it, where it is in the directory extracted into; not
/// where it is not (renamed to `.`), nor where it holds the archive.
#[test]
fn force_over_directories() {
    let d = scratch(&std::env::temp_dir(), "force-dirs");
    let grouped = |name: &str, c| Entry {
        extra: Extra { link_group: Some(1), ..Extra::default() },
        ..entry(name, EntryNode::File(c), None)
    };
    let tree = Tree {
        root: Extra::default(),
        entries: vec![
            entry("f", EntryNode::File(0), None),
            grouped("h1", 1),
            grouped("h2", 1),
            entry("l", EntryNode::Link("f".into()), None),
        ],
    };
    let out = d.join("out");
    let fill = || {
        for n in ["f", "h2", "l"] {
            fs::create_dir_all(out.join(n).join("deeper")).unwrap();
            fs::write(out.join(n).join("deeper/x"), b"x").unwrap();
        }
    };
    fs::create_dir(&out).unwrap();
    fill();
    // (without -f: told as there)
    let (_, err) = extract_with(&d, &tree, false, &["f"]);
    assert!(err.contains("f exists (-f to overwrite)"), "{err}");
    let (_, err) = extract_with(&d, &tree, true, &[]);
    assert_eq!(fs::read(out.join("f")).unwrap(), b"content 0", "{err}");
    assert_eq!(fs::read_link(out.join("l")).unwrap(), Path::new("f"), "{err}");
    {
        use std::os::unix::fs::MetadataExt;
        let (a, b) = (fs::metadata(out.join("h1")).unwrap(), fs::metadata(out.join("h2")).unwrap());
        assert_eq!((a.ino(), a.nlink()), (b.ino(), 2), "{err}");
    }

    // renamed to `.`: not taken away
    let w = d.join("w");
    fs::create_dir_all(w.join("keep")).unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_aqr"))
        .current_dir(&w)
        .args(["-d", "-f", "../a.aqr", "f", "--rename", "."])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success() && err.contains("is a directory, not taken away"), "{err}");
    assert!(w.join("keep").is_dir(), "{err}");

    // holding the archive: not taken away
    fs::remove_dir_all(&out).unwrap();
    fs::create_dir_all(out.join("f")).unwrap();
    fs::copy(d.join("a.aqr"), out.join("f/a.aqr")).unwrap();
    let inner = out.join("f/a.aqr");
    let o = Command::new(env!("CARGO_BIN_EXE_aqr"))
        .args(["-d".as_ref(), "-f".as_ref(), "-o".as_ref(), out.as_os_str(), inner.as_os_str()])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success() && err.contains("is a directory, not taken away"), "{err}");
    assert!(inner.is_file(), "{err}");
    fs::remove_dir_all(&d).unwrap();
}
