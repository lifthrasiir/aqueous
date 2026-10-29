//! ISO 9660 and UDF images (CD, DVD and BD images, UDF bridge discs) taken apart into the
//! contents of their files and the rest: the descriptors, directories and padding. Each file's
//! contents are a slice of their own, to recompress, deduplicate or compress with their like;
//! the image is their concatenation with the rest, so no module is needed. `formula` is the
//! entry point (see aqr-formula); `FORMAT.md` tells what the formula holds.
//!
//! `iso9660` and `udf` read the directory trees into files (their names and extents), and
//! [`pieces`] lays the extents of all of them out over the image.

pub mod formula;
pub mod iso9660;
pub mod udf;

use std::ops::Range;

/// Bytes of a sector, of which the volume descriptors and UDF's logical blocks are.
pub const SECTOR: usize = 2048;

/// A file of a directory tree: its path (of `/` separators, as UTF-8) and its contents, byte
/// ranges of the image in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    pub name: Vec<u8>,
    pub extents: Vec<Range<usize>>,
    /// Of the trees giving the same extent, the file of the lowest rank names it: UDF's (0),
    /// Joliet's (1), then ISO 9660's own (2), whose names are the shortest.
    pub rank: u8,
}

fn le16(b: &[u8], p: usize) -> u16 {
    u16::from_le_bytes([b[p], b[p + 1]])
}

fn le32(b: &[u8], p: usize) -> u32 {
    u32::from_le_bytes(b[p..p + 4].try_into().unwrap())
}

/// Whether `d` starts like an image: a volume structure descriptor at sector 16 (`CD001` of
/// ISO 9660, `BEA01` of UDF's recognition sequence).
pub fn starts(d: &[u8]) -> bool {
    d.get(16 * SECTOR + 1..16 * SECTOR + 6).is_some_and(|m| m == b"CD001" || m == b"BEA01")
}

/// The files of every directory tree of image `d`, UDF's first.
pub fn files(d: &[u8]) -> Vec<File> {
    let mut out = udf::files(d);
    out.extend(iso9660::files(d));
    out
}

/// A part of the image: a file's extent (`file`, an index into the files), or else the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Piece {
    pub span: Range<usize>,
    pub file: Option<usize>,
}

/// Image `d` as pieces, in order, covering it, with `files` (of [`files`]): the extents within
/// it, each the first of those starting where it does (the longest, then of the lowest rank),
/// those overlapping one before left out. Contiguous extents of a file are one. Empty where no
/// file has contents.
pub fn pieces(d: &[u8], files: &[File]) -> Vec<Piece> {
    let mut spans: Vec<(Range<usize>, usize)> = Vec::new();
    for (i, f) in files.iter().enumerate() {
        let mut last: Option<Range<usize>> = None;
        for r in &f.extents {
            let r = r.start..r.end.min(d.len());
            if r.is_empty() {
                continue;
            }
            match &mut last {
                Some(l) if l.end == r.start => l.end = r.end,
                _ => spans.extend(last.replace(r).map(|l| (l, i))),
            }
        }
        spans.extend(last.map(|l| (l, i)));
    }
    spans.sort_by_key(|(r, i)| (r.start, std::cmp::Reverse(r.end), files[*i].rank));
    let mut out = Vec::new();
    let mut at = 0;
    for (r, i) in spans {
        if r.start < at {
            continue;
        }
        if r.start > at {
            out.push(Piece { span: at..r.start, file: None });
        }
        at = r.end;
        out.push(Piece { span: r, file: Some(i) });
    }
    if out.is_empty() {
        return out;
    }
    if at < d.len() {
        out.push(Piece { span: at..d.len(), file: None });
    }
    out
}
