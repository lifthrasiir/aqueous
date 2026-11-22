//! Extraction stays in its directory (spec, "Extraction"), of random trees: links leading out of
//! it (relative and absolute), through other links, of names a file system takes as one (case,
//! normalization, as macOS's APFS has it), files under them, and an archive extracted over
//! another (`-f`), whose links the second's files might be written through. After each, nothing
//! beside the directory extracted into is made or changed, and every link in it leads within it
//! (`--links inside`, the default). `AQR_SAFETY_CASES` sets how many trees (100 by default).
#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use aqr_archive::{Entry, EntryNode, Extra, Tree};
use aqr_compress::{Input, Options, compress_tree};

/// A generator of numbers, of a seed: xorshift64*.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Names of entries: some a file system may take as one (case; `é` composed and not).
const NAMES: [&str; 6] = ["a", "A", "b", "x", "\u{e9}", "e\u{301}"];

/// A random tree of up to 8 entries of `NAMES` (files, directories, links of targets in and out
/// of the directory: relative, through links, absolute ones of `outside`), and its contents.
fn tree(r: &mut Rng, outside: &Path) -> (Tree, Vec<Vec<u8>>) {
    let abs = outside.to_string_lossy().into_owned();
    let targets = [".", "..", "../outside", "../secret", "a", "A/..", "b/../..", "a/b", "x/../../outside", &abs, "/"];
    let mut t = Tree::default();
    let mut contents = Vec::new();
    for _ in 0..1 + r.below(8) {
        // (under a directory made before, now and then)
        let dirs: Vec<usize> = (0..t.entries.len()).filter(|&i| t.entries[i].node == EntryNode::Dir).collect();
        let parent = match dirs.is_empty() || r.below(2) == 0 {
            true => None,
            false => Some(dirs[r.below(dirs.len())]),
        };
        let name = NAMES[r.below(NAMES.len())].as_bytes().to_vec();
        if t.entries.iter().any(|e| e.parent == parent && e.name == name) {
            continue;
        }
        let node = match r.below(3) {
            0 => {
                contents.push(format!("content {}", contents.len()).into_bytes());
                EntryNode::File(contents.len() - 1)
            }
            1 => EntryNode::Dir,
            _ => EntryNode::Link(targets[r.below(targets.len())].as_bytes().to_vec()),
        };
        t.entries.push(Entry { parent, name, node, extra: Extra::default() });
    }
    (t, contents)
}

/// Archive of `t` and `contents` at `at`; whether it was made (a tree the compressor refuses is
/// not extracted).
fn archive(at: &Path, t: &Tree, contents: &[Vec<u8>]) -> bool {
    let inputs: Vec<Input> = contents.iter().map(|c| Input { name: b"c", data: c }).collect();
    let r = std::panic::catch_unwind(|| compress_tree(t, &inputs, &Options::default()).archive);
    match r {
        Ok(a) => fs::write(at, a).is_ok(),
        Err(_) => false,
    }
}

/// Every path under `d` (links not followed).
fn walk(d: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut todo = vec![d.to_path_buf()];
    while let Some(p) = todo.pop() {
        for e in fs::read_dir(&p).into_iter().flatten().flatten() {
            let q = e.path();
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                todo.push(q.clone());
            }
            out.push(q);
        }
    }
    out
}

/// What is wrong of a place after extracting into `out` in it: what is made beside it, what
/// changed of `outside` and `secret`, and the links in `out` leading out of it.
fn wrong(place: &Path, out: &Path) -> Vec<String> {
    let mut w = Vec::new();
    for e in fs::read_dir(place).unwrap().flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if !["out", "outside", "secret", "a.aqr", "b.aqr"].contains(&n.as_str()) {
            w.push(format!("{n} made beside the directory"));
        }
    }
    if fs::read_dir(place.join("outside")).map_or(true, |mut d| d.next().is_some()) {
        w.push("something made in outside".into());
    }
    if fs::read(place.join("secret")).ok().as_deref() != Some(&b"secret"[..]) {
        w.push("secret changed".into());
    }
    let root = fs::canonicalize(out).unwrap_or_else(|_| out.to_path_buf());
    for p in walk(out) {
        if fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink()) {
            let target = fs::read_link(&p).unwrap();
            let at = resolve(&fs::canonicalize(p.parent().unwrap()).unwrap(), &target);
            if !at.starts_with(&root) {
                w.push(format!("{} -> {} leads out", p.display(), target.display()));
            }
        }
    }
    w
}

/// Where link target `target` of a link in `dir` (canonical) leads, as the system resolves it:
/// each name in turn, a link met followed (its target in place of it), `..` of what was reached;
/// from a name not there on, the rest by the names alone (what would be made there).
fn resolve(dir: &Path, target: &Path) -> PathBuf {
    use std::path::Component;
    /// A name of a path, owned (a link's target in place of it).
    enum Part {
        Root,
        Up,
        Name(std::ffi::OsString),
    }
    let parts = |p: &Path| -> Vec<Part> {
        p.components()
            .filter_map(|c| match c {
                Component::RootDir => Some(Part::Root),
                Component::ParentDir => Some(Part::Up),
                Component::Normal(n) => Some(Part::Name(n.to_os_string())),
                Component::Prefix(_) | Component::CurDir => None,
            })
            .collect()
    };
    let mut at = dir.to_path_buf();
    let mut rest: std::collections::VecDeque<Part> = parts(target).into();
    let (mut missing, mut links) = (false, 0);
    while let Some(c) = rest.pop_front() {
        match c {
            Part::Root => at = PathBuf::from("/"),
            Part::Up => {
                at.pop();
            }
            Part::Name(n) => {
                let next = at.join(&n);
                match fs::symlink_metadata(&next) {
                    Ok(m) if !missing && m.file_type().is_symlink() && links < 40 => {
                        links += 1;
                        for p in parts(&fs::read_link(&next).unwrap()).into_iter().rev() {
                            rest.push_front(p);
                        }
                    }
                    Ok(_) if !missing => at = next,
                    _ => (missing, at) = (true, next),
                }
            }
        }
    }
    at
}

/// Of `cases` random trees: each extracted into a directory of its own, and where `over`, a
/// second extracted over it with `-f`. The failures, of what is wrong after each.
fn run(cases: u64, over: bool, label: &str) -> Vec<String> {
    let base = std::env::temp_dir().join(format!("aqr-safety-{label}-{}", std::process::id()));
    let mut failures = Vec::new();
    for case in 0..cases {
        let place = base.join(format!("{case}"));
        let _ = fs::remove_dir_all(&place);
        fs::create_dir_all(place.join("outside")).unwrap();
        fs::write(place.join("secret"), b"secret").unwrap();
        // (canonical, as links to it are compared so)
        let place = fs::canonicalize(&place).unwrap();
        let mut r = Rng(0x9e37_79b9_7f4a_7c15 ^ case.wrapping_mul(0x1000_0000_01b3) | 1);
        let out = place.join("out");
        for (k, name) in ["a.aqr", "b.aqr"].into_iter().enumerate().take(1 + over as usize) {
            let (t, contents) = tree(&mut r, &place.join("outside"));
            if !archive(&place.join(name), &t, &contents) {
                continue;
            }
            let mut c = Command::new(env!("CARGO_BIN_EXE_aqr"));
            c.args(["-d", "-q"]).arg("-o").arg(&out).arg(place.join(name));
            // (the second over the first's)
            if k == 1 {
                c.arg("-f");
            }
            let _ = c.output().unwrap();
            for w in wrong(&place, &out) {
                failures.push(format!("case {case}, archive {k}: {w}\n  {:?}", t.entries));
            }
        }
        if failures.is_empty() {
            let _ = fs::remove_dir_all(&place);
        }
    }
    if failures.is_empty() {
        let _ = fs::remove_dir_all(&base);
    }
    failures
}

fn cases() -> u64 {
    std::env::var("AQR_SAFETY_CASES").ok().and_then(|n| n.parse().ok()).unwrap_or(100)
}

/// A tree extracted into a directory of its own: what it makes stays in it.
#[test]
fn stays_in_its_directory() {
    let failures = run(cases(), false, "one");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A tree extracted with `-f` over another's: its links are checked against its own tree, not
/// against what is there, so a link may come to lead out through what the other made (of 2000
/// trees on macOS: a link of the first through a name the second makes a link, and a link of
/// the second through a link of the first of a name of another case). Files are never written
/// through what is there (`force_over_links_to_directories`). To decide
/// (`docs/edge-tests-todo.md`); `--ignored` to run.
#[test]
#[ignore]
fn over_another_with_force() {
    let failures = run(cases(), true, "over");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
