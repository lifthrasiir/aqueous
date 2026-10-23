//! Snappy streams: a decoder, and Snappy 1.2.2's compressor at its default level
//! (`snappy::Compress`, `CompressFragment`) reimplemented to give the same bytes. `FORMAT.md`
//! §3 specifies the compressor.

use crate::crc32c;
use crate::{Error, Result, corrupt};

/// How the compressor hashes 4 bytes into its table, as it was built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hash {
    /// Multiplication, without a CRC instruction (Chrome's build, for one).
    Mul = 0,
    /// The CRC-32C instruction of x86 (SSE4.2) or ARMv8, since Snappy 1.1.10.
    Crc = 1,
}

impl Hash {
    pub const ALL: [Hash; 2] = [Hash::Mul, Hash::Crc];

    pub fn from_u8(b: u8) -> Option<Hash> {
        Hash::ALL.into_iter().find(|&h| h as u8 == b)
    }
}

/// Fragments are compressed apart (`kBlockSize`).
const FRAGMENT: usize = 1 << 16;
/// Entries of the table of a fragment, at least and at most.
const MIN_TABLE: usize = 1 << 8;
const MAX_TABLE: usize = 1 << 15;
/// Bytes at the end of a fragment that the search does not reach (`kInputMarginBytes`).
const MARGIN: usize = 15;

/// The data of the stream `src`, which must be whole, of at most `max` bytes.
pub fn decode(src: &[u8], max: usize) -> Result<Vec<u8>> {
    let mut p = 0;
    let n = length(src, &mut p)? as usize;
    if n > max {
        return Err(Error("stream longer than allowed"));
    }
    let mut out = Vec::with_capacity(n);
    while p < src.len() {
        let tag = src[p];
        p += 1;
        let (len, off) = match tag & 3 {
            0 => {
                let mut len = (tag >> 2) as usize;
                if len >= 60 {
                    let k = len - 59;
                    let b = src.get(p..p + k).ok_or_else(corrupt)?;
                    len = b.iter().rev().fold(0, |a, &x| a << 8 | x as usize);
                    p += k;
                }
                let lits = src.get(p..p.checked_add(len + 1).ok_or_else(corrupt)?).ok_or_else(corrupt)?;
                if lits.len() > n - out.len() {
                    return Err(corrupt());
                }
                out.extend_from_slice(lits);
                p += len + 1;
                continue;
            }
            1 => {
                let b = *src.get(p).ok_or_else(corrupt)?;
                p += 1;
                ((tag >> 2 & 7) as usize + 4, (tag as usize >> 5) << 8 | b as usize)
            }
            2 => {
                let b = src.get(p..p + 2).ok_or_else(corrupt)?;
                p += 2;
                ((tag >> 2) as usize + 1, u16::from_le_bytes([b[0], b[1]]) as usize)
            }
            _ => {
                let b = src.get(p..p + 4).ok_or_else(corrupt)?;
                p += 4;
                ((tag >> 2) as usize + 1, u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
            }
        };
        if off == 0 || off > out.len() || len > n - out.len() {
            return Err(corrupt());
        }
        let from = out.len() - off;
        if off >= len {
            out.extend_from_within(from..from + len);
        } else {
            for i in from..from + len {
                out.push(out[i]);
            }
        }
    }
    if out.len() != n {
        return Err(corrupt());
    }
    Ok(out)
}

/// The length that starts a stream: a varint of at most 32 bits.
fn length(src: &[u8], p: &mut usize) -> Result<u32> {
    let mut v = 0u64;
    for i in 0..5 {
        let b = *src.get(*p).ok_or_else(corrupt)?;
        *p += 1;
        v |= ((b & 0x7f) as u64) << (7 * i);
        if b < 0x80 {
            return u32::try_from(v).map_err(|_| corrupt());
        }
    }
    Err(corrupt())
}

/// The stream that `snappy::RawCompress` makes of `src`, hashing by `hash`.
pub fn compress(src: &[u8], hash: Hash) -> Result<Vec<u8>> {
    let n = u32::try_from(src.len()).map_err(|_| Error("input too long for Snappy"))?;
    let mut out = Vec::with_capacity(32 + src.len() + src.len() / 6);
    aqr_common::varint::put(&mut out, n as u64);
    let mut table = vec![0u16; table_size(src.len().min(FRAGMENT))];
    for f in src.chunks(FRAGMENT) {
        match hash {
            Hash::Mul => fragment::<false>(f, &mut table, &mut out),
            Hash::Crc => fragment::<true>(f, &mut table, &mut out),
        }
    }
    Ok(out)
}

/// Entries of the table for a fragment of `n` bytes (`CalculateTableSize`).
fn table_size(n: usize) -> usize {
    n.next_power_of_two().clamp(MIN_TABLE, MAX_TABLE)
}

fn load32(d: &[u8], p: usize) -> u32 {
    u32::from_le_bytes(d[p..p + 4].try_into().unwrap())
}

/// `CompressFragment` of `src` (at most [`FRAGMENT`] bytes) with a cleared table; `CRC` for
/// [`Hash::Crc`].
fn fragment<const CRC: bool>(src: &[u8], table: &mut [u16], out: &mut Vec<u8>) {
    let n = src.len();
    let size = table_size(n);
    let table = &mut table[..size];
    table.fill(0);
    let mask = 2 * (size as u32 - 1);
    // the entry of 4 bytes (`TableEntry`: a byte offset in the table, masked)
    let hash = |v: u32| -> usize {
        let h = if CRC { crc32c::update32(v, mask) } else { v.wrapping_mul(0x1e35_a7bd) >> 16 };
        ((h & mask) >> 1) as usize
    };
    let mut ip = 0;
    'remainder: {
        if n < MARGIN {
            break 'remainder;
        }
        let limit = n - MARGIN;
        loop {
            let next_emit = ip;
            ip += 1;
            let mut skip = 32u32;
            let mut found = None;
            if limit as isize - ip as isize >= 16 {
                // the first 16 positions, one after another
                for q in ip..ip + 16 {
                    let v = load32(src, q);
                    let h = hash(v);
                    let c = table[h] as usize;
                    table[h] = q as u16;
                    if load32(src, c) == v {
                        found = Some((q, c));
                        break;
                    }
                }
                if found.is_none() {
                    ip += 16;
                    skip += 16;
                }
            }
            let mut candidate = match found {
                Some((q, c)) => {
                    ip = q;
                    c
                }
                None => loop {
                    let v = load32(src, ip);
                    let h = hash(v);
                    let step = skip >> 5;
                    skip += step;
                    let next = ip + step as usize;
                    if next > limit {
                        ip = next_emit;
                        break 'remainder;
                    }
                    let c = table[h] as usize;
                    table[h] = ip as u16;
                    if load32(src, c) == v {
                        break c;
                    }
                    ip = next;
                },
            };
            literal(out, &src[next_emit..ip]);
            loop {
                let base = ip;
                let matched = 4 + src[ip + 4..].iter().zip(&src[candidate + 4..]).take_while(|(a, b)| a == b).count();
                ip += matched;
                copy(out, base - candidate, matched);
                if ip >= limit {
                    break 'remainder;
                }
                table[hash(load32(src, ip - 1))] = (ip - 1) as u16;
                let v = load32(src, ip);
                let h = hash(v);
                candidate = table[h] as usize;
                table[h] = ip as u16;
                if load32(src, candidate) != v {
                    break;
                }
            }
        }
    }
    if ip < n {
        literal(out, &src[ip..]);
    }
}

/// `EmitLiteral`.
fn literal(out: &mut Vec<u8>, lits: &[u8]) {
    let n = lits.len() - 1;
    if n < 60 {
        out.push((n as u8) << 2);
    } else {
        let count = (n.ilog2() as usize >> 3) + 1;
        out.push(((59 + count) as u8) << 2);
        out.extend_from_slice(&(n as u32).to_le_bytes()[..count]);
    }
    out.extend_from_slice(lits);
}

/// `EmitCopy`: copies of at most 64 bytes, with 4 at least left for the last.
fn copy(out: &mut Vec<u8>, offset: usize, mut len: usize) {
    while len >= 68 {
        copy64(out, offset, 64);
        len -= 64;
    }
    if len > 64 {
        copy64(out, offset, 60);
        len -= 60;
    }
    copy64(out, offset, len);
}

/// `EmitCopyAtMost64`: a copy with a 1-byte offset where it fits, else a 2-byte one.
fn copy64(out: &mut Vec<u8>, offset: usize, len: usize) {
    if len < 12 && offset < 2048 {
        out.push(1 | ((len - 4) as u8) << 2 | ((offset >> 8) as u8) << 5);
        out.push(offset as u8);
    } else {
        out.push(2 | ((len - 1) as u8) << 2);
        out.extend_from_slice(&(offset as u16).to_le_bytes());
    }
}
