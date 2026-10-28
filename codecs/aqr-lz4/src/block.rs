//! LZ4 blocks: a decoder, and liblz4 1.10.0's fast compressor (`LZ4_compress_generic` without
//! a dictionary, 64-bit little endian) reimplemented to give the same bytes. `FORMAT.md` §3
//! specifies the compressor.

use crate::{Error, Result, corrupt};

/// `LZ4_MAX_INPUT_SIZE`: liblz4 compresses nothing longer.
pub const MAX_INPUT: usize = 0x7E00_0000;
/// The acceleration liblz4 clamps to (`LZ4_ACCELERATION_MAX`).
pub const MAX_ACCELERATION: u32 = 65537;

const MINMATCH: usize = 4;

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

/// Entries of the table of FORMAT.md §3.1.
const TABLE: usize = 8192;

/// Where the bytes of a block go: a vector, or a reference they are compared against.
trait Sink {
    /// Appends `b`; false where the block can no longer be what is wanted.
    fn put(&mut self, b: &[u8]) -> bool;
    /// Bytes put so far.
    fn len(&self) -> usize;
}

impl Sink for Vec<u8> {
    fn put(&mut self, b: &[u8]) -> bool {
        self.extend_from_slice(b);
        true
    }

    fn len(&self) -> usize {
        Vec::len(self)
    }
}

/// A sink that checks the bytes against `expect` as they come.
struct Compare<'a> {
    expect: &'a [u8],
    at: usize,
}

impl Sink for Compare<'_> {
    fn put(&mut self, b: &[u8]) -> bool {
        let end = self.at + b.len();
        let ok = self.expect.get(self.at..end) == Some(b);
        self.at = end;
        ok
    }

    fn len(&self) -> usize {
        self.at
    }
}

/// Puts the continuation bytes of a length field of 15 or more (`v` is what lies past 15).
fn put_extra<S: Sink>(out: &mut S, mut v: usize) -> bool {
    const FULL: [u8; 64] = [255; 64];
    while v >= 255 * 64 {
        if !out.put(&FULL) {
            return false;
        }
        v -= 255 * 64;
    }
    out.put(&FULL[..v / 255]) && out.put(&[(v % 255) as u8])
}

/// Number of equal bytes at the starts of `src[a..]` and `src[b..]`, up to `max`, which must
/// keep both within `src`.
fn common(src: &[u8], a: usize, b: usize, max: usize) -> usize {
    let mut i = 0;
    while i + 8 <= max {
        let x = u64::from_le_bytes(src[a + i..a + i + 8].try_into().unwrap());
        let y = u64::from_le_bytes(src[b + i..b + i + 8].try_into().unwrap());
        if x != y {
            return i + ((x ^ y).trailing_zeros() / 8) as usize;
        }
        i += 8;
    }
    while i < max && src[a + i] == src[b + i] {
        i += 1;
    }
    i
}

/// The table and the data of one block, with what hashing and candidates need.
struct Probe<'a, const WIDE: bool> {
    table: &'a mut [u32; TABLE],
    src: &'a [u8],
    start: u32,
    small: bool,
}

impl<const WIDE: bool> Probe<'_, WIDE> {
    fn hash(&self, p: usize) -> usize {
        if WIDE {
            let v = u64::from_le_bytes(self.src[p..p + 8].try_into().unwrap());
            ((v << 24).wrapping_mul(889523592379) >> 52) as usize
        } else {
            let v = u32::from_le_bytes(self.src[p..p + 4].try_into().unwrap());
            (v.wrapping_mul(2654435761) >> 19) as usize
        }
    }

    fn record(&mut self, p: usize) {
        let h = self.hash(p);
        self.table[h] = self.start.wrapping_add(p as u32);
    }

    /// Probes `p`, giving the match source on a hit.
    fn probe(&mut self, p: usize) -> Option<usize> {
        let h = self.hash(p);
        let e = self.table[h];
        self.table[h] = self.start.wrapping_add(p as u32);
        if self.small && e < self.start {
            return None;
        }
        if WIDE && e as u64 + 65535 < self.start as u64 + p as u64 {
            return None;
        }
        let m = e.wrapping_sub(self.start) as usize;
        (m < p && self.src[m..m + 4] == self.src[p..p + 4]).then_some(m)
    }

    /// Searches from `s` (FORMAT.md §3.3) for the first hit, within the bound `b`.
    fn search(&mut self, s: usize, accel: usize, b: usize) -> Option<(usize, usize)> {
        let (mut x, mut step, mut count) = (s, 1, 0usize);
        loop {
            let next = x + step;
            if next > b {
                return None;
            }
            if let Some(m) = self.probe(x) {
                return Some((x, m));
            }
            x = next;
            step = accel + count / 64;
            count += 1;
        }
    }
}

/// Which of the two table layouts a block uses.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Cleared,
    Narrow,
    Wide,
}

/// The compression state kept between the blocks of a frame (FORMAT.md §3.1).
pub struct Compressor {
    table: Box<[u32; TABLE]>,
    mode: Mode,
    offset: u32,
}

impl Default for Compressor {
    fn default() -> Compressor {
        Compressor { table: Box::new([0; TABLE]), mode: Mode::Cleared, offset: 0 }
    }
}

impl Compressor {
    /// `block(src, acceleration)` of FORMAT.md §3.3: `src` in a block, with a fresh state and
    /// no limit. Fails only where `src` is longer than [`MAX_INPUT`].
    pub fn compress_fast(src: &[u8], acceleration: u32) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(bound(src.len()));
        Compressor::default().compress_into(src, usize::MAX, acceleration, &mut out)?;
        Ok(out)
    }

    /// Whether [`Compressor::compress_fast`] gives `expect`, found without compressing past
    /// the first difference.
    pub fn reproduces_fast(src: &[u8], acceleration: u32, expect: &[u8]) -> bool {
        let mut cmp = Compare { expect, at: 0 };
        matches!(Compressor::default().compress_into(src, usize::MAX, acceleration, &mut cmp), Ok(true))
            && cmp.at == expect.len()
    }

    /// `src` in a block of at most `cap` bytes (FORMAT.md §3.2-§3.4), keeping this state for
    /// the next block: None where the block does not fit. Fails only where `src` is longer
    /// than [`MAX_INPUT`].
    pub fn compress_reset(&mut self, src: &[u8], cap: usize, acceleration: u32) -> Result<Option<Vec<u8>>> {
        let mut out = Vec::with_capacity(cap.min(bound(src.len())));
        Ok(self.compress_into(src, cap, acceleration, &mut out)?.then_some(out))
    }

    /// Prepares for and compresses a block into `out`; false where it was cut short, because
    /// of the limit or of the sink.
    fn compress_into<S: Sink>(&mut self, src: &[u8], cap: usize, acceleration: u32, out: &mut S) -> Result<bool> {
        let n = src.len();
        if n > MAX_INPUT {
            return Err(Error("input too long for LZ4"));
        }
        let cap = if cap >= bound(n) { usize::MAX } else { cap };
        if n == 0 {
            return Ok(cap >= 1 && out.put(&[0]));
        }
        // FORMAT.md §3.2.
        let mode = if n < 65547 { Mode::Narrow } else { Mode::Wide };
        if self.mode != Mode::Cleared
            && (self.mode != mode
                || (mode == Mode::Narrow && self.offset as u64 + n as u64 >= 65535)
                || (mode == Mode::Wide && self.offset > 1 << 30)
                || n >= 4096)
        {
            self.table.fill(0);
            self.offset = 0;
            self.mode = Mode::Cleared;
        }
        if self.offset != 0 && mode == Mode::Wide {
            self.offset = self.offset.wrapping_add(65536);
        }
        let start = self.offset;
        let small = mode == Mode::Narrow && start != 0;
        self.offset = start.wrapping_add(n as u32);
        self.mode = mode;
        let accel = acceleration.clamp(1, MAX_ACCELERATION) as usize;
        let table = &mut *self.table;
        Ok(if mode == Mode::Wide {
            block_body(Probe::<true> { table, src, start, small }, accel, cap, out)
        } else {
            block_body(Probe::<false> { table, src, start, small }, accel, cap, out)
        }
        .is_some())
    }
}

/// FORMAT.md §3.3 and §3.4 for a block of at least one byte; None where it was cut short.
fn block_body<const WIDE: bool, S: Sink>(mut pr: Probe<'_, WIDE>, accel: usize, cap: usize, out: &mut S) -> Option<()> {
    let src = pr.src;
    let n = src.len();
    let mut lit = 0; // the literal start
    if n >= 13 {
        let (bound_b, end) = (n - 11, n - 5);
        pr.record(0);
        let mut hit = pr.search(1, accel, bound_b);
        let mut checked = true;
        while let Some((mut p, mut m)) = hit {
            while p > lit && m > 0 && src[p - 1] == src[m - 1] {
                p -= 1;
                m -= 1;
            }
            let k = common(src, p + 4, m + 4, end - (p + 4));
            let q = p + 4 + k;
            let l = p - lit;
            out.put(&[(l.min(15) << 4 | k.min(15)) as u8]).then_some(())?;
            if checked && out.len() + l + 8 + l / 255 > cap {
                return None;
            }
            if l >= 15 {
                put_extra(out, l - 15).then_some(())?;
            }
            out.put(&src[lit..p]).then_some(())?;
            out.put(&((p - m) as u16).to_le_bytes()).then_some(())?;
            if out.len() + 6 + (k + 240) / 255 > cap {
                return None;
            }
            if k >= 15 {
                put_extra(out, k - 15).then_some(())?;
            }
            lit = q;
            if q >= bound_b {
                break;
            }
            pr.record(q - 2);
            (hit, checked) = match pr.probe(q) {
                Some(m) => (Some((q, m)), false),
                None => (pr.search(q + 1, accel, bound_b), true),
            };
        }
    }
    let r = n - lit;
    if out.len() + r + 1 + (r + 240) / 255 > cap {
        return None;
    }
    out.put(&[(r.min(15) << 4) as u8]).then_some(())?;
    if r >= 15 {
        put_extra(out, r - 15).then_some(())?;
    }
    out.put(&src[lit..]).then_some(())
}
