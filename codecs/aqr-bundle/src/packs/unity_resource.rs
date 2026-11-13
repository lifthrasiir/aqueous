//! Unity's `.resource` files: the data that Unity's AudioClips and VideoClips stream from, one
//! member after another: FMOD's sound banks (FSB5), then the videos (WebM or MP4) as they are.
//!
//! An FSB5 bank's header, little endian: `FSB5`, the version (u32, 0 or 1), the count of samples,
//! the sizes of the samples' headers, of the table of names and of the data (u32 each), the
//! mode (u32), then in version 0 a u32 more, 8 bytes of flags and 24 of a hash: 64 bytes in
//! version 0, 60 in version 1. The bank is the header and the three parts after it. A WebM file
//! is its EBML header and its segment (elements of known sizes); an MP4 file its boxes from
//! `ftyp` on, of the kinds a file's top level has (`moov` and `mdat` among them). Each member
//! follows the one before where it ends or after up to 32 bytes of zeros (alignment).
//!
//! Each member is a file `NNN.fsb`, `NNN.webm` or `NNN.mp4` (its index, from 000); the padding
//! between them and what follows the last one read is the rest. The file must start with a bank
//! (a `.resource` of videos alone is not taken). Only version 1 of FSB5 was seen in `.resource`
//! files; version 0 is read by the description.

use super::{file, u32be, u32le, u64be};
use aqr_disk::File;

const MAGIC: &[u8; 4] = b"FSB5";
/// The longest run of padding looked across for the next member.
const MAX_PAD: usize = 32;
/// The boxes of an MP4 file's top level taken.
const BOXES: [&[u8; 4]; 13] = [
    b"ftyp", b"moov", b"mdat", b"free", b"skip", b"wide", b"uuid", b"meta", b"moof", b"mfra", b"sidx", b"styp", b"pdin",
];

/// The size of the bank at `at` of `d`, if one is there and fits.
fn bank(d: &[u8], at: usize) -> Option<usize> {
    if d.get(at..)?.get(..4)? != MAGIC {
        return None;
    }
    let head = match u32le(d, at + 4)? {
        0 => 64,
        1 => 60,
        _ => return None,
    };
    let mut size = head as u64;
    for k in [12, 16, 20] {
        size += u32le(d, at + k)? as u64;
    }
    (size <= (d.len() - at) as u64).then_some(size as usize)
}

/// An EBML element's id at `at` of `d` and its size (None where unknown), and where its data
/// starts.
fn element(d: &[u8], at: usize) -> Option<(u32, Option<u64>, usize)> {
    let id = u32be(d, at)?;
    let first = *d.get(at + 4)?;
    let len = first.leading_zeros() as usize + 1;
    if len > 8 {
        return None;
    }
    let mut size = (first as u64) & (0xff >> len);
    for &b in d.get(at + 5..at + 4 + len)? {
        size = size << 8 | b as u64;
    }
    let unknown = size == (1 << (7 * len)) - 1;
    Some((id, (!unknown).then_some(size), at + 4 + len))
}

/// The size of the WebM (Matroska) file at `at` of `d`: its EBML header and segment, of known
/// sizes.
fn webm(d: &[u8], at: usize) -> Option<usize> {
    let (0x1a45dfa3, Some(size), data) = element(d, at)? else { return None };
    let next = usize::try_from(size).ok()?.checked_add(data)?;
    let (0x18538067, Some(size), data) = element(d, next)? else { return None };
    let end = usize::try_from(size).ok()?.checked_add(data)?;
    (end <= d.len()).then_some(end - at)
}

/// The size of the MP4 file at `at` of `d`: its boxes from `ftyp` on, of the kinds a file's top
/// level has, `moov` and `mdat` among them.
fn mp4(d: &[u8], at: usize) -> Option<usize> {
    if d.get(at + 4..at + 8)? != b"ftyp" {
        return None;
    }
    let (mut end, mut moov, mut mdat) = (at, false, false);
    while let Some(kind) = d.get(end + 4..end + 8)
        && BOXES.iter().any(|b| b[..] == *kind)
    {
        moov |= kind == b"moov";
        mdat |= kind == b"mdat";
        let size = match u32be(d, end)? {
            0 => (d.len() - end) as u64,
            1 => u64be(d, end + 8)?,
            n => n as u64,
        };
        // (a box cut short: no file)
        if size < 8 || size > (d.len() - end) as u64 {
            return None;
        }
        end += size as usize;
    }
    (moov && mdat).then_some(end - at)
}

/// The member at `at` of `d`: its size and extension.
fn member(d: &[u8], at: usize) -> Option<(usize, &'static str)> {
    (bank(d, at).map(|n| (n, "fsb")))
        .or_else(|| webm(d, at).map(|n| (n, "webm")))
        .or_else(|| mp4(d, at).map(|n| (n, "mp4")))
}

pub fn starts(d: &[u8]) -> bool {
    // (a bank alone is aqr-fsb's: something must follow it)
    d.starts_with(MAGIC) && u32le(d, 4).is_some_and(|v| v <= 1) && bank(d, 0).is_some_and(|n| n < d.len())
}

/// The banks and videos of `d`; None where it does not start with a bank that fits.
pub fn files(d: &[u8]) -> Option<Vec<File>> {
    if !starts(d) {
        return None;
    }
    let mut files = Vec::new();
    let mut at = 0;
    while let Some((size, ext)) = member(d, at) {
        files.push(file(format!("{:03}.{ext}", files.len()).into_bytes(), at..at + size));
        at += size;
        // (zeros to the next member: the first after which one is)
        let pad = d[at..].iter().take(MAX_PAD).take_while(|&&b| b == 0).count();
        match (0..=pad).find(|&p| member(d, at + p).is_some()) {
            Some(p) => at += p,
            None => break,
        }
    }
    (!files.is_empty()).then_some(files)
}
