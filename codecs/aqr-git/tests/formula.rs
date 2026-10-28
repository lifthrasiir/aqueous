//! Round trips of packs made with the git CLI: ofs- and ref-deltas, odd compression levels,
//! tags, packs of other data after them; and corrupt inputs to the module.

use std::path::{Path, PathBuf};
use std::process::Command;

use aqr_formula::Formula;
use aqr_git::formula::{self, Options};
use aqr_git::layout::{self, HEADER_KEPT, REBUILT};

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.name=T", "-c", "user.email=t@example.com"])
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
    let d = std::env::temp_dir().join(format!("aqr-git-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A history of files that change a little each commit, so that packs have deltas, and a tag.
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

/// The repository's one pack.
fn pack(dir: &Path) -> Vec<u8> {
    let packs: Vec<PathBuf> = std::fs::read_dir(dir.join(".git/objects/pack"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "pack"))
        .collect();
    assert_eq!(packs.len(), 1, "{packs:?}");
    std::fs::read(&packs[0]).unwrap()
}

fn decode(f: &Formula, threads: usize) -> aqr_formula::Result<Vec<u8>> {
    f.eval(&|a: &aqr_formula::Apply, ins: &[&[u8]]| {
        formula::apply(a, ins, threads).map_err(|e| aqr_formula::Error::new(e.to_string()))
    })
}

fn layout_of(f: &Formula) -> &[u8] {
    &f.streams.iter().find(|s| s.class == "git-pack/layout").unwrap().data
}

/// Encodes `d` (aqr-flate's default profile, as aqr-compress does below level 7), checks that
/// the module gives it back, and returns the formula and the entries' tags.
fn roundtrip(d: &[u8]) -> (Formula, Vec<u8>) {
    roundtrip_with(d, &Options { threads: 4, ..Options::default() })
}

fn roundtrip_with(d: &[u8], opt: &Options) -> (Formula, Vec<u8>) {
    let f = formula::encode_with(d, opt).expect("encoded").formula;
    // (made a stream at a time, the same)
    let g = formula::encode_with(d, &Options { window: 1, ..opt.clone() }).expect("encoded").formula;
    assert!(f == g, "made otherwise in windows of a stream");
    for threads in [1, 4] {
        assert!(decode(&f, threads).unwrap() == d, "not rebuilt");
    }
    let tags = layout::tags(layout_of(&f)).unwrap().to_vec();
    (f, tags)
}

fn rebuilt(tags: &[u8]) -> usize {
    tags.iter().filter(|&&t| t & REBUILT != 0).count()
}

fn of_type(tags: &[u8], t: u8) -> usize {
    tags.iter().filter(|&&x| x & REBUILT != 0 && x & 7 == t).count()
}

fn record_bytes(f: &Formula) -> usize {
    f.streams.iter().filter(|s| s.class == "flate/record").map(|s| s.data.len()).sum()
}

#[test]
fn ofs_deltas() {
    let d = tmp("ofs");
    history(&d, 15);
    git(&d, &["gc", "-q", "--aggressive"]);
    let p = pack(&d);
    let (_, tags) = roundtrip(&p);
    assert_eq!(rebuilt(&tags), tags.len(), "every entry rebuilt");
    assert!(of_type(&tags, 6) > 10, "{tags:?}");
    assert!(tags.iter().all(|t| t & HEADER_KEPT == 0), "git's own headers derived");
    // with the adaptive profile (aqr-compress's from level 7), the zlib settings found once
    // and emulated exactly: next to no record
    let opt = Options { profile: aqr_flate::ADAPTIVE, zlib: true, threads: 4, ..Options::default() };
    let (g, _) = roundtrip_with(&p, &opt);
    assert!(record_bytes(&g) < tags.len() * 12, "{} B of records for {} entries", record_bytes(&g), tags.len());
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn ref_deltas_and_levels() {
    for (name, cfg) in [
        ("ref", "repack.useDeltaBaseOffset=false"),
        ("l0", "pack.compression=0"),
        ("l1", "pack.compression=1"),
        ("l9", "pack.compression=9"),
    ] {
        let d = tmp(name);
        history(&d, 15);
        git(&d, &["-c", cfg, "repack", "-q", "-a", "-d", "-f"]);
        let p = pack(&d);
        let (_, tags) = roundtrip(&p);
        if name == "ref" {
            assert!(of_type(&tags, 7) > 10, "{name}: {tags:?}");
        }
        // (stored blocks are taken too, as aqr-flate codes them)
        assert_eq!(rebuilt(&tags), tags.len(), "{name}");
        std::fs::remove_dir_all(&d).unwrap();
    }
}

#[test]
fn trailer_and_what_follows() {
    let d = tmp("trailer");
    history(&d, 5);
    git(&d, &["gc", "-q"]);
    let p = pack(&d);
    let (f, _) = roundtrip(&p);
    assert_eq!(layout_of(&f).last(), Some(&layout::TRAILER_DERIVED));
    // a wrong trailer is kept, and so is anything after the pack
    let mut q = p.clone();
    *q.last_mut().unwrap() ^= 1;
    q.extend_from_slice(b"after the pack");
    let (f, _) = roundtrip(&q);
    assert_eq!(layout_of(&f).last(), Some(&0));
    // a pack cut short is not one
    assert!(formula::encode(&p[..p.len() / 2], 1).is_none());
    assert!(formula::encode(b"PACK\0\0\0\x02\0\0\0\x01", 1).is_none());
    assert!(formula::encode(b"PACK\0\0\0\x04\0\0\0\0", 1).is_none());
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn limit_keeps_entries() {
    let d = tmp("limit");
    history(&d, 10);
    git(&d, &["gc", "-q"]);
    let p = pack(&d);
    let opt = Options { limit: 6_000, ..Options::default() };
    let e = formula::encode_with(&p, &opt).expect("encoded");
    assert!(e.inflated <= 6_000);
    assert!(decode(&e.formula, 1).unwrap() == p);
    let tags = layout::tags(layout_of(&e.formula)).unwrap();
    assert!(rebuilt(tags) > 0 && rebuilt(tags) < tags.len(), "{tags:?}");
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn corrupt_inputs_fail_cleanly() {
    let d = tmp("corrupt");
    history(&d, 5);
    git(&d, &["gc", "-q"]);
    let p = pack(&d);
    let (f, _) = roundtrip(&p);
    let deflated = {
        let aqr_formula::Node::Apply(a) = &f.root else { panic!("an application") };
        let mut g = f.clone();
        g.root = a.inputs[0].clone();
        decode(&g, 1).unwrap()
    };
    let (layout, kept) = (layout_of(&f).to_vec(), f.streams.iter().find(|s| s.class == "git-pack/kept").unwrap());
    let len = p.len() as u64;
    let inputs = [deflated, layout, kept.data.to_vec()];
    for k in 0..3 {
        let mut ins = inputs.clone();
        for i in (0..ins[k].len()).step_by(1.max(ins[k].len() / 200)) {
            ins[k][i] ^= 0x55;
            let refs: Vec<&[u8]> = ins.iter().map(|i| &i[..]).collect();
            if let Ok(out) = aqr_git::module::decode(&[], &refs, len, 1) {
                assert_eq!(out.len() as u64, len);
            }
            ins[k][i] ^= 0x55;
        }
        ins[k].pop();
        let refs: Vec<&[u8]> = ins.iter().map(|i| &i[..]).collect();
        assert!(aqr_git::module::decode(&[], &refs, len, 1).is_err());
    }
    std::fs::remove_dir_all(&d).unwrap();
}
