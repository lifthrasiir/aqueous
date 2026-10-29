//! JPEG parsing, fast Huffman decoding of scans into quantised coefficients, and bit-exact
//! re-emission of the scans from those coefficients.
//!
//! Decoding strategy:
//! - the entropy-coded segment is first destuffed (0xFF00 -> 0xFF, RSTn split) into a clean
//!   buffer, so the hot loop never has to look for markers;
//! - MSB-first 64-bit bit buffer with branchless refill (ryg's "variant 4" style);
//! - one combined lookup table per Huffman table, indexed by the next `FAST_BITS` bits, that
//!   yields code length + run + the sign-extended coefficient whenever code and extra bits fit,
//!   plus a two-symbol variant for AC;
//! - coefficients are stored in natural or zigzag order, with a per-block nonzero bitmask in
//!   zigzag bit order.
//!
//! Re-emission is only bit-exact for "standard" encoders. The decoder therefore checks, cheaply
//! and inline, every degree of freedom an encoder has beyond the coefficients themselves (EOB
//! placement, ZRL use, padding bits, restart markers, fill bytes, Huffman table sanity); a scan
//! that passes is guaranteed to be reproduced exactly by `encode_scan`.
//!
//! Supported here: sequential Huffman JPEGs (SOF0/SOF1, 8-bit), any number of scans as long as
//! each component is coded by exactly one scan. Progressive files: see `progressive`.

use aqr_common::wire::Wire;
use std::fmt;

pub const FAST_BITS: u32 = 12;
// Bit budget of the unrolled AC loop: one refill guarantees 56 bits for 4 fast lookups, and the
// slow path leaves >= 56 - 16 = 40 bits for the (at most 3) lookups remaining in the group.
const _: () = assert!(4 * FAST_BITS <= 56 && 3 * FAST_BITS <= 40);

/// Largest DC magnitude accepted; keeps DC prediction residuals of downstream models in 16 bits.
pub const DC_LIMIT: i32 = (1 << 14) - 1;

/// Buffers kept for reuse by a thread (to spare allocation and page faults on the next file)
/// are dropped instead above this size, so that a large image's memory is not held on to.
pub(crate) const KEEP_BYTES: usize = 16 << 20;

#[derive(Debug)]
pub struct Error(pub String);

impl Wire for Error {
    fn put(&self, out: &mut Vec<u8>) {
        self.0.put(out)
    }

    fn get(b: &mut &[u8]) -> Option<Self> {
        Some(Error(String::get(b)?))
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Error {}
pub(crate) fn err<T>(s: impl Into<String>) -> Result<T, Error> {
    Err(Error(s.into()))
}

#[derive(Clone, Default)]
pub struct HuffSpec {
    pub counts: [u8; 16], // number of codes of length 1..=16
    pub vals: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct Component {
    pub id: u8,
    pub h: usize,
    pub v: usize,
    pub tq: usize,
    /// storage size in blocks (MCU-padded for multi-component images)
    pub bw: usize,
    pub bh: usize,
    /// libjpeg's width_in_blocks / height_in_blocks (the "real" image area)
    pub wib: usize,
    pub hib: usize,
}

/// One component of a scan, with snapshots of the Huffman tables in force at the SOS.
#[derive(Clone)]
pub struct ScanComp {
    pub ci: usize,
    pub dc: Option<HuffSpec>,
    pub ac: Option<HuffSpec>,
}

#[derive(Clone)]
pub struct Scan {
    pub comps: Vec<ScanComp>,
    pub ss: u8,
    pub se: u8,
    pub ah: u8,
    pub al: u8,
    /// restart interval in MCUs (0 = none)
    pub ri: usize,
    pub mcux: usize,
    pub mcuy: usize,
    /// entropy-coded data `start..end` in the parsed buffer (up to the terminating marker,
    /// including any RST markers)
    pub start: usize,
    pub end: usize,
}

pub struct Jpeg {
    pub width: usize,
    pub height: usize,
    pub progressive: bool,
    pub comps: Vec<Component>,
    /// quantisation tables in force at the first scan (zigzag order)
    pub qt: [[u16; 64]; 4],
    pub scans: Vec<Scan>,
}

fn be16(d: &[u8], i: usize) -> Result<usize, Error> {
    match d.get(i..i + 2) {
        Some(b) => Ok(((b[0] as usize) << 8) | b[1] as usize),
        None => err("truncated"),
    }
}
fn byte(d: &[u8], i: usize) -> Result<u8, Error> {
    d.get(i).copied().ok_or(Error("truncated".into()))
}

/// End of the entropy-coded data starting at `start`: the first 0xFF (of a run of fill bytes)
/// that introduces a marker other than RSTn, or the end of the buffer. 0xFFs at the very end of
/// the buffer are taken as the start of a cut-off marker and left outside the data.
pub(crate) fn scan_end(d: &[u8], start: usize) -> usize {
    let mut i = start;
    while let Some(o) = memchr::memchr(0xFF, &d[i..]) {
        let ff = i + o;
        let mut n = ff + 1;
        while n < d.len() && d[n] == 0xFF {
            n += 1;
        }
        if n >= d.len() {
            return ff;
        }
        match d[n] {
            0x00 | 0xD0..=0xD7 => i = n + 1,
            _ => return ff,
        }
    }
    d.len()
}

/// Parse a JPEG (or a JPEG with its entropy-coded data removed: scans then come out empty).
pub fn parse(d: &[u8]) -> Result<Jpeg, Error> {
    if d.len() < 4 || d[0] != 0xFF || d[1] != 0xD8 {
        return err("not a JPEG");
    }
    let mut j =
        Jpeg { width: 0, height: 0, progressive: false, comps: Vec::new(), qt: [[0; 64]; 4], scans: Vec::new() };
    let mut dc: [Option<HuffSpec>; 4] = Default::default();
    let mut ac: [Option<HuffSpec>; 4] = Default::default();
    let mut qt_seen = [[0u16; 64]; 4];
    let mut ri = 0usize;
    let mut sof = false;
    let mut i = 2;
    loop {
        // tolerate garbage and fill bytes between segments (they are kept verbatim anyway)
        while i < d.len() && d[i] != 0xFF {
            i += 1;
        }
        while i < d.len() && d[i] == 0xFF {
            i += 1;
        }
        if i >= d.len() {
            break; // no EOI: truncated file
        }
        let m = d[i];
        i += 1;
        match m {
            0xD9 => break,
            0x01 | 0xD0..=0xD8 => continue, // standalone markers
            _ => {}
        }
        let len = be16(d, i)?;
        if len < 2 {
            return err("bad segment length");
        }
        let seg = d.get(i + 2..i + len).ok_or(Error("truncated segment".into()))?;
        match m {
            0xC0..=0xC2 => {
                if sof {
                    return err("multiple frames");
                }
                sof = true;
                j.progressive = m == 0xC2;
                if byte(seg, 0)? != 8 {
                    return err("only 8-bit precision");
                }
                j.height = be16(seg, 1)?;
                j.width = be16(seg, 3)?;
                let n = byte(seg, 5)? as usize;
                if n == 0 || n > 4 || j.width == 0 || j.height == 0 {
                    return err("bad frame header");
                }
                for c in 0..n {
                    let s = seg.get(6 + 3 * c..9 + 3 * c).ok_or(Error("truncated SOF".into()))?;
                    let (h, v) = ((s[1] >> 4) as usize, (s[1] & 15) as usize);
                    if !(1..=4).contains(&h) || !(1..=4).contains(&v) || j.comps.iter().any(|c| c.id == s[0]) {
                        return err("bad component");
                    }
                    j.comps.push(Component { id: s[0], h, v, tq: (s[2] & 3) as usize, bw: 0, bh: 0, wib: 0, hib: 0 });
                }
                let hmax = j.comps.iter().map(|c| c.h).max().unwrap();
                let vmax = j.comps.iter().map(|c| c.v).max().unwrap();
                let (mx, my) = (j.width.div_ceil(hmax * 8), j.height.div_ceil(vmax * 8));
                let single = n == 1;
                for c in j.comps.iter_mut() {
                    c.wib = (j.width * c.h).div_ceil(hmax * 8);
                    c.hib = (j.height * c.v).div_ceil(vmax * 8);
                    (c.bw, c.bh) = if single { (c.wib, c.hib) } else { (mx * c.h, my * c.v) };
                }
                if j.comps.iter().map(|c| c.bw * c.bh).sum::<usize>() > 1 << 24 {
                    return err("image too large");
                }
            }
            0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => {
                return err("unsupported SOF (lossless/arithmetic/hierarchical)");
            }
            0xC4 => {
                let mut p = 0;
                while p < seg.len() {
                    let tc = seg[p] >> 4;
                    let th = (seg[p] & 15) as usize;
                    if tc > 1 || th > 3 {
                        return err("bad DHT");
                    }
                    let mut counts = [0u8; 16];
                    counts.copy_from_slice(seg.get(p + 1..p + 17).ok_or(Error("truncated DHT".into()))?);
                    let n: usize = counts.iter().map(|&c| c as usize).sum();
                    let vals = seg.get(p + 17..p + 17 + n).ok_or(Error("truncated DHT".into()))?.to_vec();
                    let spec = Some(HuffSpec { counts, vals });
                    if tc == 0 {
                        dc[th] = spec
                    } else {
                        ac[th] = spec
                    }
                    p += 17 + n;
                }
            }
            0xDB => {
                let mut p = 0;
                while p < seg.len() {
                    let pq = seg[p] >> 4;
                    let tq = (seg[p] & 15) as usize;
                    if pq > 1 || tq > 3 {
                        return err("bad DQT");
                    }
                    for k in 0..64 {
                        qt_seen[tq][k] =
                            if pq == 0 { byte(seg, p + 1 + k)? as u16 } else { be16(seg, p + 1 + 2 * k)? as u16 };
                    }
                    p += 1 + if pq == 0 { 64 } else { 128 };
                }
            }
            0xDD => ri = be16(seg, 0)?,
            0xDA => {
                if !sof {
                    return err("SOS before SOF");
                }
                if j.scans.is_empty() {
                    j.qt = qt_seen;
                }
                let ns = byte(seg, 0)? as usize;
                if ns == 0 || ns > 4 {
                    return err("bad SOS");
                }
                let mut comps = Vec::new();
                for s in 0..ns {
                    let cid = byte(seg, 1 + 2 * s)?;
                    let t = byte(seg, 2 + 2 * s)?;
                    let ci = j.comps.iter().position(|c| c.id == cid).ok_or(Error("bad component id".into()))?;
                    if comps.iter().any(|c: &ScanComp| c.ci == ci) {
                        return err("duplicate scan component");
                    }
                    comps.push(ScanComp {
                        ci,
                        dc: dc[(t >> 4) as usize & 3].clone(),
                        ac: ac[(t & 15) as usize & 3].clone(),
                    });
                }
                let ss = byte(seg, 1 + 2 * ns)?;
                let se = byte(seg, 2 + 2 * ns)?;
                let a = byte(seg, 3 + 2 * ns)?;
                let (mcux, mcuy) = if ns == 1 {
                    let c = &j.comps[comps[0].ci];
                    (c.wib, c.hib)
                } else {
                    let hmax = j.comps.iter().map(|c| c.h).max().unwrap();
                    let vmax = j.comps.iter().map(|c| c.v).max().unwrap();
                    (j.width.div_ceil(hmax * 8), j.height.div_ceil(vmax * 8))
                };
                let start = i + len;
                let end = scan_end(d, start);
                j.scans.push(Scan { comps, ss, se, ah: a >> 4, al: a & 15, ri, mcux, mcuy, start, end });
                i = end;
                continue;
            }
            _ => {}
        }
        i += len;
    }
    if !sof || j.scans.is_empty() {
        return err("no frame or no scan");
    }
    Ok(j)
}

// ---------------------------------------------------------------- destuffing

/// Clean entropy data plus restart-segment start offsets into it.
#[derive(Default)]
pub struct Clean {
    pub data: Vec<u8>,
    pub segments: Vec<usize>,
    /// RST markers were numbered 0, 1, .., 7, 0, .. and not preceded by fill bytes
    pub markers_ok: bool,
}

pub const PAD: usize = 16;

/// Destuff the entropy-coded data `scan` (as delimited by `Scan::start..end`).
pub fn destuff(scan: &[u8], out: &mut Clean) {
    out.data.clear();
    out.segments.clear();
    out.data.reserve(scan.len() + PAD);
    out.segments.push(0);
    out.markers_ok = true;
    let mut i = 0;
    loop {
        match memchr::memchr(0xFF, &scan[i..]) {
            None => {
                out.data.extend_from_slice(&scan[i..]);
                break;
            }
            Some(o) => {
                out.data.extend_from_slice(&scan[i..i + o]);
                i += o;
                let mut n = i + 1;
                while n < scan.len() && scan[n] == 0xFF {
                    n += 1;
                }
                if n >= scan.len() {
                    // dangling 0xFF(s): not produced by `scan_end`
                    out.markers_ok = false;
                    break;
                }
                if n != i + 1 {
                    out.markers_ok = false; // fill bytes inside the scan
                }
                match scan[n] {
                    0x00 => out.data.push(0xFF),
                    m @ 0xD0..=0xD7 => {
                        if (m - 0xD0) as usize != (out.segments.len() - 1) & 7 {
                            out.markers_ok = false;
                        }
                        out.segments.push(out.data.len());
                    }
                    _ => break, // not reached for data delimited by `scan_end`
                }
                i = n + 1;
            }
        }
    }
    out.data.extend_from_slice(&[0u8; PAD]);
}

// ---------------------------------------------------------------- Huffman tables

/// Fast-table entry. `len` = bits to consume; flags in the top bits of `len`.
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct Entry {
    /// FULL: sign-extended coefficient value. CODE: size (number of extra bits).
    val: i16,
    /// run (zero coefficients skipped before this one); 64 = EOB
    run: u8,
    /// low 5 bits: bits to consume; bit 6: CODE entry (extra bits must be read); 0 = slow
    len: u8,
}
const CODE: u8 = 0x40;
const EOB_RUN: u8 = 64;

/// Two-symbol fast entry (AC only). Slot 2 is a no-op (run 0, value 0) when only one symbol
/// fits, which is signalled by `len == l1`. `len` (total bits) uses the same encoding as
/// `Entry::len` (non-fast => use `fast` + slow path); `l1` is the first symbol's length alone,
/// needed when the block fills up after slot 1 (the slot-2 bits then belong to the next block).
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct Pair {
    v1: i16,
    v2: i16,
    r1: u8,
    r2: u8,
    len: u8,
    l1: u8,
}

pub struct Table {
    pair: Box<[Pair; 1 << FAST_BITS]>,
    fast: Box<[Entry; 1 << FAST_BITS]>,
    maxcode: [i32; 18], // largest code of length l, -1 if none; maxcode[17] = sentinel
    valoff: [i32; 17],  // vals index = code + valoff[l]
    vals: Vec<u8>,
    is_ac: bool,
}

#[inline(always)]
pub(crate) fn extend(v: u32, s: u32) -> i32 {
    // JPEG HUFF_EXTEND
    if s == 0 {
        0
    } else if v < (1 << (s - 1)) {
        v as i32 - (1 << s) + 1
    } else {
        v as i32
    }
}

impl Table {
    /// Build decoding tables. Fails on tables a standard encoder cannot have produced or that
    /// break the bijection between symbols and codes: oversubscribed code space, duplicate
    /// symbols, AC symbols (r, 0) other than EOB / ZRL, DC sizes > 15.
    pub fn new(spec: &HuffSpec, is_ac: bool) -> Result<Table, Error> {
        let mut seen = [false; 256];
        for &v in &spec.vals {
            let bad = if is_ac { v & 15 == 0 && v != 0 && v != 0xF0 } else { v > 15 };
            if bad || std::mem::replace(&mut seen[v as usize], true) {
                return err("non-standard Huffman table");
            }
        }
        let mut fast = Box::new([Entry::default(); 1 << FAST_BITS]);
        let mut maxcode = [-1i32; 18];
        let mut valoff = [0i32; 17];
        let mut code = 0u32;
        let mut k = 0usize;
        for l in 1..=16u32 {
            let n = spec.counts[l as usize - 1] as usize;
            valoff[l as usize] = k as i32 - code as i32;
            if code + n as u32 > 1 << l {
                return err("oversubscribed Huffman table");
            }
            for _ in 0..n {
                let sym = spec.vals[k];
                if l <= FAST_BITS {
                    let (run, size) = if is_ac {
                        match sym {
                            0x00 => (EOB_RUN, 0),
                            0xF0 => (15, 0),
                            _ => (sym >> 4, (sym & 15) as u32),
                        }
                    } else {
                        (0, sym as u32)
                    };
                    let rem = FAST_BITS - l;
                    let base = (code << rem) as usize;
                    for idx in base..base + (1usize << rem) {
                        fast[idx] = if size <= rem {
                            let extra = ((idx as u32) >> (rem - size)) & ((1u32 << size) - 1);
                            Entry { val: extend(extra, size) as i16, run, len: (l + size) as u8 }
                        } else {
                            Entry { val: size as i16, run, len: l as u8 | CODE }
                        };
                    }
                }
                code += 1;
                k += 1;
            }
            if n > 0 {
                maxcode[l as usize] = code as i32 - 1;
            }
            code <<= 1;
        }
        maxcode[17] = i32::MAX;
        let mut pair = Box::new([Pair::default(); 1 << FAST_BITS]);
        let full = |e: &Entry| e.len & !31 == 0 && e.len != 0;
        for idx in 0..1usize << FAST_BITS {
            let e1 = fast[idx];
            let mut p = Pair { v1: e1.val, r1: e1.run, len: e1.len, l1: e1.len, ..Default::default() };
            if is_ac && full(&e1) && e1.run != EOB_RUN {
                let l1 = e1.len as u32;
                let e2 = fast[(idx << l1) & ((1 << FAST_BITS) - 1)];
                if full(&e2) && e2.len as u32 <= FAST_BITS - l1 {
                    p.v2 = e2.val;
                    p.r2 = e2.run;
                    p.len = (l1 + e2.len as u32) as u8;
                }
            }
            pair[idx] = p;
        }
        Ok(Table { pair, fast, maxcode, valoff, vals: spec.vals.clone(), is_ac })
    }

    /// Slow path for codes longer than FAST_BITS: returns (code length, symbol).
    #[cold]
    fn slow(&self, bits: u64) -> Option<(u32, u8)> {
        let mut l = FAST_BITS + 1;
        loop {
            if l > 16 {
                return None;
            }
            let c = (bits >> (64 - l)) as i32;
            if c <= self.maxcode[l as usize] {
                return self.vals.get((c + self.valoff[l as usize]) as usize).map(|&s| (l, s));
            }
            l += 1;
        }
    }
}

// ---------------------------------------------------------------- bit reader

#[derive(Clone, Copy)]
pub(crate) struct Bits<'a> {
    pub(crate) buf: u64,
    pub(crate) cnt: u32,
    pub(crate) pos: usize,
    pub(crate) data: &'a [u8],
}
impl<'a> Bits<'a> {
    /// Guarantees >= 56 valid bits. Requires PAD bytes of slack at the end of `data`.
    #[inline(always)]
    pub(crate) fn refill(&mut self) {
        let p = self.pos.min(self.data.len() - 8);
        let w = u64::from_be_bytes(self.data[p..p + 8].try_into().unwrap());
        self.buf |= w >> self.cnt;
        self.pos += ((63 - self.cnt) >> 3) as usize;
        self.cnt |= 56;
    }
    #[inline(always)]
    pub(crate) fn peek(&self, n: u32) -> u32 {
        (self.buf >> (64 - n)) as u32
    }
    #[inline(always)]
    pub(crate) fn consume(&mut self, n: u32) {
        self.buf <<= n;
        self.cnt -= n;
    }
    #[inline(always)]
    pub(crate) fn get(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let v = self.peek(n);
        self.consume(n);
        v
    }
}

// ---------------------------------------------------------------- decoding

/// Decoded coefficients of one component: blocks in natural or zigzag order (see `Order`);
/// `masks` always in zigzag bit order. Holds block rows `y0..y0 + bh`: the whole component, or
/// one stripe of it.
#[derive(Default)]
pub struct Coefs {
    pub bw: usize,
    pub bh: usize,
    pub y0: usize,
    pub blocks: Vec<[i16; 64]>,
    pub masks: Vec<u64>,
}

impl Coefs {
    /// Index of block (bx, by), if it is held.
    #[inline(always)]
    pub(crate) fn index(&self, bx: usize, by: usize) -> Option<usize> {
        let y = by.wrapping_sub(self.y0);
        (bx < self.bw && y < self.bh).then_some(y * self.bw + bx)
    }
    /// Room for block rows `rows` of a component `bw` blocks wide; contents unspecified.
    pub(crate) fn hold(&mut self, bw: usize, rows: std::ops::Range<usize>) {
        (self.bw, self.bh, self.y0) = (bw, rows.len(), rows.start);
        self.blocks.resize(bw * rows.len(), [0; 64]);
        self.masks.resize(bw * rows.len(), 0);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Order {
    Natural,
    Zigzag,
}

/// Outcome of decoding one scan: everything `encode_scan` needs besides the coefficients.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScanInfo {
    /// the scan is reproduced exactly by `encode_scan`
    pub standard: bool,
    /// number of RST markers after the last MCU (some encoders emit one or more)
    pub trailing_rst: u32,
    /// segments are padded with 0-bits instead of 1-bits
    pub pad_zero: bool,
    pub truncated: Option<Truncation>,
    /// the scan is not modelled: its entropy-coded data is stored verbatim
    pub raw: bool,
}

/// A scan whose data ends before its last MCU.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Truncation {
    /// number of complete MCUs (they are coded normally; later blocks are zero)
    pub mcus: usize,
    /// bit position in the destuffed data where those MCUs end
    pub bits: usize,
    /// destuffed data from byte `bits / 8` to the end
    pub tail: Vec<u8>,
    /// truncation happened inside a restart segment that starts right after the complete
    /// MCUs, so the previous segment's padding and RST marker precede the tail
    pub boundary: bool,
}

const BAD_RUN: u32 = 255;

thread_local! {
    /// slow-path decodes on this thread (profiling aid; thread-local to stay contention-free)
    pub static SLOW_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Slow path (code longer than FAST_BITS, or extra bits not fitting in the fast entry).
/// Operates on a `Bits` copy so the hot loop's scalars never have their address taken.
#[inline(never)]
#[cold]
fn decode_slow(b: &mut Bits, t: &Table, e: Entry) -> (u32, i32) {
    SLOW_CALLS.with(|c| c.set(c.get() + 1));
    let (run, size) = if e.len & CODE != 0 {
        b.consume((e.len & 31) as u32);
        (e.run as u32, e.val as u32)
    } else {
        let Some((l, sym)) = t.slow(b.buf) else { return (BAD_RUN, 0) };
        b.consume(l);
        if t.is_ac {
            match sym {
                0x00 => (EOB_RUN as u32, 0),
                0xF0 => (15, 0),
                _ => ((sym >> 4) as u32, (sym & 15) as u32),
            }
        } else {
            (0, sym as u32)
        }
    };
    b.refill();
    let v = b.get(size);
    // leave >= 40 bits so the caller's unrolled fast decodes (<= 3 x 12 bits) stay safe
    b.refill();
    (run, extend(v, size))
}

/// Allocate (or reuse) coefficient storage for all components. With `zero == false` the
/// contents are unspecified: only for callers that overwrite every block.
pub fn alloc_coefs(j: &Jpeg, out: &mut Vec<Coefs>, zero: bool) {
    out.resize_with(j.comps.len(), Coefs::default);
    out.truncate(j.comps.len());
    for (o, c) in out.iter_mut().zip(&j.comps) {
        o.bw = c.bw;
        o.bh = c.bh;
        o.y0 = 0;
        if zero {
            o.blocks.clear();
            o.masks.clear();
        }
        o.blocks.resize(c.bw * c.bh, [0; 64]);
        o.masks.resize(c.bw * c.bh, 0);
    }
}

/// Decode all scans of a sequential JPEG `d` into `out`. Returns per-scan info; scans that
/// cannot be decoded or reproduced are marked `raw`. Fails only on structural problems.
pub fn decode_all(
    j: &Jpeg,
    d: &[u8],
    order: Order,
    out: &mut Vec<Coefs>,
    clean: &mut Clean,
) -> Result<Vec<ScanInfo>, Error> {
    if j.progressive {
        return err("progressive JPEG: use progressive::decode_all");
    }
    // a single scan with all components writes every (MCU-padded) block
    let full = j.scans.len() == 1 && j.scans[0].comps.len() == j.comps.len();
    alloc_coefs(j, out, !full);
    let mut covered = vec![false; j.comps.len()];
    let mut infos = Vec::with_capacity(j.scans.len());
    for s in &j.scans {
        if s.ss != 0 || s.se != 63 || s.ah != 0 || s.al != 0 {
            return err("bad sequential scan parameters");
        }
        for sc in &s.comps {
            if std::mem::replace(&mut covered[sc.ci], true) {
                return err("component coded by more than one scan");
            }
        }
        destuff(&d[s.start..s.end], clean);
        let r = match order {
            Order::Natural => decode_scan::<true>(j, s, clean, out),
            Order::Zigzag => decode_scan::<false>(j, s, clean, out),
        };
        // sequential scans are independent: a bad one is simply kept verbatim
        infos.push(match r {
            Ok(i) if i.standard => i,
            _ => ScanInfo { raw: true, ..Default::default() },
        });
    }
    Ok(infos)
}

#[inline(always)]
fn store_idx<const NAT: bool>(k: usize) -> usize {
    if NAT { ZIGZAG[k] } else { k }
}

/// Block geometry of a scan: (component index, h, v) per scan component; non-interleaved
/// scans have one block per MCU.
pub(crate) fn scan_layout(j: &Jpeg, s: &Scan) -> Vec<(usize, usize, usize)> {
    let single = s.comps.len() == 1;
    s.comps.iter().map(|sc| if single { (sc.ci, 1, 1) } else { (sc.ci, j.comps[sc.ci].h, j.comps[sc.ci].v) }).collect()
}

/// Raw view of one component's coefficient arrays: chunks of a scan decoded in parallel write
/// disjoint blocks.
#[derive(Clone, Copy)]
pub(crate) struct CompPtr {
    pub(crate) blocks: *mut [i16; 64],
    pub(crate) masks: *mut u64,
    pub(crate) bw: usize,
    pub(crate) bh: usize,
    /// first held block row (see `Coefs`)
    y0: usize,
    /// index mask: all ones, or 0 when every block goes to a single scratch block
    keep: usize,
}
impl CompPtr {
    pub(crate) fn of(o: &mut Coefs) -> CompPtr {
        CompPtr { blocks: o.blocks.as_mut_ptr(), masks: o.masks.as_mut_ptr(), bw: o.bw, bh: o.bh, y0: o.y0, keep: !0 }
    }
    /// A component of `bw` x `bh` blocks whose blocks are decoded into `scratch` and dropped.
    fn discard(bw: usize, bh: usize, scratch: &mut ([i16; 64], u64)) -> CompPtr {
        CompPtr { blocks: &mut scratch.0, masks: &mut scratch.1, bw, bh, y0: 0, keep: 0 }
    }
    /// Index of block (bx, by), if it is held.
    #[inline(always)]
    fn index(&self, bx: usize, by: usize) -> Option<usize> {
        let y = by.wrapping_sub(self.y0);
        (bx < self.bw && y < self.bh).then_some((y * self.bw + bx) & self.keep)
    }
}
// SAFETY: only used to write blocks of disjoint MCUs from different threads
unsafe impl Send for CompPtr {}
unsafe impl Sync for CompPtr {}

/// Bit reader state: MSB-first, `cnt` valid bits at the top of `buf`, next byte at `pos`.
#[derive(Clone, Copy)]
struct Br {
    buf: u64,
    cnt: u32,
    pos: usize,
}
impl Br {
    /// Positioned at an arbitrary bit of `data`.
    fn at(data: &[u8], bit: usize) -> Br {
        let mut b = Br { buf: 0, cnt: 0, pos: bit / 8 };
        b.refill(data);
        b.buf <<= bit % 8;
        b.cnt -= (bit % 8) as u32;
        b
    }
    /// Unconditional (branchless) refill: afterwards cnt >= 56. Past the end of `data` it keeps
    /// reading its last 8 bytes (padding) while `used` keeps growing.
    #[inline(always)]
    fn refill(&mut self, data: &[u8]) {
        let p = self.pos.min(data.len() - 8);
        let w = u64::from_be_bytes(data[p..p + 8].try_into().unwrap());
        self.buf |= w >> self.cnt;
        self.pos += ((63 - self.cnt) >> 3) as usize;
        self.cnt |= 56;
    }
    /// absolute bit position
    #[inline(always)]
    fn used(&self) -> usize {
        self.pos * 8 - self.cnt as usize
    }
    /// Fast-table symbol: (run, value), or the slow path (on a `Bits` copy, so that the hot
    /// loop's state never has its address taken).
    #[inline(always)]
    fn sym(&mut self, data: &[u8], t: &Table, fast: &[Entry; 1 << FAST_BITS]) -> (u32, i32) {
        let e = fast[(self.buf >> (64 - FAST_BITS)) as usize];
        if e.len & !31 == 0 && e.len != 0 {
            self.buf <<= e.len;
            self.cnt -= e.len as u32;
            (e.run as u32, e.val as i32)
        } else {
            let mut b = Bits { buf: self.buf, cnt: self.cnt, pos: self.pos, data };
            let r = decode_slow(&mut b, t, e);
            (self.buf, self.cnt, self.pos) = (b.buf, b.cnt, b.pos);
            r
        }
    }
}

/// `n` bits of `data` from bit `at` (n <= 8)
fn bits_of(data: &[u8], at: usize, n: u32) -> u32 {
    let w = ((data[at / 8] as u32) << 8) | data.get(at / 8 + 1).copied().unwrap_or(0) as u32;
    ((w << (at % 8)) & 0xFFFF) >> (16 - n)
}

/// Canonical-encoding check results (see `decode_scan`).
#[derive(Default, Clone, Copy)]
struct Checks {
    bad: bool,
    pad_ones: bool,
    pad_zeros: bool,
}

/// Everything a range decoder needs about a scan.
struct Ctx<'a> {
    dct: Vec<Table>,
    act: Vec<Table>,
    layout: Vec<(usize, usize, usize)>,
    /// per component of the image
    comps: Vec<CompPtr>,
    data: &'a [u8],
    data_len: usize,
    segments: &'a [usize],
    ri: usize,
    nmcu: usize,
    mcux: usize,
}

/// How a range decoder stopped.
enum End {
    /// reached the requested end MCU
    Done,
    /// the hook asked to stop before the current MCU
    Hook,
    Trunc(Truncation),
    Err(&'static str),
}

impl Ctx<'_> {
    fn seg_end(&self, seg: usize) -> usize {
        if seg + 1 < self.segments.len() { self.segments[seg + 1] } else { self.data_len }
    }
    /// At a complete segment's end the bits up to the next byte boundary must be padding (all
    /// 1s, or all 0s consistently) and must end exactly at the segment end.
    fn check_segment_end(&self, used: usize, seg: usize, chk: &mut Checks) {
        let pad = ((8 - used % 8) % 8) as u32;
        if used.div_ceil(8) != self.seg_end(seg) {
            chk.bad = true;
        } else if pad > 0 {
            let bits = bits_of(self.data, used, pad);
            chk.pad_ones |= bits == (1 << pad) - 1;
            chk.pad_zeros |= bits == 0;
            chk.bad |= bits != 0 && bits != (1 << pad) - 1;
        }
    }

    /// DC difference of the next block.
    #[inline(always)]
    fn decode_dc(&self, br: &mut Br, si: usize) -> Result<i32, &'static str> {
        let dt = &self.dct[si];
        br.refill(self.data);
        let (run, diff) = br.sym(self.data, dt, &dt.fast);
        if run == BAD_RUN {
            return Err("bad DC code");
        }
        Ok(diff)
    }

    /// AC coefficients of the next block into `blk` (positions 1..64): returns the nonzero
    /// mask (bits 1..64) and whether the block ends other than a standard encoder ends it
    /// (right after the last nonzero coefficient, with EOB unless at 63).
    #[inline(always)]
    fn decode_ac<const NAT: bool>(
        &self,
        br: &mut Br,
        si: usize,
        blk: &mut [i16; 64],
    ) -> Result<(u64, bool), &'static str> {
        let at = &self.act[si];
        let data = self.data;
        let apair: &[Pair; 1 << FAST_BITS] = &at.pair;
        let mut mask = 0u64;
        let mut k = 1usize;
        let kend;
        // one refill gives >= 56 bits: enough for 4 fast symbols (<= 12 bits each); the slow
        // path re-establishes >= 40 bits itself.
        'ac: loop {
            br.refill(data);
            for _ in 0..4 {
                let p = apair[(br.buf >> (64 - FAST_BITS)) as usize];
                if p.len & !31 == 0 && p.len != 0 {
                    let buf0 = br.buf;
                    let cnt0 = br.cnt;
                    br.buf <<= p.len;
                    br.cnt -= p.len as u32;
                    // slot 1
                    if p.r1 == EOB_RUN {
                        kend = k;
                        break 'ac;
                    }
                    k += p.r1 as usize;
                    if k > 63 {
                        return Err("coefficient index overflow");
                    }
                    blk[store_idx::<NAT>(k)] = p.v1;
                    mask |= ((p.v1 != 0) as u64) << k;
                    k += 1;
                    if k == 64 {
                        // block full: only slot 1's bits belonged to this block
                        br.buf = buf0 << p.l1;
                        br.cnt = cnt0 - p.l1 as u32;
                        kend = 64;
                        break 'ac;
                    }
                    // slot 2 (no-op when empty: writes 0 at k, k unchanged)
                    if p.r2 == EOB_RUN {
                        kend = k;
                        break 'ac;
                    }
                    k += p.r2 as usize;
                    if k > 63 {
                        return Err("coefficient index overflow");
                    }
                    blk[store_idx::<NAT>(k)] = p.v2;
                    mask |= ((p.v2 != 0) as u64) << k;
                    k += (p.len != p.l1) as usize;
                } else {
                    let (run, val) = br.sym(data, at, &at.fast);
                    if run == EOB_RUN as u32 {
                        kend = k;
                        break 'ac;
                    }
                    k += run as usize;
                    if k > 63 {
                        return Err(if run == BAD_RUN { "bad AC code" } else { "coefficient index overflow" });
                    }
                    blk[store_idx::<NAT>(k)] = val as i16;
                    mask |= ((val != 0) as u64) << k;
                    k += 1;
                }
                if k == 64 {
                    kend = 64;
                    break 'ac;
                }
            }
        }
        let last_nz = if mask == 0 { 0 } else { 63 - mask.leading_zeros() as usize };
        Ok((mask, kend - 1 != last_nz))
    }

    /// Decode MCUs `*m..m1` into the coefficient arrays, from bit state `br` with DC
    /// predictions `pred` (MCU `*m` must start a restart interval or continue the decoding of
    /// MCU `*m - 1`). `hook(bit position, MCU)` is asked before every MCU whether to stop there.
    #[inline(always)]
    fn run<const NAT: bool>(
        &self,
        br: &mut Br,
        pred: &mut [i32; 4],
        m: &mut usize,
        m1: usize,
        chk: &mut Checks,
        hook: impl FnMut(usize, usize) -> bool,
    ) -> End {
        self.run_in::<NAT, false>(&self.comps, br, pred, m, m1, false, chk, hook)
    }

    /// `run_in` (zigzag order) without a hook.
    #[allow(clippy::too_many_arguments)]
    fn span<const REL: bool>(
        &self,
        comps: &[CompPtr],
        br: &mut Br,
        pred: &mut [i32; 4],
        m: &mut usize,
        m1: usize,
        cont: bool,
        chk: &mut Checks,
    ) -> End {
        self.run_in::<false, REL>(comps, br, pred, m, m1, cont, chk, |_, _| false)
    }

    /// `run` into the components `comps`. With `cont`, MCU `*m` continues an earlier run even
    /// where it starts a restart interval (whose marker is then taken here). With `REL`, the
    /// predictions start from an unknown offset: DC values are only checked against twice the
    /// limit, and the masks' DC bits are left for the caller to set.
    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    fn run_in<const NAT: bool, const REL: bool>(
        &self,
        comps: &[CompPtr],
        br: &mut Br,
        pred: &mut [i32; 4],
        m: &mut usize,
        m1: usize,
        cont: bool,
        chk: &mut Checks,
        mut hook: impl FnMut(usize, usize) -> bool,
    ) -> End {
        let first = *m;
        let ri = self.ri;
        let lim = if REL { 2 * DC_LIMIT } else { DC_LIMIT };
        let mut mcu_end = br.used(); // bit position after the last complete MCU
        while *m < m1 {
            let mm = *m;
            if (mm != first || cont) && mm.is_multiple_of(ri) {
                self.check_segment_end(br.used(), mm / ri - 1, chk);
                match self.segments.get(mm / ri) {
                    Some(&p) => *br = Br { buf: 0, cnt: 0, pos: p },
                    // data (and markers) ended in the middle of the scan
                    None => return End::Trunc(Truncation { mcus: mm, bits: mcu_end, ..Default::default() }),
                }
                *pred = [0; 4];
                mcu_end = br.used();
            }
            if hook(br.used(), mm) {
                return End::Hook;
            }
            let (mx, my) = (mm % self.mcux, mm / self.mcux);
            // decode one MCU
            let r: Result<(), &'static str> = 'mcu: {
                for (si, &(ci, ch, cv)) in self.layout.iter().enumerate() {
                    let o = comps[ci];
                    for v in 0..cv {
                        for h in 0..ch {
                            let (bx, by) = (mx * ch + h, my * cv + v);
                            let Some(bi) = o.index(bx, by) else {
                                break 'mcu Err("block outside the component");
                            };
                            // SAFETY: bi is within the component's arrays; MCUs of concurrent
                            // ranges are disjoint
                            let blk = unsafe { &mut *o.blocks.add(bi) };
                            *blk = [0; 64];
                            let diff = match self.decode_dc(br, si) {
                                Ok(d) => d,
                                Err(e) => break 'mcu Err(e),
                            };
                            pred[si] += diff;
                            if pred[si].abs() > lim {
                                break 'mcu Err("DC out of range");
                            }
                            blk[0] = pred[si] as i16;
                            let (mask, bad) = match self.decode_ac::<NAT>(br, si, blk) {
                                Ok(x) => x,
                                Err(e) => break 'mcu Err(e),
                            };
                            chk.bad |= bad;
                            unsafe { *o.masks.add(bi) = mask | (pred[si] != 0) as u64 };
                        }
                    }
                }
                Ok(())
            };
            let used = br.used();
            let seg = mm / ri;
            // An MCU that reads past the end of the segment's data (or fails there) means the
            // data was truncated: keep the complete MCUs, store the remaining bits verbatim.
            if used > self.seg_end(seg) * 8 || (r.is_err() && used + 64 > self.seg_end(seg) * 8) {
                if seg + 1 < self.segments.len() {
                    return End::Err("restart segment overrun");
                }
                let boundary = mm > 0 && mm.is_multiple_of(ri);
                return End::Trunc(Truncation { mcus: mm, bits: mcu_end, boundary, ..Default::default() });
            }
            if let Err(e) = r {
                return End::Err(e);
            }
            mcu_end = used;
            *m += 1;
        }
        End::Done
    }
}

/// Zero the blocks of MCUs `from..` of a scan (not decoded because the data was truncated).
fn clear_mcus(s: &Scan, layout: &[(usize, usize, usize)], from: usize, out: &mut [Coefs]) {
    for m in from..s.mcux * s.mcuy {
        let (mx, my) = (m % s.mcux, m / s.mcux);
        for &(ci, ch, cv) in layout {
            let o = &mut out[ci];
            for v in 0..cv {
                for h in 0..ch {
                    let (bx, by) = (mx * ch + h, my * cv + v);
                    if bx < o.bw && by < o.bh {
                        o.blocks[by * o.bw + bx] = [0; 64];
                        o.masks[by * o.bw + bx] = 0;
                    }
                }
            }
        }
    }
}

/// Decode one sequential scan, checking on the way every degree of freedom an encoder has
/// beyond the coefficients (see the module documentation). Large scans are decoded in parallel
/// chunks: whole restart intervals if there are any, otherwise by speculative decoding (see
/// `decode_speculative`); the outcome is identical to decoding serially.
fn decode_scan<const NAT: bool>(j: &Jpeg, s: &Scan, clean: &Clean, out: &mut [Coefs]) -> Result<ScanInfo, Error> {
    let mut dct = Vec::new();
    let mut act = Vec::new();
    for sc in &s.comps {
        let (Some(dc), Some(ac)) = (&sc.dc, &sc.ac) else { return err("missing Huffman table") };
        dct.push(Table::new(dc, false)?);
        act.push(Table::new(ac, true)?);
    }
    let layout = scan_layout(j, s);
    let nmcu = s.mcux * s.mcuy;
    let ri = if s.ri > 0 { s.ri } else { nmcu };
    let nseg = nmcu.div_ceil(ri);
    let data_len = clean.data.len() - PAD;
    let comps = out.iter_mut().map(CompPtr::of).collect();
    let ctx =
        Ctx { dct, act, layout, comps, data: &clean.data, data_len, segments: &clean.segments, ri, nmcu, mcux: s.mcux };
    let mut info = ScanInfo { standard: clean.markers_ok, ..Default::default() };
    // segments beyond the expected ones must be empty (RSTs after the last MCU)
    if clean.segments.len() > nseg {
        if clean.segments[nseg..].iter().all(|&p| p == data_len) {
            info.trailing_rst = (clean.segments.len() - nseg) as u32;
        } else {
            info.standard = false;
        }
    }
    let blocks_per_mcu: usize = ctx.layout.iter().map(|&(_, h, v)| h * v).sum();
    let chunks = if crate::par::current_num_threads() > 1 {
        (nmcu * blocks_per_mcu / CHUNK_BLOCKS).min(crate::par::current_num_threads() * 4)
    } else {
        1
    };
    let serial = |ctx: &Ctx| -> ScanResult {
        let mut chk = Checks::default();
        let (mut br, mut pred, mut m) = (Br { buf: 0, cnt: 0, pos: 0 }, [0; 4], 0);
        match ctx.run::<NAT>(&mut br, &mut pred, &mut m, nmcu, &mut chk, |_, _| false) {
            End::Done => {
                ctx.check_segment_end(br.used(), nseg - 1, &mut chk);
                Ok((chk, None))
            }
            End::Trunc(t) => Ok((chk, Some(t))),
            End::Err(e) => err(e),
            End::Hook => unreachable!(),
        }
    };
    let mut r = None;
    if chunks >= 2 && nseg > 1 && clean.segments.len() >= nseg {
        r = decode_intervals::<NAT>(&ctx, chunks);
    } else if chunks >= 2 && clean.segments.len() == 1 {
        r = decode_speculative::<NAT>(&ctx, chunks);
    }
    if let Some(p) = &r
        && std::env::var_os("JRC_VERIFY_PARALLEL").is_some()
    {
        // test oracle: the parallel outcome must equal the serial one
        let par: Vec<(Vec<[i16; 64]>, Vec<u64>)> = out.iter().map(|o| (o.blocks.clone(), o.masks.clone())).collect();
        let ctx2 = Ctx { comps: out.iter_mut().map(CompPtr::of).collect(), ..ctx };
        let ser = serial(&ctx2);
        let same_res = match (p, &ser) {
            (Ok((a, ta)), Ok((b, tb))) => (a.bad, a.pad_ones, a.pad_zeros, ta) == (b.bad, b.pad_ones, b.pad_zeros, tb),
            (Err(_), Err(_)) => true,
            _ => false,
        };
        assert!(same_res, "parallel scan decoding: outcome differs from serial");
        if ser.is_ok() {
            for (o, (b, m)) in out.iter().zip(&par) {
                assert!(o.blocks == *b && o.masks == *m, "parallel scan decoding: coefficients differ from serial");
            }
        }
        return finish(s, &ctx2.layout, out, clean, info, ser);
    }
    let r = match r {
        Some(x) => x,
        None => serial(&ctx),
    };
    finish(s, &ctx.layout, out, clean, info, r)
}

fn finish(
    s: &Scan,
    layout: &[(usize, usize, usize)],
    out: &mut [Coefs],
    clean: &Clean,
    mut info: ScanInfo,
    r: ScanResult,
) -> Result<ScanInfo, Error> {
    let (chk, trunc) = r?;
    let data_len = clean.data.len() - PAD;
    if let Some(mut t) = trunc {
        clear_mcus(s, layout, t.mcus, out);
        if info.trailing_rst > 0 {
            info.standard = false;
        }
        t.tail = clean.data[t.bits / 8..data_len].to_vec();
        info.truncated = Some(t);
    }
    if chk.bad || (chk.pad_ones && chk.pad_zeros) {
        info.standard = false;
    }
    info.pad_zero = chk.pad_zeros && !chk.pad_ones;
    Ok(info)
}

type ScanResult = Result<(Checks, Option<Truncation>), Error>;

/// Restart intervals in parallel: chunks of whole intervals. Only for scans whose RST markers
/// are all present; a truncation can then only happen in the last interval (last chunk).
fn decode_intervals<const NAT: bool>(c: &Ctx, chunks: usize) -> Option<ScanResult> {
    use crate::par::prelude::*;
    let nseg = c.nmcu.div_ceil(c.ri);
    let k = chunks.min(nseg);
    let res: Vec<(Checks, End)> = (0..k)
        .into_par_iter()
        .map(|i| {
            let (s0, s1) = (i * nseg / k, (i + 1) * nseg / k);
            let mut chk = Checks::default();
            let mut br = Br { buf: 0, cnt: 0, pos: c.segments[s0] };
            let (mut pred, mut m) = ([0; 4], s0 * c.ri);
            let e = c.run::<NAT>(&mut br, &mut pred, &mut m, (s1 * c.ri).min(c.nmcu), &mut chk, |_, _| false);
            if let End::Done = e {
                c.check_segment_end(br.used(), s1 - 1, &mut chk);
            }
            (chk, e)
        })
        .collect();
    let mut all = Checks::default();
    let mut trunc = None;
    for (chk, e) in res {
        all.bad |= chk.bad;
        all.pad_ones |= chk.pad_ones;
        all.pad_zeros |= chk.pad_zeros;
        match e {
            End::Done => {}
            End::Trunc(t) => trunc = Some(t),
            End::Err(e) => return Some(err(e)),
            End::Hook => unreachable!(),
        }
    }
    Some(Ok((all, trunc)))
}

/// An MCU start seen by a speculative decoder.
#[derive(Clone, Copy)]
struct Rec {
    /// bit position
    bit: usize,
    /// index of the MCU's first block in the chunk's block buffer
    blk: u32,
    /// blocks failing the canonical-ending check before this MCU (within the chunk)
    bad: u32,
    /// running sums of the DC differences per scan component (within the chunk)
    dc: [i32; 4],
}

/// Output of a speculative decoder: blocks in decoding order (DC as differences) and the MCU
/// starts, as runs: a run ends where its decoding failed (`failed`) or at the first MCU start
/// past the chunk (its last record).
#[derive(Default)]
struct SpecOut {
    blocks: Vec<[i16; 64]>,
    masks: Vec<u64>,
    recs: Vec<Rec>,
    /// per run: (index of its last record, failed)
    runs: Vec<(usize, bool)>,
}

thread_local! {
    static SPEC: std::cell::RefCell<Vec<SpecOut>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Speculative decoding of chunk `[from, to)` (byte offsets): starting at `from` as if an MCU
/// started there, recording every MCU start. Huffman codes resynchronise quickly, so the
/// decoding soon coincides with the true one (same bit position at an MCU start); a decoding
/// error means the guess was wrong so far, and decoding restarts after it.
fn speculate<const NAT: bool>(c: &Ctx, from: usize, to: usize, o: &mut SpecOut) {
    let bpm: usize = c.layout.iter().map(|&(_, h, v)| h * v).sum();
    let (stop, limit) = (to * 8, c.data_len * 8);
    o.recs.clear();
    o.runs.clear();
    let mut nb = 0usize;
    let mut bit = from * 8;
    'runs: loop {
        let mut br = Br::at(c.data, bit);
        let (mut bad, mut dc) = (0u32, [0i32; 4]);
        loop {
            let at = br.used();
            o.recs.push(Rec { bit: at, blk: nb as u32, bad, dc });
            if at >= stop || at > limit {
                o.runs.push((o.recs.len() - 1, false));
                break 'runs;
            }
            if nb + bpm > o.blocks.len() {
                let n = (o.blocks.len() * 2).max(nb + bpm).max(1024);
                o.blocks.resize(n, [0; 64]);
                o.masks.resize(n, 0);
            }
            let r: Result<(), &'static str> = 'mcu: {
                let mut b = nb;
                for (si, &(_, ch, cv)) in c.layout.iter().enumerate() {
                    for _ in 0..ch * cv {
                        let blk = &mut o.blocks[b];
                        *blk = [0; 64];
                        let diff = match c.decode_dc(&mut br, si) {
                            Ok(d) => d,
                            Err(e) => break 'mcu Err(e),
                        };
                        blk[0] = diff as i16; // |diff| < 2^15
                        dc[si] = dc[si].wrapping_add(diff);
                        let (mask, bd) = match c.decode_ac::<NAT>(&mut br, si, blk) {
                            Ok(x) => x,
                            Err(e) => break 'mcu Err(e),
                        };
                        o.masks[b] = mask;
                        bad += bd as u32;
                        b += 1;
                    }
                }
                Ok(())
            };
            if r.is_err() {
                o.runs.push((o.recs.len() - 1, true));
                // restart after the failure
                bit = br.used().max(at + 1);
                if bit >= stop {
                    break 'runs;
                }
                continue 'runs;
            }
            nb += bpm;
        }
    }
}

/// Scans without restart intervals: chunks of the data are decoded speculatively in parallel
/// (the first one for real), then the true decoding is followed serially across each chunk
/// boundary until it meets an MCU start the next chunk's decoder recorded; from there on that
/// decoder's blocks are the true ones and are copied into place (DC differences summed).
/// Returns None (decode serially) whenever anything is out of the ordinary.
fn spec_dbg(why: &str) {
    if std::env::var_os("JRC_DEBUG_SPEC").is_some() {
        eprintln!("speculative decoding fell back: {why}");
    }
}

fn decode_speculative<const NAT: bool>(c: &Ctx, chunks: usize) -> Option<ScanResult> {
    use crate::par::prelude::*;
    let bpm: usize = c.layout.iter().map(|&(_, h, v)| h * v).sum();
    let bounds: Vec<usize> = (0..=chunks).map(|i| i * c.data_len / chunks).collect();
    let mut outs: Vec<SpecOut> = SPEC.with(|s| std::mem::take(&mut *s.borrow_mut()));
    outs.resize_with(chunks, SpecOut::default);
    let t0 = std::time::Instant::now();
    // chunk 0 for real (until the first MCU start in chunk 1), the others speculatively
    let ((mut chk, mut br, mut pred, mut m, e0), ()) = crate::par::join(
        || {
            let mut chk = Checks::default();
            let (mut br, mut pred, mut m) = (Br { buf: 0, cnt: 0, pos: 0 }, [0; 4], 0);
            let e = c.run::<NAT>(&mut br, &mut pred, &mut m, c.nmcu, &mut chk, |b, _| b >= bounds[1] * 8);
            (chk, br, pred, m, e)
        },
        || outs[1..].par_iter_mut().enumerate().for_each(|(i, o)| speculate::<NAT>(c, bounds[i + 1], bounds[i + 2], o)),
    );
    let t1 = std::time::Instant::now();
    let mut t2 = t1;
    let result = (|| {
        if !matches!(e0, End::Hook | End::Done) {
            {
                spec_dbg("1");
                return None;
            }
        }
        // (chunk, first record, MCU index there, DC predictions there) of each synchronised chunk
        let mut synced: Vec<(usize, usize, usize, [i32; 4], usize)> = Vec::new();
        let mut i = 1;
        while m < c.nmcu {
            // follow the true decoding until it meets a record of chunk i (or a later chunk)
            let mut hit = None;
            let e = c.run::<NAT>(&mut br, &mut pred, &mut m, c.nmcu, &mut chk, |b, _| {
                while i < chunks {
                    let recs = &outs[i].recs;
                    let p = recs.partition_point(|r| r.bit < b);
                    if p < recs.len() && recs[p].bit == b {
                        hit = Some(p);
                        return true;
                    }
                    if i + 1 < chunks && b >= bounds[i + 1] * 8 {
                        i += 1;
                        continue;
                    }
                    return false;
                }
                false
            });
            match e {
                End::Done => break,
                End::Hook => {}
                _ => return None,
            }
            let r = hit.unwrap();
            let o = &outs[i];

            // the run containing record r
            let ri = o.runs.partition_point(|&(last, _)| last < r);
            let (last, failed) = o.runs[ri];
            // MCUs r..last of the chunk are the true MCUs m..; only those before the scan end
            let n = (last - r).min(c.nmcu - m);
            if failed && m + (last - r) < c.nmcu {
                {
                    spec_dbg("2");
                    return None;
                } // the true decoding fails there
            }
            synced.push((i, r, m, pred, n));
            let (rr, re) = (&o.recs[r], &o.recs[r + n]);
            for si in 0..4 {
                pred[si] = pred[si].wrapping_add(re.dc[si].wrapping_sub(rr.dc[si]));
            }
            chk.bad |= re.bad != rr.bad;
            m += n;
            if m >= c.nmcu {
                // the scan ends inside this chunk's records: check the end as `run` would
                if re.bit > c.data_len * 8 {
                    {
                        spec_dbg("3");
                        return None;
                    } // truncated: leave it to the serial decoder
                }
                br = Br::at(c.data, re.bit);
                break;
            }
            br = Br::at(c.data, re.bit);
            i += 1;
            if i >= chunks {
                {
                    spec_dbg("4");
                    return None;
                }
            }
        }
        c.check_segment_end(br.used(), 0, &mut chk);
        t2 = std::time::Instant::now();
        // copy the synchronised chunks' blocks into place
        let ok = synced.par_iter().all(|&(i, r, m0, pred0, n)| {
            let o = &outs[i];
            let mut pred = pred0;
            let mut b = o.recs[r].blk as usize;
            for m in m0..m0 + n {
                let (mx, my) = (m % c.mcux, m / c.mcux);
                for (si, &(ci, ch, cv)) in c.layout.iter().enumerate() {
                    let p = c.comps[ci];
                    for v in 0..cv {
                        for h in 0..ch {
                            let (bx, by) = (mx * ch + h, my * cv + v);
                            if bx >= p.bw || by >= p.bh {
                                return false;
                            }
                            let mut blk = o.blocks[b];
                            pred[si] += blk[0] as i32;
                            if pred[si].abs() > DC_LIMIT {
                                return false;
                            }
                            blk[0] = pred[si] as i16;
                            let bi = by * p.bw + bx;
                            // SAFETY: within the component; chunks cover disjoint MCUs
                            unsafe {
                                *p.blocks.add(bi) = blk;
                                *p.masks.add(bi) = o.masks[b] | (pred[si] != 0) as u64;
                            }
                            b += 1;
                        }
                    }
                }
            }
            debug_assert_eq!(b, o.recs[r].blk as usize + n * bpm);
            true
        });
        ok.then_some(Ok((chk, None)))
    })();
    if outs.iter().all(|o| o.blocks.len() * 136 <= KEEP_BYTES) {
        SPEC.with(|s| *s.borrow_mut() = outs);
    }
    if std::env::var_os("JRC_DEBUG_SPEC").is_some() {
        let ms = |a: std::time::Instant, b: std::time::Instant| (b - a).as_secs_f64() * 1e3;
        eprintln!(
            "speculative: {chunks} chunks, decode {:.2} ms, sync {:.2} ms, copy {:.2} ms",
            ms(t0, t1),
            ms(t1, t2),
            ms(t2, std::time::Instant::now())
        );
    }
    result
}

// ---------------------------------------------------------------- stripe-wise decoding

/// Every component of the image, decoded into `scratch` and dropped.
fn discard_all(j: &Jpeg, scratch: &mut ([i16; 64], u64)) -> Vec<CompPtr> {
    j.comps.iter().map(|c| CompPtr::discard(c.bw, c.bh, scratch)).collect()
}

/// Decode a sequential scan that codes every component stripe by stripe, so that only a few
/// stripes' coefficients are held at a time (zigzag order, with nonzero masks): the MCUs
/// `ranges[k]` (whole MCU rows, in order, covering the scan) are decoded into the buffers
/// `take(k)` gives, which hold exactly their rows, and handed on by `put(k, buffers)`, in any
/// order and from any thread. A truncated scan is handled as `decode_scan` does (the blocks
/// after the truncation are zero). As soon as the scan is known not to be reproducible (it
/// fails to decode, or fails a check), the rest is left undecoded and the info returned says
/// so (`standard` false), as decoding it whole would.
/// Restart intervals let the stripes be decoded in parallel right away; without them their
/// starting points are first located as by speculative decoding (see `locate`). Anything
/// unusual there returns None: the caller is to start over with `serial_only`, which always
/// gives the outcome.
pub(crate) fn decode_striped(
    j: &Jpeg,
    s: &Scan,
    clean: &Clean,
    ranges: &[(usize, usize)],
    serial_only: bool,
    take: impl Fn(usize) -> Vec<Coefs> + Sync,
    put: impl Fn(usize, Vec<Coefs>) + Sync,
) -> Option<ScanInfo> {
    use crate::par::prelude::*;
    let rejected = ScanInfo { standard: false, ..Default::default() };
    let mut dct = Vec::new();
    let mut act = Vec::new();
    for sc in &s.comps {
        let (Some(dc), Some(ac)) = (&sc.dc, &sc.ac) else { return Some(rejected) };
        let (Ok(dc), Ok(ac)) = (Table::new(dc, false), Table::new(ac, true)) else { return Some(rejected) };
        dct.push(dc);
        act.push(ac);
    }
    let layout = scan_layout(j, s);
    let nmcu = s.mcux * s.mcuy;
    let ri = if s.ri > 0 { s.ri } else { nmcu };
    let nseg = nmcu.div_ceil(ri);
    let data_len = clean.data.len() - PAD;
    let c = Ctx {
        dct,
        act,
        layout,
        comps: Vec::new(),
        data: &clean.data,
        data_len,
        segments: &clean.segments,
        ri,
        nmcu,
        mcux: s.mcux,
    };
    let mut info = ScanInfo { standard: clean.markers_ok, ..Default::default() };
    if clean.segments.len() > nseg {
        if clean.segments[nseg..].iter().all(|&p| p == data_len) {
            info.trailing_rst = (clean.segments.len() - nseg) as u32;
        } else {
            info.standard = false;
        }
    }
    if !info.standard {
        return Some(info);
    }
    let failed = |chk: &Checks| chk.bad || (chk.pad_ones && chk.pad_zeros);
    let ptrs = |b: &mut Vec<Coefs>| -> Vec<CompPtr> { b.iter_mut().map(CompPtr::of).collect() };
    let done = |e: End| matches!(e, End::Done);
    // the outcome, None where the scan fails to decode
    let serial = || -> Option<(Checks, Option<Truncation>)> {
        let mut chk = Checks::default();
        let (mut br, mut pred, mut m) = (Br { buf: 0, cnt: 0, pos: 0 }, [0; 4], 0);
        for (k, &(m0, m1)) in ranges.iter().enumerate() {
            debug_assert_eq!(m, m0);
            let mut b = take(k);
            match c.span::<false>(&ptrs(&mut b), &mut br, &mut pred, &mut m, m1, m0 > 0, &mut chk) {
                End::Done => {
                    if m1 == nmcu {
                        c.check_segment_end(br.used(), nseg - 1, &mut chk);
                    }
                    // with the verdict known, the stripe needs no modelling
                    if failed(&chk) {
                        return Some((chk, None));
                    }
                    put(k, b);
                }
                End::Trunc(t) => {
                    // the blocks from the truncation on are zero
                    clear_held(s, &c.layout, t.mcus, m1, &mut b);
                    put(k, b);
                    for k in k + 1..ranges.len() {
                        let mut b = take(k);
                        for o in &mut b {
                            o.blocks.fill([0; 64]);
                            o.masks.fill(0);
                        }
                        put(k, b);
                    }
                    return Some((chk, Some(t)));
                }
                _ => return None,
            }
        }
        Some((chk, None))
    };
    // each stripe from the start of its restart interval (the MCUs before it are skipped)
    let intervals = || -> Option<Checks> {
        let res: Vec<Option<Checks>> = ranges
            .par_iter()
            .enumerate()
            .map(|(k, &(m0, m1))| {
                let mut chk = Checks::default();
                let seg = m0 / ri;
                let mut br = Br { buf: 0, cnt: 0, pos: c.segments[seg] };
                let (mut pred, mut m) = ([0; 4], seg * ri);
                if m < m0 {
                    let mut scratch = ([0; 64], 0);
                    let skip = discard_all(j, &mut scratch);
                    if !done(c.span::<false>(&skip, &mut br, &mut pred, &mut m, m0, false, &mut chk)) {
                        return None;
                    }
                }
                let mut b = take(k);
                if !done(c.span::<false>(&ptrs(&mut b), &mut br, &mut pred, &mut m, m1, false, &mut chk)) {
                    return None;
                }
                if m1 == nmcu || m1.is_multiple_of(ri) {
                    c.check_segment_end(br.used(), (m1 - 1) / ri, &mut chk);
                }
                put(k, b);
                Some(chk)
            })
            .collect();
        res.into_iter().try_fold(Checks::default(), |a, x| x.map(|x| a | x))
    };
    // windows of stripes decoded in parallel from their located starts, with DC predictions
    // relative to each stripe's start, then rebased in order
    let located = |bits: Vec<usize>| -> Option<Checks> {
        let w = crate::par::current_num_threads();
        let mut chk = Checks::default();
        let mut end = [0i32; 4]; // predictions after the previous stripe
        for (wi, win) in ranges.chunks(w).enumerate() {
            // per stripe: its buffers, the predictions after it (relative), its checks
            type Dec = Option<(Vec<Coefs>, [i32; 4], Checks)>;
            let dec: Vec<Dec> = win
                .par_iter()
                .enumerate()
                .map(|(i, &(m0, m1))| {
                    let k = wi * w + i;
                    let mut chk = Checks::default();
                    let mut br = Br::at(c.data, bits[k]);
                    let (mut pred, mut m) = ([0; 4], m0);
                    let mut b = take(k);
                    if !done(c.span::<true>(&ptrs(&mut b), &mut br, &mut pred, &mut m, m1, false, &mut chk)) {
                        return None;
                    }
                    if m1 == nmcu {
                        c.check_segment_end(br.used(), nseg - 1, &mut chk);
                    }
                    Some((b, pred, chk))
                })
                .collect();
            let mut items = Vec::with_capacity(win.len());
            for d in dec {
                let (b, rel, x) = d?;
                items.push((b, end));
                for si in 0..4 {
                    end[si] += rel[si];
                }
                chk = chk | x;
            }
            if failed(&chk) {
                return Some(chk);
            }
            let ok = items.into_par_iter().enumerate().all(|(i, (mut b, off))| {
                rebase(&c.layout, &mut b, off) && {
                    put(wi * w + i, b);
                    true
                }
            });
            if !ok {
                return None;
            }
        }
        Some(chk)
    };
    let threads = crate::par::current_num_threads();
    let bpm: usize = c.layout.iter().map(|&(_, h, v)| h * v).sum();
    let chunks = (nmcu * bpm / CHUNK_BLOCKS).min(threads * 4);
    // Ok(outcome), Err(()) to start over serially
    let parallel = |chk: Option<Checks>| chk.map(|c| Some((c, None))).ok_or(());
    let (mode, r) = if serial_only || threads == 1 || ranges.len() == 1 {
        ("serial", Ok(serial()))
    } else if nseg > 1 && clean.segments.len() >= nseg {
        ("intervals", parallel(intervals()))
    } else if clean.segments.len() == 1 && chunks >= 2 {
        let starts: Vec<usize> = ranges.iter().map(|r| r.0).collect();
        match locate(&c, &discard_all(j, &mut ([0; 64], 0)), &starts, chunks) {
            Some(bits) => ("located", parallel(located(bits))),
            None => ("serial (not located)", Ok(serial())),
        }
    } else {
        ("serial", Ok(serial()))
    };
    if std::env::var_os("JRC_DEBUG_STRIPED").is_some() {
        let how = match &r {
            Ok(Some((chk, _))) if failed(chk) => "not reproducible",
            Ok(Some((_, None))) => "ok",
            Ok(Some((_, Some(_)))) => "ok, truncated",
            Ok(None) => "fails to decode",
            Err(()) => "start over serially",
        };
        eprintln!("striped decoding: {} stripes, {mode}: {how}", ranges.len());
    }
    let Some((chk, trunc)) = r.ok()? else { return Some(rejected) };
    if let Some(mut t) = trunc {
        if info.trailing_rst > 0 {
            info.standard = false;
        }
        t.tail = clean.data[t.bits / 8..data_len].to_vec();
        info.truncated = Some(t);
    }
    if failed(&chk) {
        info.standard = false;
    }
    info.pad_zero = chk.pad_zeros && !chk.pad_ones;
    Some(info)
}

/// Zero the blocks of MCUs `from..to` (of the stripe held in `bufs`).
fn clear_held(s: &Scan, layout: &[(usize, usize, usize)], from: usize, to: usize, bufs: &mut [Coefs]) {
    for m in from..to {
        let (mx, my) = (m % s.mcux, m / s.mcux);
        for &(ci, ch, cv) in layout {
            let o = &mut bufs[ci];
            for v in 0..cv {
                for h in 0..ch {
                    if let Some(bi) = o.index(mx * ch + h, my * cv + v) {
                        o.blocks[bi] = [0; 64];
                        o.masks[bi] = 0;
                    }
                }
            }
        }
    }
}

impl std::ops::BitOr for Checks {
    type Output = Checks;
    fn bitor(self, o: Checks) -> Checks {
        Checks { bad: self.bad | o.bad, pad_ones: self.pad_ones | o.pad_ones, pad_zeros: self.pad_zeros | o.pad_zeros }
    }
}

/// Add the DC offsets `off` (per scan component) to a stripe decoded with relative
/// predictions, setting the masks' DC bits; false if a DC value leaves the limit.
fn rebase(layout: &[(usize, usize, usize)], bufs: &mut [Coefs], off: [i32; 4]) -> bool {
    for (si, &(ci, _, _)) in layout.iter().enumerate() {
        let o = &mut bufs[ci];
        for (b, m) in o.blocks.iter_mut().zip(o.masks.iter_mut()) {
            let dc = b[0] as i32 + off[si];
            if dc.abs() > DC_LIMIT {
                return false;
            }
            b[0] = dc as i16;
            *m = (*m & !1) | (dc != 0) as u64;
        }
    }
    true
}

/// MCU starts seen by a speculative decoder (`skim`): bit positions relative to its chunk,
/// in runs as in `SpecOut`.
#[derive(Default)]
struct Skim {
    bits: Vec<u32>,
    runs: Vec<(usize, bool)>,
}

/// `speculate` keeping nothing but the MCU starts.
fn skim(c: &Ctx, from: usize, to: usize, o: &mut Skim) {
    let (base, stop, limit) = (from * 8, to * 8, c.data_len * 8);
    let mut blk = [0i16; 64];
    let mut bit = base;
    'runs: loop {
        let mut br = Br::at(c.data, bit);
        loop {
            let at = br.used();
            o.bits.push((at - base) as u32);
            if at >= stop || at > limit {
                o.runs.push((o.bits.len() - 1, false));
                break 'runs;
            }
            let r: Result<(), &'static str> = 'mcu: {
                for (si, &(_, ch, cv)) in c.layout.iter().enumerate() {
                    for _ in 0..ch * cv {
                        if let Err(e) = c.decode_dc(&mut br, si) {
                            break 'mcu Err(e);
                        }
                        if let Err(e) = c.decode_ac::<false>(&mut br, si, &mut blk) {
                            break 'mcu Err(e);
                        }
                    }
                }
                Ok(())
            };
            if r.is_err() {
                o.runs.push((o.bits.len() - 1, true));
                bit = br.used().max(at + 1);
                if bit >= stop {
                    break 'runs;
                }
                continue 'runs;
            }
        }
    }
}

/// Bit positions of the MCUs `starts` (ascending) of a scan without restart intervals, found
/// as `decode_speculative` synchronises its chunks, but keeping no blocks: the true decoding
/// (into `skip`) only follows each chunk boundary until it meets an MCU start that chunk's
/// speculative decoder saw. None where that fails.
fn locate(c: &Ctx, skip: &[CompPtr], starts: &[usize], chunks: usize) -> Option<Vec<usize>> {
    use crate::par::prelude::*;
    // positions within a chunk are kept as u32
    if c.data_len / chunks >= 1 << 28 {
        return None;
    }
    let bounds: Vec<usize> = (0..=chunks).map(|i| i * c.data_len / chunks).collect();
    let mut outs: Vec<Skim> = (0..chunks).map(|_| Skim::default()).collect();
    let mut found = Vec::with_capacity(starts.len());
    let mut chk = Checks::default();
    // `see(bit, MCU)` notes a start
    let see = |found: &mut Vec<usize>, b: usize, mm: usize| {
        if found.len() < starts.len() && starts[found.len()] == mm {
            found.push(b);
        }
    };
    // chunk 0 for real (until the first MCU start in chunk 1), the others speculatively
    let ((mut br, mut m, e0), ()) = crate::par::join(
        || {
            let (mut br, mut pred, mut m) = (Br { buf: 0, cnt: 0, pos: 0 }, [0; 4], 0);
            let e = c.run_in::<false, true>(skip, &mut br, &mut pred, &mut m, c.nmcu, false, &mut chk, |b, mm| {
                see(&mut found, b, mm);
                b >= bounds[1] * 8
            });
            (br, m, e)
        },
        || outs[1..].par_iter_mut().enumerate().for_each(|(i, o)| skim(c, bounds[i + 1], bounds[i + 2], o)),
    );
    if !matches!(e0, End::Hook | End::Done) {
        return None;
    }
    let mut i = 1;
    while m < c.nmcu {
        // follow the true decoding until it meets a record of chunk i (or a later chunk); the
        // predictions are relative to where it was last resumed
        let mut hit = None;
        let mut pred = [0; 4];
        let e = c.run_in::<false, true>(skip, &mut br, &mut pred, &mut m, c.nmcu, false, &mut chk, |b, mm| {
            see(&mut found, b, mm);
            while i < chunks {
                let base = bounds[i] * 8;
                let recs = &outs[i].bits;
                if b >= base && b - base <= u32::MAX as usize {
                    let p = recs.partition_point(|&r| (r as usize) < b - base);
                    if p < recs.len() && recs[p] as usize == b - base {
                        hit = Some(p);
                        return true;
                    }
                }
                if i + 1 < chunks && b >= bounds[i + 1] * 8 {
                    i += 1;
                    continue;
                }
                return false;
            }
            false
        });
        match e {
            End::Done => break,
            End::Hook => {}
            _ => return None,
        }
        let p = hit.unwrap();
        let o = &outs[i];
        // the run containing record p: its MCUs p..last are the true MCUs m..
        let (last, failed) = o.runs[o.runs.partition_point(|&(last, _)| last < p)];
        let n = (last - p).min(c.nmcu - m);
        if failed && m + (last - p) < c.nmcu {
            return None; // the true decoding fails there
        }
        let base = bounds[i] * 8;
        while found.len() < starts.len() && starts[found.len()] < m + n {
            found.push(base + o.bits[p + starts[found.len()] - m] as usize);
        }
        m += n;
        let end = base + o.bits[p + n] as usize;
        if m >= c.nmcu {
            if end > c.data_len * 8 {
                return None; // truncated: leave it to the serial decoder
            }
            break;
        }
        br = Br::at(c.data, end);
        i += 1;
        if i >= chunks {
            return None;
        }
    }
    (found.len() == starts.len()).then_some(found)
}

pub const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21,
    28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54,
    47, 55, 62, 63,
];

// ---------------------------------------------------------------- Huffman encoding (re-emission)

/// Canonical Huffman code per symbol: (code << 8) | length, 0 if the symbol has no code.
pub(crate) struct EncTable(pub(crate) [u32; 256]);

impl EncTable {
    pub(crate) fn new(spec: &HuffSpec) -> EncTable {
        let mut t = [0u32; 256];
        let mut code = 0u32;
        let mut k = 0;
        for l in 1..=16u32 {
            for _ in 0..spec.counts[l as usize - 1] {
                t[spec.vals[k] as usize] = code << 8 | l;
                code += 1;
                k += 1;
            }
            code <<= 1;
        }
        EncTable(t)
    }
}

/// Branchless MSB-first bit writer into an unstuffed buffer (stuffing is a separate pass).
pub(crate) struct BitOut {
    pub(crate) acc: u64, // low `n` bits are pending (n < 8 between puts)
    pub(crate) n: u32,
    pub(crate) buf: Vec<u8>,
    pub(crate) pos: usize,
}
impl BitOut {
    #[inline(always)]
    pub(crate) fn reserve(&mut self, bytes: usize) {
        if self.pos + bytes + 8 > self.buf.len() {
            self.buf.resize((self.buf.len() * 2).max(self.pos + bytes + 8), 0);
        }
    }
    /// 1 <= len <= 27; caller keeps 8 bytes of slack (`reserve`).
    #[inline(always)]
    pub(crate) fn put(&mut self, len: u32, bits: u32) {
        self.acc = (self.acc << len) | bits as u64;
        self.n += len;
        // SAFETY: reserve() keeps >= 8 bytes past pos
        unsafe {
            (self.buf.as_mut_ptr().add(self.pos) as *mut u64).write_unaligned((self.acc << (64 - self.n)).to_be())
        };
        self.pos += (self.n >> 3) as usize;
        self.n &= 7;
    }
    /// Pad to a byte boundary with 1-bits (or 0-bits).
    pub(crate) fn pad(&mut self, zero: bool) {
        if self.n > 0 {
            let p = 8 - self.n;
            self.put(p, if zero { 0 } else { (1 << p) - 1 });
        }
    }
}

/// Append `src` to `out` with 0xFF -> 0xFF 0x00 stuffing.
pub(crate) fn stuff_into(src: &[u8], out: &mut Vec<u8>) {
    let mut i = 0;
    while let Some(o) = memchr::memchr(0xFF, &src[i..]) {
        out.extend_from_slice(&src[i..i + o + 1]);
        out.push(0);
        i += o + 1;
    }
    out.extend_from_slice(&src[i..]);
}

/// JPEG magnitude category and the `size` extra bits of a value.
#[inline(always)]
pub(crate) fn size_bits(v: i32) -> (u32, u32) {
    let s = 32 - v.unsigned_abs().leading_zeros();
    // v - 1 when negative, without a branch: the sign is unpredictable, and the wasm32 backend
    // turns an `if` here into a branch that not every Wasm compiler folds back into a select
    let bits = v.wrapping_add(v >> 31) as u32 & ((1u32 << s) - 1);
    (s, bits)
}

/// Re-emit the entropy-coded data of scan `s` (including RST markers) from zigzag-ordered
/// blocks with nonzero masks. Bit-exact for scans that decoded with `ScanInfo::standard`.
/// Large scans are encoded as chunks of MCUs in parallel (see `encode_chunked`).
pub fn encode_scan(j: &Jpeg, s: &Scan, info: &ScanInfo, coefs: &[Coefs], out: &mut Vec<u8>) -> Result<(), Error> {
    out.clear();
    let mut dct = Vec::new();
    let mut act = Vec::new();
    for sc in &s.comps {
        let (Some(dc), Some(ac)) = (&sc.dc, &sc.ac) else { return err("missing Huffman table") };
        dct.push(EncTable::new(dc));
        act.push(EncTable::new(ac));
    }
    let layout = scan_layout(j, s);
    let nmcu = s.mcux * s.mcuy;
    let blocks_per_mcu: usize = layout.iter().map(|&(_, h, v)| h * v).sum();
    let chunks = (nmcu * blocks_per_mcu / CHUNK_BLOCKS).min(crate::par::current_num_threads() * 4);
    if chunks >= 2 && info.truncated.is_none() {
        return encode_chunked(s, info, coefs, out, &layout, &dct, &act, chunks);
    }
    let mut w = BitOut { acc: 0, n: 0, buf: take_scratch(), pos: 0 };
    let r = encode_scan_into(s, info, coefs, out, &layout, &dct, &act, &mut w);
    put_scratch(w.buf);
    r
}

/// Minimum chunk size (in blocks) for parallel Huffman encoding / decoding.
const CHUNK_BLOCKS: usize = 1 << 13;

thread_local! {
    static SCRATCH: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
}
/// A reusable byte buffer (contents unspecified) for the bit writers.
pub(crate) fn take_scratch() -> Vec<u8> {
    SCRATCH.with(|b| std::mem::take(&mut *b.borrow_mut()))
}
pub(crate) fn put_scratch(v: Vec<u8>) {
    if v.capacity() <= KEEP_BYTES {
        SCRATCH.with(|b| *b.borrow_mut() = v);
    }
}

/// Encode MCUs `m0..m1` (starting with DC predictions `pred`), with the padding and an RST
/// position (into `seg_ends`) before every MCU > m0 that starts a restart interval.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn encode_mcus(
    s: &Scan,
    coefs: &[Coefs],
    layout: &[(usize, usize, usize)],
    dct: &[EncTable],
    act: &[EncTable],
    w: &mut BitOut,
    (m0, m1): (usize, usize),
    pad_zero: bool,
    pred: &mut [i32; 4],
    seg_ends: &mut Vec<usize>,
) -> Result<(), Error> {
    let ri = if s.ri > 0 { s.ri } else { usize::MAX };
    for m in m0..m1 {
        if m > m0 && m % ri == 0 {
            w.pad(pad_zero);
            seg_ends.push(w.pos);
            *pred = [0; 4];
        }
        let (mx, my) = (m % s.mcux, m / s.mcux);
        for (si, &(ci, ch, cv)) in layout.iter().enumerate() {
            let o = &coefs[ci];
            for v in 0..cv {
                for h in 0..ch {
                    let Some(bi) = o.index(mx * ch + h, my * cv + v) else {
                        return err("block outside the component");
                    };
                    encode_block(w, &o.blocks[bi], o.masks[bi], &mut pred[si], &dct[si], &act[si])?;
                }
            }
        }
    }
    Ok(())
}

/// Code one block: its DC as the difference to `*pred`, then its AC coefficients.
#[inline(always)]
fn encode_block(
    w: &mut BitOut,
    blk: &[i16; 64],
    mask: u64,
    pred: &mut i32,
    dt: &EncTable,
    at: &EncTable,
) -> Result<(), Error> {
    let (dt, at) = (&dt.0, &at.0);
    let (eob, zrl) = (at[0x00], at[0xF0]);
    // a block needs at most 64 codes of <= 27 bits
    w.reserve(64 * 4);
    let dc = blk[0] as i32;
    let (sz, bits) = size_bits(dc - *pred);
    *pred = dc;
    let e = dt[sz as usize];
    if e == 0 {
        return err("DC symbol without Huffman code");
    }
    w.put((e & 255) + sz, (e >> 8) << sz | bits);
    let mut mk = mask & !1;
    let mut last = 0u32;
    while mk != 0 {
        let k = mk.trailing_zeros();
        mk &= mk - 1;
        let mut run = k - last - 1;
        while run >= 16 {
            if zrl == 0 {
                return err("ZRL without Huffman code");
            }
            w.put(zrl & 255, zrl >> 8);
            run -= 16;
        }
        let (sz, bits) = size_bits(blk[k as usize] as i32);
        let e = at[(run << 4 | sz) as usize];
        if e == 0 {
            return err("AC symbol without Huffman code");
        }
        w.put((e & 255) + sz, (e >> 8) << sz | bits);
        last = k;
    }
    if last < 63 {
        if eob == 0 {
            return err("EOB without Huffman code");
        }
        w.put(eob & 255, eob >> 8);
    }
    Ok(())
}

/// Parallel encoding of a (not truncated) scan: MCU ranges are encoded independently (DC
/// predictions are known from the coefficients), then each chunk's bits are shifted to their
/// position in the scan's bit stream (the first bits of a chunk's first byte come from the
/// previous chunk), stuffed, and concatenated. With restart intervals the chunks are whole
/// intervals and every chunk starts byte-aligned.
#[allow(clippy::too_many_arguments)]
fn encode_chunked(
    s: &Scan,
    info: &ScanInfo,
    coefs: &[Coefs],
    out: &mut Vec<u8>,
    layout: &[(usize, usize, usize)],
    dct: &[EncTable],
    act: &[EncTable],
    chunks: usize,
) -> Result<(), Error> {
    use crate::par::prelude::*;
    let nmcu = s.mcux * s.mcuy;
    let ri = if s.ri > 0 { s.ri } else { nmcu };
    let nseg = nmcu.div_ceil(ri);
    // chunk boundaries in MCUs: whole restart intervals if there are several
    let bounds: Vec<usize> = if nseg > 1 {
        let k = chunks.min(nseg);
        (0..=k).map(|i| (i * nseg / k * ri).min(nmcu)).collect()
    } else {
        (0..=chunks).map(|i| i * nmcu / chunks).collect()
    };
    struct Part {
        buf: Vec<u8>,
        nbits: usize,
        seg_ends: Vec<usize>,
    }
    let parts: Vec<Part> = bounds
        .par_windows(2)
        .map(|r| {
            let (m0, m1) = (r[0], r[1]);
            let mut pred = [0i32; 4];
            if m0 % ri != 0 {
                // the previous block of each scan component: the last one of MCU m0 - 1
                let (mx, my) = ((m0 - 1) % s.mcux, (m0 - 1) / s.mcux);
                for (si, &(ci, ch, cv)) in layout.iter().enumerate() {
                    let o = &coefs[ci];
                    let Some(bi) = o.index(mx * ch + ch - 1, my * cv + cv - 1) else {
                        return err("block outside the component");
                    };
                    pred[si] = o.blocks[bi][0] as i32;
                }
            }
            let mut w = BitOut { acc: 0, n: 0, buf: Vec::new(), pos: 0 };
            let mut seg_ends = Vec::new();
            encode_mcus(s, coefs, layout, dct, act, &mut w, (m0, m1), info.pad_zero, &mut pred, &mut seg_ends)?;
            if m1 < nmcu {
                // the chunk ends at a restart boundary (with restart intervals)
                if m1 % ri == 0 {
                    w.pad(info.pad_zero);
                    seg_ends.push(w.pos);
                }
            }
            w.reserve(16);
            // flush the pending bits into the buffer (zero-filled below them)
            let nbits = w.pos * 8 + w.n as usize;
            if w.n > 0 {
                w.buf[w.pos] = (w.acc << (8 - w.n)) as u8;
            }
            Ok(Part { buf: w.buf, nbits, seg_ends })
        })
        .collect::<Result<_, Error>>()?;
    if parts.iter().any(|p| p.nbits < 8) {
        // a chunk's first byte must only take bits from its predecessor
        return err("chunk too small");
    }
    let mut starts = Vec::with_capacity(parts.len() + 1);
    let mut total = 0;
    for p in &parts {
        starts.push(total);
        total += p.nbits;
    }
    let last = parts.len() - 1;
    // `len` (<= 25) bits of `buf` from bit `at` (MSB-first); buf has >= 4 bytes past them
    let bits_at = |buf: &[u8], at: usize, len: u32| {
        let v = u32::from_be_bytes(buf[at / 8..at / 8 + 4].try_into().unwrap());
        (v << (at % 8)) >> (32 - len)
    };
    let pieces: Vec<Vec<u8>> = (0..parts.len())
        .into_par_iter()
        .map(|k| {
            let p = &parts[k];
            let c = (starts[k] % 8) as u32;
            let n = c as usize + p.nbits;
            // the chunk's bits shifted right by c, behind the previous chunk's last c bits
            let shifted;
            let src: &[u8] = if c == 0 {
                &p.buf
            } else {
                let q = &parts[k - 1];
                let carry = bits_at(&q.buf, q.nbits - c as usize, c) as u64;
                let nb = n.div_ceil(8);
                let mut v = vec![0u8; nb + 8];
                v[0] = ((carry << (8 - c)) as u8) | (p.buf[0] >> c);
                let mut i = 1;
                while i < nb {
                    // bits [8i - c, 8i - c + 64) of the chunk
                    let y = u64::from_be_bytes(p.buf[i..i + 8].try_into().unwrap());
                    let word = (y >> c) | ((p.buf[i - 1] as u64) << (64 - c));
                    v[i..i + 8].copy_from_slice(&word.to_be_bytes());
                    i += 8;
                }
                shifted = v;
                &shifted
            };
            let nbytes = n / 8;
            let mut piece = Vec::with_capacity(nbytes + nbytes / 64 + 16);
            let mut from = 0;
            let mut r = bounds[k] / ri; // index of the next RST marker
            for &e in &p.seg_ends {
                debug_assert!(c == 0);
                stuff_into(&src[from..e], &mut piece);
                piece.extend_from_slice(&[0xFF, 0xD0 + (r & 7) as u8]);
                r += 1;
                from = e;
            }
            stuff_into(&src[from..nbytes], &mut piece);
            if k == last {
                // the final partial byte, padded
                if !n.is_multiple_of(8) {
                    let pad = 8 - (n % 8) as u32;
                    let b = src[nbytes] | if info.pad_zero { 0 } else { (1u8 << pad) - 1 };
                    stuff_into(&[b], &mut piece);
                }
                for _ in 0..info.trailing_rst {
                    piece.extend_from_slice(&[0xFF, 0xD0 + (r & 7) as u8]);
                    r += 1;
                }
            }
            piece
        })
        .collect();
    out.reserve(pieces.iter().map(|p| p.len()).sum());
    for p in &pieces {
        out.extend_from_slice(p);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn encode_scan_into(
    s: &Scan,
    info: &ScanInfo,
    coefs: &[Coefs],
    out: &mut Vec<u8>,
    layout: &[(usize, usize, usize)],
    dct: &[EncTable],
    act: &[EncTable],
    w: &mut BitOut,
) -> Result<(), Error> {
    let nmcu = s.mcux * s.mcuy;
    let mut seg_ends = Vec::new(); // unstuffed offsets where an RST marker follows
    let mut pred = [0i32; 4];
    let emit = info.truncated.as_ref().map_or(nmcu, |t| t.mcus.min(nmcu));
    encode_mcus(s, coefs, layout, dct, act, w, (0, emit), info.pad_zero, &mut pred, &mut seg_ends)?;
    match &info.truncated {
        None => w.pad(info.pad_zero),
        Some(t) => {
            if t.boundary {
                w.pad(info.pad_zero);
                seg_ends.push(w.pos);
            }
            // our bits must end exactly where the stored tail begins; the tail starts with the
            // (identical) partial byte, so it simply overwrites from there
            if w.pos * 8 + w.n as usize != t.bits {
                return err("truncation point mismatch");
            }
            w.reserve(t.tail.len());
            w.buf[w.pos..w.pos + t.tail.len()].copy_from_slice(&t.tail);
            w.pos += t.tail.len();
            w.n = 0;
        }
    }
    for _ in 0..info.trailing_rst {
        seg_ends.push(w.pos);
    }
    // stuffing + RST markers
    out.reserve(w.pos + w.pos / 64 + seg_ends.len() * 2 + 16);
    let mut start = 0;
    for (r, &end) in seg_ends.iter().enumerate() {
        stuff_into(&w.buf[start..end], out);
        out.extend_from_slice(&[0xFF, 0xD0 + (r as u8 & 7)]);
        start = end;
    }
    stuff_into(&w.buf[start..w.pos], out);
    Ok(())
}

// ---------------------------------------------------------------- stripe-wise re-emission

/// MSB-first bits: the first `n` bits of `buf`, followed by zero bits and >= 8 spare bytes.
#[derive(Default)]
struct BitStr {
    buf: Vec<u8>,
    n: usize,
}

/// (the bytes up to the bits' last and the 8 spare: not the room `BitOut` grew)
impl Wire for BitStr {
    fn put(&self, out: &mut Vec<u8>) {
        let b = self.kept();
        b.len().put(out);
        out.extend_from_slice(b);
        self.n.put(out);
    }

    fn size(&self) -> usize {
        8 + self.kept().len() + 8
    }

    fn get(b: &mut &[u8]) -> Option<Self> {
        let (buf, n) = Wire::get(b)?;
        Some(BitStr { buf, n })
    }
}

impl BitStr {
    fn of(mut w: BitOut) -> BitStr {
        w.reserve(16);
        if w.n > 0 {
            w.buf[w.pos] = (w.acc << (8 - w.n)) as u8;
            w.buf[w.pos + 1..w.pos + 9].fill(0);
        }
        BitStr { n: w.pos * 8 + w.n as usize, buf: w.buf }
    }
    /// The bytes of the bits, and 8 spare.
    fn kept(&self) -> &[u8] {
        &self.buf[..(self.n / 8 + 9).min(self.buf.len())]
    }

    /// The last `k` (<= 8) bits.
    fn tail(&self, k: u32) -> u32 {
        let at = self.n - k as usize;
        let v = u16::from_be_bytes([self.buf[at / 8], self.buf[at / 8 + 1]]) as u32;
        ((v << (at % 8)) & 0xFFFF) >> (16 - k)
    }
}

impl BitOut {
    fn new() -> BitOut {
        BitOut { acc: 0, n: 0, buf: Vec::new(), pos: 0 }
    }
    /// Append the bits of `src`.
    fn append(&mut self, src: &BitStr) {
        let nb = src.n / 8;
        self.reserve(nb + 8);
        let p = self.n;
        if p == 0 {
            self.buf[self.pos..self.pos + nb].copy_from_slice(&src.buf[..nb]);
        } else if nb > 0 {
            // whole bytes of `src` behind the p pending bits, 8 at a time
            let mut prev = self.acc & ((1 << p) - 1);
            for i in (0..nb).step_by(8) {
                let y = u64::from_be_bytes(src.buf[i..i + 8].try_into().unwrap());
                let word = (y >> p) | (prev << (64 - p));
                self.buf[self.pos + i..self.pos + i + 8].copy_from_slice(&word.to_be_bytes());
                prev = y;
            }
            self.acc = src.buf[nb - 1] as u64;
        }
        self.pos += nb;
        let r = (src.n % 8) as u32;
        if r > 0 {
            self.put(r, (src.buf[nb] >> (8 - r)) as u32);
        }
    }
}

/// A stripe's share of a scan (see `encode_striped`): MCUs `m0..m1`.
struct Part {
    /// MCU m0's blocks (in coding order), when its DC predictions continue the previous stripe
    head: Vec<([i16; 64], u64)>,
    /// the MCUs after the head up to the first restart boundary
    mid: BitStr,
    /// a restart boundary (padding, RST) follows head and mid
    brk: bool,
    /// the MCUs from the first restart boundary on: byte-aligned bits, and the offsets (into
    /// them) where RST markers follow
    body: Option<(BitStr, Vec<usize>)>,
    /// index of the first RST marker the part emits
    rst: usize,
    /// DC predictions after MCU m1 - 1
    end: [i32; 4],
}

/// As the result of a task (on WebAssembly).
impl Wire for Part {
    fn put(&self, out: &mut Vec<u8>) {
        self.head.put(out);
        self.mid.put(out);
        self.brk.put(out);
        self.body.put(out);
        (self.rst, self.end).put(out);
    }

    fn size(&self) -> usize {
        self.head.size() + self.mid.size() + 1 + self.body.size() + 8 + 16
    }

    fn get(b: &mut &[u8]) -> Option<Self> {
        let (head, mid) = (Wire::get(b)?, Wire::get(b)?);
        let (brk, body, rst, end) = Wire::get(b)?;
        Some(Part { head, mid, brk, body, rst, end })
    }
}

/// Re-emit a sequential scan that codes every component stripe by stripe, returning its bytes
/// in pieces: `fill(k, buffers)` puts the coefficients of stripe k (MCUs `ranges[k]`, whole MCU
/// rows, in order, covering the scan) into the buffers, and only a stripe per worker is held at
/// a time. Each stripe is coded on its own, but for its first MCU where that continues the
/// previous stripe's DC predictions: such heads are coded once all stripes are, then the parts'
/// bits are joined as in `encode_chunked`. A truncated scan ends as in `encode_scan_into` (the
/// stripes after its complete MCUs are not filled at all); it needs at least one such MCU.
pub(crate) fn encode_striped(
    j: &Jpeg,
    s: &Scan,
    info: &ScanInfo,
    ranges: &[(usize, usize)],
    fill: impl Fn(usize, &mut Vec<Coefs>) -> Result<(), Error> + Sync,
) -> Result<Vec<Vec<u8>>, Error> {
    use crate::par::prelude::*;
    let mut dct = Vec::new();
    let mut act = Vec::new();
    for sc in &s.comps {
        let (Some(dc), Some(ac)) = (&sc.dc, &sc.ac) else { return err("missing Huffman table") };
        dct.push(EncTable::new(dc));
        act.push(EncTable::new(ac));
    }
    let layout = scan_layout(j, s);
    let nmcu = s.mcux * s.mcuy;
    let ri = if s.ri > 0 { s.ri } else { nmcu };
    let nseg = nmcu.div_ceil(ri);
    let pad_zero = info.pad_zero;
    // MCUs up to `emit` are coded; a restart boundary (padding, RST) follows MCU m1 - 1 of a
    // part where `brk_at(m1)`
    let emit = info.truncated.as_ref().map_or(nmcu, |t| t.mcus.min(nmcu));
    if emit == 0 {
        return err("no complete MCU");
    }
    let brk_at = |m1: usize| match &info.truncated {
        Some(t) if m1 == emit => t.boundary,
        _ => m1 < nmcu && m1.is_multiple_of(ri),
    };
    let ranges: Vec<(usize, usize)> =
        ranges.iter().take_while(|r| r.0 < emit).map(|&(m0, m1)| (m0, m1.min(emit))).collect();
    let pool = std::sync::Mutex::new(Vec::<Vec<Coefs>>::new());
    let part = |k: usize| -> Result<Part, Error> {
        let (m0, m1) = ranges[k];
        let mut b = pool.lock().unwrap().pop().unwrap_or_default();
        fill(k, &mut b)?;
        let head = m0 % ri != 0;
        // the first restart boundary from m0 on
        let r1 = if head { (m0 / ri + 1) * ri } else { m0 };
        let mut pred = [0i32; 4];
        let mut part = Part { head: Vec::new(), mid: BitStr::default(), brk: false, body: None, rst: 0, end: [0; 4] };
        let mut ends = Vec::new();
        if head {
            let (mx, my) = (m0 % s.mcux, m0 / s.mcux);
            for (si, &(ci, ch, cv)) in layout.iter().enumerate() {
                let o = &b[ci];
                for v in 0..cv {
                    for h in 0..ch {
                        let Some(bi) = o.index(mx * ch + h, my * cv + v) else {
                            return err("block outside the component");
                        };
                        part.head.push((o.blocks[bi], o.masks[bi]));
                        pred[si] = o.blocks[bi][0] as i32;
                    }
                }
            }
            if m0 + 1 < r1.min(m1) {
                let mut w = BitOut::new();
                encode_mcus(s, &b, &layout, &dct, &act, &mut w, (m0 + 1, r1.min(m1)), pad_zero, &mut pred, &mut ends)?;
                part.mid = BitStr::of(w);
            }
            part.brk = r1 < m1 || (r1 == m1 && brk_at(m1));
            part.rst = r1 / ri - 1;
        } else {
            part.rst = m0 / ri;
        }
        if r1 < m1 {
            let mut w = BitOut::new();
            pred = [0; 4];
            encode_mcus(s, &b, &layout, &dct, &act, &mut w, (r1, m1), pad_zero, &mut pred, &mut ends)?;
            if brk_at(m1) {
                w.pad(pad_zero);
                ends.push(w.pos);
            }
            part.body = Some((BitStr::of(w), ends));
        }
        part.end = pred;
        // (a task's own, dropped before its result is made)
        if !cfg!(target_family = "wasm") {
            pool.lock().unwrap().push(b);
        }
        Ok(part)
    };
    // (as tasks on WebAssembly, where rayon has no threads)
    let parts: Vec<Part> = if cfg!(target_family = "wasm") {
        aqr_common::pool::par_map_apart(ranges.len(), ranges.len(), part).into_iter().collect::<Result<_, Error>>()?
    } else {
        (0..ranges.len()).into_par_iter().map(part).collect::<Result<_, Error>>()?
    };
    drop(pool);
    // the heads, and the bits (<= 7) each part's first byte takes from what precedes it
    let mut heads = Vec::with_capacity(parts.len());
    let mut carries = Vec::with_capacity(parts.len());
    let (mut pos, mut last8) = (0usize, 0u32);
    let push = |pos: &mut usize, last8: &mut u32, x: &BitStr| {
        let k = x.n.min(8) as u32;
        if k > 0 {
            *last8 = (*last8 << k) | x.tail(k);
            *pos += x.n;
        }
    };
    for k in 0..parts.len() {
        let p = &parts[k];
        carries.push((((pos % 8) as u32), last8 & ((1 << (pos % 8)) - 1)));
        let mut w = BitOut::new();
        if !p.head.is_empty() {
            let mut pred = parts[k - 1].end;
            let mut i = 0;
            for (si, &(_, ch, cv)) in layout.iter().enumerate() {
                for _ in 0..ch * cv {
                    let (blk, mask) = &p.head[i];
                    encode_block(&mut w, blk, *mask, &mut pred[si], &dct[si], &act[si])?;
                    i += 1;
                }
            }
        }
        let head = BitStr::of(w);
        push(&mut pos, &mut last8, &head);
        push(&mut pos, &mut last8, &p.mid);
        if p.brk {
            pos = pos.next_multiple_of(8);
        }
        if let Some((b, _)) = &p.body {
            debug_assert!(pos.is_multiple_of(8));
            push(&mut pos, &mut last8, b);
        }
        heads.push(head);
    }
    if let Some(t) = &info.truncated
        && pos != t.bits
    {
        // our bits must end exactly where the stored tail begins
        return err("truncation point mismatch");
    }
    let last = parts.len() - 1;
    // each part is dropped as soon as its piece is made
    parts
        .into_par_iter()
        .zip(heads)
        .zip(carries)
        .enumerate()
        .map(|(k, ((p, head), (c, carry)))| {
            let mut w = BitOut::new();
            let mut ends = Vec::new();
            if c > 0 {
                w.reserve(8);
                w.put(c, carry);
            }
            w.append(&head);
            w.append(&p.mid);
            if p.brk {
                w.pad(pad_zero);
                ends.push(w.pos);
            }
            if let Some((b, e)) = &p.body {
                let base = w.pos;
                w.append(b);
                ends.extend(e.iter().map(|x| base + x));
            }
            if k == last {
                match &info.truncated {
                    None => w.pad(pad_zero),
                    Some(t) => {
                        // the tail starts with the (identical) partial byte: it overwrites it
                        w.reserve(t.tail.len());
                        w.buf[w.pos..w.pos + t.tail.len()].copy_from_slice(&t.tail);
                        (w.pos, w.n) = (w.pos + t.tail.len(), 0);
                    }
                }
            }
            // the bytes up to the last whole one (a partial one is the next part's carry)
            let mut piece = Vec::with_capacity(w.pos + w.pos / 64 + 2 * ends.len() + 16);
            let mut from = 0;
            for (r, &e) in (p.rst..).zip(&ends) {
                stuff_into(&w.buf[from..e], &mut piece);
                piece.extend_from_slice(&[0xFF, 0xD0 + (r & 7) as u8]);
                from = e;
            }
            stuff_into(&w.buf[from..w.pos], &mut piece);
            if k == last {
                for t in 0..info.trailing_rst as usize {
                    piece.extend_from_slice(&[0xFF, 0xD0 + ((nseg - 1 + t) & 7) as u8]);
                }
            }
            Ok(piece)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::size_bits;

    #[test]
    fn size_bits_matches_definition() {
        for v in -(1i32 << 20)..(1 << 20) {
            let s = 32 - v.unsigned_abs().leading_zeros();
            let bits = if v < 0 { (v - 1) as u32 & ((1u32 << s) - 1) } else { v as u32 };
            assert_eq!(size_bits(v), (s, bits), "{v}");
        }
    }
}
