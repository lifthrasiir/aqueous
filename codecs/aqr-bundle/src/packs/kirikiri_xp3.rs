//! Kirikiri's XP3 archives (the visual novel engine's `.xp3`), little endian. Written from the
//! public descriptions and unverified on real files: no sample was at hand, only archives the
//! tests write.
//!
//! The header: the 11 bytes `XP3\r\n \n\x1a\x8b\x67\x01` and the index's offset (u64). A 2.x
//! header has there the offset of a small record instead (a version u32 of 1, the byte `0x80`
//! and offsets after it); since the layout of that record is not pinned down, the u64 after the
//! byte and the one 8 bytes on are tried, and the one that leads to an index that reads is taken.
//!
//! The index: a flag byte (0 raw: the size u64 and the data; 1 zlib: the packed size u64, the
//! original size u64 and the zlib stream), whose data is chunks of a 4-byte tag, a size (u64) and
//! the contents. A `File` chunk holds the chunks of one file: `info` (flags u32, original size
//! u64, packed size u64, the name's length in UTF-16 units u16 and the name in UTF-16, made UTF-8),
//! `segm` (the segments, 28 bytes each: flags u32 whose low three bits are the compression, 0 raw
//! or 1 zlib, the offset u64, the original size u64, the packed size u64), `adlr` (the Adler-32)
//! and others, which are skipped.
//!
//! A file all of whose segments are raw is a file of their extents; one segment of zlib is a
//! plain file of its zlib bytes; other files (several zlib segments, mixed ones) and files with
//! the flag of protection (bit 31 of the `info` flags: encrypted) are left out. Encryption
//! that leaves the flag clear (the games' own) cannot be told apart.

use super::{u32le, u64le, within, zlib_inflate};
use aqr_disk::File;

const MAGIC: &[u8; 11] = b"XP3\r\n \n\x1a\x8b\x67\x01";

pub fn starts(d: &[u8]) -> bool {
    d.starts_with(MAGIC)
}

/// The index at `at`: its chunks' bytes.
fn index(d: &[u8], at: u64) -> Option<Vec<u8>> {
    let at = usize::try_from(at).ok()?;
    match *d.get(at)? & 7 {
        0 => {
            let r = within(d, at as u64 + 9, u64le(d, at + 1)?)?;
            Some(d[r].to_vec())
        }
        1 => {
            let (packed, original) = (u64le(d, at + 1)?, u64le(d, at + 9)?);
            let r = within(d, at as u64 + 17, packed)?;
            let data = zlib_inflate(&d[r], usize::try_from(original).ok()?.min(512 << 20))?;
            (data.len() as u64 == original).then_some(data)
        }
        _ => None,
    }
}

fn utf16(b: &[u8]) -> Vec<u8> {
    let units = b.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]]));
    let s: String = char::decode_utf16(units).map(|c| c.unwrap_or('\u{fffd}')).collect();
    s.replace('\\', "/").into_bytes()
}

/// The chunks of `d` as (tag, contents).
fn chunks(d: &[u8]) -> Option<Vec<(&[u8], &[u8])>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < d.len() {
        let tag = d.get(at..at + 4)?;
        let r = within(d, at as u64 + 12, u64le(d, at + 4)?)?;
        at = r.end;
        out.push((tag, &d[r]));
    }
    Some(out)
}

fn entries(index: &[u8], d: &[u8]) -> Option<Vec<File>> {
    let mut out = Vec::new();
    for (tag, body) in chunks(index)? {
        if tag != b"File" {
            continue;
        }
        let (mut name, mut protected) = (None, false);
        let mut segments = Vec::new();
        for (tag, c) in chunks(body)? {
            match tag {
                b"info" => {
                    protected = u32le(c, 0)? & 0x8000_0000 != 0;
                    let n = u16::from_le_bytes(c.get(20..22)?.try_into().ok()?) as usize;
                    name = Some(utf16(c.get(22..22 + 2 * n)?));
                }
                b"segm" => {
                    for s in c.as_chunks::<28>().0 {
                        segments.push((u32le(s, 0)? & 7, u64le(s, 4)?, u64le(s, 12)?, u64le(s, 20)?));
                    }
                }
                _ => {}
            }
        }
        let Some(name) = name else { continue };
        if protected {
            continue;
        }
        let extents: Option<Vec<_>> = match segments[..] {
            [(1, offset, _, packed)] => within(d, offset, packed).map(|r| vec![r]).into_iter().next(),
            _ if segments.iter().all(|s| s.0 == 0) => {
                let mut v = Vec::new();
                let mut ok = true;
                for &(_, offset, size, _) in &segments {
                    match within(d, offset, size) {
                        Some(r) => v.extend((!r.is_empty()).then_some(r)),
                        None => ok = false,
                    }
                }
                ok.then_some(v)
            }
            _ => None,
        };
        if let Some(extents) = extents.filter(|e| !e.is_empty()) {
            out.push(File { name, extents, rank: 0, compressed: None });
        }
    }
    Some(out)
}

pub fn files(d: &[u8]) -> Option<Vec<File>> {
    if !starts(d) {
        return None;
    }
    let at = u64le(d, 11)?;
    let mut candidates = vec![at];
    let here = usize::try_from(at).ok()?;
    if u32le(d, here) == Some(1) && here.checked_add(4).and_then(|i| d.get(i)) == Some(&0x80) {
        candidates = vec![u64le(d, here.checked_add(5)?)?, u64le(d, here.checked_add(13)?)?];
    }
    candidates.into_iter().find_map(|at| entries(&index(d, at)?, d))
}
