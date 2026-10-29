//! LZ4 frames: a decoder, and liblz4 1.10.0's `LZ4F_compressFrame` at its fast levels with
//! independent blocks reimplemented to give the same bytes (`FORMAT.md` §4).

use xxhash_rust::xxh32::xxh32;

use crate::block::{self, Compressor};
use crate::{Error, Result, corrupt};

pub const MAGIC: [u8; 4] = [0x04, 0x22, 0x4d, 0x18];

/// FLG bits.
const INDEPENDENT: u8 = 0x20;
const BLOCK_CHECKSUM: u8 = 0x10;
const CONTENT_SIZE: u8 = 0x08;
const CONTENT_CHECKSUM: u8 = 0x04;
const DICT_ID: u8 = 0x01;

/// A frame decoded.
pub struct Frame {
    /// FLG and BD bytes of the descriptor.
    pub flg: u8,
    pub bd: u8,
    /// Bytes of the frame.
    pub len: usize,
    pub data: Vec<u8>,
}

/// Bytes of the largest block of a BD byte, if it is valid.
fn block_max(bd: u8) -> Option<usize> {
    let id = bd >> 4;
    (bd & 0x8f == 0 && (4..=7).contains(&id)).then(|| 1 << (8 + 2 * id))
}

/// Whether FLG is valid: version 01, no reserved bit.
fn flg_valid(flg: u8) -> bool {
    flg >> 6 == 1 && flg & 2 == 0
}

/// Bytes of the header from the magic number to the header checksum.
fn header_len(flg: u8) -> usize {
    4 + 2 + if flg & CONTENT_SIZE != 0 { 8 } else { 0 } + if flg & DICT_ID != 0 { 4 } else { 0 } + 1
}

/// The frame at the start of `d`, decoded. A frame with a dictionary is not decoded, nor one
/// whose header checksum is wrong; checksums of the data are not checked.
pub fn decode(d: &[u8]) -> Result<Frame> {
    let (flg, bd) = match d {
        [0x04, 0x22, 0x4d, 0x18, flg, bd, ..] => (*flg, *bd),
        _ => return Err(Error("not an LZ4 frame")),
    };
    let max = block_max(bd).filter(|_| flg_valid(flg)).ok_or_else(corrupt)?;
    if flg & DICT_ID != 0 {
        return Err(Error("LZ4 frame with a dictionary"));
    }
    let mut p = header_len(flg);
    let header = d.get(4..p - 1).ok_or_else(corrupt)?;
    if d.get(p - 1) != Some(&((xxh32(header, 0) >> 8) as u8)) {
        return Err(corrupt());
    }
    let mut data = Vec::new();
    loop {
        let w = u32::from_le_bytes(*d.get(p..).and_then(|s| s.first_chunk()).ok_or_else(corrupt)?);
        p += 4;
        if w == 0 {
            break;
        }
        let n = (w & 0x7fff_ffff) as usize;
        let b = d.get(p..p + n).filter(|_| n <= max).ok_or_else(corrupt)?;
        if w >> 31 != 0 {
            data.extend_from_slice(b);
        } else {
            let floor = if flg & INDEPENDENT != 0 { data.len() } else { 0 };
            block::decode(b, &mut data, floor, max)?;
        }
        p += n + if flg & BLOCK_CHECKSUM != 0 { 4 } else { 0 };
    }
    p += if flg & CONTENT_CHECKSUM != 0 { 4 } else { 0 };
    d.get(..p).ok_or_else(corrupt)?;
    Ok(Frame { flg, bd, len: p, data })
}

/// Bytes of the frame at the start of `d`, by its block sizes; None where its structure does
/// not hold.
pub fn end(d: &[u8]) -> Option<usize> {
    let flg = *d.get(4)?;
    let max = block_max(*d.get(5)?).filter(|_| d.starts_with(&MAGIC) && flg_valid(flg))?;
    let mut p = header_len(flg);
    loop {
        let w = u32::from_le_bytes(*d.get(p..)?.first_chunk()?);
        p += 4;
        if w == 0 {
            break;
        }
        let n = (w & 0x7fff_ffff) as usize;
        if n > max {
            return None;
        }
        p += n + if flg & BLOCK_CHECKSUM != 0 { 4 } else { 0 };
    }
    p += if flg & CONTENT_CHECKSUM != 0 { 4 } else { 0 };
    (p <= d.len()).then_some(p)
}

/// Whether `LZ4F_compressFrame` may write a frame of these FLG and BD bytes at a fast level:
/// independent blocks, no dictionary.
pub fn encodable(flg: u8, bd: u8) -> bool {
    flg_valid(flg) && flg & INDEPENDENT != 0 && flg & DICT_ID == 0 && block_max(bd).is_some()
}

/// The frame of `data` that `LZ4F_compressFrame` writes with the FLG and BD bytes given (which
/// must be [`encodable`]) at the fast level of `acceleration`. With `expect`, None where the
/// frame is not that, found a block at a time.
pub fn encode(data: &[u8], flg: u8, bd: u8, acceleration: u32, expect: Option<&[u8]>) -> Result<Option<Vec<u8>>> {
    encode_in(data, flg, bd, acceleration, expect, usize::MAX)
}

/// [`encode`] into a vector of `capacity` bytes at first (the length expected, where it is
/// known).
pub fn encode_in(
    data: &[u8],
    flg: u8,
    bd: u8,
    acceleration: u32,
    expect: Option<&[u8]>,
    capacity: usize,
) -> Result<Option<Vec<u8>>> {
    if !encodable(flg, bd) {
        return Err(Error("LZ4 frame descriptor not supported"));
    }
    let max = block_max(bd).unwrap();
    let mut out = Vec::with_capacity(capacity.min(header_len(flg) + data.len() + data.len() / max * 8 + 12));
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&[flg, bd]);
    if flg & CONTENT_SIZE != 0 {
        out.extend_from_slice(&(data.len() as u64).to_le_bytes());
    }
    out.push((xxh32(&out[4..], 0) >> 8) as u8);
    let differs = |out: &[u8], from: usize| expect.is_some_and(|e| e.get(from..out.len()) != Some(&out[from..]));
    if differs(&out, 0) {
        return Ok(None);
    }
    let mut c = Compressor::default();
    for src in data.chunks(max) {
        let from = out.len();
        let at = out.len() + 4;
        match c.compress_reset(src, src.len() - 1, acceleration)? {
            Some(b) if b.len() < src.len() => {
                out.extend_from_slice(&(b.len() as u32).to_le_bytes());
                out.extend_from_slice(&b);
            }
            _ => {
                out.extend_from_slice(&(src.len() as u32 | 1 << 31).to_le_bytes());
                out.extend_from_slice(src);
            }
        }
        if flg & BLOCK_CHECKSUM != 0 {
            let sum = xxh32(&out[at..], 0);
            out.extend_from_slice(&sum.to_le_bytes());
        }
        if differs(&out, from) {
            return Ok(None);
        }
    }
    let from = out.len();
    out.extend_from_slice(&[0; 4]);
    if flg & CONTENT_CHECKSUM != 0 {
        out.extend_from_slice(&xxh32(data, 0).to_le_bytes());
    }
    if differs(&out, from) || expect.is_some_and(|e| e.len() != out.len()) {
        return Ok(None);
    }
    Ok(Some(out))
}
