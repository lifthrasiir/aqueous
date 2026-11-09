//! Disk images (a file system's image, a disk's of partitions) taken apart into the contents of
//! their files and the rest: the partition tables, the file systems' own structures, free space.
//! Each file's contents are one content, to recompress, deduplicate or compress with their like;
//! the image is their pieces put back with the rest, so no module is needed. `formula` is the
//! entry point (see aqr-formula); `FORMAT.md` tells what the formula holds.
//!
//! The file systems are read by crates of their own (aqr-iso, …), each a [`FileSystem`] that
//! gives the files of a volume and their extents; [`files`] runs them on the disk (the image, or
//! the virtual disk of its [`container`]) and on each partition of its partition table
//! ([`partition`]), and [`pieces`] lays the extents out over the image.

pub mod chunked;
pub mod compressed;
pub mod container;
pub mod dmg;
pub mod ewf;
pub mod formula;
pub mod image;
pub mod partition;

use std::ops::Range;

pub use image::{Image, Plain, Sub};

/// A file of a file system: its path (of `/` separators, as UTF-8) and its contents, byte ranges
/// in order (of the volume, as a [`FileSystem`] gives them; of the image, as [`files`] does).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    pub name: Vec<u8>,
    pub extents: Vec<Range<usize>>,
    /// Of the files giving the same extent, the one of the lowest rank names it (a file system's
    /// own order of its trees: aqr-iso's UDF, Joliet, then ISO 9660).
    pub rank: u8,
    /// How its contents are stored, where compressed: its extents are then of the compressed
    /// bytes, and its contents are what they decompress to.
    pub compressed: Option<Compressed>,
}

/// How a file's contents are stored compressed by its file system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Compressed {
    /// By Windows' WOF (an NTFS file's `WofCompressedData`): a table of where its chunks start
    /// (but the first; 4 bytes each, 8 where `len` is over 4 GiB), then the chunks, each of the
    /// chunk's bytes of data but the last, compressed on its own (stored where that is no
    /// smaller), by `algorithm`: 0 XPRESS (Huffman) of 4 KiB chunks, 1 LZX of 32 KiB, 2 and 3
    /// XPRESS of 8 and 16 KiB. `len` bytes of data in all.
    Wof { algorithm: u32, len: u64 },
    /// By NTFS's LZNT1 (a compressed attribute): in units of `unit` bytes of data (the last
    /// fewer), each of `allocated` bytes of the extents in order: as many as `unit` stored as it is,
    /// 0 a hole of zeros, else compressed (the chunks of LZNT1, then what is left of the last
    /// cluster). `len` bytes of data in all.
    Lznt1 { len: u64, unit: u32, allocated: Vec<u32> },
}

impl Compressed {
    /// Bytes of data.
    pub fn len(&self) -> u64 {
        match *self {
            Compressed::Wof { len, .. } | Compressed::Lznt1 { len, .. } => len,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Bytes of data of a WOF chunk of `algorithm`.
    pub fn wof_chunk(algorithm: u32) -> Option<usize> {
        match algorithm {
            0 => Some(4 << 10),
            1 => Some(32 << 10),
            2 => Some(8 << 10),
            3 => Some(16 << 10),
            _ => None,
        }
    }
}

/// A reader of a file system.
#[derive(Debug, Clone, Copy)]
pub struct FileSystem {
    pub name: &'static str,
    /// Whether a volume starts like one (its superblock's magic bytes, as cheap as that).
    pub starts: fn(&dyn Image) -> bool,
    /// The files of a volume it starts ([`FileSystem::starts`]), their extents of it: as many as
    /// can be read, whatever the bytes, reading only what it needs (of a container, each read
    /// puts its bytes together).
    pub files: fn(&dyn Image) -> Vec<File>,
}

/// The parts of disk `d` a file system may be on: all of it, then each partition of its
/// partition table (but one starting where it does, as an ISO 9660 image's own partitions do),
/// cut at its end.
pub fn volumes(d: &dyn Image) -> Vec<Range<usize>> {
    #[allow(clippy::single_range_in_vec_init)]
    let mut out = vec![0..d.len()];
    let within = |s: Range<usize>| (s.start > 0 && s.start < d.len()).then(|| s.start..s.end.min(d.len()));
    out.extend(partition::partitions(d).into_iter().filter_map(|p| within(p.span)));
    out
}

/// Whether disk `d` has a volume one of `file_systems` starts ([`volumes`]).
fn starts_on(d: &dyn Image, file_systems: &[FileSystem]) -> bool {
    // (all of it first, the partitions only where it is none)
    let on = |v: &Range<usize>| file_systems.iter().any(|fs| (fs.starts)(&Sub { of: d, span: v.clone() }));
    on(&(0..d.len())) || volumes(d)[1..].iter().any(on)
}

/// Whether image `d` (a disk, or a container of one: [`container`]) has a volume one of
/// `file_systems` starts.
pub fn starts(d: &[u8], file_systems: &[FileSystem]) -> bool {
    match container::open(d) {
        Some(m) => starts_on(&m, file_systems),
        None => starts_on(&Plain(d), file_systems),
    }
}

/// The files of every volume of disk `d` ([`volumes`]): of each, those of the first of
/// `file_systems` that starts it, their extents of the disk.
fn files_on(d: &dyn Image, file_systems: &[FileSystem]) -> Vec<File> {
    let mut out = Vec::new();
    for v in volumes(d) {
        let vd = Sub { of: d, span: v.clone() };
        let Some(fs) = file_systems.iter().find(|fs| (fs.starts)(&vd)) else { continue };
        let mut files = (fs.files)(&vd);
        for f in &mut files {
            // (an extent past the volume's end is left to [`pieces`] to cut at the image's)
            f.extents.iter_mut().for_each(|r| *r = r.start.saturating_add(v.start)..r.end.saturating_add(v.start));
        }
        out.extend(files);
    }
    out
}

/// The files of image `d` (`files_on` of its disk), their extents of the image: of a
/// container, where the disk's are in it, those of blocks it does not hold (zeros) left out.
pub fn files(d: &[u8], file_systems: &[FileSystem]) -> Vec<File> {
    let Some(m) = container::open(d) else { return files_on(&Plain(d), file_systems) };
    let mut out = files_on(&m, file_systems);
    for f in &mut out {
        f.extents = f.extents.iter().flat_map(|r| m.translate(r.start.min(m.len())..r.end.min(m.len()))).collect();
    }
    out
}

/// A part of the image: a piece of a file's contents, or else the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Piece {
    pub span: Range<usize>,
    /// The file (an index into the files) and which of its pieces, in the order of its contents.
    pub file: Option<(usize, usize)>,
}

/// Image `d` as pieces, in order, covering it, with `files`: the extents within it, each the
/// first of those starting where it does (the longest, then of the lowest rank), those overlapping
/// one before left out. Contiguous extents of a file are one. A file's pieces are numbered in the
/// order of its extents, those left out skipped; its contents are its pieces in that order. Empty
/// where no file has contents.
pub fn pieces(d: &[u8], files: &[File]) -> Vec<Piece> {
    // (each extent with its file and its place among the file's)
    let mut spans: Vec<(Range<usize>, usize, usize)> = Vec::new();
    for (i, f) in files.iter().enumerate() {
        let mut last: Option<Range<usize>> = None;
        let mut k = 0;
        for r in &f.extents {
            let r = r.start..r.end.min(d.len());
            if r.is_empty() {
                continue;
            }
            match &mut last {
                Some(l) if l.end == r.start => l.end = r.end,
                _ => {
                    if let Some(l) = last.replace(r) {
                        spans.push((l, i, k));
                        k += 1;
                    }
                }
            }
        }
        spans.extend(last.map(|l| (l, i, k)));
    }
    spans.sort_by_key(|(r, i, _)| (r.start, std::cmp::Reverse(r.end), files[*i].rank));
    let mut out = Vec::new();
    let mut at = 0;
    for (r, i, k) in spans {
        if r.start < at {
            continue;
        }
        if r.start > at {
            out.push(Piece { span: at..r.start, file: None });
        }
        at = r.end;
        out.push(Piece { span: r, file: Some((i, k)) });
    }
    if out.is_empty() {
        return out;
    }
    if at < d.len() {
        out.push(Piece { span: at..d.len(), file: None });
    }
    // each file's pieces numbered again, in order, as some were left out
    let mut of: Vec<Vec<usize>> = vec![Vec::new(); files.len()];
    for (j, p) in out.iter().enumerate() {
        if let Some((i, _)) = p.file {
            of[i].push(j);
        }
    }
    for js in &mut of {
        js.sort_by_key(|&j| out[j].file.map(|f| f.1));
        for (k, &j) in js.iter().enumerate() {
            out[j].file.as_mut().expect("a file's").1 = k;
        }
    }
    out
}
