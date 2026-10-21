//! Lossless recompression of git checkouts: a directory with its `.git` becomes a fixed set
//! of interconnected streams, from which any file can be rebuilt alone (README.md has the
//! design and its trade-offs, FORMAT.md the format).
//!
//! ```no_run
//! # let dir = std::path::Path::new(".");
//! let d = aqr_git::deconstruct(dir, &Default::default()).unwrap();
//! let files = aqr_git::reconstruct(&d.streams).unwrap();
//! ```

#![allow(clippy::needless_range_loop)]

pub mod budget;
pub mod container;
mod dec;
pub mod enc;
mod gitfile;
pub mod obj;
mod rd;
mod rec;
mod table;
mod typed;
pub mod zl;

use std::io;
use std::path::Path;

pub use dec::Archive;

pub const NSTREAMS: usize = 10;
/// The file table.
pub const S_FILES: usize = 0;
/// Object ids, 20 bytes each.
pub const S_SHA: usize = 1;
/// Object records, and the id positions of git-dir files.
pub const S_ENTRY: usize = 2;
/// Delta instructions.
pub const S_OPS: usize = 3;
/// Object references.
pub const S_REFS: usize = 4;
pub const S_BLOB: usize = 5;
pub const S_TREE: usize = 6;
/// Commits and tags.
pub const S_COMMIT: usize = 7;
/// Verbatim bytes.
pub const S_RAW: usize = 8;
/// Git-dir files with their ids removed.
pub const S_TEXT: usize = 9;
pub const STREAM_NAMES: [&str; NSTREAMS] =
    ["files", "sha", "entry", "ops", "refs", "blob", "tree", "commit", "raw", "text"];

pub type Streams = [Vec<u8>; NSTREAMS];

#[derive(Clone, Debug)]
pub struct Options {
    /// Bytes of resolved objects to keep (delta bases, trees for typed literals).
    pub cache_bytes: usize,
    /// Time allowed, in ns per byte of input, by the cost model (see `budget`): zlib streams
    /// are rebuilt by emulation, largest first, while it allows; the rest stay as they are.
    /// `None`: every one that emulation reproduces.
    pub ns_per_byte: Option<f64>,
    /// The order in which zlib streams are offered to the budget.
    pub order: budget::Order,
    /// Pass over streams that do not fit, rather than stop at the first.
    pub skip: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            cache_bytes: DEFAULT_CACHE,
            ns_per_byte: Some(budget::TARGET_NS_PER_BYTE),
            order: budget::Order::Gain,
            skip: false,
        }
    }
}

/// The default budget of the resolved-object caches.
pub const DEFAULT_CACHE: usize = 32 << 20;

/// What a directory became: the streams, and what they do not hold.
pub struct Deconstructed {
    pub streams: Streams,
    /// Entries that are not regular files nor directories: symlinks with their targets.
    pub symlinks: Vec<(String, Vec<u8>)>,
    /// xxh3-64 of each file, in path order.
    pub hashes: Vec<u64>,
    /// Counters for analysis.
    pub stats: Stats,
}

/// How the files and objects were coded.
#[derive(Clone, Debug, Default)]
pub struct Stats {
    /// Files per kind (RAW, SAME, BLOB, BLOB_CRLF, LOOSE, PACK, IDX, REV, GIT).
    pub kinds: [u64; 9],
    /// Bytes per kind.
    pub kind_bytes: [u64; 9],
    /// Objects whose zlib stream emulation reproduces, of those chosen (and their bytes of
    /// data); zlib streams kept as they are (not chosen, or not reproduced); whole entries
    /// left verbatim (unreadable, or kept).
    pub zlib_emulated: u64,
    pub emulated_bytes: u64,
    pub emulated_zbytes: u64,
    /// The cost model's time of zlib emulation for the streams chosen (ns).
    pub emulated_est: f64,
    pub zlib_verbatim: u64,
    pub entry_verbatim: u64,
    /// The cost model's input, its estimate for the whole, and its budget (ns).
    pub input: budget::Input,
    pub model_ns: f64,
    pub budget_ns: f64,
    /// Deltas kept whole, and deltas whose literals stayed plain for want of the base.
    pub delta_verbatim: u64,
    pub delta_untyped: u64,
    /// Entries per pack type (0: verbatim, 1-4, 6 ofs-delta, 7 ref-delta).
    pub entry_types: [u64; 8],
}

pub const KIND_NAMES: [&str; 9] = ["raw", "same", "blob", "blob-crlf", "loose", "pack", "idx", "rev", "git"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub &'static str);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Error {}

impl From<aqr_common::Corrupt> for Error {
    fn from(e: aqr_common::Corrupt) -> Self {
        Error(e.0)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cold]
pub(crate) fn corrupt() -> Error {
    Error("corrupt input")
}

/// Reads `dir` (regular files and symlinks; everything else is skipped) and deconstructs it.
pub fn deconstruct(dir: &Path, opt: &Options) -> io::Result<Deconstructed> {
    let mut files = vec![];
    let mut symlinks = vec![];
    walk(dir, String::new(), &mut files, &mut symlinks)?;
    let mut sink = enc::VecSink::default();
    let (hashes, stats) = enc::encode(files, opt, &mut sink);
    let streams = sink.finish();
    symlinks.sort();
    Ok(Deconstructed { streams, symlinks, hashes, stats })
}

fn walk(
    dir: &Path,
    prefix: String,
    files: &mut Vec<(String, Vec<u8>)>,
    links: &mut Vec<(String, Vec<u8>)>,
) -> io::Result<()> {
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let name = e.file_name();
        let Some(name) = name.to_str() else {
            return Err(io::Error::new(io::ErrorKind::InvalidData, format!("non-UTF-8 name in {}", dir.display())));
        };
        let path = format!("{prefix}{name}");
        let ft = e.file_type()?;
        if ft.is_symlink() {
            use std::os::unix::ffi::OsStrExt;
            links.push((path, std::fs::read_link(e.path())?.as_os_str().as_bytes().to_vec()));
        } else if ft.is_dir() {
            walk(&e.path(), format!("{path}/"), files, links)?;
        } else if ft.is_file() {
            files.push((path, std::fs::read(e.path())?));
        }
    }
    Ok(())
}

/// Deconstructs a set of files (paths relative, `/`-separated).
pub fn deconstruct_files(files: Vec<(String, Vec<u8>)>, opt: &Options) -> Streams {
    let mut sink = enc::VecSink::default();
    enc::encode(files, opt, &mut sink);
    sink.finish()
}

/// One file, rebuilt from the streams.
pub fn reconstruct_single(streams: &Streams, path: &str) -> Result<Vec<u8>> {
    Archive::open(&streams.each_ref().map(|s| &s[..]))?.get(path)
}

/// Every file, rebuilt from the streams (each pack is rebuilt once).
pub fn reconstruct(streams: &Streams) -> Result<Vec<(String, Vec<u8>)>> {
    let a = Archive::open(&streams.each_ref().map(|s| &s[..]))?;
    let mut out = vec![];
    a.for_each(|p, b| {
        out.push((p.to_string(), b));
        Ok(())
    })?;
    Ok(out)
}
