//! GameMaker's `data.win` (also `game.unx`, `game.ios`, `.droid`, and `audiogroupN.dat`), little
//! endian: an IFF-like `FORM` (the tag, then the size of what follows, u32), the chunks after it
//! (a 4-byte tag and a u32 size, then that many bytes), `GEN8` first in a `data.win` (an
//! `audiogroupN.dat` has `AUDO` alone).
//!
//! `TXTR` holds a count (u32) and that many pointers (absolute offsets in the file) to the
//! textures' entries, which have 1 to 7 words depending on the version: the scaling flag (u32),
//! from 2022 the generated mipmap levels, from 2022.9 the blob's size, the width and height and
//! the index in its group, and last the pointer to the texture's data. The data is a PNG, or in
//! newer versions QOI (`fioq`, u16 width and height and the stream; or a plain `qoif`) or BZip2
//! compressed QOI (`2zoq`, the same header). The entries are read for the pointer to a magic
//! (the first word of them that points at one); the data's length is found by walking a PNG's
//! chunks to its `IEND`, any other's is the size the entry tells (2022.9 on) or runs to the next
//! texture's data (or the
//! end of the chunk).
//! They are the files `txtr/NNN.png` (`.qoi`, `.zqoi` for the BZip2 one).
//!
//! `AUDO` holds a count and the pointers to its entries, each a length (u32) and that many bytes
//! of a WAV or an Ogg file: the files `audo/NNN.wav` (`.ogg`, else `.bin`). Their pointers may be
//! only in another order than their data's; overlaps are dropped by aqr-disk.
//!
//! The rest is every other chunk (sprites' and fonts' tables, code, strings), the entries and
//! the padding between the data.

use super::{file, u32be, u32le, within};
use aqr_disk::File;

const PNG: [u8; 8] = [0x89, b'P', b'N', b'G', 13, 10, 0x1a, 10];

pub fn starts(d: &[u8]) -> bool {
    if !d.starts_with(b"FORM") || !u32le(d, 4).is_some_and(|n| n as usize + 8 <= d.len()) {
        return false;
    }
    d.get(8..12).is_some_and(|t| t == b"GEN8" || t == b"AUDO")
}

/// The length of the PNG at the start of `d`, up to the end of its `IEND`.
fn png_len(d: &[u8]) -> Option<usize> {
    let mut at = 8;
    loop {
        let len = u32be(d, at)? as usize;
        let end = at.checked_add(len)?.checked_add(12)?;
        if end > d.len() {
            return None;
        }
        if &d[at + 4..at + 8] == b"IEND" {
            return Some(end);
        }
        at = end;
    }
}

/// The pointers of the list at `at` of a chunk `chunk` of `d`: a count, then the pointers.
fn pointers(d: &[u8], at: usize, chunk: &std::ops::Range<usize>) -> Option<Vec<usize>> {
    let count = u32le(d, at)? as usize;
    let list = within(d, at as u64 + 4, count as u64 * 4)?;
    if list.end > chunk.end {
        return None;
    }
    Some((0..count).map(|i| u32le(d, list.start + 4 * i).unwrap_or(0) as usize).collect())
}

pub fn files(d: &[u8]) -> Option<Vec<File>> {
    if !starts(d) {
        return None;
    }
    let end = (u32le(d, 4)? as usize + 8).min(d.len());
    let mut files = Vec::new();
    let mut at = 8;
    while at + 8 <= end {
        let tag = &d[at..at + 4];
        let body = within(d, at as u64 + 8, u32le(d, at + 4)? as u64)?;
        if tag == b"TXTR" {
            textures(d, &body, &mut files);
        } else if tag == b"AUDO" {
            audio(d, &body, &mut files);
        }
        at = body.end;
    }
    Some(files)
}

/// The format of a texture's blob.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Png,
    Qoi,
    Zqoi,
}

impl Format {
    fn extension(self) -> &'static str {
        match self {
            Format::Png => "png",
            Format::Qoi => "qoi",
            Format::Zqoi => "zqoi",
        }
    }
}

/// A texture's blob: where it starts, its format and the size its entry gives (if it does).
struct Blob {
    at: usize,
    format: Format,
    size: Option<usize>,
}

fn textures(d: &[u8], chunk: &std::ops::Range<usize>, files: &mut Vec<File>) {
    let Some(entries) = pointers(d, chunk.start, chunk) else { return };
    let mut blobs: Vec<Blob> = Vec::new();
    for (i, &p) in entries.iter().enumerate() {
        let limit = entries.get(i + 1).copied().filter(|&n| n > p).unwrap_or(p + 28).min(chunk.end);
        for w in (p..limit.min(p + 28)).step_by(4) {
            let Some(target) = u32le(d, w) else { break };
            let target = target as usize;
            let Some(head) = d.get(target..target + 4) else { continue };
            let format = match head {
                [0x89, b'P', b'N', b'G'] => Format::Png,
                b"fioq" | b"qoif" => Format::Qoi,
                b"2zoq" => Format::Zqoi,
                _ => continue,
            };
            if (chunk.start..chunk.end).contains(&target) {
                // (the entries of 2022.9 on have the blob's size before the dimensions)
                let size = (w == p + 24).then(|| u32le(d, p + 8)).flatten().map(|s| s as usize);
                blobs.push(Blob { at: target, format, size });
            }
            break;
        }
    }
    let mut starts: Vec<usize> = blobs.iter().map(|b| b.at).collect();
    starts.sort_unstable();
    for (n, &Blob { at: start, format, size }) in blobs.iter().enumerate() {
        let len = if format == Format::Png {
            d.get(start..chunk.end).and_then(|s| s.starts_with(&PNG).then(|| png_len(s)).flatten())
        } else {
            let next = starts.partition_point(|&s| s <= start);
            let bound = starts.get(next).copied().unwrap_or(chunk.end).min(chunk.end) - start;
            Some(size.filter(|&s| s <= bound).unwrap_or(bound))
        };
        let Some(len) = len.filter(|&l| l > 0) else { continue };
        files.push(file(format!("txtr/{n:03}.{}", format.extension()).into_bytes(), start..start + len));
    }
}

fn audio(d: &[u8], chunk: &std::ops::Range<usize>, files: &mut Vec<File>) {
    let Some(entries) = pointers(d, chunk.start, chunk) else { return };
    for (n, &p) in entries.iter().enumerate() {
        let Some(len) = u32le(d, p) else { continue };
        let Some(body) = within(d, p as u64 + 4, len as u64) else { continue };
        if body.is_empty() || body.end > chunk.end || p < chunk.start {
            continue;
        }
        let ext = match d.get(body.start..body.start + 4) {
            Some(b"RIFF") => "wav",
            Some(b"OggS") => "ogg",
            _ => "bin",
        };
        files.push(file(format!("audo/{n:03}.{ext}").into_bytes(), body));
    }
}
