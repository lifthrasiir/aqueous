//! The readers on archives of real tools (tests/data, `scripts/gen-fixtures.py`; aqr-7z's 7z
//! archives): the trees and contents they tell, and what they leave out.

use std::collections::BTreeMap;
use std::sync::Mutex;

use aqr_formula::aqr::Step;
use aqr_unpack::{Done, EntryNode, Extra, Filter, Format, Opened, Select, Sink, Time, Tree, by_name};

/// What the tree tells: by path, the node (a file's content) and its metadata.
#[derive(Debug, Clone, PartialEq)]
enum Node {
    Dir,
    File(Vec<u8>),
    Link(Vec<u8>),
    Fifo,
    Other,
}

#[derive(Default)]
struct Rec {
    tree: Mutex<Tree>,
    told: Mutex<Vec<bool>>,
    contents: Mutex<BTreeMap<usize, Vec<u8>>>,
    notes: Mutex<Vec<String>>,
    /// The units started.
    started: Mutex<Vec<usize>>,
}

impl Sink for Rec {
    fn entries(&self, t: &Tree, new: &[usize]) -> Result<(), String> {
        let mut told = self.told.lock().unwrap();
        told.resize(t.entries.len(), false);
        for &i in new {
            if let Some(p) = t.entries[i].parent {
                assert!(p < i && told[p] || new.contains(&p), "entry {i} told before its directory");
            }
            told[i] = true;
        }
        *self.tree.lock().unwrap() = t.clone();
        Ok(())
    }

    fn content(&self, c: usize, d: Vec<u8>) -> Result<(), String> {
        let t = self.tree.lock().unwrap();
        assert!(t.entries.iter().any(|e| e.node == EntryNode::File(c)), "content {c} of no file told");
        self.contents.lock().unwrap().insert(c, d);
        Ok(())
    }

    fn note(&self, what: &str) {
        self.notes.lock().unwrap().push(what.to_string());
    }

    fn step(&self, s: Step) {
        if let Step::Started(i) = s {
            self.started.lock().unwrap().push(i);
        }
    }
}

fn data(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// The lines of text the fixtures hold.
fn text() -> Vec<u8> {
    (0..1000).flat_map(|i| format!("line {i} of some text\n").into_bytes()).collect()
}

/// `d` (an archive of format `f`) read: what was told, and the tree as `Done` gives it.
fn unpack(f: Format, d: &[u8]) -> Result<(Done, Rec), String> {
    let r = Rec::default();
    let done = Opened::open(f, d)?.unpack(4, &r)?;
    assert_eq!(*r.tree.lock().unwrap(), done.tree, "the tree told is the tree done");
    Ok((done, r))
}

/// `d` read as [`unpack`] does, `paths` alone (`/` between names) selected.
fn unpack_some(f: Format, d: &[u8], paths: &[&str]) -> Result<(Done, Rec), String> {
    let r = Rec::default();
    let mut o = Opened::open(f, d)?;
    o.select(Select::new(paths.iter().map(|p| p.split('/').map(|n| n.as_bytes().to_vec()).collect()).collect()));
    let done = o.unpack(4, &r)?;
    assert_eq!(*r.tree.lock().unwrap(), done.tree, "the tree told is the tree done");
    Ok((done, r))
}

/// By path, the files whose contents were given, and their contents.
fn given(done: &Done, r: &Rec) -> BTreeMap<String, Vec<u8>> {
    let contents = r.contents.lock().unwrap();
    let mut paths: Vec<String> = Vec::new();
    let mut out = BTreeMap::new();
    for e in &done.tree.entries {
        let name = String::from_utf8_lossy(&e.name).into_owned();
        let path = e.parent.map_or(name.clone(), |p| format!("{}/{name}", paths[p]));
        if let EntryNode::File(c) = e.node
            && let Some(d) = contents.get(&c)
        {
            out.insert(path.clone(), d.clone());
        }
        paths.push(path);
    }
    out
}

/// By path, the node and the metadata.
fn listing(done: &Done, r: &Rec) -> BTreeMap<String, (Node, Extra)> {
    let t = &done.tree;
    let contents = r.contents.lock().unwrap();
    let mut paths: Vec<String> = Vec::new();
    let mut out = BTreeMap::new();
    for e in &t.entries {
        let name = String::from_utf8_lossy(&e.name).into_owned();
        let path = e.parent.map_or(name.clone(), |p| format!("{}/{name}", paths[p]));
        let node = match &e.node {
            EntryNode::Dir => Node::Dir,
            EntryNode::File(c) => Node::File(contents.get(c).cloned().expect("every content given")),
            EntryNode::Link(t) => Node::Link(t.clone()),
            EntryNode::Fifo => Node::Fifo,
            _ => Node::Other,
        };
        out.insert(path.clone(), (node, e.extra.clone()));
        paths.push(path);
    }
    out
}

fn at(secs: i64) -> Option<Time> {
    Some(Time::new(secs, 0))
}

#[test]
fn names() {
    let f = |n: &str| by_name(n.as_bytes());
    assert_eq!(f("a.tar.gz"), Some((Format::Tar(Some(Filter::Gzip)), 7)));
    assert_eq!(f("A.TGZ"), Some((Format::Tar(Some(Filter::Gzip)), 4)));
    assert_eq!(f("a.gz"), Some((Format::Single(Filter::Gzip), 3)));
    assert_eq!(f("a.ZIP"), Some((Format::Zip, 4)));
    assert_eq!(f("x.7z"), Some((Format::SevenZip, 3)));
    assert_eq!(f("x.rar"), Some((Format::Rar, 4)));
    assert_eq!(f("x.CBR"), Some((Format::Rar, 4)));
    assert_eq!(f("x.alz"), Some((Format::Alz, 4)));
    assert_eq!(f("x.EGG"), Some((Format::Egg, 4)));
    assert_eq!(f("a.tar.zst"), Some((Format::Tar(Some(Filter::Zstd)), 8)));
    assert_eq!(f(".zip"), None);
    assert_eq!(f("a.aqr"), None);
    assert_eq!(f("zip"), None);
}

#[test]
fn tar_trees() {
    let pax = {
        let (done, r) = unpack(Format::Tar(None), &data("tree-pax.tar")).unwrap();
        assert_eq!(done.skipped, 1);
        let notes = r.notes.lock().unwrap().clone();
        assert!(notes.len() == 1 && notes[0].starts_with("../evil.txt:"), "{notes:?}");
        listing(&done, &r)
    };
    let get = |p: &str| pax.get(p).unwrap_or_else(|| panic!("{p} not in {:?}", pax.keys()));
    assert_eq!(get("dir").0, Node::Dir);
    assert_eq!(get("dir").1.mtime, at(1_400_000_000));
    assert_eq!(get("dir/file.txt").0, Node::File(text()));
    let x = &get("dir/file.txt").1;
    assert_eq!(x.mtime, Some(Time::new(1_500_000_001, 250_000_000)));
    assert_eq!(x.mode, Some(0o644));
    assert_eq!(x.xattrs, [(b"user.note"[..].into(), b"kept"[..].into())]);
    assert_eq!(get("dir/ro.txt").1.mode, Some(0o444));
    // a hard link: a file of the same content, in a link group with it
    assert_eq!(get("dir/hard").0, Node::File(text()));
    assert!(x.link_group.is_some() && get("dir/hard").1.link_group == x.link_group);
    assert_eq!(get("dir/sym").0, Node::Link(b"file.txt".to_vec()));
    assert_eq!(get("dir/escape").0, Node::Link(b"../../outside".to_vec()));
    assert_eq!(get("abs.txt").0, Node::File(b"absolute".to_vec()));
    assert_eq!(get("12:30.txt").0, Node::File(b"colon".to_vec()));
    assert_eq!(get("dup.txt").0, Node::File(b"second".to_vec()));
    assert_eq!(get("fifo").0, Node::Fifo);
    assert_eq!(get("한글/파일.txt").0, Node::File("유니코드".as_bytes().to_vec()));
    let long = format!("long/{}/{}.txt", "n".repeat(150), "x".repeat(120));
    assert_eq!(get(&long).0, Node::File(b"long name".to_vec()));
    assert!(!pax.keys().any(|p| p.contains("evil")));

    // the same of GNU's format (but for what pax alone keeps)
    let (done, r) = unpack(Format::Tar(None), &data("tree-gnu.tar")).unwrap();
    let gnu = listing(&done, &r);
    assert_eq!(gnu.keys().collect::<Vec<_>>(), pax.keys().collect::<Vec<_>>());
    assert_eq!(gnu["dir/file.txt"].1.mtime, at(1_500_000_001));
    assert_eq!(gnu["dir/hard"].0, Node::File(text()));

    // and of each compressor
    for (name, f) in [
        ("tree.tar.gz", Filter::Gzip),
        ("tree.tar.bz2", Filter::Bzip2),
        ("tree.tar.xz", Filter::Xz),
        ("tree.tar.lzma", Filter::Lzma),
    ] {
        let (done, r) = unpack(Format::Tar(Some(f)), &data(name)).unwrap();
        assert_eq!(listing(&done, &r), pax, "{name}");
    }
}

#[test]
fn zip_tree() {
    let (done, r) = unpack(Format::Zip, &data("tree.zip")).unwrap();
    assert_eq!(done.skipped, 1);
    assert!(r.notes.lock().unwrap()[0].starts_with("../evil.txt:"));
    let l = listing(&done, &r);
    for f in ["stored.txt", "deflated.txt", "bzip2.txt", "lzma.txt"] {
        assert_eq!(l[f].0, Node::File(text()), "{f}");
        assert!(l[f].1.mtime.is_some());
    }
    assert_eq!(l["dir"].0, Node::Dir);
    assert_eq!(l["dir/in.txt"].0, Node::File(b"in dir".to_vec()));
    assert_eq!(l["한글.txt"].0, Node::File(b"utf-8".to_vec()));
    assert_eq!(l["link"].0, Node::Link(b"stored.txt".to_vec()));
    assert_eq!(l["link"].1.mode, Some(0o777));
    assert_eq!(l.len(), 8);
}

#[test]
fn zip_left_out() {
    let (done, r) = unpack(Format::Zip, &data("skip.zip")).unwrap();
    let l = listing(&done, &r);
    assert_eq!(l.keys().collect::<Vec<_>>(), ["ok.txt"]);
    assert_eq!(l["ok.txt"].0, Node::File(b"ok.txt".to_vec()));
    assert_eq!(done.skipped, 2);
    let notes = r.notes.lock().unwrap().join("\n");
    assert!(notes.contains("secret.txt: encrypted") && notes.contains("d64.txt: of Deflate64"), "{notes}");
}

#[test]
fn sevenz_fixtures() {
    // (aqr-7z's: their CRC-32s check the files)
    let dir = format!("{}/../../codecs/aqr-7z/tests/data", env!("CARGO_MANIFEST_DIR"));
    let mut words = None;
    let mut n = 0;
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_none_or(|e| e != "7z") {
            continue;
        }
        let d = std::fs::read(&p).unwrap();
        let (done, r) = unpack(Format::SevenZip, &d).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        assert_eq!(done.skipped, 0, "{}: {:?}", p.display(), r.notes.lock().unwrap());
        let l = listing(&done, &r);
        assert!(l.values().any(|v| matches!(v.0, Node::File(_))), "{}", p.display());
        if let Some((Node::File(w), x)) = l.get("docs/words.txt") {
            assert!(x.mtime.is_some(), "{}", p.display());
            assert_eq!(words.get_or_insert_with(|| w.clone()), w, "{}", p.display());
        }
        n += 1;
    }
    assert!(n >= 10 && words.is_some());
}

#[test]
fn single_files() {
    for (name, f) in [
        ("one.txt.gz", Filter::Gzip),
        ("one.txt.bz2", Filter::Bzip2),
        ("one.txt.xz", Filter::Xz),
        ("one.txt.lzma", Filter::Lzma),
    ] {
        let d = data(name);
        let o = Opened::open(Format::Single(f), &d).unwrap();
        assert!(!o.is_tree());
        let mut out = Vec::new();
        let n = o
            .decompress(
                4,
                &mut |b| {
                    out.extend_from_slice(b);
                    Ok(())
                },
                &|_| {},
            )
            .unwrap();
        assert_eq!(n as usize, out.len());
        assert!(out == text(), "{name}");
    }
}

#[test]
fn damage_fails() {
    // a byte of the compressed data changed: an error, not files of other contents
    for (name, f, at) in [
        ("tree.tar.gz", Format::Tar(Some(Filter::Gzip)), 200),
        ("tree.tar.xz", Format::Tar(Some(Filter::Xz)), 200),
        ("tree.tar.bz2", Format::Tar(Some(Filter::Bzip2)), 200),
        ("tree.zip", Format::Zip, 100),
    ] {
        let mut d = data(name);
        d[at] ^= 0x10;
        assert!(unpack(f, &d).is_err(), "{name}");
    }
    // cut short
    let d = data("tree-pax.tar");
    assert!(unpack(Format::Tar(None), &d[..d.len() / 2 + 100]).is_err());
    let d = data("tree.zip");
    assert!(Opened::open(Format::Zip, &d[..d.len() - 30]).is_err());
}

/// RAR archives of a tree by rar 7.23 (RAR5) and 6.24 (RAR 3.x's format): their nodes, contents,
/// modes and times (RAR5's to the nanosecond, RAR 3.x's extended times to 100 ns), the hard
/// link of RAR5's left out (told).
#[test]
fn rar_trees() {
    for (name, hard) in [("tree-rar5.rar", true), ("tree-rar4.rar", false)] {
        let (done, r) = unpack(Format::Rar, &data(name)).unwrap();
        let notes = r.notes.lock().unwrap().clone();
        assert_eq!(done.skipped, hard as usize, "{name}: {notes:?}");
        assert!(!hard || notes[0].starts_with("dir/hard:"), "{name}: {notes:?}");
        let l = listing(&done, &r);
        let get = |p: &str| l.get(p).unwrap_or_else(|| panic!("{name}: {p} not in {:?}", l.keys()));
        assert_eq!(get("dir").0, Node::Dir, "{name}");
        assert_eq!(get("dir").1.mtime, at(1_400_000_000), "{name}");
        assert_eq!(get("dir/file.txt").0, Node::File(text()), "{name}");
        let x = &get("dir/file.txt").1;
        assert_eq!(x.mtime, Some(Time::new(1_500_000_001, 250_000_000)), "{name}");
        assert_eq!(x.mode, Some(0o644), "{name}");
        assert_eq!(get("dir/ro.txt").0, Node::File(b"read only\n".to_vec()), "{name}");
        assert_eq!(get("dir/ro.txt").1.mode, Some(0o444), "{name}");
        assert_eq!(get("dir/sym").0, Node::Link(b"file.txt".to_vec()), "{name}");
        assert_eq!(get("한글/파일.txt").0, Node::File("유니코드".as_bytes().to_vec()), "{name}");
        assert!(!l.contains_key("dir/hard"), "{name}");
    }
}

/// aqr-rar's fixtures (RAR5 and RAR 3.x: solid groups, filters, PPMd, audio) extracted whole,
/// their CRC-32s checked as they are decoded.
#[test]
fn rar_streams() {
    let dir = format!("{}/../../codecs/aqr-rar/tests/data", env!("CARGO_MANIFEST_DIR"));
    for name in ["r5_solid", "r5_m3", "r5_stored", "r4_solid", "r4_ppmd", "r4_audio", "r4_m3", "r4_stored"] {
        let d = std::fs::read(format!("{dir}/{name}.rar")).unwrap();
        let (done, r) = unpack(Format::Rar, &d).unwrap();
        assert_eq!(done.skipped, 0, "{name}: {:?}", r.notes.lock().unwrap());
        let l = listing(&done, &r);
        assert!(!l.is_empty() && l.values().all(|(n, _)| matches!(n, Node::File(f) if !f.is_empty())), "{name}");
    }
    // (damaged: a byte of a stream changed fails, not panics)
    let mut d = std::fs::read(format!("{dir}/r4_solid.rar")).unwrap();
    let k = d.len() / 2;
    d[k] ^= 0x55;
    assert!(unpack(Format::Rar, &d).is_err());
}

/// A codec crate's test archive.
fn codec_data(krate: &str, name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/../../codecs/{krate}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// unalz's ALZ archives (aqr-alz's tests/data): CP949 names, directories, stored, DEFLATE and
/// ALZip's bzip2, encrypted files and a first volume left out.
#[test]
fn alz_trees() {
    let alz = |name| unpack(Format::Alz, &codec_data("aqr-alz", name)).unwrap();
    for name in ["normal.alz", "low.alz"] {
        let (done, r) = alz(name);
        assert_eq!(done.skipped, 0, "{name}: {:?}", r.notes.lock().unwrap());
        let l = listing(&done, &r);
        assert_eq!(l.len(), 12, "{name}: {:?}", l.keys());
        assert!(matches!(&l["한글파일.txt"].0, Node::File(f) if !f.is_empty()), "{name}");
        // (뷁: of CP949, not EUC-KR)
        assert!(matches!(&l["뷁테스트.txt"].0, Node::File(f) if !f.is_empty()), "{name}");
        assert_eq!(l["empty.txt"].0, Node::File(Vec::new()), "{name}");
        assert_eq!(l["subdir"].0, Node::Dir, "{name}");
        assert!(matches!(&l["subdir/nested/deep.txt"].0, Node::File(_)), "{name}");
        let x = &l["hello.txt"].1;
        assert!(x.mtime.is_some() && x.win_attrs == Some(0x20), "{name}: {x:?}");
    }
    let (done, r) = alz("dir_tree.alz");
    let l = listing(&done, &r);
    assert_eq!(l["work/tree/empty_folder"].0, Node::Dir);
    assert!(matches!(&l["work/tree/sub/deeper/deepest/three.txt"].0, Node::File(_)));
    for name in ["bzip2.alz", "bzip2_dlz.alz", "deflate_empty.alz", "store_empty.alz"] {
        let (done, r) = alz(name);
        assert_eq!(done.skipped, 0, "{name}: {:?}", r.notes.lock().unwrap());
        assert!(listing(&done, &r).values().any(|(n, _)| matches!(n, Node::File(f) if !f.is_empty())), "{name}");
    }
    let (done, r) = alz("enc_deflate.alz");
    assert_eq!(done.skipped, 2);
    assert!(r.notes.lock().unwrap().iter().all(|n| n.ends_with("encrypted, not extracted")));
    let (done, r) = alz("split_store.alz");
    assert_eq!(done.skipped, 1, "{:?}", r.notes.lock().unwrap());
    assert!(listing(&done, &r).contains_key("part0.bin"));
    // (damaged: a byte of a DEFLATE body changed fails the file's CRC-32)
    let mut d = codec_data("aqr-alz", "normal.alz");
    let k = d.len() / 2;
    d[k] ^= 0x55;
    assert!(unpack(Format::Alz, &d).is_err());
}

/// EggDotNet's EGG archives (aqr-egg's tests/data): a solid one, names of a code page, Windows'
/// attributes and times, directories, LZMA and AZO, an encrypted file left out.
#[test]
fn egg_trees() {
    let egg = |name| unpack(Format::Egg, &codec_data("aqr-egg", name)).unwrap();
    let lorem = |n: &Node| matches!(n, Node::File(f) if f.starts_with(b"Lorem ipsum"));
    let (done, r) = egg("solid.egg");
    assert_eq!(done.skipped, 0, "{:?}", r.notes.lock().unwrap());
    let l = listing(&done, &r);
    assert!(lorem(&l["s/lorem_ipsum_short.txt"].0) && lorem(&l["s/lorem_ipsum_short2.txt"].0), "{:?}", l.keys());
    let (done, r) = egg("small_kor.egg");
    assert!(listing(&done, &r).contains_key("작은.txt"));
    let (done, r) = egg("lorem_short_readonlyhidden.egg");
    let l = listing(&done, &r);
    let (ro, hidden) = (&l["lorem_ipsum_short_readonly.txt"], &l["lorem_ipsum_short_hidden.txt"]);
    assert!(lorem(&ro.0) && lorem(&hidden.0));
    assert_eq!((ro.1.win_attrs, hidden.1.win_attrs), (Some(1), Some(2)));
    assert_eq!(ro.1.mtime, at(1_703_855_226));
    let (done, r) = egg("directories.egg");
    assert_eq!(listing(&done, &r)["dir1/dir2/dir3/test.txt"].0, Node::File(b"This is some text.".to_vec()));
    let (done, r) = egg("extattr.egg");
    assert_eq!(listing(&done, &r)["Some folder"].0, Node::Dir);
    for (name, files) in
        [("lzma_simple.egg", 2), ("azonumbers.egg", 1), ("azoshort_txt.egg", 1), ("globalcomment.egg", 1)]
    {
        let (done, r) = egg(name);
        assert_eq!(done.skipped, 0, "{name}: {:?}", r.notes.lock().unwrap());
        let l = listing(&done, &r);
        assert_eq!(l.values().filter(|(n, _)| matches!(n, Node::File(f) if !f.is_empty())).count(), files, "{name}");
    }
    let (done, r) = egg("lorem_long_zipEnc.egg");
    assert_eq!(done.skipped, 1, "{:?}", r.notes.lock().unwrap());
    // (cut short: told; damaged: a byte of an AZO block changed fails its CRC-32)
    let d = codec_data("aqr-egg", "azonumbers.egg");
    let (done, _) = unpack(Format::Egg, &d[..d.len() / 2]).unwrap();
    assert_eq!(done.skipped, 1);
    let mut d = d;
    let k = d.len() / 2;
    d[k] ^= 0x55;
    assert!(unpack(Format::Egg, &d).is_err());
}

/// An ALZ archive of stored files of `names` (each holding its name), as ALZip lays it out.
fn alz_of(names: &[&[u8]]) -> Vec<u8> {
    let mut d = b"ALZ\x01\x0a\0\0\0".to_vec();
    for n in names {
        d.extend_from_slice(b"BLZ\x01");
        d.extend_from_slice(&(n.len() as u16).to_le_bytes());
        // archive bit, no time, sizes of 1 byte
        d.extend_from_slice(&[0x20, 0, 0, 0, 0, 0x10, 0, 0, 0]);
        d.extend_from_slice(&aqr_common::crc32::hash(n).to_le_bytes());
        d.extend_from_slice(&[n.len() as u8, n.len() as u8]);
        d.extend_from_slice(n);
        d.extend_from_slice(n);
    }
    d.extend_from_slice(b"CLZ\x01\0\0\0\0\0\0\0\0CLZ\x02");
    d
}

/// ALZ's names are of one encoding for the whole archive: UTF-8 where all are, else CP949, even
/// a name that would be UTF-8 by itself (찼 of CP949, C3 A1, is UTF-8's á).
#[test]
fn alz_names_by_archive() {
    let names = |names: &[&[u8]]| {
        let (done, r) = unpack(Format::Alz, &alz_of(names)).unwrap();
        assert_eq!(done.skipped, 0, "{:?}", r.notes.lock().unwrap());
        listing(&done, &r).into_keys().collect::<Vec<_>>()
    };
    assert_eq!(names(&[b"\xb0\xa1.txt", b"\xc3\xa1.txt"]), ["가.txt", "찼.txt"]);
    assert_eq!(names(&["가.txt".as_bytes(), b"\xc3\xa1.txt"]), ["á.txt", "가.txt"]);
}

/// Archives made by ALZip itself (names-alzip.alz and .egg, by hand): `뷁.txt` ("break"; 뷁 is in
/// CP949, not EUC-KR) and `⣿.txt` ("full"; U+28FF, in no Korean code page). ALZ has CP949 names,
/// ALZip writing `?` for the braille; EGG has UTF-8 names (with no locale), the braille kept.
#[test]
fn alzip_names() {
    for (format, name, braille) in
        [(Format::Alz, "names-alzip.alz", "?.txt"), (Format::Egg, "names-alzip.egg", "⣿.txt")]
    {
        let (done, r) = unpack(format, &data(name)).unwrap();
        assert_eq!(done.skipped, 0, "{name}: {:?}", r.notes.lock().unwrap());
        let l = listing(&done, &r);
        assert_eq!(l.len(), 2, "{name}: {:?}", l.keys());
        assert_eq!(l["뷁.txt"].0, Node::File(b"break".to_vec()), "{name}");
        assert_eq!(l[braille].0, Node::File(b"full".to_vec()), "{name}");
    }
}

/// `d` (of format `f`) read with passphrase `pass` given ([`Opened::unlock`]): whether it took
/// it, and what was told.
fn unpack_with(f: Format, d: &[u8], pass: &[u8]) -> Result<(bool, Done, Rec), String> {
    let r = Rec::default();
    let mut o = Opened::open(f, d)?;
    assert!(o.encrypted());
    let took = o.unlock(pass)?;
    let done = o.unpack(4, &r)?;
    Ok((took, done, r))
}

/// Encrypted ZIP entries (the traditional encryption, WinZip's AES) and 7z folders and headers
/// decrypted by their passphrase, left out without it, and a wrong one not taken.
#[test]
fn encrypted() {
    let want = [("secret.txt", text()), ("small.txt", b"small\n".to_vec())];
    for (name, f, pass) in [
        ("enc-zipcrypto.zip", Format::Zip, "pass word"),
        ("enc-aes128.zip", Format::Zip, "pass word"),
        ("enc-aes256.zip", Format::Zip, "pass word"),
        ("enc.7z", Format::SevenZip, "päss wörd"),
        ("enc-headers.7z", Format::SevenZip, "päss wörd"),
    ] {
        let d = data(name);
        let (took, done, r) = unpack_with(f, &d, pass.as_bytes()).unwrap();
        assert!(took, "{name}");
        assert_eq!(done.skipped, 0, "{name}: {:?}", r.notes.lock().unwrap());
        let l = listing(&done, &r);
        for (p, c) in &want {
            assert_eq!(l[*p].0, Node::File(c.clone()), "{name}: {p}");
        }
        // a wrong one: not taken (the smallest entry decoded by it), the entries left out
        let mut o = Opened::open(f, &d).unwrap();
        assert!(!o.unlock(b"pass wor").unwrap(), "{name}");
        assert!(!o.unlock(b"").unwrap(), "{name}");
        match name {
            "enc-headers.7z" => assert!(o.sealed() && o.unpack(4, &Rec::default()).is_err()),
            _ => {
                let r = Rec::default();
                let done = o.unpack(4, &r).unwrap();
                assert_eq!(done.skipped, 2, "{name}");
                assert!(r.notes.lock().unwrap().iter().all(|n| n.contains("encrypted")), "{name}");
            }
        }
    }
    // the traditional encryption: the passphrase as its bytes
    let (took, ..) = unpack_with(Format::Zip, &data("enc-zipcrypto.zip"), "päss wörd".as_bytes()).unwrap();
    assert!(!took);
}

/// Paths selected of each format: only their files given (a directory's with what is under
/// it), only the units holding them started, and nothing told of the entries not selected.
#[test]
fn selected() {
    let map = |v: &[(&str, &[u8])]| v.iter().map(|(p, d)| (p.to_string(), d.to_vec())).collect::<BTreeMap<_, _>>();
    // ZIP: `../evil.txt` (left out, told when all is) not told
    let (done, r) = unpack_some(Format::Zip, &data("tree.zip"), &["dir"]).unwrap();
    assert_eq!(given(&done, &r), map(&[("dir/in.txt", b"in dir")]));
    assert_eq!((done.skipped, r.started.lock().unwrap().len()), (0, 1), "{:?}", r.notes.lock().unwrap());
    let (done, r) = unpack_some(Format::Zip, &data("tree.zip"), &["stored.txt", "lzma.txt"]).unwrap();
    assert_eq!(given(&done, &r), map(&[("stored.txt", &text()), ("lzma.txt", &text())]));
    assert_eq!(r.started.lock().unwrap().len(), 2);

    // tar (read through; the tree of what is selected); a hard link to a file not selected told
    for (name, f) in [("tree-pax.tar", None), ("tree.tar.gz", Some(Filter::Gzip))] {
        let (done, r) = unpack_some(Format::Tar(f), &data(name), &["dir/file.txt", "한글"]).unwrap();
        let want = map(&[("dir/file.txt", &text()), ("한글/파일.txt", "유니코드".as_bytes())]);
        assert_eq!(given(&done, &r), want, "{name}");
        assert_eq!(done.skipped, 0, "{name}: {:?}", r.notes.lock().unwrap());
        let l = listing(&done, &r);
        assert_eq!(l.keys().collect::<Vec<_>>(), ["dir", "dir/file.txt", "한글", "한글/파일.txt"], "{name}");
        let (done, r) = unpack_some(Format::Tar(f), &data(name), &["dir/hard"]).unwrap();
        assert!(given(&done, &r).is_empty() && done.skipped == 1, "{name}");
        assert!(r.notes.lock().unwrap()[0].contains("not selected"), "{:?}", r.notes.lock().unwrap());
    }

    // RAR: RAR5's hard link (left out) not told
    let (done, r) = unpack_some(Format::Rar, &data("tree-rar5.rar"), &["dir/file.txt"]).unwrap();
    assert_eq!(given(&done, &r), map(&[("dir/file.txt", &text())]));
    assert_eq!(done.skipped, 0, "{:?}", r.notes.lock().unwrap());

    // solid ones: what is given as it is extracted whole
    let solid = |f: Format, d: &[u8], path: &str, units: Option<usize>| {
        let (done, r) = unpack(f, d).unwrap();
        let all = given(&done, &r);
        let (done, r) = unpack_some(f, d, &[path]).unwrap();
        let got = given(&done, &r);
        let want: BTreeMap<_, _> =
            all.into_iter().filter(|(p, _)| *p == path || p.starts_with(&format!("{path}/"))).collect();
        assert!(!want.is_empty() && got == want, "{path}: {:?} of {:?}", got.keys(), want.keys());
        assert!(units.is_none_or(|n| r.started.lock().unwrap().len() == n), "{path}");
    };
    let rar = |name: &str| codec_data("aqr-rar", &format!("{name}.rar"));
    for name in ["r5_solid", "r4_solid"] {
        let d = rar(name);
        let (done, r) = unpack(Format::Rar, &d).unwrap();
        // (a file in the middle, and the last)
        let files: Vec<String> = given(&done, &r).into_keys().collect();
        for p in [&files[files.len() / 2], files.last().unwrap()] {
            solid(Format::Rar, &d, p, Some(1));
        }
    }
    let blocks = codec_data("aqr-7z", "blocks.7z");
    solid(Format::SevenZip, &blocks, "docs/words.txt", Some(1));
    solid(Format::SevenZip, &blocks, "docs", Some(1));
    solid(Format::SevenZip, &blocks, "bin/noise.dat", Some(1));
    solid(Format::Egg, &codec_data("aqr-egg", "solid.egg"), "s/lorem_ipsum_short2.txt", Some(1));
    solid(Format::Alz, &codec_data("aqr-alz", "normal.alz"), "subdir", None);
}
