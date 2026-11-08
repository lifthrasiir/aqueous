//! The module as a part module (see `FORMAT.md` §7): the input taken and the output put in order
//! through windows, so that what it holds does not depend on the lengths.
//!
//! A frame is cut into the blocks of its pieces, each held with its compressed block. A block
//! (kind 0) of `65547` bytes or more is compressed by [`wide`] from a sliding view of the data:
//! the 64 KiB a match can reach back, and the literals not yet written (a sequence's token comes
//! before them and holds the length of its match), which are at most [`LIT_MAX`].

use aqr_api::stream::{In, Pull, Put, Slice, Written};
use xxhash_rust::xxh32::{Xxh32, xxh32};

use crate::block::{self, Compressor, MAX_ACCELERATION, MAX_INPUT, TABLE_BYTES};
use crate::module::{BLOCK, FRAME};
use crate::{Error, Result, corrupt, frame};

/// Pages of the windows of the module as a part module ([`decode_part`]): of the input and of
/// the output.
pub const WINDOW: u32 = 4;

/// Literals a sequence may have at most in a block taken from a view (kind 0 with 65547 bytes or
/// more): a longer run is an error, and the formula's encoder does not make one.
pub const LIT_MAX: usize = 4 << 20;
/// The distance a match reaches back at most.
const HIST: usize = 65535 + 1;
/// Bytes of the view of the data: the literals, the history, and a piece to read ahead.
const VIEW: usize = LIT_MAX + 2 * HIST;
/// Bytes of the buffer that sequences are put through.
const SPOOL: usize = LIT_MAX + LIT_MAX / 255 + 16;
/// Bytes of the buffer of the output.
const OUT: usize = 16 << 10;
/// Entries of the table of a block taken from a view (the wide table).
const WIDE_ENTRIES: usize = 4096;
/// Length from which a block is compressed with the wide table (FORMAT.md §3.2).
const WIDE: usize = 65547;

/// The parameters: the kind, the acceleration, and for a frame its FLG and BD.
struct Params {
    kind: u8,
    accel: u32,
    flg: u8,
    bd: u8,
}

fn params(p: &[u8]) -> Result<Params> {
    let mut at = 1;
    let a = aqr_common::varint::get(p, &mut at).map_err(|_| corrupt())?;
    if !(1..=MAX_ACCELERATION as u64).contains(&a) {
        return Err(corrupt());
    }
    let accel = a as u32;
    match (p.first(), &p[at.min(p.len())..]) {
        (Some(&BLOCK), []) => Ok(Params { kind: BLOCK, accel, flg: 0, bd: 0 }),
        (Some(&FRAME), &[flg, bd]) if frame::encodable(flg, bd) => Ok(Params { kind: FRAME, accel, flg, bd }),
        _ => Err(corrupt()),
    }
}

/// The memory plan of the module as a part module ([`aqr_api::stack`]): what [`decode_part`]
/// takes beyond its initial memory (`initial` bytes) at most, of parameters `p` and an input of
/// `input` bytes (`None` for parameters it does not take): the buffer of the output, and by kind:
/// - a block of less than 65547 bytes: the data, the block and the compressor's table;
/// - a larger block: the view, the table, the buffer of the sequence and that of the output;
/// - a frame: the compressor's table, the largest piece of the data and the block made of it.
pub fn plan(initial: u64, p: &[u8], input: u64) -> Option<u64> {
    let pr = params(p).ok()?;
    let mut m = aqr_api::stack::Sim::new(initial);
    let n = usize::try_from(input).ok()?;
    m.take(OUT as u64);
    match pr.kind {
        BLOCK if n > MAX_INPUT => return None,
        BLOCK if n < WIDE => {
            use aqr_api::stack::block as b;
            m.take(n as u64);
            m.peak(b(block::bound(n) as u64) + b(TABLE_BYTES));
        }
        BLOCK => {
            m.take(TABLE_BYTES / 2);
            m.take(VIEW.min(n) as u64);
            m.take(SPOOL as u64);
        }
        _ => {
            let max = frame::block_max(pr.bd)?;
            let largest = n.min(max);
            m.take(TABLE_BYTES);
            m.take(largest as u64);
            m.peak(largest.saturating_sub(1).min(block::bound(largest)) as u64);
        }
    }
    Some(m.plan())
}

/// The output put in pieces of [`OUT`] bytes through `sink`.
struct Out<'a> {
    buf: Vec<u8>,
    sink: &'a mut dyn Put,
}

impl<'a> Out<'a> {
    fn new(sink: &'a mut dyn Put) -> Out<'a> {
        Out { buf: Vec::with_capacity(OUT), sink }
    }

    fn put(&mut self, d: &[u8]) -> Result<()> {
        if self.buf.len() + d.len() > OUT {
            self.flush()?;
            if d.len() > OUT {
                return self.sink.put(d).ok_or(Error("output of another length"));
            }
        }
        self.buf.extend_from_slice(d);
        Ok(())
    }

    /// The continuation bytes of a length field of 15 or more (`v` is what lies past 15).
    fn extra(&mut self, mut v: usize) -> Result<()> {
        const FULL: [u8; 64] = [255; 64];
        while v >= 255 * 64 {
            self.put(&FULL)?;
            v -= 255 * 64;
        }
        self.put(&FULL[..v / 255])?;
        self.put(&[(v % 255) as u8])
    }

    fn flush(&mut self) -> Result<()> {
        self.sink.put(&self.buf).ok_or(Error("output of another length"))?;
        self.buf.clear();
        Ok(())
    }
}

/// A view of the data pulled in order: the bytes from `base` to the end of what is held.
struct View<'a> {
    src: &'a mut dyn Pull,
    buf: Vec<u8>,
    base: usize,
    filled: usize,
}

impl<'a> View<'a> {
    fn new(src: &'a mut dyn Pull, n: usize) -> View<'a> {
        View { src, buf: vec![0; VIEW.min(n)], base: 0, filled: 0 }
    }

    /// Makes the bytes up to `hi` (of the data's `n`) held, dropping those before `keep`.
    #[inline(always)]
    fn need(&mut self, hi: usize, keep: usize) -> Result<()> {
        if hi > self.base + self.filled { self.fill(hi, keep) } else { Ok(()) }
    }

    #[cold]
    fn fill(&mut self, hi: usize, keep: usize) -> Result<()> {
        if hi - self.base > self.buf.len() {
            let drop = keep.saturating_sub(self.base).min(self.filled);
            self.buf.copy_within(drop..self.filled, 0);
            (self.filled, self.base) = (self.filled - drop, self.base + drop);
            if hi - self.base > self.buf.len() {
                return Err(Error("literals too long"));
            }
        }
        while self.base + self.filled < hi {
            let k = self.src.pull(&mut self.buf[self.filled..]);
            if k == 0 {
                return Err(Error("data of another length"));
            }
            self.filled += k;
        }
        Ok(())
    }

    #[inline(always)]
    fn at(&self, p: usize) -> u8 {
        self.buf[p - self.base]
    }

    #[inline(always)]
    fn u32(&self, p: usize) -> u32 {
        u32::from_le_bytes(self.buf[p - self.base..][..4].try_into().unwrap())
    }

    #[inline(always)]
    fn u64(&self, p: usize) -> u64 {
        u64::from_le_bytes(self.buf[p - self.base..][..8].try_into().unwrap())
    }
}

/// The state of [`wide_block`], a block of `n` bytes (at least [`WIDE`], at most [`MAX_INPUT`]) that a
/// fresh state compresses (FORMAT.md §3) from the data pulled from `src`: the same bytes as
/// [`Compressor::compress_fast`], and an error where a sequence has more than [`LIT_MAX`] literals
/// (or its search passes that many bytes without a match).
struct Wide<'a> {
    v: View<'a>,
    table: Vec<u32>,
    /// The literal start.
    lit: usize,
    accel: usize,
}

impl Wide<'_> {
    /// The oldest byte that is needed: a match source reaches back from the literal start by a
    /// match's distance (the literals' extension backwards stops at the literal start).
    fn keep(&self) -> usize {
        self.lit.saturating_sub(HIST)
    }

    fn hash(&self, p: usize) -> usize {
        ((self.v.u64(p) << 24).wrapping_mul(889523592379) >> 52) as usize
    }

    fn record(&mut self, p: usize) -> Result<()> {
        self.v.need(p + 8, self.keep())?;
        let h = self.hash(p);
        self.table[h] = p as u32;
        Ok(())
    }

    /// Probes `p`, giving the match source on a hit.
    fn probe(&mut self, p: usize) -> Result<Option<usize>> {
        self.v.need(p + 8, self.keep())?;
        let h = self.hash(p);
        let e = self.table[h] as usize;
        self.table[h] = p as u32;
        if e + 65535 < p {
            return Ok(None);
        }
        Ok((e < p && self.v.u32(e) == self.v.u32(p)).then_some(e))
    }

    /// Searches from `s` (FORMAT.md §3.3) for the first hit, within the bound `b`.
    fn search(&mut self, s: usize, b: usize) -> Result<Option<(usize, usize)>> {
        let (mut x, mut step, mut count) = (s, 1, 0usize);
        loop {
            let next = x + step;
            if next > b {
                return Ok(None);
            }
            if x - self.lit > LIT_MAX {
                return Err(Error("literals too long"));
            }
            if let Some(m) = self.probe(x)? {
                return Ok(Some((x, m)));
            }
            x = next;
            step = self.accel + count / 64;
            count += 1;
        }
    }

    /// The number of equal bytes at `a` and `b` (before it), up to `max`.
    fn common(&mut self, a: usize, b: usize, max: usize) -> Result<usize> {
        let mut i = 0;
        while i + 8 <= max {
            self.v.need(a + i + 8, (a + i).saturating_sub(HIST + 16))?;
            let (x, y) = (self.v.u64(a + i), self.v.u64(b + i));
            if x != y {
                return Ok(i + ((x ^ y).trailing_zeros() / 8) as usize);
            }
            i += 8;
        }
        while i < max {
            self.v.need(a + i + 1, (a + i).saturating_sub(HIST + 16))?;
            if self.v.at(a + i) != self.v.at(b + i) {
                break;
            }
            i += 1;
        }
        Ok(i)
    }
}

fn wide_block(src: &mut dyn Pull, n: usize, accel: usize, out: &mut Out) -> Result<()> {
    let table = vec![0; WIDE_ENTRIES];
    let mut w = Wide { v: View::new(src, n), table, lit: 0, accel };
    let mut seq: Vec<u8> = Vec::with_capacity(SPOOL);
    let (bound_b, end) = (n - 11, n - 5);
    w.record(0)?;
    let mut hit = w.search(1, bound_b)?;
    while let Some((mut p, mut m)) = hit {
        while p > w.lit && m > 0 && w.v.at(p - 1) == w.v.at(m - 1) {
            p -= 1;
            m -= 1;
        }
        // (the token, the literals' length, the literals and the offset, before the match is measured)
        let l = p - w.lit;
        seq.clear();
        seq.push(0);
        if l >= 15 {
            seq.extend(core::iter::repeat_n(255u8, (l - 15) / 255));
            seq.push(((l - 15) % 255) as u8);
        }
        seq.extend_from_slice(&w.v.buf[w.lit - w.v.base..p - w.v.base]);
        seq.extend_from_slice(&((p - m) as u16).to_le_bytes());
        let k = w.common(p + 4, m + 4, end - (p + 4))?;
        let q = p + 4 + k;
        seq[0] = (l.min(15) << 4 | k.min(15)) as u8;
        out.put(&seq)?;
        if k >= 15 {
            out.extra(k - 15)?;
        }
        w.lit = q;
        if q >= bound_b {
            break;
        }
        w.record(q - 2)?;
        hit = match w.probe(q)? {
            Some(m) => Some((q, m)),
            None => w.search(q + 1, bound_b)?,
        };
    }
    // the final literals, in pieces as they come
    let r = n - w.lit;
    out.put(&[(r.min(15) << 4) as u8])?;
    if r >= 15 {
        out.extra(r - 15)?;
    }
    let mut at = w.lit;
    while at < n {
        let hi = (at + OUT).min(n);
        w.v.need(hi, at)?;
        out.put(&w.v.buf[at - w.v.base..hi - w.v.base])?;
        at = hi;
    }
    Ok(())
}

/// Fills `buf` from `src`; false where it ends before.
fn read_all(src: &mut dyn Pull, buf: &mut [u8]) -> bool {
    let mut at = 0;
    while at < buf.len() {
        match src.pull(&mut buf[at..]) {
            0 => return false,
            k => at += k,
        }
    }
    true
}

/// The block of kind 0 of `n` bytes pulled from `src`, put to `out`.
fn block_kind(src: &mut dyn Pull, n: usize, accel: u32, out: &mut Out) -> Result<()> {
    if n > MAX_INPUT {
        return Err(Error("input too long for LZ4"));
    }
    if n >= WIDE {
        return wide_block(src, n, accel as usize, out);
    }
    let mut data = vec![0; n];
    if !read_all(src, &mut data) {
        return Err(Error("data of another length"));
    }
    let b = Compressor::compress_fast_in(&data, accel, block::bound(n))?;
    out.put(&b)
}

/// The frame of the data of `n` bytes pulled from `src`, put to `out`.
fn frame_kind(src: &mut dyn Pull, n: usize, p: &Params, out: &mut Out) -> Result<()> {
    let max = frame::block_max(p.bd).ok_or_else(corrupt)?;
    // (the descriptor: FLG, BD, the content size, then its checksum)
    let mut desc = [0u8; 11];
    desc[..2].copy_from_slice(&[p.flg, p.bd]);
    let mut d = 2;
    if p.flg & frame::CONTENT_SIZE != 0 {
        desc[2..10].copy_from_slice(&(n as u64).to_le_bytes());
        d = 10;
    }
    desc[d] = (xxh32(&desc[..d], 0) >> 8) as u8;
    out.put(&frame::MAGIC)?;
    out.put(&desc[..=d])?;
    let mut state = Compressor::default();
    let mut piece = vec![0; n.min(max)];
    let mut sum = Xxh32::new(0);
    let mut left = n;
    while left > 0 {
        let src_len = left.min(max);
        let piece = &mut piece[..src_len];
        if !read_all(src, piece) {
            return Err(Error("data of another length"));
        }
        left -= src_len;
        sum.update(piece);
        let b = state.compress_reset(piece, src_len - 1, p.accel)?;
        let bytes: &[u8] = match &b {
            Some(b) if b.len() < src_len => {
                out.put(&(b.len() as u32).to_le_bytes())?;
                b
            }
            _ => {
                out.put(&(src_len as u32 | 1 << 31).to_le_bytes())?;
                piece
            }
        };
        out.put(bytes)?;
        if p.flg & frame::BLOCK_CHECKSUM != 0 {
            out.put(&xxh32(bytes, 0).to_le_bytes())?;
        }
    }
    out.put(&[0; 4])?;
    if p.flg & frame::CONTENT_CHECKSUM != 0 {
        out.put(&sum.digest().to_le_bytes())?;
    }
    Ok(())
}

/// The module on the data of `n` bytes pulled from `src`, put to `sink` (the length of the
/// stream is for the caller to check).
pub fn run(params_: &[u8], src: &mut dyn Pull, n: u64, sink: &mut dyn Put) -> Result<()> {
    let p = params(params_)?;
    let n = usize::try_from(n).map_err(|_| corrupt())?;
    let mut out = Out::new(sink);
    match p.kind {
        BLOCK => block_kind(src, n, p.accel, &mut out)?,
        _ => frame_kind(src, n, &p, &mut out)?,
    }
    out.flush()
}

/// The module as a part module (its entry point, of `aqr_api::entry!`): what
/// [`crate::module::decode`] gives, its input taken and its output put in order through windows
/// ([`WINDOW`]) in pieces.
pub fn decode_part(params_len: u64, in_len: u64, size: u64) {
    aqr_api::finish(part(params_len, in_len, size));
}

fn part(params_len: u64, in_len: u64, size: u64) -> Result<()> {
    use aqr_api::Memory;
    // (a kind, a varint of three bytes at most, FLG and BD)
    let mut buf = [0; 8];
    let p = buf.get_mut(..params_len as usize).ok_or_else(corrupt)?;
    aqr_api::Params::read(0, p);
    let mut data = In::<0>::new(in_len);
    let mut out = Written::new(size);
    run(p, &mut data, in_len, &mut out)?;
    match out.total() == size {
        true => Ok(()),
        false => Err(Error("output of another length")),
    }
}

/// [`decode_part`] on a whole input, natively.
pub fn decode_streamed(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let &[data] = inputs else { return Err(Error("one input wanted")) };
    let mut out = Vec::new();
    run(params, &mut Slice(data), data.len() as u64, &mut out)?;
    match out.len() as u64 == len {
        true => Ok(out),
        false => Err(Error("output of another length")),
    }
}

/// Whether the module gives `expect` for `data` and the params, found without going past the first
/// difference; false too where it fails (a run of literals past [`LIT_MAX`]).
pub fn reproduces(params: &[u8], data: &[u8], expect: &[u8]) -> bool {
    struct Compare<'a> {
        expect: &'a [u8],
        at: usize,
    }
    impl Put for Compare<'_> {
        fn put(&mut self, d: &[u8]) -> Option<()> {
            let end = self.at.checked_add(d.len())?;
            let ok = self.expect.get(self.at..end) == Some(d);
            self.at = end;
            ok.then_some(())
        }
    }
    let mut cmp = Compare { expect, at: 0 };
    run(params, &mut Slice(data), data.len() as u64, &mut cmp).is_ok() && cmp.at == expect.len()
}
