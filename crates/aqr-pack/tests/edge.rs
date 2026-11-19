//! Edge cases of the writers: many entries, names of every length and charset, passphrases of
//! every kind, names that collide, every compressor under tar. Each is written and read back
//! by aqr-unpack.

use std::collections::BTreeMap;
use std::io::Cursor;

use aqr_pack::{EntryNode, Extra, Filter, Format, Options, Time, Tree};
use aqr_unpack::Opened;

mod common;
use common::{Files, Kept, entry};

fn unpack_format(f: Format) -> aqr_unpack::Format {
    match f {
        Format::Zip => aqr_unpack::Format::Zip,
        Format::SevenZip => aqr_unpack::Format::SevenZip,
        Format::Alz => aqr_unpack::Format::Alz,
        Format::Egg => aqr_unpack::Format::Egg,
        Format::Tar(f) => aqr_unpack::Format::Tar(f),
        Format::Single(f) => aqr_unpack::Format::Single(f),
    }
}

/// `tree` written as `f` and read back: the archive's entries and the contents by entry (and
/// nothing told).
fn round(f: Format, tree: &Tree, files: &Files, o: &Options) -> std::io::Result<(Tree, BTreeMap<usize, Vec<u8>>)> {
    let (t, contents, notes) = round_told(f, tree, files, o)?;
    assert!(notes.is_empty(), "{f:?}: {notes:?}");
    Ok((t, contents))
}

/// What the reader tells.
#[derive(Default)]
struct Told(Kept, std::sync::Mutex<Vec<String>>);

impl aqr_unpack::Sink for Told {
    fn entries(&self, t: &Tree, new: &[usize]) -> Result<(), String> {
        self.0.entries(t, new)
    }
    fn content(&self, c: usize, d: Vec<u8>) -> Result<(), String> {
        self.0.content(c, d)
    }
    fn note(&self, n: &str) {
        self.1.lock().unwrap().push(n.to_string());
    }
}

/// [`round`], and what the reader told.
fn round_told(f: Format, tree: &Tree, files: &Files, o: &Options) -> std::io::Result<(Tree, Contents, Vec<String>)> {
    let mut out = Cursor::new(Vec::new());
    let w = aqr_pack::write(f, tree, files, o, &mut out, &|_| {})?;
    let out = out.into_inner();
    assert_eq!(w.len, out.len() as u64);
    let mut opened = Opened::open(unpack_format(f), &out).unwrap();
    if let Some(p) = &o.passphrase {
        assert!(opened.encrypted());
        assert!(opened.unlock(p).unwrap(), "the passphrase {p:?}");
    }
    if let Format::Single(_) = f {
        let mut data = Vec::new();
        opened
            .decompress(
                o.threads,
                &mut |b| {
                    data.extend_from_slice(b);
                    Ok(())
                },
                &|_| {},
            )
            .unwrap();
        return Ok((Tree::default(), BTreeMap::from([(0, data)]), Vec::new()));
    }
    let told = Told::default();
    let done = opened.unpack(o.threads, &told).unwrap();
    let notes = told.1.into_inner().unwrap();
    assert_eq!(done.skipped, notes.len());
    Ok((done.tree, told.0.0.into_inner().unwrap(), notes))
}

type Contents = BTreeMap<usize, Vec<u8>>;

fn file_tree(names: &[&[u8]]) -> Tree {
    let entries = names.iter().map(|n| entry(None, n, EntryNode::File(0), Extra::default())).collect();
    Tree { root: Extra::default(), entries }
}

const ALL: [Format; 6] =
    [Format::Zip, Format::SevenZip, Format::Alz, Format::Egg, Format::Tar(None), Format::Tar(Some(Filter::Gzip))];

/// 0xFFFF and 0x10000 entries (the ZIP's end record's limit), and more: all of them come back.
#[test]
fn many_entries() {
    let files = Files(vec![b"x".to_vec()]);
    for n in [0xfffe, 0xffff, 0x10000, 70_000] {
        let entries = (0..n).map(|i| entry(None, format!("f{i:06}"), EntryNode::File(0), Extra::default())).collect();
        let tree = Tree { root: Extra::default(), entries };
        for f in [Format::Zip, Format::SevenZip, Format::Tar(None), Format::Egg] {
            let o = Options { level: Some(1), threads: 2, ..Options::default() };
            let (got, contents) = round(f, &tree, &files, &o).unwrap();
            assert_eq!(got.entries.len(), n, "{f:?} {n}");
            assert_eq!(contents.len(), n, "{f:?} {n}");
            assert!(contents.values().all(|c| c == b"x"));
            let mut names: Vec<_> = got.entries.iter().map(|e| e.name.clone()).collect();
            names.sort();
            assert_eq!(names[n - 1], format!("f{:06}", n - 1).into_bytes(), "{f:?} {n}");
        }
    }
}

/// Names of 65535 and 65536 bytes: kept whole, or refused, never cut at 16 bits.
#[test]
fn long_names() {
    let files = Files(vec![b"x".to_vec()]);
    for len in [255, 300, 0xffff, 0x10000, 70_000] {
        let name = "n".repeat(len).into_bytes();
        let tree = file_tree(&[&name]);
        for f in ALL {
            let o = Options { level: Some(1), ..Options::default() };
            match round(f, &tree, &files, &o) {
                Ok((got, _)) => assert!(got.entries.len() == 1 && got.entries[0].name == name, "{f:?} {len}"),
                // (ZIP's and ALZ's name lengths are 16 bits)
                Err(e) => {
                    assert!(len > 0xffff && matches!(f, Format::Zip | Format::Alz), "{f:?} {len}: {e}");
                    assert!(e.to_string().contains("too long"), "{e}");
                }
            }
        }
    }
}

/// Names that are no UTF-8 (Latin-1 and Shift JIS bytes, a lone surrogate's WTF-8): the bytes
/// come back as they were, or the writer says what it could not keep.
#[test]
fn raw_names() {
    let files = Files(vec![b"x".to_vec()]);
    let names: [&[u8]; 3] = [b"caf\xe9.txt", b"\x83e\x83X\x83g", b"a\xed\xa0\x80b"];
    for name in names {
        let tree = file_tree(&[name, b"plain"]);
        for f in ALL {
            let mut out = Cursor::new(Vec::new());
            let w = aqr_pack::write(f, &tree, &files, &Options::default(), &mut out, &|_| {}).unwrap();
            let out = out.into_inner();
            let opened = Opened::open(unpack_format(f), &out).unwrap();
            let done = opened.unpack(1, &Kept::default()).unwrap();
            assert_eq!(done.tree.entries.len(), 2, "{f:?}");
            // (a name of other bytes than UTF-8 is marked as such in a tree)
            let mut got: Vec<_> =
                done.tree.entries.iter().map(|e| aqr_archive::name_bytes(&e.name).0.to_vec()).collect();
            got.sort();
            let mut want = vec![name.to_vec(), b"plain".to_vec()];
            want.sort();
            // (the reader may take an unflagged name for another charset's: then the bytes
            // are in the archive as they were; 7z keeps UTF-16, told where it wrote lossily)
            let verbatim = out.windows(name.len()).any(|w| w == name);
            let told = !w.notes.is_empty();
            assert!(
                got == want || (verbatim && f != Format::SevenZip) || told,
                "{f:?} {name:?}: {got:?} {:?}",
                w.notes
            );
        }
    }
}

/// Passphrases of every kind: empty, a space, non-ASCII, with a NUL, very long.
#[test]
fn passphrases() {
    let files = Files(vec![b"secret text ".repeat(50)]);
    let tree = file_tree(&[b"a"]);
    let long = "p\u{ac00}".repeat(5000).into_bytes();
    let pws: [&[u8]; 5] = [b"", b" ", "한글 \u{1f600}".as_bytes(), b"a\0b", &long];
    for f in [Format::Zip, Format::SevenZip] {
        for p in pws {
            let o = Options { threads: 2, level: Some(1), passphrase: Some(p.to_vec()), ..Options::default() };
            let (got, contents) = round(f, &tree, &files, &o).unwrap();
            assert_eq!(got.entries.len(), 1, "{f:?}");
            assert_eq!(contents[&0], files.0[0], "{f:?} {:?}", &p[..p.len().min(8)]);
        }
    }
}

/// Two entries of one name, or of names differing in case only (which a case-folding file
/// system takes for one): both are written, each with its own content; read back, those of one
/// name give the later, the earlier told left out (but in tar, where the later is an update).
#[test]
fn colliding_names() {
    let files = Files(vec![b"first".to_vec(), b"second".to_vec()]);
    for names in [["same", "same"], ["Same", "sAME"]] {
        let entries = names.iter().enumerate().map(|(i, n)| entry(None, n, EntryNode::File(i), Extra::default()));
        let tree = Tree { root: Extra::default(), entries: entries.collect() };
        for f in ALL {
            let mut out = Cursor::new(Vec::new());
            let o = Options { level: Some(0), ..Options::default() };
            aqr_pack::write(f, &tree, &files, &o, &mut out, &|_| {}).unwrap();
            let out = out.into_inner();
            // (a reader may keep the last of one name: both are in the archive, stored)
            for d in &files.0 {
                assert!(out.windows(d.len()).any(|w| w == &d[..]), "{f:?} {names:?}: {d:?} lost");
            }
            let (got, contents, notes) = round_told(f, &tree, &files, &o).unwrap();
            // (of one name: the later kept; but for tar's update, the earlier told left out)
            let again = names[0] == names[1] && !matches!(f, Format::Tar(_));
            let want: &[&str] = if again { &["same: given again, the earlier left out"] } else { &[] };
            assert_eq!(notes, want, "{f:?} {names:?}");
            if names[0] != names[1] {
                assert_eq!(got.entries.len(), 2, "{f:?} {names:?}");
                let mut data: Vec<_> = contents.into_values().collect();
                data.sort();
                assert_eq!(data, [b"first".to_vec(), b"second".to_vec()], "{f:?} {names:?}");
            }
        }
    }
}

/// Every compressor under tar, and alone on a single file, at its lowest and highest level,
/// on one thread and several, over data that is empty, tiny, and a few blocks.
#[test]
fn compressors() {
    let big: Vec<u8> = (0..300_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 21) as u8 & 0x3f).collect();
    let files = Files(vec![Vec::new(), b"a".to_vec(), big]);
    let x = Extra { mtime: Some(Time::new(1_000_000_000, 0)), ..Extra::default() };
    let tree = Tree {
        root: Extra::default(),
        entries: (0..3).map(|i| entry(None, format!("f{i}"), EntryNode::File(i), x.clone())).collect(),
    };
    for filter in [Filter::Gzip, Filter::Bzip2, Filter::Xz, Filter::Lzma, Filter::Zstd] {
        let (range, _) = Format::Tar(Some(filter)).levels();
        for level in [*range.start(), *range.end()] {
            for threads in [1, 3] {
                let o = Options { level: Some(level), threads, ..Options::default() };
                let (got, contents) = round(Format::Tar(Some(filter)), &tree, &files, &o).unwrap();
                assert_eq!(got.entries.len(), 3, "{filter:?} {level} {threads}");
                for (i, e) in got.entries.iter().enumerate() {
                    let EntryNode::File(c) = e.node else { panic!() };
                    assert_eq!(contents[&c], files.0[i], "{filter:?} {level} {threads} {i}");
                }
                let one =
                    Tree { root: Extra::default(), entries: vec![entry(None, "f2", EntryNode::File(0), x.clone())] };
                let files1 = Files(vec![files.0[2].clone()]);
                let (_, c) = round(Format::Single(filter), &one, &files1, &o).unwrap();
                assert_eq!(c.values().next().unwrap(), &files.0[2], "single {filter:?} {level} {threads}");
            }
        }
    }
}
