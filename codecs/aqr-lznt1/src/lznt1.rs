//! LZNT1 chunks and units, read into their parse and written back ([MS-XCA] 2.5; `FORMAT.md` §3).
//!
//! A buffer is chunks of up to 4096 bytes of data each, ended by a header of 0 or not. A chunk is
//! a header word (the bytes after it less 1 in the low 12 bits, 3 in bits 12 to 14, bit 15 set
//! for a compressed chunk) and its bytes: the data (stored), or groups of a flag byte and up to 8
//! tokens, a literal byte or a word of a displacement and a length whose bit split depends on how
//! much of the chunk is done. A unit is the chunks of a stretch of a file (NTFS: 16 clusters),
//! then an end header and the padding of its last cluster.

/// Bytes of data in a chunk, at most.
pub const CHUNK: usize = 4096;
/// The least length of a match.
pub const MIN_MATCH: usize = 3;

/// A token: a literal (`len` 0) or a match of `len` bytes from `off` bytes back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tok {
    pub len: u16,
    pub off: u16,
}

impl Tok {
    pub const LIT: Tok = Tok { len: 0, off: 0 };
}

/// Bits of the displacement of a match when `done` bytes of the chunk are made: the largest
/// `M` in 4 to 12 with `2^(M-1) < done`, else 4. The length has the other `16 - M`.
#[inline]
pub fn disp_bits(done: usize) -> u32 {
    (usize::BITS - done.wrapping_sub(1).leading_zeros()).clamp(4, 12)
}

/// The most bytes a match is of when `done` bytes of the chunk are made.
#[inline]
pub fn max_len(done: usize) -> usize {
    (1 << (16 - disp_bits(done))) + 2
}

/// What is read of a chunk: whether it is stored, the bytes of data it holds, its tokens (in
/// [`Unit::toks`], by range) and the unused flag bits of its last group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChunkInfo {
    pub stored: bool,
    pub len: usize,
    pub toks: std::ops::Range<usize>,
    pub slack: u8,
}

/// The chunks of a unit and what follows them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Unit {
    pub chunks: Vec<ChunkInfo>,
    pub toks: Vec<Tok>,
    /// Bytes of the chunks.
    pub used: usize,
    /// Whether an end header (two zero bytes) follows them.
    pub end: bool,
}

/// The chunk at the start of `b`: its data appended to `out` and its tokens to `toks`; its size
/// in bytes and what was read. None where it does not read whole (the vectors then hold what was
/// read of it).
pub fn read_chunk(b: &[u8], out: &mut Vec<u8>, toks: &mut Vec<Tok>) -> Option<(usize, ChunkInfo)> {
    let h = u16::from_le_bytes(b.get(..2)?.try_into().ok()?) as usize;
    if h >> 12 & 7 != 3 {
        return None;
    }
    let size = (h & 0xfff) + 3;
    let payload = b.get(2..size)?;
    let t0 = toks.len();
    if h & 0x8000 == 0 {
        out.extend_from_slice(payload);
        return Some((size, ChunkInfo { stored: true, len: payload.len(), toks: t0..t0, slack: 0 }));
    }
    let start = out.len();
    let (mut i, mut slack) = (0, 0);
    while i < payload.len() {
        let flags = payload[i];
        i += 1;
        let mut k = 0;
        while k < 8 && i < payload.len() {
            let done = out.len() - start;
            if flags >> k & 1 == 0 {
                out.push(payload[i]);
                i += 1;
                toks.push(Tok::LIT);
            } else {
                let w = u16::from_le_bytes(payload.get(i..i + 2)?.try_into().ok()?) as usize;
                i += 2;
                let l = 16 - disp_bits(done);
                let (off, len) = ((w >> l) + 1, (w & ((1 << l) - 1)) + MIN_MATCH);
                if off > done || done + len > CHUNK {
                    return None;
                }
                for _ in 0..len {
                    out.push(out[out.len() - off]);
                }
                toks.push(Tok { len: len as u16, off: off as u16 });
            }
            k += 1;
        }
        // (a flag byte alone is no group)
        if k == 0 {
            return None;
        }
        if k < 8 {
            slack = flags >> k;
        }
    }
    if out.len() == start {
        return None;
    }
    Some((size, ChunkInfo { stored: false, len: out.len() - start, toks: t0..toks.len(), slack }))
}

/// The chunks of `b`, as far as they read (the first must), then an end header if one follows:
/// the unit read, its data appended to `out` (cleared first). None where no chunk reads.
pub fn read_unit(b: &[u8], out: &mut Vec<u8>, u: &mut Unit) -> Option<()> {
    out.clear();
    u.chunks.clear();
    u.toks.clear();
    u.end = false;
    let mut p = 0;
    while p + 2 <= b.len() {
        if b[p] == 0 && b[p + 1] == 0 {
            u.end = true;
            break;
        }
        let (o, t) = (out.len(), u.toks.len());
        match read_chunk(&b[p..], out, &mut u.toks) {
            Some((size, info)) => {
                p += size;
                u.chunks.push(info);
            }
            None => {
                out.truncate(o);
                u.toks.truncate(t);
                break;
            }
        }
    }
    u.used = p;
    (!u.chunks.is_empty()).then_some(())
}

/// A stored chunk of `data` (1 to 4096 bytes) written onto `out`.
pub fn write_stored(data: &[u8], out: &mut Vec<u8>) -> Option<()> {
    if data.is_empty() || data.len() > CHUNK {
        return None;
    }
    out.extend_from_slice(&(0x3000 | (data.len() - 1) as u16).to_le_bytes());
    out.extend_from_slice(data);
    Some(())
}

/// A compressed chunk of the data `d` that the tokens `toks` and `slack` (the unused flag bits of
/// the last group) make, written onto `out`. None where they do not make it, or it comes to more
/// than 4096 bytes after its header (`out` then holds a little of it).
pub fn write_chunk(toks: &[Tok], slack: u8, d: &[u8], out: &mut Vec<u8>) -> Option<()> {
    let h = out.len();
    out.extend_from_slice(&[0, 0]);
    let (mut done, mut flag, mut k) = (0, 0, 8);
    for t in toks {
        if k == 8 {
            (flag, k) = (out.len(), 0);
            out.push(0);
        }
        if t.len == 0 {
            out.push(*d.get(done)?);
            done += 1;
        } else {
            let (m, len, off) = (disp_bits(done), t.len as usize, t.off as usize);
            let l = 16 - m;
            if len < MIN_MATCH || len - MIN_MATCH >= 1 << l || off == 0 || off > done || off > 1 << m {
                return None;
            }
            out.extend_from_slice(&(((off - 1) << l | (len - MIN_MATCH)) as u16).to_le_bytes());
            out[flag] |= 1 << k;
            done += len;
        }
        k += 1;
        if out.len() - h > CHUNK + 2 {
            return None;
        }
    }
    if done != d.len() || toks.is_empty() {
        return None;
    }
    if k < 8 {
        if u32::from(slack) >> (8 - k) != 0 {
            return None;
        }
        out[flag] |= slack << k;
    } else if slack != 0 {
        return None;
    }
    let payload = out.len() - h - 2;
    if payload > CHUNK {
        return None;
    }
    out[h..h + 2].copy_from_slice(&(0xb000 | (payload - 1) as u16).to_le_bytes());
    Some(())
}
