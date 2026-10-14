//! Round trips over repositories made with the git CLI: packs with ofs- and ref-deltas,
//! odd compression levels, loose objects, CRLF checkouts, submodules, tags.

use std::path::{Path, PathBuf};
use std::process::Command;

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.name=T", "-c", "user.email=t@example.com", "-c", "protocol.file.allow=always"])
        .args(["-c", "init.defaultBranch=main", "-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_DATE", "2020-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2020-01-01T00:00:00Z")
        .output()
        .expect("git");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("git-recons-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A history with files that change a little each commit, so that packs have deltas.
fn history(dir: &Path, commits: usize) {
    git(dir, &["init", "-q"]);
    let mut text: Vec<String> = (0..300).map(|i| format!("line {i} {}\n", "x".repeat(i % 40))).collect();
    for c in 0..commits {
        text[c * 7 % 300] = format!("changed in {c}\n");
        std::fs::write(dir.join("a.txt"), text.concat()).unwrap();
        std::fs::create_dir_all(dir.join("sub/dir")).unwrap();
        std::fs::write(dir.join(format!("sub/dir/f{}.txt", c % 5)), text[..100 + c].concat()).unwrap();
        let bin: Vec<u8> =
            (0..5000u32).map(|i| (i.wrapping_mul(2654435761).wrapping_add(c as u32) >> 13) as u8).collect();
        std::fs::write(dir.join("bin.dat"), bin).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", &format!("commit {c}\n\nbody {c}")]);
    }
    git(dir, &["tag", "-a", "v1", "-m", "tag one"]);
}

fn read_all(dir: &Path, prefix: &str, out: &mut Vec<(String, Vec<u8>)>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        let p = format!("{prefix}{}", e.file_name().to_str().unwrap());
        let ft = e.file_type().unwrap();
        if ft.is_dir() {
            read_all(&e.path(), &format!("{p}/"), out);
        } else if ft.is_file() {
            out.push((p, std::fs::read(e.path()).unwrap()));
        }
    }
}

/// Deconstructs `dir` with every zlib stream emulated, with none, and with a budget that
/// takes some, checks every file rebuilt each time (all at once and one by one), and returns
/// the stats of the first.
fn check(dir: &Path) -> git_recons::Stats {
    let mut files = vec![];
    read_all(dir, "", &mut files);
    files.sort();
    let mut first = None;
    for ns in [None, Some(0.0), Some(git_recons::budget::TARGET_NS_PER_BYTE)] {
        let opt = git_recons::Options { ns_per_byte: ns, ..Default::default() };
        let d = git_recons::deconstruct(dir, &opt).unwrap();
        let got = git_recons::reconstruct(&d.streams).unwrap();
        assert_eq!(got.len(), files.len());
        for (g, f) in got.iter().zip(&files) {
            assert_eq!(g.0, f.0);
            assert!(g.1 == f.1, "{} differs ({ns:?})", f.0);
        }
        for (p, b) in &files {
            assert!(git_recons::reconstruct_single(&d.streams, p).unwrap() == *b, "{p} differs alone ({ns:?})");
        }
        if ns == Some(0.0) {
            assert_eq!(d.stats.zlib_emulated, 0);
        }
        eprintln!("{} ({ns:?}): {:?}", dir.display(), d.stats);
        first.get_or_insert(d.stats);
    }
    first.unwrap()
}

const PACK: usize = 5;
const IDX: usize = 6;
const BLOB: usize = 2;

#[test]
fn loose_then_packed() {
    let d = tmp("loose");
    history(&d, 12);
    let st = check(&d);
    assert_eq!(st.kinds[PACK], 0);
    assert!(st.kinds[4] > 30, "{st:?}");
    git(&d, &["gc", "-q", "--aggressive"]);
    // Objects after the pack stay loose.
    std::fs::write(d.join("a.txt"), "new\n").unwrap();
    git(&d, &["commit", "-q", "-am", "after gc"]);
    let st = check(&d);
    assert_eq!(st.kinds[PACK], 1);
    assert_eq!(st.kinds[IDX], 1);
    assert!(st.kinds[BLOB] >= 7, "{st:?}");
    assert_eq!(st.zlib_verbatim + st.entry_verbatim + st.delta_verbatim, 0, "{st:?}");
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn ref_deltas_and_levels() {
    for (name, cfg) in [
        ("ref", "repack.useDeltaBaseOffset=false"),
        ("l0", "core.compression=0"),
        ("l9", "core.compression=9"),
        ("l1", "pack.compression=1"),
    ] {
        let d = tmp(name);
        history(&d, 15);
        git(&d, &["-c", cfg, "repack", "-q", "-a", "-d", "-f"]);
        let st = check(&d);
        assert_eq!(st.kinds[PACK], 1, "{name}");
        // Level 0 (stored blocks) is not emulated: those entries stay whole.
        if name == "l0" {
            assert!(st.entry_verbatim > 10, "{name}: {st:?}");
            std::fs::remove_dir_all(&d).unwrap();
            continue;
        }
        assert!(st.entry_types[if name == "ref" { 7 } else { 6 }] > 10, "{name}: {st:?}");
        assert_eq!(st.zlib_verbatim, 0, "{name}: {st:?}");
        std::fs::remove_dir_all(&d).unwrap();
    }
}

#[test]
fn crlf_checkout() {
    let d = tmp("crlf");
    git(&d, &["init", "-q"]);
    std::fs::write(d.join("w.txt"), "one\ntwo\nthree\n").unwrap();
    git(&d, &["add", "-A"]);
    git(&d, &["commit", "-q", "-m", "lf"]);
    std::fs::write(d.join("w.txt"), "one\r\ntwo\r\nthree\r\n").unwrap();
    std::fs::write(d.join("mixed.txt"), "a\r\nb\n").unwrap();
    let st = check(&d);
    assert_eq!(st.kinds[3], 1, "{st:?}");
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn submodule() {
    let root = tmp("submodule");
    let inner = root.join("inner");
    let outer = root.join("outer");
    std::fs::create_dir_all(&inner).unwrap();
    std::fs::create_dir_all(&outer).unwrap();
    history(&inner, 4);
    history(&outer, 3);
    git(&outer, &["submodule", "add", "-q", inner.to_str().unwrap(), "inner"]);
    git(&outer, &["commit", "-q", "-m", "submodule"]);
    git(&outer, &["gc", "-q"]);
    git(&outer.join("inner"), &["gc", "-q"]);
    let st = check(&outer);
    assert_eq!(st.kinds[PACK], 2, "{st:?}");
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn corrupt_streams_fail_cleanly() {
    let d = tmp("corrupt");
    history(&d, 5);
    git(&d, &["gc", "-q"]);
    let opt = git_recons::Options { ns_per_byte: None, ..Default::default() };
    let dc = git_recons::deconstruct(&d, &opt).unwrap();
    for k in 0..git_recons::NSTREAMS {
        let mut s = dc.streams.clone();
        if s[k].is_empty() {
            continue;
        }
        for step in [1usize, 7, 31] {
            for i in (0..s[k].len()).step_by(step.max(s[k].len() / 64)) {
                s[k][i] ^= 0x55;
                let _ = git_recons::reconstruct(&s);
                s[k][i] ^= 0x55;
            }
        }
        s[k].pop();
        let _ = git_recons::reconstruct(&s);
    }
    std::fs::remove_dir_all(&d).unwrap();
}
