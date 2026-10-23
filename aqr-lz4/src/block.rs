//! LZ4 blocks: a decoder, and liblz4 1.10.0's fast compressor (`LZ4_compress_generic` without
//! a dictionary, 64-bit little endian) reimplemented to give the same bytes. `FORMAT.md` §3
//! specifies the compressor.

use crate::{Error, Result, corrupt};

/// `LZ4_MAX_INPUT_SIZE`: liblz4 compresses nothing longer.
pub const MAX_INPUT: usize = 0x7E00_0000;
/// The acceleration liblz4 clamps to (`LZ4_ACCELERATION_MAX`).
pub const MAX_ACCELERATION: u32 = 65537;

const MINMATCH: usize = 4;
const MFLIMIT: usize = 12;
const LASTLITERALS: usize = 5;
const MAX_DISTANCE: u32 = 65535;
/// Inputs shorter than this use a table of 16-bit positions (`LZ4_64Klimit`).
const LIMIT_64K: usize = 65536 + MFLIMIT - 1;
const SKIP_TRIGGER: u32 = 6;
/// `LZ4_HASHLOG` of the default `LZ4_MEMORY_USAGE` (14): entries of the table of 32-bit positions.
const HASH_LOG: u32 = 12;

/// `LZ4_compressBound`.
pub fn bound(n: usize) -> usize {
    n + n / 255 + 16
}

/// Appends to `out` the data of the block `src`, which must be whole. Matches may reach back
/// into `out` down to `floor` (the start of the block, or of the frame for linked blocks), and
/// `out` may grow by at most `max` bytes.
pub fn decode(src: &[u8], out: &mut Vec<u8>, floor: usize, max: usize) -> Result<()> {
    let limit = out.len().checked_add(max).ok_or_else(corrupt)?;
    let mut p = 0;
    let len = |p: &mut usize, mut n: usize| -> Result<usize> {
        if n == 15 {
            loop {
                let b = *src.get(*p).ok_or_else(corrupt)?;
                *p += 1;
                n += b as usize;
                if b != 255 {
                    break;
                }
            }
        }
        Ok(n)
    };
    loop {
        let token = *src.get(p).ok_or_else(corrupt)?;
        p += 1;
        let lit = len(&mut p, (token >> 4) as usize)?;
        let lits = src.get(p..p.checked_add(lit).ok_or_else(corrupt)?).ok_or_else(corrupt)?;
        if lit > limit - out.len() {
            return Err(Error("block longer than allowed"));
        }
        out.extend_from_slice(lits);
        p += lit;
        if p == src.len() {
            return Ok(());
        }
        let off = u16::from_le_bytes([*src.get(p).ok_or_else(corrupt)?, *src.get(p + 1).ok_or_else(corrupt)?]) as usize;
        p += 2;
        let n = len(&mut p, (token & 15) as usize)? + MINMATCH;
        if off == 0 || off > out.len() - floor {
            return Err(corrupt());
        }
        if n > limit - out.len() {
            return Err(Error("block longer than allowed"));
        }
        let from = out.len() - off;
        if off >= n {
            out.extend_from_within(from..from + n);
        } else {
            for i in from..from + n {
                out.push(out[i]);
            }
        }
    }
}

/// Bytes of the block at the start of `src` holding exactly `n` bytes of data, which ends
/// with the literals that complete them; None where it does not hold.
pub fn end(src: &[u8], n: usize) -> Option<usize> {
    let (mut p, mut have) = (0usize, 0usize);
    let len = |p: &mut usize, mut v: usize| -> Option<usize> {
        if v == 15 {
            loop {
                let b = *src.get(*p)?;
                *p += 1;
                v = v.checked_add(b as usize)?;
                if b != 255 {
                    break;
                }
            }
        }
        Some(v)
    };
    loop {
        let token = *src.get(p)?;
        p += 1;
        let lit = len(&mut p, (token >> 4) as usize)?;
        p = p.checked_add(lit).filter(|&e| e <= src.len())?;
        have = have.checked_add(lit).filter(|&h| h <= n)?;
        if have == n {
            return Some(p);
        }
        let off = u16::from_le_bytes([*src.get(p)?, *src.get(p + 1)?]) as usize;
        p += 2;
        let m = len(&mut p, (token & 15) as usize)? + MINMATCH;
        if off == 0 || off > have {
            return None;
        }
        have = have.checked_add(m).filter(|&h| h < n)?;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Table {
    Cleared,
    U16,
    U32,
}

/// The compression state that liblz4 keeps between blocks (`LZ4_stream_t_internal`).
pub struct Compressor {
    /// 4096 32-bit positions, or 8192 16-bit ones (`hashTable`, as `table` says).
    hash: Box<[u32; 1 << (HASH_LOG + 1)]>,
    offset: u32,
    table: Table,
}

impl Default for Compressor {
    fn default() -> Compressor {
        Compressor { hash: Box::new([0; 1 << (HASH_LOG + 1)]), offset: 0, table: Table::Cleared }
    }
}

/// How the sequences found end up.
struct Out<'a> {
    out: Vec<u8>,
    /// The most bytes, where the output is limited.
    cap: Option<usize>,
    /// Bytes the output must be, if known: compression stops where it is not.
    expect: Option<&'a [u8]>,
}

/// Why compression stopped.
enum Stop {
    /// The output would not fit its limit.
    Full,
    /// The output is not what was expected.
    Differs,
}

impl Out<'_> {
    /// Whether `need` more bytes may exceed the limit (liblz4's checks, which overestimate).
    fn over(&self, need: usize) -> bool {
        self.cap.is_some_and(|cap| self.out.len() + need > cap)
    }

    /// Checks the bytes written since `from` against those expected.
    fn check(&self, from: usize) -> std::result::Result<(), Stop> {
        match self.expect {
            Some(e) if e.get(from..self.out.len()) != Some(&self.out[from..]) => Err(Stop::Differs),
            _ => Ok(()),
        }
    }
}

impl Compressor {
    /// `LZ4_compress_fast`: `src` in a block, with a fresh state.
    pub fn compress_fast(src: &[u8], acceleration: u32) -> Result<Vec<u8>> {
        let mut c = Compressor::default();
        match c.run(src, None, None, acceleration) {
            Ok(out) => Ok(out),
            Err(_) => unreachable!("unlimited output"),
        }
    }

    /// Whether [`Compressor::compress_fast`] gives `expect`, found without compressing past
    /// the first difference.
    pub fn reproduces_fast(src: &[u8], acceleration: u32, expect: &[u8]) -> bool {
        let mut c = Compressor::default();
        c.run(src, None, Some(expect), acceleration).is_ok_and(|out| out == expect)
    }

    /// `LZ4_compress_fast_extState_fastReset` into at most `cap` bytes, keeping this state:
    /// None where the block does not fit.
    pub fn compress_reset(&mut self, src: &[u8], cap: usize, acceleration: u32) -> Result<Option<Vec<u8>>> {
        let cap = (cap < bound(src.len())).then_some(cap);
        match self.run(src, cap, None, acceleration) {
            Ok(out) => Ok(Some(out)),
            Err(None) => Ok(None),
            Err(Some(e)) => Err(e),
        }
    }

    /// `LZ4_prepareTable` then `LZ4_compress_generic`, as `LZ4_compress_fast_extState_fastReset`
    /// calls them (a fresh state makes `LZ4_compress_fast_extState`). Err(None) where the
    /// output stops short.
    fn run(
        &mut self,
        src: &[u8],
        cap: Option<usize>,
        expect: Option<&[u8]>,
        acceleration: u32,
    ) -> std::result::Result<Vec<u8>, Option<Error>> {
        if src.len() > MAX_INPUT {
            return Err(Some(Error("input too long for LZ4")));
        }
        let acceleration = acceleration.clamp(1, MAX_ACCELERATION);
        let mut out = Out { out: Vec::with_capacity(bound(src.len()).min(cap.unwrap_or(usize::MAX))), cap, expect };
        let table = if src.len() < LIMIT_64K { Table::U16 } else { Table::U32 };
        self.prepare(src.len(), table);
        if src.is_empty() {
            // `LZ4_compress_generic` writes a lone token, unless nothing may be written.
            if cap == Some(0) {
                return Err(None);
            }
            out.out.push(0);
            out.check(0).map_err(|_| None)?;
            return Ok(out.out);
        }
        // a 16-bit table used before: positions below this block are not in it
        let small = table == Table::U16 && self.offset != 0;
        let r = match table {
            Table::U16 => self.generic::<false>(src, &mut out, small, acceleration),
            _ => self.generic::<true>(src, &mut out, small, acceleration),
        };
        r.map(|_| out.out).map_err(|_| None)
    }

    fn prepare(&mut self, n: usize, table: Table) {
        if self.table != Table::Cleared
            && (self.table != table
                || table == Table::U16 && self.offset as u64 + n as u64 >= 0xFFFF
                || table == Table::U32 && self.offset > 1 << 30
                || n >= 4096)
        {
            self.hash.fill(0);
            self.offset = 0;
            self.table = Table::Cleared;
        }
        // a gap, so that every position kept is too far back
        if self.offset != 0 && table == Table::U32 {
            self.offset += 65536;
        }
    }

    /// `LZ4_compress_generic_validated` for a block without a dictionary, in one segment; `WIDE`
    /// for the table of 32-bit positions. With `small`, positions below the block are not
    /// matched (`dictSmall`).
    fn generic<const WIDE: bool>(
        &mut self,
        src: &[u8],
        out: &mut Out,
        small: bool,
        acceleration: u32,
    ) -> std::result::Result<(), Stop> {
        let n = src.len();
        let start = self.offset;
        self.offset += n as u32;
        self.table = if WIDE { Table::U32 } else { Table::U16 };
        let hash = &mut self.hash;
        let hash_at = |p: usize| -> usize {
            if WIDE {
                let v = u64::from_le_bytes(src[p..p + 8].try_into().unwrap());
                ((v << 24).wrapping_mul(889523592379) >> (64 - HASH_LOG)) as usize
            } else {
                (read32(src, p).wrapping_mul(2654435761) >> (32 - (HASH_LOG + 1))) as usize
            }
        };
        // where a position of the table is in `src`, if it is in this block
        let pos = |index: u32| index.checked_sub(start).map(|i| i as usize);

        let mut anchor = 0;
        'seqs: {
            if n < MFLIMIT + 1 {
                break 'seqs;
            }
            let mflimit1 = n - MFLIMIT + 1;
            let matchlimit = n - LASTLITERALS;
            hash[hash_at(0)] = start;
            let mut ip = 1;
            let mut forward_h = hash_at(ip);
            loop {
                // find a match
                let mut m;
                {
                    let mut forward = ip;
                    let mut step = 1;
                    let mut search = acceleration << SKIP_TRIGGER;
                    loop {
                        let h = forward_h;
                        let current = start + forward as u32;
                        let index = hash[h];
                        ip = forward;
                        forward += step;
                        step = (search >> SKIP_TRIGGER) as usize;
                        search += 1;
                        if forward > mflimit1 {
                            break 'seqs;
                        }
                        forward_h = hash_at(forward);
                        hash[h] = current;
                        if small && index < start || WIDE && index + MAX_DISTANCE < current {
                            continue;
                        }
                        if let Some(p) = pos(index)
                            && read32(src, p) == read32(src, ip)
                        {
                            m = p;
                            break;
                        }
                    }
                }
                // catch up
                while ip > anchor && m > 0 && src[ip - 1] == src[m - 1] {
                    ip -= 1;
                    m -= 1;
                }
                // literals
                let lit = ip - anchor;
                let from = out.out.len();
                let mut token = out.out.len();
                out.out.push(0);
                if out.over(lit + 2 + 1 + LASTLITERALS + lit / 255) {
                    return Err(Stop::Full);
                }
                if lit >= 15 {
                    out.out[token] = 15 << 4;
                    push_len(&mut out.out, lit - 15);
                } else {
                    out.out[token] = (lit as u8) << 4;
                }
                out.out.extend_from_slice(&src[anchor..ip]);
                loop {
                    // the match
                    out.out.extend_from_slice(&((ip - m) as u16).to_le_bytes());
                    let code = count(src, ip + MINMATCH, m + MINMATCH, matchlimit);
                    ip += code + MINMATCH;
                    if out.over(1 + LASTLITERALS + (code + 240) / 255) {
                        return Err(Stop::Full);
                    }
                    if code >= 15 {
                        out.out[token] += 15;
                        push_len(&mut out.out, code - 15);
                    } else {
                        out.out[token] += code as u8;
                    }
                    out.check(from)?;
                    anchor = ip;
                    if ip >= mflimit1 {
                        break 'seqs;
                    }
                    // fill the table, and test the next position
                    hash[hash_at(ip - 2)] = start + (ip - 2) as u32;
                    let h = hash_at(ip);
                    let current = start + ip as u32;
                    let index = hash[h];
                    hash[h] = current;
                    if !(small && index < start || WIDE && index + MAX_DISTANCE < current)
                        && let Some(p) = pos(index)
                        && read32(src, p) == read32(src, ip)
                    {
                        m = p;
                        token = out.out.len();
                        out.out.push(0);
                        continue;
                    }
                    break;
                }
                ip += 1;
                forward_h = hash_at(ip);
            }
        }
        // the last literals
        let from = out.out.len();
        let last = n - anchor;
        if out.over(last + 1 + (last + 255 - 15) / 255) {
            return Err(Stop::Full);
        }
        if last >= 15 {
            out.out.push(15 << 4);
            push_len(&mut out.out, last - 15);
        } else {
            out.out.push((last as u8) << 4);
        }
        out.out.extend_from_slice(&src[anchor..]);
        out.check(from)
    }
}

fn read32(d: &[u8], p: usize) -> u32 {
    u32::from_le_bytes(d[p..p + 4].try_into().unwrap())
}

/// Bytes from `a` and `b` on that are equal, `a` going no further than `limit`.
fn count(d: &[u8], a: usize, b: usize, limit: usize) -> usize {
    if a >= limit {
        return 0;
    }
    d[a..limit].iter().zip(&d[b..]).take_while(|(x, y)| x == y).count()
}

/// The bytes after a length field of 15: 255s, then the rest.
fn push_len(out: &mut Vec<u8>, mut n: usize) {
    while n >= 255 {
        out.push(255);
        n -= 255;
    }
    out.push(n as u8);
}
