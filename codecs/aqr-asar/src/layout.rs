//! The header and the index (`FORMAT.md`, "The header", "The index"), and [`build`], which
//! rebuilds the archive from them and the data. The header is the JSON with *holes* where a
//! value follows from the data: a NUL, then what the hole is.

use aqr_common::varint;
use sha2::{Digest, Sha256};

use crate::{Error, Result, corrupt};

/// What starts a hole in the header (a NUL is never in JSON text, not even in a string).
pub const HOLE: u8 = 0;
/// The size of the next file of the index, in decimal; that file is the current one.
pub const SIZE: u8 = 1;
/// The current file's offset, in decimal.
pub const OFFSET: u8 = 2;
/// The SHA-256 of the current file, in lower case hexadecimal.
pub const HASH: u8 = 3;
/// After it a varint, the block size: the SHA-256 of each block of the current file, as JSON
/// strings separated by commas.
pub const BLOCKS: u8 = 4;

/// Bytes of the pickles' sizes before the JSON.
pub const PREFIX: usize = 16;

/// A file's contents in the data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Body {
    pub offset: u64,
    pub size: u64,
}

impl Body {
    fn of<'a>(&self, data: &'a [u8]) -> &'a [u8] {
        &data[self.offset as usize..][..self.size as usize]
    }
}

/// Bytes from `at` up to the next multiple of `align`.
pub fn pad_to(at: usize, align: usize) -> usize {
    (align - at % align) % align
}

/// The pickles' sizes before a JSON of `e` bytes, as Chromium's `Pickle` writes them: 4 (the
/// payload of the first, a `u32`), the second's size, its payload's (the string's length and
/// the string, padded to 4 bytes), then the string's length.
pub fn prefix(e: usize) -> [u8; PREFIX] {
    let c = 4 + e + pad_to(e, 4);
    let mut p = [0; PREFIX];
    for (k, v) in [4, c + 4, c, e].into_iter().enumerate() {
        p[k * 4..][..4].copy_from_slice(&(v as u32).to_le_bytes());
    }
    p
}

/// The index of `bodies` (in the order of the header's [`SIZE`] holes), the archive's first
/// [`PREFIX`] bytes `head` for a JSON of `e` bytes, and the bytes `rest` between the JSON and
/// the data.
pub fn index(bodies: &[Body], head: &[u8; PREFIX], e: usize, rest: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bodies.len() * 3 + PREFIX + rest.len() + 8);
    varint::put(&mut out, bodies.len() as u64);
    let mut end = 0u64;
    for b in bodies {
        varint::put_signed(&mut out, b.offset.wrapping_sub(end) as i64);
        varint::put(&mut out, b.size);
        end = b.offset + b.size;
    }
    let p = prefix(e);
    out.extend(head.iter().zip(&p).map(|(h, p)| h ^ p));
    out.extend_from_slice(rest);
    out
}

fn decimal(out: &mut Vec<u8>, v: u64) {
    let mut buf = [0; 20];
    let mut at = buf.len();
    let mut v = v;
    loop {
        at -= 1;
        buf[at] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    out.extend_from_slice(&buf[at..]);
}

fn hex(out: &mut Vec<u8>, h: &[u8]) {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    out.extend(h.iter().flat_map(|&b| [DIGITS[(b >> 4) as usize], DIGITS[(b & 15) as usize]]));
}

/// What a [`BLOCKS`] hole of block size `size` outputs for `d`.
pub fn blocks(out: &mut Vec<u8>, d: &[u8], size: usize) {
    for (k, c) in d.chunks(size).enumerate() {
        if k > 0 {
            out.push(b',');
        }
        out.push(b'"');
        hex(out, &Sha256::digest(c));
        out.push(b'"');
    }
}

/// What a [`HASH`] hole outputs for `d`.
pub fn hash(out: &mut Vec<u8>, d: &[u8]) {
    hex(out, &Sha256::digest(d));
}

/// The archive of `len` bytes rebuilt from `data` (the bytes after the header), `header` (the
/// JSON with holes) and `index`.
pub fn build(data: &[u8], header: &[u8], index: &[u8], len: u64) -> Result<Vec<u8>> {
    let len = usize::try_from(len).map_err(|_| corrupt())?;
    let mut at = 0;
    let n = varint::get(index, &mut at)?;
    // (each body takes two bytes of the index at least)
    if n > index.len() as u64 / 2 {
        return Err(corrupt());
    }
    let mut bodies = Vec::with_capacity(n as usize);
    let mut end = 0u64;
    for _ in 0..n {
        let offset = end.wrapping_add(varint::get_signed(index, &mut at)? as u64);
        let size = varint::get(index, &mut at)?;
        if offset.checked_add(size).is_none_or(|e| e > data.len() as u64) {
            return Err(corrupt());
        }
        bodies.push(Body { offset, size });
        end = offset + size;
    }
    let head: [u8; PREFIX] = *index.get(at..).and_then(|r| r.first_chunk()).ok_or_else(corrupt)?;
    let rest = &index[at + PREFIX..];

    let mut out = Vec::with_capacity(len);
    out.extend_from_slice(&[0; PREFIX]);
    let mut next = bodies.iter();
    // the current body, and its hash where known
    let mut cur: Option<(Body, Option<[u8; 32]>)> = None;
    let mut h = 0;
    while h < header.len() {
        let Some(z) = header[h..].iter().position(|&b| b == HOLE) else {
            out.extend_from_slice(&header[h..]);
            break;
        };
        out.extend_from_slice(&header[h..h + z]);
        h += z + 1;
        let what = *header.get(h).ok_or_else(corrupt)?;
        h += 1;
        if what == SIZE {
            let b = *next.next().ok_or_else(corrupt)?;
            decimal(&mut out, b.size);
            cur = Some((b, None));
        } else {
            let (b, whole) = cur.as_mut().ok_or_else(corrupt)?;
            let d = b.of(data);
            match what {
                OFFSET => decimal(&mut out, b.offset),
                HASH => hex(&mut out, whole.get_or_insert_with(|| Sha256::digest(d).into())),
                BLOCKS => {
                    let size = varint::get(header, &mut h)?;
                    if size == 0 {
                        return Err(corrupt());
                    }
                    let size = size.min(d.len().max(1) as u64) as usize;
                    // (each block outputs 66 bytes or 67)
                    if out.len().saturating_add(d.len().div_ceil(size).saturating_mul(66)) > len {
                        return Err(Error("past the length"));
                    }
                    match whole {
                        // (a block of all of it: its hash)
                        Some(w) if size == d.len() => {
                            out.push(b'"');
                            hex(&mut out, w);
                            out.push(b'"');
                        }
                        _ => blocks(&mut out, d, size),
                    }
                }
                _ => return Err(corrupt()),
            }
        }
        if out.len() > len {
            return Err(Error("past the length"));
        }
    }
    if next.next().is_some() {
        return Err(Error("files left over"));
    }
    let e = out.len() - PREFIX;
    let p = prefix(e);
    out[..PREFIX].iter_mut().zip(head.iter().zip(&p)).for_each(|(o, (h, p))| *o = h ^ p);
    let start = (u32::from_le_bytes(out[4..8].try_into().unwrap()) as usize).checked_add(8);
    if start != Some(out.len() + rest.len()) {
        return Err(corrupt());
    }
    if out.len() + rest.len() + data.len() != len {
        return Err(Error("not of the length"));
    }
    out.extend_from_slice(rest);
    out.extend_from_slice(data);
    Ok(out)
}
