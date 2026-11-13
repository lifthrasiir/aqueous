//! Bethesda's BA2 archives (`BTDX`; Fallout 4, Fallout 76, Starfield), little endian. Written
//! from the public descriptions (UESP wiki, the modding community's notes) and unverified on
//! real files: no sample was at hand, only archives the tests write.
//!
//! The header: the magic, the version (u32), the type (`GNRL` general, `DX10` textures), the
//! file count (u32) and the offset of the name table (u64, 0 where there is none); versions 2
//! and 3 (Starfield) add a u64, and 3 a compression method (u32: 0 zlib, 3 LZ4), after them.
//! Versions 1, 7 and 8 are taken to have the header of 1 (the layout of 7 and 8 is not pinned
//! down). Other versions are not read.
//!
//! `GNRL`: a record of 36 bytes for each file: the name's hash (u32), the extension (4 bytes),
//! the directory's hash (u32), flags (u32), the offset (u64), the size packed (u32; 0 where it
//! is stored), the size unpacked (u32) and `BAADF00D`. A stored file is a plain file; packed,
//! zlib is a plain file of its bytes, and LZ4 (version 3 with its method 3) a file of
//! compressed contents ([`aqr_disk::Compressed::Opaque`]) with the size unpacked.
//!
//! `DX10`: for each texture a record of 24 bytes (hashes and extension as above, a byte,
//! the count of chunks (u8), the size of a chunk's record (u16, 24), height, width (u16 each),
//! the mips, the format, the flags and the tile mode (u8 each)), then its chunks' records of 24
//! bytes (the offset u64, the size packed u32, the size unpacked u32, the first and last mips
//! u16 each, `BAADF00D`). A texture whose chunks are all stored is one file of the chunks'
//! extents named `.dx10chunks` (the DDS header that the readers make is not in the archive,
//! so it is not a `.dds`); a texture with a packed chunk is left out.
//!
//! The names are the name table's (u16 length and the bytes for each file, in order, with
//! `\` made `/`); without the table, the index with the extension.

use super::{file, u16le, u32le, u64le, within};
use aqr_disk::{Compressed, File};

pub fn starts(d: &[u8]) -> bool {
    d.starts_with(b"BTDX")
        && matches!(u32le(d, 4), Some(1 | 2 | 3 | 7 | 8))
        && matches!(d.get(8..12), Some(b"GNRL" | b"DX10"))
}

fn ext(r: &[u8]) -> &[u8] {
    let n = r.iter().position(|&b| b == 0).unwrap_or(r.len());
    &r[..n]
}

fn named(path: &[u8], i: usize, extension: &[u8]) -> Vec<u8> {
    let mut n = if path.is_empty() {
        let mut n = format!("{i:08}").into_bytes();
        if !extension.is_empty() {
            n.push(b'.');
            n.extend_from_slice(extension);
        }
        n
    } else {
        path.to_vec()
    };
    for b in &mut n {
        if *b == b'\\' {
            *b = b'/';
        }
    }
    n
}

pub fn files(d: &[u8]) -> Option<Vec<File>> {
    if !starts(d) {
        return None;
    }
    let version = u32le(d, 4)?;
    let general = &d[8..12] == b"GNRL";
    let (count, names_at) = (u32le(d, 12)? as usize, u64le(d, 16)?);
    let (header, lz4) = match version {
        2 => (32, false),
        3 => (
            36,
            match u32le(d, 32)? {
                0 => false,
                3 => true,
                _ => return None,
            },
        ),
        _ => (24, false),
    };
    if header > d.len() {
        return None;
    }

    let mut names = Vec::new();
    if names_at != 0 {
        let mut at = usize::try_from(names_at).ok()?;
        for _ in 0..count {
            let len = u16le(d, at)? as usize;
            names.push(d.get(at + 2..at + 2 + len)?);
            at += 2 + len;
        }
    }
    let name = |i: usize, extension: &[u8]| named(names.get(i).copied().unwrap_or(b""), i, extension);

    let mut out = Vec::new();
    let mut at = header;
    for i in 0..count {
        if general {
            let r = d.get(at..at + 36)?;
            at += 36;
            let (offset, packed, unpacked) = (u64le(r, 16)?, u32le(r, 24)?, u32le(r, 28)?);
            let (len, compressed) = if packed == 0 {
                (unpacked, None)
            } else {
                (packed, lz4.then_some(Compressed::Opaque { len: unpacked as u64 }))
            };
            let Some(extent) = within(d, offset, len as u64) else { continue };
            if !extent.is_empty() {
                let mut f = file(name(i, ext(&r[4..8])), extent);
                f.compressed = compressed;
                out.push(f);
            }
        } else {
            let r = d.get(at..at + 24)?;
            at += 24;
            let (chunks, size) = (r[13] as usize, u16le(r, 14)?);
            if size != 24 {
                return None;
            }
            let records = d.get(at..at.checked_add(chunks * 24)?)?;
            at += chunks * 24;
            let mut extents = Vec::new();
            let mut stored = true;
            for c in records.as_chunks::<24>().0 {
                let (offset, packed, unpacked) = (u64le(c, 0)?, u32le(c, 8)?, u32le(c, 12)?);
                stored &= packed == 0;
                match within(d, offset, unpacked as u64) {
                    Some(e) if !e.is_empty() => extents.push(e),
                    Some(_) => {}
                    None => stored = false,
                }
            }
            if stored && !extents.is_empty() {
                let mut n = name(i, ext(&r[4..8]));
                n.extend_from_slice(b".dx10chunks");
                out.push(File { name: n, extents, rank: 0, compressed: None });
            }
        }
    }
    Some(out)
}
