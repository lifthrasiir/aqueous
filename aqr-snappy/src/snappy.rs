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

/// FORMAT.md §3.3: `src` (at most [`FRAGMENT`] bytes) compressed with `table` (cleared, and cut to
/// [`table_size`] entries, here), appended to `out`; `CRC` for [`Hash::Crc`] (§3.2).
fn fragment<const CRC: bool>(src: &[u8], table: &mut [u16], out: &mut Vec<u8>) {
    let n = src.len();
    if n < MARGIN {
        if n > 0 {
            literal(out, src);
        }
        return;
    }
    // The buffer is shared by fragments, and the last one may need less of it.
    let table = &mut table[..table_size(n)];
    table.fill(0);
    let bound = n - MARGIN;
    let mask = 2 * (table.len() - 1) as u32;
    // The table slot of the 4 bytes `v` (§3.2).
    let slot = |v: u32| -> usize {
        let h = if CRC { crc32c::update32(v, mask) } else { v.wrapping_mul(0x1E35_A7BD) >> 16 };
        ((h & mask) >> 1) as usize
    };
    // Looks `q` up, and records it: the position of the earlier occurrence on a hit.
    let probe = |table: &mut [u16], q: usize| -> Option<usize> {
        let v = load32(src, q);
        let e = &mut table[slot(v)];
        let c = *e as usize;
        *e = q as u16;
        (load32(src, c) == v).then_some(c)
    };
    // The first hit at a position after `start`, as (position, earlier position).
    let search = |table: &mut [u16], start: usize| -> Option<(usize, usize)> {
        let mut x = start + 1;
        let mut k = 32;
        if x + 16 <= bound {
            for q in x..x + 16 {
                if let Some(c) = probe(table, q) {
                    return Some((q, c));
                }
            }
            x += 16;
            k = 48;
        }
        loop {
            let gap = k / 32;
            k += gap;
            if x + gap > bound {
                return None;
            }
            if let Some(c) = probe(table, x) {
                return Some((x, c));
            }
            x += gap;
        }
    };

    let mut emitted = 0;
    let mut found = search(table, 0);
    while let Some((mut q, mut c)) = found {
        if q > emitted {
            literal(out, &src[emitted..q]);
        }
        // Copies, one after another while the position right after a copy hits again.
        loop {
            let len = 4 + common_len(src, c + 4, q + 4);
            copy(out, q - c, len);
            let r = q + len;
            emitted = r;
            if r >= bound {
                found = None;
                break;
            }
            table[slot(load32(src, r - 1))] = (r - 1) as u16;
            match probe(table, r) {
                Some(c2) => (q, c) = (r, c2),
                None => {
                    found = search(table, r);
                    break;
                }
            }
        }
    }
    if emitted < n {
        literal(out, &src[emitted..]);
    }
}

/// The number of equal bytes from `a` and from `b` (`a < b`) on, up to the end of `d`.
fn common_len(d: &[u8], a: usize, b: usize) -> usize {
    let mut i = 0;
    while b + i + 8 <= d.len() {
        let x = u64::from_le_bytes(d[a + i..a + i + 8].try_into().unwrap())
            ^ u64::from_le_bytes(d[b + i..b + i + 8].try_into().unwrap());
        if x != 0 {
            return i + (x.trailing_zeros() >> 3) as usize;
        }
        i += 8;
    }
    while b + i < d.len() && d[a + i] == d[b + i] {
        i += 1;
    }
    i
}

/// Writes a literal element of the non-empty `lits` (FORMAT.md §3.3).
fn literal(out: &mut Vec<u8>, lits: &[u8]) {
    let m = lits.len() as u32 - 1;
    if m < 60 {
        out.push((m << 2) as u8);
    } else {
        // Bytes needed for `m`; the tag says how many follow.
        let width = (m.ilog2() / 8 + 1) as usize;
        out.push(((59 + width as u32) << 2) as u8);
        out.extend_from_slice(&m.to_le_bytes()[..width]);
    }
    out.extend_from_slice(lits);
}

/// Writes copy elements of `len` (at least 4) bytes at `offset` (FORMAT.md §3.3): pieces of 64,
/// then a 60 if more than 64 are left, then the rest.
fn copy(out: &mut Vec<u8>, offset: usize, mut len: usize) {
    while len > 0 {
        let k = match len {
            68.. => 64,
            65..=67 => 60,
            _ => len,
        };
        if k < 12 && offset < 2048 {
            out.push(1 | ((k - 4) << 2 | (offset >> 8) << 5) as u8);
            out.push(offset as u8);
        } else {
            out.push(2 | ((k - 1) << 2) as u8);
            out.extend_from_slice(&(offset as u16).to_le_bytes());
        }
        len -= k;
    }
}
