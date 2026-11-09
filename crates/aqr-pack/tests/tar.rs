//! Trees written as tar archives (alone, and under each compressor), read back by aqr-unpack
//! and by `tar` where it is at hand: pax where ustar does not hold an entry.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::sync::{Arc, Mutex};

use aqr_pack::{Bytes, Contents, Entry, EntryNode, Extra, Filter, Format, Options, Time, Tree};
use aqr_unpack::{Opened, Sink};

struct Files(Vec<Vec<u8>>);

impl Contents for Files {
    fn size(&self, c: usize) -> u64 {
        self.0[c].len() as u64
    }

    fn read(&self, c: usize) -> std::io::Result<Bytes> {
        Ok(Bytes::from(self.0[c].clone()))
    }
}

#[derive(Default)]
struct Kept(Mutex<BTreeMap<usize, Vec<u8>>>);

impl Sink for Kept {
    fn entries(&self, _: &Tree, _: &[usize]) -> Result<(), String> {
        Ok(())
    }

    fn content(&self, c: usize, d: Vec<u8>) -> Result<(), String> {
        self.0.lock().unwrap().insert(c, d);
        Ok(())
    }

    fn note(&self, n: &str) {
        panic!("{n}");
    }
}

fn entry(parent: Option<usize>, name: &[u8], node: EntryNode, extra: Extra) -> Entry {
    Entry { parent, name: name.to_vec(), node, extra }
}

/// The entries by path: what each is (a file's content), and its metadata kept by tar.
fn listed(t: &Tree, contents: &BTreeMap<usize, Vec<u8>>) -> BTreeMap<Vec<u8>, String> {
    let path = |mut i: usize| {
        let mut names = vec![t.entries[i].name.clone()];
        while let Some(p) = t.entries[i].parent {
            names.push(t.entries[p].name.clone());
            i = p;
        }
        names.reverse();
        names.join(&b'/')
    };
    (t.entries.iter().enumerate())
        .map(|(i, e)| {
            let what = match &e.node {
                EntryNode::File(c) => format!("{} bytes {}", contents[c].len(), aqr_common::crc32::hash(&contents[c])),
                n => format!("{n:?}"),
            };
            let x = &e.extra;
            let meta = format!("{what} {:?} {:?} {:?}/{:?} {:?}", x.mtime, x.mode, x.uid, x.gid, x.xattrs);
            (path(i), meta)
        })
        .collect()
}

#[test]
fn round_trip() {
    let t0 = Time::new(1_700_000_000, 123_456_700);
    let x = |mode| Extra { mtime: Some(t0), mode: Some(mode), uid: Some(501), gid: Some(20), ..Extra::default() };
    let long = "d".repeat(120);
    let text: Vec<u8> = (0..40000).flat_map(|i| format!("line {i}\n").into_bytes()).collect();
    let files = Files(vec![text.clone(), b"tiny".to_vec(), Vec::new(), text.clone()]);
    let xattr: (Arc<[u8]>, Arc<[u8]>) = (b"user.note".as_slice().into(), b"\x00binary\n=".as_slice().into());
    let tree = Tree {
        root: Extra::default(),
        entries: vec![
            entry(None, b"dir", EntryNode::Dir, x(0o755)),
            entry(Some(0), long.as_bytes(), EntryNode::Dir, x(0o700)),
            entry(Some(1), "한글 이름.txt".as_bytes(), EntryNode::File(0), Extra { link_group: Some(7), ..x(0o644) }),
            entry(Some(0), b"tiny", EntryNode::File(1), Extra { xattrs: vec![xattr], ..x(0o600) }),
            entry(None, b"empty", EntryNode::File(2), x(0o644)),
            entry(None, b"hard", EntryNode::File(3), Extra { link_group: Some(7), ..x(0o644) }),
            entry(None, b"link", EntryNode::Link(long.as_bytes().repeat(2)), x(0o777)),
            entry(None, b"fifo", EntryNode::Fifo, x(0o644)),
            entry(None, b"dev", EntryNode::Device { block: true, major: 8, minor: 1 }, x(0o660)),
            entry(None, b"old", EntryNode::File(1), Extra { mtime: Some(Time::new(-10, 250_000_000)), ..x(0o644) }),
        ],
    };
    for f in [None, Some(Filter::Gzip), Some(Filter::Bzip2), Some(Filter::Xz), Some(Filter::Lzma), Some(Filter::Zstd)] {
        for threads in [1, 4] {
            let mut out = Cursor::new(Vec::new());
            let o = Options { threads, ..Options::default() };
            let w = aqr_pack::write(Format::Tar(f), &tree, &files, &o, &mut out, &|_| {}).unwrap();
            let out = out.into_inner();
            assert_eq!(w.len, out.len() as u64);
            assert!(w.notes.is_empty(), "{:?}", w.notes);
            let format = aqr_unpack::Format::Tar(f.map(|f| match f {
                Filter::Gzip => aqr_unpack::Filter::Gzip,
                Filter::Bzip2 => aqr_unpack::Filter::Bzip2,
                Filter::Xz => aqr_unpack::Filter::Xz,
                Filter::Lzma => aqr_unpack::Filter::Lzma,
                Filter::Zstd => aqr_unpack::Filter::Zstd,
            }));
            let kept = Kept::default();
            let done = Opened::open(format, &out).unwrap().unpack(threads, &kept).unwrap();
            let got = listed(&done.tree, &kept.0.into_inner().unwrap());
            let mut want = listed(&tree, &files.0.iter().cloned().enumerate().collect());
            // (a hard link's content is its first file's; the link group is the reader's own)
            for (k, v) in &got {
                if k.ends_with(b"hard") || k.ends_with(b".txt") {
                    want.insert(k.clone(), v.clone());
                }
            }
            assert_eq!(got, want, "{f:?} on {threads}");
            if f.is_none() && threads == 1 {
                tar_lists(&out);
            }
        }
    }
}

/// `tar -tf` reads it (where tar is at hand), the long names whole (in whatever normal form
/// it gives them).
fn tar_lists(archive: &[u8]) {
    let dir = std::env::temp_dir().join(format!("aqr-pack-tar-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("t.tar");
    std::fs::write(&path, archive).unwrap();
    let r = std::process::Command::new("tar").arg("-tf").arg(&path).output();
    std::fs::remove_dir_all(&dir).unwrap();
    let Ok(r) = r else { return eprintln!("no tar: not checked") };
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    let listed = String::from_utf8_lossy(&r.stdout);
    assert!(listed.lines().any(|l| l.ends_with(".txt") && l.len() > 130), "{listed}");
    assert_eq!(listed.lines().count(), 10, "{listed}");
}

/// A local time of an unknown time zone (`Zone::Local`) as local time here, as MS-DOS times are
/// read and written: a tar archive's mtime and a gzip header's, the same local date and time.
#[test]
fn local_times() {
    use aqr_formula::aqr::Zone;
    let local = Time { secs: 1_700_000_001, nanos: 250_000_000, zone: Zone::Local, unit: 1 };
    let here = aqr_foreign::time::instant(local).unwrap();
    let tree = Tree {
        root: Extra::default(),
        entries: vec![entry(None, b"a", EntryNode::File(0), Extra { mtime: Some(local), ..Extra::default() })],
    };
    let files = Files(vec![b"x".to_vec()]);
    let mut out = Cursor::new(Vec::new());
    aqr_pack::write(Format::Tar(None), &tree, &files, &Options::default(), &mut out, &|_| {}).unwrap();
    let out = out.into_inner();
    let done = Opened::open(aqr_unpack::Format::Tar(None), &out).unwrap().unpack(1, &Kept::default()).unwrap();
    assert_eq!(done.tree.entries[0].extra.mtime.map(|t| (t.secs, t.nanos)), Some((here.secs, here.nanos)));
    let mut out = Cursor::new(Vec::new());
    aqr_pack::write(Format::Single(Filter::Gzip), &tree, &files, &Options::default(), &mut out, &|_| {}).unwrap();
    let out = out.into_inner();
    assert_eq!(u32::from_le_bytes(out[4..8].try_into().unwrap()) as i64, here.secs);
}
