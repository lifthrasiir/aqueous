//! `pack`'s body ([`pack`]): a tree made of the input written by aqr-pack as an archive of
//! another format (as `aqr -c --format FORMAT` writes it) and read back by aqr-unpack (as `aqr
//! -d` reads it): its files have to come back as they were, by their paths; its directories and
//! links too where the writer kept all of the tree (told nothing left out).

use std::collections::BTreeMap;
use std::io::Cursor;
use std::sync::Mutex;

use aqr_archive::{Entry, EntryNode, Extra, Tree};
use aqr_pack::{Bytes, Contents, Filter, Format, Options};
use libfuzzer_sys::arbitrary::{Result, Unstructured};

/// The formats written, by the input's first byte: those aqr-pack writes and aqr-unpack reads.
const FORMATS: [Format; 16] = [
    Format::Zip,
    Format::SevenZip,
    Format::Alz,
    Format::Egg,
    Format::Tar(None),
    Format::Tar(Some(Filter::Gzip)),
    Format::Tar(Some(Filter::Bzip2)),
    Format::Tar(Some(Filter::Xz)),
    Format::Tar(Some(Filter::Lzma)),
    Format::Tar(Some(Filter::Zstd)),
    Format::Single(Filter::Gzip),
    Format::Single(Filter::Bzip2),
    Format::Single(Filter::Xz),
    Format::Single(Filter::Lzma),
    Format::Single(Filter::Zstd),
    Format::Zip,
];

/// The names of a tree's entries: ASCII, and Hangul (of more than one byte in UTF-8). Names not
/// UTF-8 are left out: a reader takes their encoding as it guesses it.
const NAMES: [&str; 6] = ["a", "b", "c", "d", "가", "나다"];

/// The format aqr-unpack reads an archive of format `f` as.
fn read_as(f: Format) -> aqr_unpack::Format {
    match f {
        Format::Zip => aqr_unpack::Format::Zip,
        Format::SevenZip => aqr_unpack::Format::SevenZip,
        Format::Alz => aqr_unpack::Format::Alz,
        Format::Egg => aqr_unpack::Format::Egg,
        Format::Tar(f) => aqr_unpack::Format::Tar(f),
        Format::Single(f) => aqr_unpack::Format::Single(f),
    }
}

/// What a tree has at a path: a file's bytes, a directory, a link's target.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    File(Vec<u8>),
    Dir,
    Link(Vec<u8>),
}

type Paths = BTreeMap<Vec<Vec<u8>>, Node>;

/// The contents of a tree's files.
struct Files(Vec<Vec<u8>>);

impl Contents for Files {
    fn size(&self, c: usize) -> u64 {
        self.0[c].len() as u64
    }

    fn read(&self, c: usize) -> std::io::Result<Bytes> {
        Ok(self.0[c].clone().into())
    }
}

/// A tree of `u` (files, directories and links at paths of up to 3 of [`NAMES`]; of a single
/// file where `single`), the contents of its files, and what it has by path.
fn tree(u: &mut Unstructured, single: bool) -> Result<(Tree, Files, Paths)> {
    let (mut t, mut files, mut want) = (Tree::default(), Vec::new(), Paths::new());
    let mut at: BTreeMap<Vec<Vec<u8>>, usize> = BTreeMap::new();
    let n = if single { 1 } else { u.int_in_range(1..=8)? };
    for _ in 0..n {
        let depth = if single { 1 } else { u.int_in_range(1..=3)? };
        let p: Vec<Vec<u8>> =
            (0..depth).map(|_| u.choose(&NAMES).map(|n| n.as_bytes().to_vec())).collect::<Result<_>>()?;
        // (its directories, where they are not other nodes; the path not taken)
        if at.contains_key(&p) || (1..p.len()).any(|k| want.get(&p[..k]).is_some_and(|w| *w != Node::Dir)) {
            continue;
        }
        let mut parent = None;
        for k in 1..p.len() {
            parent = Some(*at.entry(p[..k].to_vec()).or_insert_with(|| {
                want.insert(p[..k].to_vec(), Node::Dir);
                t.entries.push(Entry { parent, name: p[k - 1].clone(), node: EntryNode::Dir, extra: Extra::default() });
                t.entries.len() - 1
            }));
        }
        let (node, w) = match if single { 0 } else { u.int_in_range(0..=5)? } {
            0..=3 => {
                let len = u.int_in_range(0..=4096)?;
                let d: Vec<u8> = match u.arbitrary::<bool>()? {
                    true => u.bytes(len.min(u.len()))?.to_vec(),
                    false => {
                        let b: u8 = u.arbitrary()?;
                        vec![b; len]
                    }
                };
                files.push(d.clone());
                (EntryNode::File(files.len() - 1), Node::File(d))
            }
            4 => (EntryNode::Dir, Node::Dir),
            _ => {
                let to = b"a/b".to_vec();
                (EntryNode::Link(to.clone()), Node::Link(to))
            }
        };
        t.entries.push(Entry { parent, name: p.last().unwrap().clone(), node, extra: Extra::default() });
        at.insert(p.clone(), t.entries.len() - 1);
        want.insert(p, w);
    }
    Ok((t, Files(files), want))
}

/// What aqr-unpack is told of an archive: its tree and its contents.
#[derive(Default)]
struct Rec {
    contents: Mutex<BTreeMap<usize, Vec<u8>>>,
}

impl aqr_unpack::Sink for Rec {
    fn entries(&self, _: &Tree, _: &[usize]) -> std::result::Result<(), String> {
        Ok(())
    }

    fn content(&self, c: usize, d: Vec<u8>) -> std::result::Result<(), String> {
        self.contents.lock().unwrap().insert(c, d);
        Ok(())
    }

    fn note(&self, _: &str) {}
}

/// What tree `t` has by path, of contents `contents`.
fn paths(t: &Tree, contents: &BTreeMap<usize, Vec<u8>>) -> Paths {
    let mut ps: Vec<Vec<Vec<u8>>> = Vec::with_capacity(t.entries.len());
    let mut out = Paths::new();
    for e in &t.entries {
        let mut p = e.parent.map_or_else(Vec::new, |q| ps[q].clone());
        p.push(aqr_archive::name_bytes(&e.name).0.to_vec());
        let n = match &e.node {
            &EntryNode::File(c) => Some(Node::File(contents.get(&c).cloned().unwrap_or_default())),
            EntryNode::Dir => Some(Node::Dir),
            EntryNode::Link(to) => Some(Node::Link(aqr_archive::name_bytes(to).0.to_vec())),
            _ => None,
        };
        if let Some(n) = n {
            out.insert(p.clone(), n);
        }
        ps.push(p);
    }
    out
}

pub fn pack(data: &[u8]) {
    let mut u = Unstructured::new(data);
    let Ok(b) = u.arbitrary::<u8>() else { return };
    let f = FORMATS[b as usize % FORMATS.len()];
    let single = matches!(f, Format::Single(_));
    let (Ok(level), Ok(sealed), Ok((t, files, want))) = (u.int_in_range(0..=2u8), u.ratio(1, 4), tree(&mut u, single))
    else {
        return;
    };
    let pass = (sealed && f.encrypts()).then(|| b"pass word".to_vec());
    let o = Options {
        level: Some(f.levels().0.start().max(&level).to_owned()),
        passphrase: pass.clone(),
        ..Options::default()
    };
    let mut out = Cursor::new(Vec::new());
    let written = match aqr_pack::write(f, &t, &files, &o, &mut out, &|_| {}) {
        Ok(w) => w,
        Err(e) => panic!("{f:?}: a tree not written: {e}"),
    };
    let a = out.into_inner();
    let mut o = match aqr_unpack::Opened::open(read_as(f), &a) {
        Ok(o) => o,
        Err(e) => panic!("{f:?}: an archive written not opened: {e}"),
    };
    // (of files encrypted: an archive of no file has nothing to unlock)
    if let Some(p) = &pass
        && o.encrypted()
    {
        assert!(o.unlock(p).unwrap_or(false), "{f:?}: not unlocked by its passphrase");
    }
    if single {
        let mut d = Vec::new();
        if let Err(e) = o.decompress(1, &mut |b| Ok(d.extend_from_slice(b)), &|_| {}) {
            panic!("{f:?}: a stream written not decompressed: {e}");
        }
        assert!(want.values().next() == Some(&Node::File(d)), "{f:?}: decompressed otherwise");
        return;
    }
    let r = Rec::default();
    let done = match o.unpack(1, &r) {
        Ok(d) => d,
        Err(e) => panic!("{f:?}: an archive written not unpacked: {e}"),
    };
    let got = paths(&done.tree, &r.contents.into_inner().unwrap());
    // (its files always; the rest where all was kept)
    let of_files = |m: &Paths| -> Paths {
        m.iter().filter(|(_, n)| matches!(n, Node::File(_))).map(|(p, n)| (p.clone(), n.clone())).collect()
    };
    assert!(of_files(&got) == of_files(&want), "{f:?}: files read back otherwise\n{got:?}\n{want:?}");
    if written.notes.is_empty() {
        assert!(got == want, "{f:?}: the tree read back otherwise\n{got:?}\n{want:?}");
    }
}
