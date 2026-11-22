//! `unpack`'s body ([`unpack`]): an archive of another format (ZIP, 7z, RAR, ALZ, EGG, tar alone
//! or under each compressor, a compressor's stream of a single file) read by aqr-unpack as
//! `aqr -d` reads it, unlocked by the seeds' passphrases where it is encrypted; and where it
//! unpacks, unpacked again of every other file's path alone, those files as they were.

use std::collections::BTreeMap;
use std::sync::Mutex;

use aqr_unpack::{EntryNode, Filter, Format, Opened, Select, Sink, Tree};

/// The formats, by the input's first byte.
const FORMATS: [Format; 16] = [
    Format::Zip,
    Format::SevenZip,
    Format::Rar,
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
];

/// The byte telling format `f` ([`FORMATS`]), for the seeds.
pub fn format_byte(f: Format) -> u8 {
    FORMATS.iter().position(|&g| g == f).expect("a format fuzzed") as u8
}

/// The passphrases of the seeds' encrypted archives (aqr-unpack's tests'): ZIP's and 7z's.
const PASSPHRASES: [&str; 2] = ["pass word", "päss wörd"];

/// What an archive is unpacked to, checked as it is told: each entry after its directory, each
/// content of a file or a fork told.
#[derive(Default)]
struct Rec {
    tree: Mutex<Tree>,
    told: Mutex<Vec<bool>>,
    contents: Mutex<BTreeMap<usize, Vec<u8>>>,
}

impl Sink for Rec {
    fn entries(&self, t: &Tree, new: &[usize]) -> Result<(), String> {
        let mut told = self.told.lock().unwrap();
        told.resize(t.entries.len(), false);
        for &i in new {
            if let Some(p) = t.entries[i].parent {
                assert!(p < i && (told[p] || new.contains(&p)), "entry {i} told before its directory");
            }
            told[i] = true;
        }
        *self.tree.lock().unwrap() = t.clone();
        Ok(())
    }

    fn content(&self, c: usize, d: Vec<u8>) -> Result<(), String> {
        let t = self.tree.lock().unwrap();
        let of = |e: &aqr_unpack::Entry| e.node == EntryNode::File(c) || e.extra.forks.iter().any(|f| f.1 == c);
        assert!(t.entries.iter().any(of), "content {c} of no file or fork told");
        self.contents.lock().unwrap().insert(c, d);
        Ok(())
    }

    fn note(&self, _: &str) {}
}

/// The path of each entry of `t`, of its names' bytes (a name not WTF-8 without the mark the
/// tree has it of: `name_bytes`), as a selection takes them.
fn paths(t: &Tree) -> Vec<Vec<Vec<u8>>> {
    let mut out: Vec<Vec<Vec<u8>>> = Vec::with_capacity(t.entries.len());
    for e in &t.entries {
        let mut p = e.parent.map_or_else(Vec::new, |q| out[q].clone());
        p.push(aqr_archive::name_bytes(&e.name).0.to_vec());
        out.push(p);
    }
    out
}

/// Of the files of `t`, their contents in `contents` by their paths.
fn by_path(t: &Tree, contents: &BTreeMap<usize, Vec<u8>>) -> BTreeMap<Vec<Vec<u8>>, Vec<u8>> {
    let mut out = BTreeMap::new();
    for (p, e) in paths(t).into_iter().zip(&t.entries) {
        if let EntryNode::File(c) = e.node
            && let Some(d) = contents.get(&c)
        {
            out.insert(p, d.clone());
        }
    }
    out
}

/// Archive `d` of format `f` opened, unlocked where it is encrypted (by the seeds' passphrases),
/// of `select` alone where given.
fn opened(f: Format, d: &[u8], select: Option<Select>) -> Option<Opened<'_>> {
    let mut o = Opened::open(f, d).ok()?;
    if o.encrypted() {
        for p in PASSPHRASES {
            if o.unlock(p.as_bytes()).unwrap_or(false) {
                break;
            }
        }
    }
    if let Some(s) = select {
        o.select(s);
    }
    Some(o)
}

pub fn unpack(data: &[u8]) {
    let Some((&b, d)) = data.split_first() else { return };
    let f = FORMATS[b as usize % FORMATS.len()];
    let Some(o) = opened(f, d, None) else { return };
    let _ = (o.units(), o.names_told(), o.sealed());
    if !o.is_tree() {
        let _ = o.decompress(1, &mut |_| Ok(()), &|_| {});
        return;
    }
    let all = Rec::default();
    let Ok(done) = o.unpack(1, &all) else { return };
    let files = by_path(&done.tree, &all.contents.into_inner().unwrap());
    // of every other file's path alone: those files as they were (the tree told is the whole
    // archive's but of tar's, where it is what is selected)
    let some: Vec<Vec<Vec<u8>>> = paths(&done.tree)
        .into_iter()
        .zip(&done.tree.entries)
        .filter(|(_, e)| matches!(e.node, EntryNode::File(_)))
        .map(|(p, _)| p)
        .step_by(2)
        .collect();
    if some.is_empty() {
        return;
    }
    let Some(o) = opened(f, d, Some(Select::new(some.clone()))) else {
        panic!("not opened again to select");
    };
    let picked = Rec::default();
    match o.unpack(1, &picked) {
        Ok(again) => {
            let got = by_path(&again.tree, &picked.contents.into_inner().unwrap());
            for p in &some {
                let (g, w) = (got.get(p), files.get(p));
                assert!(
                    g == w,
                    "of some paths: {p:?} unpacked otherwise ({:?} bytes, not {:?})",
                    g.map(Vec::len),
                    w.map(Vec::len)
                );
            }
        }
        Err(e) => panic!("of some paths: not unpacked, though the archive unpacks: {e}"),
    }
}
