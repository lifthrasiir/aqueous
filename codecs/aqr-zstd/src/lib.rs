//! Zstandard frames ([RFC 8878]) taken apart by their structure, so that an archive keeps them
//! as they are and gives what they hold to the files having the same: the blocks of a frame are
//! what an archive's Patch instruction decompresses (of an empty source), the frame's content
//! split from it ([`formula`], with the feature `encode`). No compressor is reproduced, as
//! rebuilding a frame from its content would gain little: the frame is already about as small
//! as the archive would make the content.
//!
//! [RFC 8878]: https://www.rfc-editor.org/rfc/rfc8878

#[cfg(feature = "encode")]
pub mod formula;

use std::ops::Range;

/// The magic number of a frame (`0xFD2FB528`, little endian).
pub const MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];

/// The most bytes of content a frame may have to be taken: the window of the frame that the
/// Patch instruction is decompressed in, a single segment of its content, is at most 2^31
/// bytes (`ZSTD_WINDOWLOG_MAX` of 64-bit builds).
pub const CONTENT_MAX: u64 = 1 << 31;

/// The most bytes a block holds (`Block_Maximum_Size` at most), which a Patch instruction's
/// blocks may hold.
const BLOCK_MAX: usize = 128 << 10;

/// A piece of a sequence of frames ([`parse`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    /// A Zstandard frame of the bytes `at`: its blocks those of `blocks`.
    Frame(Frame),
    /// A skippable frame of the bytes `at`.
    Skippable { at: Range<usize> },
}

/// A Zstandard frame ([`Piece::Frame`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Its bytes: the header, the blocks, the checksum.
    pub at: Range<usize>,
    /// The bytes of its blocks, the last one the final one.
    pub blocks: Range<usize>,
    /// Its dictionary ID (0 where it has none).
    pub dict: u32,
    /// The size of its content that its header tells, if it does.
    pub content: Option<u64>,
    /// The size its blocks' headers tell, of those raw or RLE (an upper bound of the content
    /// otherwise: a compressed block holds at most [`BLOCK_MAX`]).
    pub bound: u64,
}

/// Frames and skippable frames one after another from the start of `d` ([`parse`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frames {
    pub pieces: Vec<Piece>,
    /// Bytes of `d` that they are: what follows is something else.
    pub len: usize,
}

impl Frames {
    /// The Zstandard frames.
    pub fn frames(&self) -> impl Iterator<Item = &Frame> {
        self.pieces.iter().filter_map(|p| match p {
            Piece::Frame(f) => Some(f),
            Piece::Skippable { .. } => None,
        })
    }
}

/// Whether `d` starts like a Zstandard frame: its magic number, a frame header and the header
/// of its first block that hold. The rest of the frame may be past the end of `d`.
pub fn starts(d: &[u8]) -> bool {
    d.starts_with(&MAGIC) && header(d, 0).is_some_and(|h| d.get(h.0..h.0 + 3).is_some_and(|b| b[0] >> 1 & 3 != 3))
}

/// The frames and skippable frames from the start of `d`, as long as their structure holds
/// (their headers, their blocks' headers): None where `d` does not start with a Zstandard
/// frame. Nothing is decompressed.
pub fn parse(d: &[u8]) -> Option<Frames> {
    if !d.starts_with(&MAGIC) {
        return None;
    }
    let (mut pieces, mut at) = (Vec::new(), 0);
    while at < d.len() {
        let p = match d.get(at..at + 4) {
            Some(m) if m == MAGIC => frame(d, at).map(Piece::Frame),
            Some(&[m, 0x2a, 0x4d, 0x18]) if m & 0xf0 == 0x50 => skippable(d, at),
            _ => None,
        };
        let Some(p) = p else { break };
        at = match &p {
            Piece::Frame(f) => f.at.end,
            Piece::Skippable { at } => at.end,
        };
        pieces.push(p);
    }
    (!pieces.is_empty()).then_some(Frames { pieces, len: at })
}

fn skippable(d: &[u8], at: usize) -> Option<Piece> {
    let n = u32::from_le_bytes(*d.get(at + 4..)?.first_chunk()?) as usize;
    let end = (at + 8).checked_add(n).filter(|&e| e <= d.len())?;
    Some(Piece::Skippable { at: at..end })
}

/// The frame at `at` of `d`, if its structure holds.
fn frame(d: &[u8], at: usize) -> Option<Frame> {
    let (mut p, fhd, dict, content, window) = header(d, at)?;
    let block_max = window.min(BLOCK_MAX as u64) as usize;
    let start = p;
    let mut bound = 0u64;
    loop {
        let h = d.get(p..p + 3)?;
        let h = h[0] as u32 | (h[1] as u32) << 8 | (h[2] as u32) << 16;
        let (last, kind, size) = (h & 1 != 0, (h >> 1) & 3, (h >> 3) as usize);
        let stored = match kind {
            0 => size,
            1 => 1,
            2 => size,
            _ => return None,
        };
        if size > block_max {
            return None;
        }
        bound += if kind == 2 { block_max as u64 } else { size as u64 };
        p = (p + 3).checked_add(stored).filter(|&e| e <= d.len())?;
        if last {
            break;
        }
    }
    let blocks = start..p;
    if fhd & 0x04 != 0 {
        p += 4;
        if p > d.len() {
            return None;
        }
    }
    Some(Frame { at: at..p, blocks, dict, content, bound })
}

/// The header of the frame at `at` of `d`: where its blocks start, its descriptor, dictionary
/// ID, content size if told, and window size.
fn header(d: &[u8], at: usize) -> Option<(usize, u8, u32, Option<u64>, u64)> {
    let fhd = *d.get(at + 4)?;
    // (the reserved bit)
    if fhd & 0x08 != 0 {
        return None;
    }
    let single = fhd & 0x20 != 0;
    let mut p = at + 5;
    let window = if single {
        None
    } else {
        let wd = *d.get(p)?;
        p += 1;
        let base = 1u64 << (10 + (wd >> 3));
        Some(base + base / 8 * (wd & 7) as u64)
    };
    let dict_len = [0, 1, 2, 4][(fhd & 3) as usize];
    let mut dict = [0; 4];
    dict[..dict_len].copy_from_slice(d.get(p..p + dict_len)?);
    p += dict_len;
    let fcs_len = [if single { 1 } else { 0 }, 2, 4, 8][(fhd >> 6) as usize];
    let mut fcs = [0; 8];
    fcs[..fcs_len].copy_from_slice(d.get(p..p + fcs_len)?);
    p += fcs_len;
    let content = match fcs_len {
        0 => None,
        2 => Some(u64::from_le_bytes(fcs) + 256),
        _ => Some(u64::from_le_bytes(fcs)),
    };
    let window = window.or(content)?;
    Some((p, fhd, u32::from_le_bytes(dict), content, window))
}

/// The contents of the frames of `fr` (in `d`), one after another, each one of a size that
/// its header tells if it does and has its checksum if it has one: None where a frame has a
/// dictionary, does not decompress, or holds more than [`CONTENT_MAX`], or they hold more
/// than `limit` in all.
pub fn decode(d: &[u8], fr: &Frames, limit: u64) -> Option<(Vec<u8>, Vec<u64>)> {
    let (mut out, mut lens) = (Vec::new(), Vec::new());
    for f in fr.frames() {
        if f.dict != 0 || f.content.is_some_and(|n| n > CONTENT_MAX || out.len() as u64 + n > limit) {
            return None;
        }
        let start = out.len();
        let mut dec = zstd::stream::read::Decoder::with_buffer(&d[f.at.clone()]).ok()?;
        dec.window_log_max(31).ok()?;
        let room = limit.saturating_sub(start as u64).min(CONTENT_MAX);
        // (one more than the room, to tell one past it)
        let n = std::io::copy(&mut std::io::Read::take(dec, room + 1), &mut out).ok()?;
        if n > room || f.content.is_some_and(|c| c != n) {
            return None;
        }
        lens.push(n);
    }
    Some((out, lens))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frames of each kind of header, skippable frames between them, and what follows them; a
    /// frame cut short is not one.
    #[test]
    fn frames() {
        let data: Vec<u8> = (0..400_000u32).map(|i| (i / 5 % 253) as u8).collect();
        let mut d = zstd::bulk::compress(&data, 3).unwrap();
        let first = d.len();
        d.extend_from_slice(&[0x5a, 0x2a, 0x4d, 0x18, 3, 0, 0, 0, 1, 2, 3]);
        let mut c = zstd::bulk::Compressor::new(1).unwrap();
        c.set_parameter(zstd::zstd_safe::CParameter::ChecksumFlag(true)).unwrap();
        c.set_parameter(zstd::zstd_safe::CParameter::ContentSizeFlag(false)).unwrap();
        d.extend_from_slice(&c.compress(b"a little more").unwrap());
        let len = d.len();
        d.extend_from_slice(b"what follows");
        let fr = parse(&d).unwrap();
        assert_eq!(fr.len, len);
        assert_eq!(fr.pieces.len(), 3);
        assert!(matches!(&fr.pieces[1], Piece::Skippable { at } if *at == (first..first + 11)));
        let fs: Vec<&Frame> = fr.frames().collect();
        assert_eq!((fs[0].content, fs[1].content), (Some(data.len() as u64), None));
        assert_eq!(fs[1].at.end - fs[1].blocks.end, 4);
        let (out, lens) = decode(&d, &fr, u64::MAX).unwrap();
        assert!(out == [&data[..], b"a little more"].concat());
        assert_eq!(lens, [data.len() as u64, 13]);
        assert!(decode(&d, &fr, data.len() as u64).is_none());
        // what Patch instructions give of the blocks
        let patched = aqr_formula::patch::apply(b"", &d[fs[0].blocks.clone()], data.len() as u64).unwrap();
        assert!(patched == data);
        assert!(parse(&d[..first - 1]).is_none() && starts(&d[..20]) && !starts(&d[..5]) && !starts(b"(\xb5/\xfd\x08"));
        assert_eq!(parse(&d[..first + 5]).unwrap().pieces.len(), 1);
    }
}
