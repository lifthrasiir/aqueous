//! Disk images (a file system's image, a disk's of partitions) taken apart into the contents of
//! their files and the rest: the partition tables, the file systems' own structures, free space.
//! Each file's contents are one content, to recompress, deduplicate or compress with their like;
//! the image is their pieces put back with the rest, so no module is needed. `formula` is the
//! entry point (see aqr-formula); `FORMAT.md` tells what the formula holds.
//!
//! The file systems are read by crates of their own (aqr-iso, …), each a [`FileSystem`] that
//! gives the files of a volume and their extents; [`files`] runs them on the image and on each
//! partition of its partition table ([`partition`]), and [`pieces`] lays the extents out.

pub mod formula;
pub mod partition;

use std::ops::Range;

/// A file of a file system: its path (of `/` separators, as UTF-8) and its contents, byte ranges
/// of the image in order (of the volume, as a [`FileSystem`] gives them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    pub name: Vec<u8>,
    pub extents: Vec<Range<usize>>,
    /// Of the files giving the same extent, the one of the lowest rank names it (a file system's
    /// own order of its trees: aqr-iso's UDF, Joliet, then ISO 9660).
    pub rank: u8,
}

/// A reader of a file system.
#[derive(Debug, Clone, Copy)]
pub struct FileSystem {
    pub name: &'static str,
    /// Whether a volume starts like one (its superblock's magic bytes, as cheap as that).
    pub starts: fn(&[u8]) -> bool,
    /// The files of a volume it starts ([`FileSystem::starts`]), their extents of it: as many as
    /// can be read, whatever the bytes.
    pub files: fn(&[u8]) -> Vec<File>,
}

/// The parts of image `d` a file system may be on: all of it, then each partition of its
/// partition table (but one starting where it does, as an ISO 9660 image's own partitions do),
/// cut at its end.
pub fn volumes(d: &[u8]) -> Vec<Range<usize>> {
    #[allow(clippy::single_range_in_vec_init)]
    let mut out = vec![0..d.len()];
    let within = |s: Range<usize>| (s.start > 0 && s.start < d.len()).then(|| s.start..s.end.min(d.len()));
    out.extend(partition::partitions(d).into_iter().filter_map(|p| within(p.span)));
    out
}

/// Whether image `d` has a volume one of `file_systems` starts ([`volumes`]).
pub fn starts(d: &[u8], file_systems: &[FileSystem]) -> bool {
    // (all of it first, the partitions only where it is none)
    let on = |v: &Range<usize>| file_systems.iter().any(|fs| (fs.starts)(&d[v.clone()]));
    on(&(0..d.len())) || volumes(d)[1..].iter().any(on)
}

/// The files of every volume of image `d` ([`volumes`]): of each, those of the first of
/// `file_systems` that starts it, their extents of the image.
pub fn files(d: &[u8], file_systems: &[FileSystem]) -> Vec<File> {
    let mut out = Vec::new();
    for v in volumes(d) {
        let vd = &d[v.clone()];
        let Some(fs) = file_systems.iter().find(|fs| (fs.starts)(vd)) else { continue };
        let mut files = (fs.files)(vd);
        for f in &mut files {
            // (an extent past the volume's end is left to [`pieces`] to cut at the image's)
            f.extents.iter_mut().for_each(|r| *r = r.start.saturating_add(v.start)..r.end.saturating_add(v.start));
        }
        out.extend(files);
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
