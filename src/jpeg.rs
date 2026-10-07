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

use std::fmt;

pub const FAST_BITS: u32 = 12;
// Bit budget of the unrolled AC loop: one refill guarantees 56 bits for 4 fast lookups, and the
// slow path leaves >= 56 - 16 = 40 bits for the (at most 3) lookups remaining in the group.
const _: () = assert!(4 * FAST_BITS <= 56 && 3 * FAST_BITS <= 40);

/// Largest DC magnitude accepted; keeps DC prediction residuals of downstream models in 16 bits.
pub const DC_LIMIT: i32 = (1 << 14) - 1;

#[derive(Debug)]
pub struct Error(pub String);
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
    let mut j = Jpeg { width: 0, height: 0, progressive: false, comps: Vec::new(), qt: [[0; 64]; 4], scans: Vec::new() };
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
            0xC0 | 0xC1 | 0xC2 => {
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
            0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => return err("unsupported SOF (lossless/arithmetic/hierarchical)"),
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
                    if tc == 0 { dc[th] = spec } else { ac[th] = spec }
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
                        qt_seen[tq][k] = if pq == 0 { byte(seg, p + 1 + k)? as u16 } else { be16(seg, p + 1 + 2 * k)? as u16 };
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
                    comps.push(ScanComp { ci, dc: dc[(t >> 4) as usize & 3].clone(), ac: ac[(t & 15) as usize & 3].clone() });
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
    if s == 0 { 0 } else if v < (1 << (s - 1)) { v as i32 - (1 << s) + 1 } else { v as i32 }
}

impl Table {
    /// Build decoding tables. Fails on tables a standard encoder cannot have produced or that
    /// break the bijection between symbols and codes: oversubscribed code space, duplicate
    /// symbols, AC symbols (r, 0) other than EOB / ZRL, sizes > 15 (AC) / > 15 (DC).
    pub fn new(spec: &HuffSpec, is_ac: bool) -> Result<Table, Error> {
        let mut seen = [false; 256];
        for &v in &spec.vals {
            let bad = if is_ac { (v & 15 == 0 && v != 0 && v != 0xF0) || v & 15 > 15 } else { v > 15 };
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
/// `masks` always in zigzag bit order.
pub struct Coefs {
    pub bw: usize,
    pub bh: usize,
    pub blocks: Vec<[i16; 64]>,
    pub masks: Vec<u64>,
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

pub static SLOW_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Slow path (code longer than FAST_BITS, or extra bits not fitting in the fast entry).
/// Operates on a `Bits` copy so the hot loop's scalars never have their address taken.
#[inline(never)]
#[cold]
fn decode_slow(b: &mut Bits, t: &Table, e: Entry) -> (u32, i32) {
    SLOW_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
    out.resize_with(j.comps.len(), || Coefs { bw: 0, bh: 0, blocks: Vec::new(), masks: Vec::new() });
    out.truncate(j.comps.len());
    for (o, c) in out.iter_mut().zip(&j.comps) {
        o.bw = c.bw;
        o.bh = c.bh;
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
pub fn decode_all(j: &Jpeg, d: &[u8], order: Order, out: &mut Vec<Coefs>, clean: &mut Clean) -> Result<Vec<ScanInfo>, Error> {
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
    let data = &clean.data[..];
    let data_len = data.len() - PAD; // real (destuffed) length
    let last = data.len() - 8;
    let mut info = ScanInfo { standard: clean.markers_ok, ..Default::default() };
    // segments beyond the expected ones must be empty (RSTs after the last MCU)
    if clean.segments.len() > nseg {
        if clean.segments[nseg..].iter().all(|&p| p == data_len) {
            info.trailing_rst = (clean.segments.len() - nseg) as u32;
        } else {
            info.standard = false;
        }
    }
    let seg_end = |seg: usize| if seg + 1 < clean.segments.len() { clean.segments[seg + 1] } else { data_len };
    // bit reader state as plain locals (MSB-first, `cnt` valid bits at the top of `buf`)
    let mut buf = 0u64;
    let mut cnt = 0u32;
    let mut pos = 0usize;
    // At a complete segment's end the bits up to the next byte boundary must be padding (all 1s,
    // or all 0s consistently) and must end exactly at the segment end.
    let mut bad = 0u64;
    let (mut pad_ones, mut pad_zeros) = (false, false);
    macro_rules! used_bits {
        () => {
            pos * 8 - cnt as usize // absolute bit position in `data`
        };
    }
    macro_rules! check_segment_end {
        ($seg:expr) => {{
            let used = used_bits!();
            let pad = (8 - used % 8) % 8;
            if used.div_ceil(8) != seg_end($seg) {
                bad |= 1;
            } else if pad > 0 {
                let bits = buf >> (64 - pad);
                pad_ones |= bits == (1 << pad) - 1;
                pad_zeros |= bits == 0;
                bad |= (bits != 0 && bits != (1 << pad) - 1) as u64;
            }
        }};
    }
    // unconditional (branchless) refill: afterwards cnt >= 56
    macro_rules! refill {
        () => {{
            let p = pos.min(last);
            let w = u64::from_be_bytes(data[p..p + 8].try_into().unwrap());
            buf |= w >> cnt;
            pos += ((63 - cnt) >> 3) as usize;
            cnt |= 56;
        }};
    }
    // returns (run, value)
    macro_rules! sym {
        ($t:expr, $fast:expr) => {{
            let e = $fast[(buf >> (64 - FAST_BITS)) as usize];
            if e.len & !31 == 0 && e.len != 0 {
                buf <<= e.len;
                cnt -= e.len as u32;
                (e.run as u32, e.val as i32)
            } else {
                let mut b = Bits { buf, cnt, pos, data };
                let r = decode_slow(&mut b, $t, e);
                buf = b.buf;
                cnt = b.cnt;
                pos = b.pos;
                r
            }
        }};
    }
    let mut seg = 0;
    let mut pred = [0i32; 4];
    let mut mcu_end = 0usize; // bit position after the last complete MCU
    for m in 0..nmcu {
        if m > 0 && m % ri == 0 {
            check_segment_end!(seg);
            seg += 1;
            match clean.segments.get(seg) {
                Some(&p) => pos = p,
                None => {
                    // data (and markers) ended in the middle of the scan
                    info.truncated = Some(Truncation { mcus: m, bits: mcu_end, ..Default::default() });
                    clear_mcus(s, &layout, m, out);
                    break;
                }
            }
            buf = 0;
            cnt = 0;
            pred = [0; 4];
            mcu_end = pos * 8;
        }
        let (mx, my) = (m % s.mcux, m / s.mcux);
        // decode one MCU; Err(msg) on a decoding error
        let r: Result<(), &'static str> = 'mcu: {
            for (si, &(ci, ch, cv)) in layout.iter().enumerate() {
                let o = &mut out[ci];
                let (dt, at) = (&dct[si], &act[si]);
                let (dfast, apair): (&[Entry; 1 << FAST_BITS], &[Pair; 1 << FAST_BITS]) = (&dt.fast, &at.pair);
                for v in 0..cv {
                    for h in 0..ch {
                        let (bx, by) = (mx * ch + h, my * cv + v);
                        if bx >= o.bw || by >= o.bh {
                            break 'mcu Err("block outside the component");
                        }
                        let bi = by * o.bw + bx;
                        let blk = &mut o.blocks[bi];
                        *blk = [0; 64];
                        refill!();
                        let (run, diff) = sym!(dt, dfast);
                        if run == BAD_RUN {
                            break 'mcu Err("bad DC code");
                        }
                        pred[si] += diff;
                        if pred[si].abs() > DC_LIMIT {
                            break 'mcu Err("DC out of range");
                        }
                        blk[0] = pred[si] as i16;
                        let mut mask = (pred[si] != 0) as u64;
                        let mut k = 1usize;
                        // position after the last decoded symbol: a standard encoder ends a block
                        // right after its last nonzero coefficient (EOB) or at 63
                        let kend;
                        // one refill gives >= 56 bits: enough for 4 fast symbols (<= 12 bits
                        // each); the slow path re-establishes >= 40 bits itself.
                        'ac: loop {
                            refill!();
                            for _ in 0..4 {
                                let p = apair[(buf >> (64 - FAST_BITS)) as usize];
                                if p.len & !31 == 0 && p.len != 0 {
                                    let buf0 = buf;
                                    let cnt0 = cnt;
                                    buf <<= p.len;
                                    cnt -= p.len as u32;
                                    // slot 1
                                    if p.r1 == EOB_RUN {
                                        kend = k;
                                        break 'ac;
                                    }
                                    k += p.r1 as usize;
                                    if k > 63 {
                                        break 'mcu Err("coefficient index overflow");
                                    }
                                    blk[store_idx::<NAT>(k)] = p.v1;
                                    mask |= ((p.v1 != 0) as u64) << k;
                                    k += 1;
                                    if k == 64 {
                                        // block full: only slot 1's bits belonged to this block
                                        buf = buf0 << p.l1;
                                        cnt = cnt0 - p.l1 as u32;
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
                                        break 'mcu Err("coefficient index overflow");
                                    }
                                    blk[store_idx::<NAT>(k)] = p.v2;
                                    mask |= ((p.v2 != 0) as u64) << k;
                                    k += (p.len != p.l1) as usize;
                                } else {
                                    let e = at.fast[(buf >> (64 - FAST_BITS)) as usize];
                                    let mut b = Bits { buf, cnt, pos, data };
                                    let (run, val) = decode_slow(&mut b, at, e);
                                    buf = b.buf;
                                    cnt = b.cnt;
                                    pos = b.pos;
                                    if run == EOB_RUN as u32 {
                                        kend = k;
                                        break 'ac;
                                    }
                                    k += run as usize;
                                    if k > 63 {
                                        break 'mcu Err(if run == BAD_RUN { "bad AC code" } else { "coefficient index overflow" });
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
                        let ac = mask & !1;
                        let last_nz = if ac == 0 { 0 } else { 63 - ac.leading_zeros() as usize };
                        bad |= (kend - 1 != last_nz) as u64;
                        o.masks[bi] = mask;
                    }
                }
            }
            Ok(())
        };
        let used = used_bits!();
        // An MCU that reads past the end of the segment's data (or fails there) means the data
        // was truncated: keep the complete MCUs, store the remaining bits verbatim.
        if used > seg_end(seg) * 8 || (r.is_err() && used + 64 > seg_end(seg) * 8) {
            if seg + 1 < clean.segments.len() {
                return err("restart segment overrun");
            }
            let boundary = m > 0 && m % ri == 0;
            info.truncated = Some(Truncation { mcus: m, bits: mcu_end, boundary, ..Default::default() });
            clear_mcus(s, &layout, m, out);
            break;
        }
        if let Err(e) = r {
            return err(e);
        }
        mcu_end = used;
    }
    match &mut info.truncated {
        Some(t) => {
            if info.trailing_rst > 0 {
                info.standard = false;
            }
            t.tail = data[t.bits / 8..data_len].to_vec();
        }
        None => check_segment_end!(seg),
    }
    if bad != 0 || (pad_ones && pad_zeros) {
        info.standard = false;
    }
    info.pad_zero = pad_zeros && !pad_ones;
    Ok(info)
}

pub const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27,
    20, 13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58,
    59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
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
        unsafe { (self.buf.as_mut_ptr().add(self.pos) as *mut u64).write_unaligned((self.acc << (64 - self.n)).to_be()) };
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
    let bits = if v < 0 { (v - 1) as u32 & ((1u32 << s) - 1) } else { v as u32 };
    (s, bits)
}

/// Re-emit the entropy-coded data of scan `s` (including RST markers) from zigzag-ordered
/// blocks with nonzero masks. Bit-exact for scans that decoded with `ScanInfo::standard`.
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
    let mut w = BitOut { acc: 0, n: 0, buf: take_scratch(), pos: 0 };
    let r = encode_scan_into(j, s, info, coefs, out, &layout, &dct, &act, &mut w);
    put_scratch(w.buf);
    r
}

thread_local! {
    static SCRATCH: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
}
/// A reusable byte buffer (contents unspecified) for the bit writers.
pub(crate) fn take_scratch() -> Vec<u8> {
    SCRATCH.with(|b| std::mem::take(&mut *b.borrow_mut()))
}
pub(crate) fn put_scratch(v: Vec<u8>) {
    SCRATCH.with(|b| *b.borrow_mut() = v);
}

#[allow(clippy::too_many_arguments)]
fn encode_scan_into(
    j: &Jpeg,
    s: &Scan,
    info: &ScanInfo,
    coefs: &[Coefs],
    out: &mut Vec<u8>,
    layout: &[(usize, usize, usize)],
    dct: &[EncTable],
    act: &[EncTable],
    w: &mut BitOut,
) -> Result<(), Error> {
    let _ = j;
    let nmcu = s.mcux * s.mcuy;
    let ri = if s.ri > 0 { s.ri } else { nmcu };
    let mut seg_ends = Vec::new(); // unstuffed offsets where an RST marker follows
    let mut pred = [0i32; 4];
    let emit = info.truncated.as_ref().map_or(nmcu, |t| t.mcus.min(nmcu));
    for m in 0..emit {
        if m > 0 && m % ri == 0 {
            w.pad(info.pad_zero);
            seg_ends.push(w.pos);
            pred = [0; 4];
        }
        let (mx, my) = (m % s.mcux, m / s.mcux);
        for (si, &(ci, ch, cv)) in layout.iter().enumerate() {
            let o = &coefs[ci];
            let (dt, at) = (&dct[si].0, &act[si].0);
            let (eob, zrl) = (at[0x00], at[0xF0]);
            for v in 0..cv {
                for h in 0..ch {
                    // a block needs at most 64 codes of <= 27 bits
                    w.reserve(64 * 4);
                    let (bx, by) = (mx * ch + h, my * cv + v);
                    if bx >= o.bw || by >= o.bh {
                        return err("block outside the component");
                    }
                    let bi = by * o.bw + bx;
                    let blk = &o.blocks[bi];
                    let dc = blk[0] as i32;
                    let (sz, bits) = size_bits(dc - pred[si]);
                    pred[si] = dc;
                    let e = dt[sz as usize];
                    if e == 0 {
                        return err("DC symbol without Huffman code");
                    }
                    w.put((e & 255) + sz, (e >> 8) << sz | bits);
                    let mut mk = o.masks[bi] & !1;
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
                }
            }
        }
    }
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
