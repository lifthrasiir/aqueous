//! Chromium's resource packs (`.pak`: `resources.pak`, `chrome_100_percent.pak`, locales' packs;
//! the packs of Electron's, NW.js's and CEF's games), little endian, versions 4 and 5.
//!
//! Version 4: the version (u32), the count of resources (u32) and the text encoding (u8), then
//! `count + 1` entries of an id (u16) and an offset (u32). Version 5: the version (u32), the
//! encoding (u8) and 3 bytes of padding, the count of resources and of aliases (u16 each), then
//! `count + 1` entries as in version 4 and the aliases (an id and the index of the entry whose
//! data they share, u16 each). The last entry (id 0) only gives the end of the last resource's
//! data. A resource's data runs from its offset to the next entry's (the data is in the order of
//! the
//! entries; where it is not, to the next greater offset).
//!
//! A resource is a file `ID.EXT`, its extension by its magic (`png`, `jpg`, `gif`, `woff2`, `ttf`,
//! `gz` for a gzip stream, `html`, `svg` or `txt` for text, else `bin`). A resource of gzip is
//! left to be found as it is. One compressed by Brotli, which Chromium marks with a header
//! (`1E 9B` and the size of what it was, u48), is a file `ID.br` of compressed contents
//! ([`aqr_disk::Compressed::Opaque`]) of that size. Aliases are no files; the rest is the header
//! and the entries.

use super::{u16le, u32le, within};
use aqr_disk::{Compressed, File};

pub fn starts(d: &[u8]) -> bool {
    header(d).is_some()
}

/// The resources' count, where their entries start and the encoding, if `d` is a pack.
fn header(d: &[u8]) -> Option<(usize, usize)> {
    let (count, entries, encoding) = match u32le(d, 0)? {
        4 => (u32le(d, 4)? as usize, 9, *d.get(8)?),
        5 => {
            if d.get(5..8)? != [0; 3] {
                return None;
            }
            (u16le(d, 8)? as usize, 12, d[4])
        }
        _ => return None,
    };
    if encoding > 3 {
        return None;
    }
    // (the entries and the end's, then version 5's aliases: the first resource right after
    // them, the end's offset the end of the data, as Chromium writes them; else a header of a few
    // bytes would tell too many files)
    let end = count.checked_add(1)?.checked_mul(6)?.checked_add(entries)?;
    let aliases = if u32le(d, 0)? == 5 { u16le(d, 10)? as usize * 4 } else { 0 };
    let first = u32le(d, entries + 2)? as usize;
    let last = u32le(d, entries + 6 * count + 2)? as usize;
    (end + aliases <= d.len() && first == end + aliases && last == d.len()).then_some((count, entries))
}

fn kind(b: &[u8]) -> (&'static str, Option<u64>) {
    if b.starts_with(&[0x1e, 0x9b]) && b.len() > 8 {
        let mut n = [0u8; 8];
        n[..6].copy_from_slice(&b[2..8]);
        return ("br", Some(u64::from_le_bytes(n)));
    }
    let ext = match b {
        [0x89, b'P', b'N', b'G', ..] => "png",
        [0xff, 0xd8, 0xff, ..] => "jpg",
        [b'G', b'I', b'F', b'8', ..] => "gif",
        [0x1f, 0x8b, ..] => "gz",
        b"wOF2" | [b'w', b'O', b'F', b'2', ..] => "woff2",
        [b'w', b'O', b'F', b'F', ..] => "woff",
        [0, 1, 0, 0, ..] | [b't', b'r', b'u', b'e', ..] => "ttf",
        [b'O', b'T', b'T', b'O', ..] => "otf",
        [b'R', b'I', b'F', b'F', ..] => "riff",
        [b'O', b'g', b'g', b'S', ..] => "ogg",
        _ => {
            let head = &b[..b.len().min(512)];
            let trimmed = head.trim_ascii_start();
            if trimmed.starts_with(b"<svg") || (trimmed.starts_with(b"<?xml") && find(head, b"<svg")) {
                "svg"
            } else if trimmed.starts_with(b"<!") || trimmed.starts_with(b"<html") {
                "html"
            } else if !head.is_empty() && head.iter().all(|&c| c >= 9 && (c >= 32 || c == 9 || c == 10 || c == 13)) {
                "txt"
            } else {
                "bin"
            }
        }
    };
    (ext, None)
}

fn find(h: &[u8], n: &[u8]) -> bool {
    h.windows(n.len()).any(|w| w == n)
}

pub fn files(d: &[u8]) -> Option<Vec<File>> {
    let (count, entries) = header(d)?;
    let offset = |i: usize| u32le(d, entries + 6 * i + 2).map(|o| o as usize);
    let mut offsets: Vec<usize> = (0..=count).filter_map(offset).collect();
    offsets.sort_unstable();
    offsets.dedup();
    let mut files = Vec::new();
    for i in 0..count {
        let (Some(id), Some(start)) = (u16le(d, entries + 6 * i), offset(i)) else { continue };
        let following = offset(i + 1).filter(|&o| o >= start);
        let Some(end) = following.or_else(|| offsets.get(offsets.partition_point(|&o| o <= start)).copied()) else {
            continue;
        };
        let Some(range) = within(d, start as u64, (end - start) as u64) else { continue };
        if range.is_empty() {
            continue;
        }
        let (ext, packed) = kind(&d[range.clone()]);
        let compressed = packed.map(|len| Compressed::Opaque { len });
        files.push(File { name: format!("{id}.{ext}").into_bytes(), extents: vec![range], rank: 0, compressed });
    }
    Some(files)
}
