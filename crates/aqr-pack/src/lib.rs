//! Archives of other formats than Aqueous written from a [`Tree`] as `aqr -c` gathers it, the
//! counterpart of aqr-unpack: ZIP ([`zip`]), 7z ([`sevenz`]), ALZ and EGG ([`alzip`]), tar
//! ([`tar`]) alone or under gzip, bzip2, xz, LZMA or Zstandard ([`filter`]), and those
//! compressors' streams of a single file ([`Format::Single`]).
//!
//! What a format cannot keep of the tree (special files, extended attributes, forks) is left
//! out and told ([`Written::notes`]); the rest of the metadata goes where the format has room
//! for it. The work is spread over the pool's threads: the files of a batch compressed side by
//! side (ZIP, ALZ, EGG), or a stream cut into parts compressed side by side (gzip members,
//! bzip2 blocks, xz blocks, LZMA2 chunks, Zstandard jobs); only LZMA's `.lzma` is one thread's.

use std::io;

pub use aqr_formula::Bytes;
pub use aqr_formula::aqr::{Entry, EntryNode, Extra, Time, Tree};

pub mod alzip;
mod crypt;
pub mod filter;
pub mod sevenz;
pub mod tar;
mod time;
pub mod zip;

/// The contents of the files of a [`Tree`] ([`EntryNode::File`]), read as they are written.
pub trait Contents: Sync {
    /// The bytes of content `c`, as gathered (read in whole by [`Contents::read`]).
    fn size(&self, c: usize) -> u64;
    fn read(&self, c: usize) -> io::Result<Bytes>;
}

/// How far the writing is: content `c` begun and done, and the bytes of the contents done of
/// all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    Started(usize),
    Ended(usize),
    Done { done: u64, total: u64 },
}

/// Where an archive is written: a 7z archive's start header is written last, at its start.
pub trait Output: io::Write + io::Seek {}

impl<T: io::Write + io::Seek> Output for T {}

/// What is told of the progress.
pub type Tell<'a> = dyn Fn(Progress) + Sync + 'a;

/// An archive written: the bytes read of the files and written, and what of the tree it does
/// not keep.
#[derive(Debug, Clone, Default)]
pub struct Written {
    pub read: u64,
    pub len: u64,
    pub notes: Vec<String>,
}

/// A compressor of a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    Gzip,
    Bzip2,
    Xz,
    Lzma,
    Zstd,
}

/// What an archive is written as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Zip,
    SevenZip,
    /// ALZip's ALZ.
    Alz,
    /// ALZip's EGG.
    Egg,
    /// A tar archive, under a compressor where given.
    Tar(Option<Filter>),
    /// A compressor's stream of a single file.
    Single(Filter),
}

/// The formats by their names (`--format`), the first of each the extension it is written
/// with; and the other extensions a name ending in tells it.
const NAMES: &[(&str, Format)] = {
    use Filter::*;
    use Format::*;
    &[
        ("zip", Zip),
        ("7z", SevenZip),
        ("alz", Alz),
        ("egg", Egg),
        ("tar", Tar(None)),
        ("tar.gz", Tar(Some(Gzip))),
        ("tgz", Tar(Some(Gzip))),
        ("tar.bz2", Tar(Some(Bzip2))),
        ("tbz2", Tar(Some(Bzip2))),
        ("tbz", Tar(Some(Bzip2))),
        ("tar.xz", Tar(Some(Xz))),
        ("txz", Tar(Some(Xz))),
        ("tar.lzma", Tar(Some(Lzma))),
        ("tlz", Tar(Some(Lzma))),
        ("tar.zst", Tar(Some(Zstd))),
        ("tzst", Tar(Some(Zstd))),
        ("gz", Single(Gzip)),
        ("bz2", Single(Bzip2)),
        ("xz", Single(Xz)),
        ("lzma", Single(Lzma)),
        ("zst", Single(Zstd)),
    ]
};

impl Format {
    /// The format of a name given to `--format` (an extension without its dot).
    pub fn by_format_name(s: &str) -> Option<Format> {
        let s = s.to_ascii_lowercase();
        NAMES.iter().find(|n| n.0 == s).map(|n| n.1)
    }

    /// The names `--format` takes.
    pub fn names() -> impl Iterator<Item = &'static str> {
        NAMES.iter().map(|n| n.0)
    }

    /// The format a file's name tells by its extension (case aside), where it is one of these.
    pub fn by_name(name: &[u8]) -> Option<Format> {
        let lower = name.to_ascii_lowercase();
        // (the longest extension first: .tar.gz before .gz)
        let mut best: Option<(usize, Format)> = None;
        for &(ext, f) in NAMES {
            let n = ext.len() + 1;
            if lower.len() > n && lower.ends_with(ext.as_bytes()) && lower[lower.len() - n] == b'.' {
                best = best.filter(|b| b.0 >= n).or(Some((n, f)));
            }
        }
        best.map(|b| b.1)
    }

    /// The extension its archives are written with (with its dot).
    pub fn ext(self) -> String {
        let name = NAMES.iter().find(|n| n.1 == self).map_or("", |n| n.0);
        format!(".{name}")
    }

    /// Its levels (as `-l` gives them), and the one taken where none is given.
    pub fn levels(self) -> (std::ops::RangeInclusive<u8>, u8) {
        match self {
            Format::Zip | Format::Alz | Format::Egg => (0..=9, 6),
            Format::Tar(None) => (0..=0, 0),
            Format::SevenZip => (0..=9, 6),
            Format::Tar(Some(f)) | Format::Single(f) => match f {
                Filter::Gzip => (0..=9, 6),
                Filter::Bzip2 => (1..=9, 9),
                Filter::Xz | Filter::Lzma => (0..=9, 6),
                Filter::Zstd => (1..=22, 3),
            },
        }
    }

    /// Whether it encrypts with a passphrase.
    pub fn encrypts(self) -> bool {
        matches!(self, Format::Zip | Format::SevenZip)
    }
}

/// How an archive is written.
#[derive(Debug, Clone)]
pub struct Options {
    /// The format's level ([`Format::levels`]); its default where `None`.
    pub level: Option<u8>,
    pub threads: usize,
    /// About the most bytes of files read and compressed at once (one larger alone).
    pub batch: u64,
    /// The passphrase the archive is encrypted with ([`Format::encrypts`]).
    pub passphrase: Option<Vec<u8>>,
}

impl Default for Options {
    fn default() -> Self {
        Options { level: None, threads: 1, batch: 512 << 20, passphrase: None }
    }
}

impl Options {
    /// The level for format `f`.
    pub(crate) fn level_of(&self, f: Format) -> u8 {
        let (range, default) = f.levels();
        self.level.unwrap_or(default).clamp(*range.start(), *range.end())
    }
}

/// Writes tree `t`, the contents of its files read from `c`, as an archive of format `f` to
/// `w`, telling `tell` how far it is.
pub fn write(
    f: Format,
    t: &Tree,
    c: &dyn Contents,
    o: &Options,
    w: &mut dyn Output,
    tell: &Tell,
) -> io::Result<Written> {
    if o.passphrase.is_some() && !f.encrypts() {
        return Err(io::Error::other(format!("{}: not encrypted by a passphrase", f.ext())));
    }
    match f {
        Format::Zip => zip::write(t, c, o, w, tell),
        Format::SevenZip => sevenz::write(t, c, o, w, tell),
        Format::Alz => alzip::write_alz(t, c, o, w, tell),
        Format::Egg => alzip::write_egg(t, c, o, w, tell),
        Format::Tar(filter) => tar::write(filter, t, c, o, w, tell),
        Format::Single(filter) => filter::write_single(filter, t, c, o, w, tell),
    }
}

/// The path of entry `i` of `t`, `/` between the names (as their bytes are), and whether every
/// name was UTF-8 ([`aqr_formula::aqr::name_bytes`]).
pub(crate) fn path_of(t: &Tree, i: usize) -> (Vec<u8>, bool) {
    let mut names = Vec::new();
    let mut at = Some(i);
    while let Some(k) = at {
        names.push(&t.entries[k].name[..]);
        at = t.entries[k].parent;
    }
    let (mut path, mut utf8) = (Vec::new(), true);
    for (n, name) in names.iter().rev().enumerate() {
        let (b, raw) = aqr_formula::aqr::name_bytes(name);
        utf8 &= !raw && std::str::from_utf8(b).is_ok();
        if n > 0 {
            path.push(b'/');
        }
        path.extend_from_slice(b);
    }
    (path, utf8)
}

/// An entry to write: its path (without a `/` after a directory's), whether it is UTF-8, its
/// index in the tree and what it is.
pub(crate) struct Item {
    pub path: Vec<u8>,
    pub utf8: bool,
    pub entry: usize,
    pub kind: Kind,
}

pub(crate) enum Kind {
    Dir,
    File(usize),
    /// A symbolic link, of the target's bytes.
    Link(Vec<u8>),
    Fifo,
    Device {
        block: bool,
        major: u64,
        minor: u64,
    },
}

/// What a format keeps of the special entries and metadata, for [`items`].
#[derive(Clone, Copy)]
pub(crate) struct Keeps {
    /// FIFOs and devices.
    pub special: bool,
    /// Extended attributes.
    pub xattrs: bool,
    /// Symbolic links.
    pub links: bool,
}

/// The entries of `t` as format `name` writes them, with what it keeps; what it does not is
/// left out and told in `notes`.
pub(crate) fn items(t: &Tree, name: &str, k: Keeps, notes: &mut Vec<String>) -> Vec<Item> {
    let mut items = Vec::new();
    let (mut dropped, mut forks) = (0, 0);
    for (i, e) in t.entries.iter().enumerate() {
        let (path, utf8) = path_of(t, i);
        let lossy = || String::from_utf8_lossy(&path).into_owned();
        let kind = match &e.node {
            EntryNode::Dir => Kind::Dir,
            &EntryNode::File(c) => Kind::File(c),
            EntryNode::Link(_) if !k.links => {
                notes.push(format!("{}: a symbolic link, not kept in {name}", lossy()));
                continue;
            }
            EntryNode::Link(to) => Kind::Link(aqr_formula::aqr::name_bytes(to).0.to_vec()),
            EntryNode::Fifo if k.special => Kind::Fifo,
            &EntryNode::Device { block, major, minor } if k.special => Kind::Device { block, major, minor },
            EntryNode::Fifo | EntryNode::Device { .. } => {
                notes.push(format!("{}: a FIFO or device, not kept in {name}", lossy()));
                continue;
            }
            EntryNode::Special(_) => {
                notes.push(format!("{}: a special file, not kept in {name}", lossy()));
                continue;
            }
        };
        dropped += (!k.xattrs && !e.extra.xattrs.is_empty()) as usize;
        forks += !e.extra.forks.is_empty() as usize;
        items.push(Item { path, utf8, entry: i, kind });
    }
    let entries = |n: usize| if n == 1 { "1 entry".to_string() } else { format!("{n} entries") };
    if dropped > 0 {
        notes.push(format!("the extended attributes of {}, not kept in {name}", entries(dropped)));
    }
    if forks > 0 {
        notes.push(format!("the forks of {}, not kept in {name}", entries(forks)));
    }
    items
}

/// The batches of `items` to read at once: up to `batch` bytes of files each (`size` of an
/// item), one item at least.
pub(crate) fn batches<T>(items: &[T], batch: u64, size: impl Fn(&T) -> u64) -> Vec<std::ops::Range<usize>> {
    let (mut out, mut start) = (Vec::new(), 0);
    while start < items.len() {
        let (mut end, mut bytes) = (start, 0u64);
        while end < items.len() && (end == start || bytes + size(&items[end]) <= batch) {
            bytes += size(&items[end]);
            end += 1;
        }
        out.push(start..end);
        start = end;
    }
    out
}

/// A writer counting the bytes written through it.
pub(crate) struct Counted<'w> {
    pub w: &'w mut dyn io::Write,
    pub n: u64,
}

impl io::Write for Counted<'_> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        let n = self.w.write(b)?;
        self.n += n as u64;
        Ok(n)
    }

    fn write_all(&mut self, b: &[u8]) -> io::Result<()> {
        self.w.write_all(b)?;
        self.n += b.len() as u64;
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.w.flush()
    }
}

/// `n` random bytes (salts, initialization vectors).
pub(crate) fn random<const N: usize>() -> io::Result<[u8; N]> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|e| io::Error::other(format!("no random bytes: {e}")))?;
    Ok(b)
}
