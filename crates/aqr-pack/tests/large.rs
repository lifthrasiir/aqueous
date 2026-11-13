//! A large file deflated in chunks on several threads (pigz's way): the stream is one valid
//! DEFLATE stream, read back by aqr-unpack (and by `unzip` and 7-Zip, where they are at hand).

use aqr_pack::{Entry, EntryNode, Extra, Options, Tree};
use aqr_unpack::{Format, Opened};

mod common;
use common::{Files, Kept};

/// Text with long repeats (matches across chunk boundaries) and noise, `n` bytes: a size that is
/// not a multiple of a chunk.
fn data(n: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(n + 64);
    let mut i = 0u32;
    while v.len() < n {
        if i.is_multiple_of(7) {
            v.extend((0..300u32).map(|k| (k.wrapping_add(i).wrapping_mul(2_654_435_761) >> 11) as u8));
        } else {
            v.extend(format!("line {} of the large file, again {}\n", i % 1000, i / 1000).into_bytes());
        }
        i += 1;
    }
    v.truncate(n);
    v
}

fn tree(files: usize) -> Tree {
    let entries = (0..files)
        .map(|i| Entry {
            parent: None,
            name: format!("f{i}").into_bytes(),
            node: EntryNode::File(i),
            extra: Extra::default(),
        })
        .collect();
    Tree { root: Extra::default(), entries }
}

fn check(format: Format, out: &[u8], files: &Files) {
    let opened = Opened::open(format, out).unwrap();
    let kept = Kept::default();
    opened.unpack(2, &kept).unwrap();
    let got = kept.0.into_inner().unwrap();
    assert_eq!(got.len(), files.0.len());
    for (i, f) in files.0.iter().enumerate() {
        assert!(got[&i] == *f, "content {i} differs");
    }
}

fn external(tool: &str, args: &[&str], out: &[u8], ext: &str) {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("aqr-pack-large-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("t.{ext}"));
    std::fs::write(&path, out).unwrap();
    let r = std::process::Command::new(tool).args(args).arg(&path).output();
    std::fs::remove_dir_all(&dir).unwrap();
    let Ok(r) = r else { return eprintln!("no {tool}: not checked") };
    let said = String::from_utf8_lossy(&r.stdout);
    assert!(r.status.success(), "{tool}: {said}{}", String::from_utf8_lossy(&r.stderr));
}

fn files() -> Files {
    // (over the threshold, not a multiple of a chunk; one more of exactly two chunks and a byte)
    Files(vec![data(9 * 1024 * 1024 + 12345), data(2 * (512 << 10) + 1), data(5 << 20)])
}

#[test]
fn zip() {
    let (files, t) = (files(), tree(3));
    let o = Options { level: Some(6), threads: 4, ..Options::default() };
    let mut out = Vec::new();
    aqr_pack::zip::write(&t, &files, &o, &mut out, &|_| {}).unwrap();
    check(Format::Zip, &out, &files);
    external("unzip", &["-tq"], &out, "zip");
    external("7zz", &["t"], &out, "zip");
    // (smaller than the data, by a little more than a single thread's)
    let mut one = Vec::new();
    aqr_pack::zip::write(&t, &files, &Options { threads: 1, ..o.clone() }, &mut one, &|_| {}).unwrap();
    assert!(out.len() < files.0[0].len() / 2);
    assert!(out.len() < one.len() + one.len() / 20, "{} vs {}", out.len(), one.len());
}

#[test]
fn zip_levels() {
    let files = Files(vec![data(6 << 20)]);
    for level in [1, 3, 4, 9] {
        let o = Options { level: Some(level), threads: 3, ..Options::default() };
        let mut out = Vec::new();
        aqr_pack::zip::write(&tree(1), &files, &o, &mut out, &|_| {}).unwrap();
        check(Format::Zip, &out, &files);
        external("unzip", &["-tq"], &out, "zip");
    }
}

#[test]
fn zip_aes() {
    let (files, t) = (Files(vec![data(5 << 20)]), tree(1));
    let o = Options { threads: 4, passphrase: Some(b"secret".to_vec()), ..Options::default() };
    let mut out = Vec::new();
    aqr_pack::zip::write(&t, &files, &o, &mut out, &|_| {}).unwrap();
    let mut opened = Opened::open(Format::Zip, &out).unwrap();
    assert!(opened.unlock(b"secret").unwrap());
    let kept = Kept::default();
    opened.unpack(2, &kept).unwrap();
    assert!(kept.0.into_inner().unwrap()[&0] == files.0[0]);
    external("7zz", &["t", "-psecret"], &out, "zip");
}

#[test]
fn alz_and_egg() {
    let (files, t) = (files(), tree(3));
    let o = Options { level: Some(6), threads: 4, ..Options::default() };
    let mut out = Vec::new();
    aqr_pack::alzip::write_alz(&t, &files, &o, &mut out, &|_| {}).unwrap();
    check(Format::Alz, &out, &files);
    let mut out = Vec::new();
    aqr_pack::alzip::write_egg(&t, &files, &o, &mut out, &|_| {}).unwrap();
    check(Format::Egg, &out, &files);
}
