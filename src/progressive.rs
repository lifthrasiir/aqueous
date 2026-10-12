//! Progressive JPEG (SOF2, Huffman): decoding of all scans into the final coefficients, and
//! bit-exact re-emission of each scan from those coefficients.
//!
//! Decoding follows libjpeg's jdphuff.c, encoding follows jcphuff.c (EOBRUN flushed at 0x7FFF,
//! correction bits buffered up to MAX_CORR_BITS, ZRLs folded into EOB runs where possible),
//! which is what libjpeg, libjpeg-turbo and mozjpeg produce. Encoders that decide differently
//! are detected at compression time by re-encoding every scan and comparing with the original.

use crate::jpeg::{
    self, Bits, Clean, Coefs, CompPtr, DC_LIMIT, EncTable, Error, HuffSpec, Jpeg, PAD, Scan, ScanInfo, err, extend,
    size_bits,
};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Canonical Huffman decoder for arbitrary symbols (progressive scans use (r, 0) symbols as EOB
/// runs, which the sequential fast tables reject). 9-bit lookahead + canonical slow path.
struct SymDec {
    look: Box<[u16; 512]>, // len << 8 | symbol, 0 if the code is longer than 9 bits
    maxcode: [i32; 18],
    valoff: [i32; 17],
    vals: Vec<u8>,
}

impl SymDec {
    fn new(spec: &HuffSpec) -> Result<SymDec, Error> {
        let mut seen = [false; 256];
        for &v in &spec.vals {
            if std::mem::replace(&mut seen[v as usize], true) {
                return err("duplicate Huffman symbol");
            }
        }
        let mut look = Box::new([0u16; 512]);
        let mut maxcode = [-1i32; 18];
        let mut valoff = [0i32; 17];
        let (mut code, mut k) = (0u32, 0usize);
        for l in 1..=16u32 {
            let n = spec.counts[l as usize - 1] as usize;
            valoff[l as usize] = k as i32 - code as i32;
            if code + n as u32 > 1 << l {
                return err("oversubscribed Huffman table");
            }
            for _ in 0..n {
                if l <= 9 {
                    let base = (code << (9 - l)) as usize;
                    for e in &mut look[base..base + (1 << (9 - l))] {
                        *e = (l as u16) << 8 | spec.vals[k] as u16;
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
        Ok(SymDec { look, maxcode, valoff, vals: spec.vals.clone() })
    }

    /// Decode one symbol; the caller has refilled `b` (>= 16 valid bits).
    #[inline(always)]
    fn decode(&self, b: &mut Bits) -> Result<u32, Error> {
        let e = self.look[b.peek(9) as usize];
        if e != 0 {
            b.consume((e >> 8) as u32);
            return Ok((e & 255) as u32);
        }
        for l in 10..=16u32 {
            let c = b.peek(l) as i32;
            if c <= self.maxcode[l as usize] {
                b.consume(l);
                return self
                    .vals
                    .get((c + self.valoff[l as usize]) as usize)
                    .map(|&s| s as u32)
                    .ok_or(Error("bad Huffman code".into()));
            }
        }
        err("bad Huffman code")
    }
}

/// Positions below `k` (k <= 64) as a mask.
#[inline(always)]
fn below(k: usize) -> u64 {
    if k >= 64 { u64::MAX } else { (1u64 << k) - 1 }
}

/// Combined lookup for AC refinement scans, indexed by the next RBITS bits: one entry decodes
/// a whole symbol including its sign bit / EOB-run bits when they fit.
const RBITS: u32 = 12;
#[derive(Clone, Copy, Default)]
struct REntry {
    /// bits to consume (0 = not in the table: use the generic path)
    len: u8,
    /// 1 = new coefficient (run r, `val` = sign), 2 = EOB run (`val` = run length), 3 = ZRL
    kind: u8,
    r: u8,
    val: u16,
}
fn refine_table(spec: &HuffSpec) -> Box<[REntry; 1 << RBITS]> {
    let mut t = Box::new([REntry::default(); 1 << RBITS]);
    let (mut code, mut k) = (0u32, 0usize);
    for l in 1..=16u32 {
        for _ in 0..spec.counts[l as usize - 1] {
            let sym = spec.vals[k];
            let (r, sz) = ((sym >> 4) as u32, (sym & 15) as u32);
            // extra bits after the code: sign (s = 1), r bits (EOB run), none (ZRL)
            let extra = match (sz, r) {
                (1, _) => Some(1),
                (0, 15) => Some(0),
                (0, _) => Some(r),
                _ => None, // invalid in refinement scans: generic path reports it
            };
            if let Some(x) = extra
                && l + x <= RBITS
            {
                let rem = RBITS - l;
                let base = (code << rem) as usize;
                for idx in base..base + (1 << rem) {
                    let ebits = ((idx as u32) >> (rem - x)) & ((1 << x) - 1);
                    t[idx] = match (sz, r) {
                        (1, _) => REntry { len: (l + 1) as u8, kind: 1, r: r as u8, val: ebits as u16 },
                        (0, 15) => REntry { len: l as u8, kind: 3, r: 15, val: 0 },
                        _ => REntry { len: (l + x) as u8, kind: 2, r: r as u8, val: ((1 << r) + ebits) as u16 },
                    };
                }
            }
            code += 1;
            k += 1;
        }
        code <<= 1;
    }
    t
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    DcFirst,
    DcRefine,
    AcFirst,
    AcRefine,
}

fn kind(s: &Scan) -> Result<Kind, Error> {
    let ok_al = s.al <= 13 && (s.ah == 0 || s.al + 1 == s.ah);
    match (s.ss, s.se) {
        (0, 0) if ok_al => Ok(if s.ah == 0 { Kind::DcFirst } else { Kind::DcRefine }),
        (1..=63, _) if s.se >= s.ss && s.se <= 63 && s.comps.len() == 1 && ok_al => {
            Ok(if s.ah == 0 { Kind::AcFirst } else { Kind::AcRefine })
        }
        _ => err("bad progressive scan parameters"),
    }
}

/// Final coefficient values must fit in i16 (and DC within DC_LIMIT, as for sequential files).
#[inline(always)]
fn store(v: i32) -> Result<i16, Error> {
    if v.abs() > 32767 {
        return err("coefficient out of range");
    }
    Ok(v as i16)
}

/// Result of decoding a scan: whether `encode_scan` reproduces it, judged inline from the
/// decisions a libjpeg-style encoder makes (see `decode_scan`), and the padding used.
#[derive(Clone, Copy, Debug, Default)]
struct Canon {
    standard: bool,
    pad_zero: bool,
    /// first reason the scan was judged non-canonical
    why: Option<&'static str>,
}

/// Row-level synchronisation of scans decoded concurrently: `progress[scan][component]` = block
/// rows of the component the scan has finished (usize::MAX once the scan has ended, also on
/// failure). A scan only touches a block row once every earlier scan of the same component
/// has finished it, so each block is accessed by one scan at a time, in scan order.
struct ScanSync<'a> {
    progress: &'a [[AtomicUsize; 4]],
    scan: usize,
}
impl ScanSync<'_> {
    /// Wait until the earlier scans touching `layout`'s components have finished their rows
    /// below MCU row `my + 1`.
    fn wait(&self, j: &Jpeg, layout: &[(usize, usize, usize)], my: usize) {
        for &(ci, _, cv) in layout {
            let need = (my + 1) * cv;
            for (k, p) in self.progress[..self.scan].iter().enumerate() {
                if j.scans[k].comps.iter().any(|sc| sc.ci == ci) {
                    while p[ci].load(Ordering::Acquire) < need {
                        std::thread::yield_now();
                    }
                }
            }
        }
    }
    fn publish(&self, layout: &[(usize, usize, usize)], rows_of_mcus: usize) {
        for &(ci, _, cv) in layout {
            self.progress[self.scan][ci].store(rows_of_mcus * cv, Ordering::Release);
        }
    }
}
impl Drop for ScanSync<'_> {
    fn drop(&mut self) {
        for p in &self.progress[self.scan] {
            p.store(usize::MAX, Ordering::Release);
        }
    }
}

/// Decode one scan into `out`, checking on the way that it is exactly what `encode_scan` (i.e.
/// libjpeg's jcphuff) produces for the resulting coefficients:
/// - padding before markers is all 1-bits or all 0-bits (consistently), segments are consumed
///   exactly, markers are numbered in order (only RSTs may follow the last MCU);
/// - EOB runs never cross a restart boundary or the end of the scan;
/// - a run is only cut short where libjpeg cuts it: the next block does not start with another
///   EOB run unless the run reached 0x7FFF blocks or (refinement) more than 937 buffered
///   correction bits;
/// - (refinement) a run never continues past a block after which more than 937 correction bits
///   are buffered;
/// - a ZRL is always followed by a nonzero coefficient in the same block.
fn decode_scan(j: &Jpeg, s: &Scan, clean: &Clean, out: &[CompPtr], sync: Option<&ScanSync>) -> Result<Canon, Error> {
    let kind = kind(s)?;
    let mut dec = Vec::new();
    for sc in &s.comps {
        let spec = match kind {
            Kind::DcFirst => sc.dc.as_ref(),
            Kind::AcFirst | Kind::AcRefine => sc.ac.as_ref(),
            Kind::DcRefine => None,
        };
        dec.push(match spec {
            Some(t) => Some(SymDec::new(t)?),
            None if kind == Kind::DcRefine => None,
            None => return err("missing Huffman table"),
        });
    }
    let rtab = match kind {
        Kind::AcRefine => Some(refine_table(s.comps[0].ac.as_ref().unwrap())),
        _ => None,
    };
    let layout = jpeg::scan_layout(j, s);
    let nmcu = s.mcux * s.mcuy;
    let ri = if s.ri > 0 { s.ri } else { nmcu };
    let data = &clean.data[..];
    let mut b = Bits { buf: 0, cnt: 0, pos: 0, data };
    let (al, ss, se) = (s.al as u32, s.ss as usize, s.se as usize);
    let (p1, m1) = (1i32 << al, -1i32 << al);
    let band = if ss > 0 { (u64::MAX >> (63 - se)) & (u64::MAX << ss) } else { 0 };
    let mut pred = [0i32; 4];
    let mut eobrun = 0u32;
    let mut seg = 0;
    // --- canonical-encoding checks
    let data_len = data.len() - PAD;
    let nseg = nmcu.div_ceil(ri);
    let mut why: Option<&'static str> = None;
    macro_rules! flag {
        ($c:expr, $w:expr) => {{
            if $c && why.is_none() {
                why = Some($w);
            }
        }};
    }
    flag!(!clean.markers_ok, "markers");
    flag!(clean.segments.len() > nseg && !clean.segments[nseg..].iter().all(|&p| p == data_len), "segments");
    let (mut pad_ones, mut pad_zeros) = (false, false);
    let seg_end = |seg: usize| if seg + 1 < clean.segments.len() { clean.segments[seg + 1] } else { data_len };
    macro_rules! check_segment_end {
        ($seg:expr) => {{
            let used = b.pos * 8 - b.cnt as usize;
            let pad = ((8 - used % 8) % 8) as u32;
            flag!(used.div_ceil(8) != seg_end($seg), "segment not consumed exactly");
            flag!(eobrun != 0, "EOB run crosses segment end");
            if pad > 0 {
                b.refill();
                let bits = b.peek(pad);
                pad_ones |= bits == (1 << pad) - 1;
                pad_zeros |= bits == 0;
                flag!(bits != 0 && bits != (1 << pad) - 1, "padding");
            }
        }};
    }
    let mut run_len = 0u32; // length of the current / last EOB run
    let mut run_be = 0usize; // correction bits buffered with the current EOB run
    let mut prev_end: Option<bool> = None; // previous block ended an EOB run: Some(forced)
    for m in 0..nmcu {
        if m > 0 && m % ri == 0 {
            check_segment_end!(seg);
            seg += 1;
            let p = *clean.segments.get(seg).ok_or(Error("missing RST".into()))?;
            b = Bits { buf: 0, cnt: 0, pos: p, data };
            pred = [0; 4];
            eobrun = 0;
            prev_end = None;
        }
        let (mx, my) = (m % s.mcux, m / s.mcux);
        if let (0, Some(sy)) = (mx, sync) {
            sy.publish(&layout, my);
            sy.wait(j, &layout, my);
        }
        for (si, &(ci, ch, cv)) in layout.iter().enumerate() {
            let o = out[ci];
            for v in 0..cv {
                for h in 0..ch {
                    let (bx, by) = (mx * ch + h, my * cv + v);
                    if bx >= o.bw || by >= o.bh {
                        return err("block outside the component");
                    }
                    let bi = by * o.bw + bx;
                    // SAFETY: within the component; concurrent scans are kept off this block
                    // row by `ScanSync`
                    let (blk, mask) = unsafe { (&mut *o.blocks.add(bi), &mut *o.masks.add(bi)) }; // mask: nonzero history (AC bits kept up to date)
                    b.refill();
                    match kind {
                        Kind::DcFirst => {
                            let sz = dec[si].as_ref().unwrap().decode(&mut b)?;
                            if sz > 15 {
                                return err("bad DC size");
                            }
                            b.refill();
                            pred[si] += extend(b.get(sz), sz);
                            if pred[si].abs() > DC_LIMIT {
                                return err("DC out of range");
                            }
                            blk[0] = store(pred[si] * p1)?;
                        }
                        Kind::DcRefine => {
                            if b.get(1) != 0 {
                                blk[0] = store(blk[0] as i32 | p1)?;
                            }
                        }
                        Kind::AcFirst => {
                            if eobrun > 0 {
                                eobrun -= 1;
                                prev_end = (eobrun == 0).then_some(run_len == 0x7FFF);
                                continue;
                            }
                            let d = dec[si].as_ref().unwrap();
                            let mut k = ss;
                            let (mut first, mut zrl, mut ended) = (true, false, false);
                            while k <= se {
                                b.refill();
                                let sym = d.decode(&mut b)?;
                                let (r, sz) = (sym >> 4, sym & 15);
                                if sz != 0 {
                                    k += r as usize;
                                    if k > se {
                                        return err("coefficient index overflow");
                                    }
                                    b.refill();
                                    blk[k] = store(extend(b.get(sz), sz) * p1)?;
                                    *mask |= 1 << k;
                                    zrl = false;
                                } else if r == 15 {
                                    k += 15;
                                    zrl = true;
                                } else {
                                    // EOB run: canonical only if not right after a ZRL and not
                                    // continuing a run that libjpeg would not have cut
                                    flag!(zrl, "AC1 ZRL before EOB");
                                    flag!(first && prev_end == Some(false), "AC1 run split");
                                    b.refill();
                                    run_len = (1 << r) + b.get(r);
                                    eobrun = run_len - 1;
                                    prev_end = (eobrun == 0).then_some(run_len == 0x7FFF);
                                    ended = true;
                                    break;
                                }
                                first = false;
                                k += 1;
                            }
                            if !ended {
                                flag!(zrl, "AC1 ZRL at band end");
                                prev_end = None;
                            }
                        }
                        Kind::AcRefine => {
                            // Coefficients that were nonzero before this scan ("history") each
                            // take one correction bit when passed; a symbol (r, s) skips r
                            // zero-history positions and, for s = 1, sets a new coefficient at the
                            // next one. The history positions are taken from the mask, and their
                            // correction bits are read in bulk.
                            let d = dec[si].as_ref().unwrap();
                            let hist = *mask & band;
                            // correction bits are collected in order (one per history position
                            // passed) and applied once at the end of the block
                            let mut corr = [0u64; 2]; // up to 63 bits, MSB first, across two words
                            let mut ncorr = 0u32;
                            macro_rules! collect {
                                ($m:expr) => {{
                                    let mut c = ($m).count_ones();
                                    while c > 0 {
                                        let take = c.min(24);
                                        b.refill();
                                        let v = b.get(take) as u64;
                                        // shift into the 128-bit accumulator
                                        corr[0] = corr[0] << take | corr[1] >> (64 - take);
                                        corr[1] = corr[1] << take | v;
                                        ncorr += take;
                                        c -= take;
                                    }
                                }};
                            }
                            let mut k = ss;
                            let mut covered = 0u64; // history positions whose bits were read
                            if eobrun == 0 {
                                let rt = rtab.as_deref().unwrap();
                                let (mut first, mut zrl, mut started) = (true, false, false);
                                while k <= se {
                                    b.refill();
                                    let e = rt[b.peek(RBITS) as usize];
                                    let (r, val) = match e.kind {
                                        1 => {
                                            b.consume(e.len as u32);
                                            (e.r as u32, if e.val != 0 { p1 } else { m1 })
                                        }
                                        2 => {
                                            b.consume(e.len as u32);
                                            eobrun = e.val as u32;
                                            started = true;
                                            break;
                                        }
                                        3 => {
                                            b.consume(e.len as u32);
                                            (15, 0)
                                        }
                                        _ => {
                                            let sym = d.decode(&mut b)?;
                                            let (r, sz) = (sym >> 4, sym & 15);
                                            if sz > 1 {
                                                return err("bad refinement symbol");
                                            }
                                            b.refill();
                                            if sz == 1 {
                                                (r, if b.get(1) != 0 { p1 } else { m1 })
                                            } else if r != 15 {
                                                eobrun = (1 << r) + b.get(r);
                                                started = true;
                                                break;
                                            } else {
                                                (15, 0)
                                            }
                                        }
                                    };
                                    zrl = val == 0;
                                    first = false;
                                    // target: the (r+1)-th zero-history position >= k (r is
                                    // usually small; a branch-free select measured slower)
                                    let mut z = !hist & band & (u64::MAX << k);
                                    for _ in 0..r {
                                        z &= z.wrapping_sub(1);
                                    }
                                    let target = if z == 0 { se + 1 } else { z.trailing_zeros() as usize };
                                    let m = hist & (u64::MAX << k) & below(target);
                                    collect!(m);
                                    covered |= m;
                                    if val != 0 {
                                        if target > se {
                                            return err("coefficient index overflow");
                                        }
                                        blk[target] = val as i16;
                                        *mask |= 1 << target;
                                    }
                                    k = target + 1;
                                }
                                if started {
                                    flag!(zrl, "ACr ZRL before EOB");
                                    flag!(first && prev_end == Some(false), "ACr run split");
                                    run_len = eobrun;
                                    run_be = 0;
                                } else {
                                    flag!(zrl, "ACr ZRL at band end");
                                    prev_end = None;
                                }
                            }
                            if eobrun > 0 {
                                if k <= se {
                                    let m = hist & (u64::MAX << k);
                                    run_be += m.count_ones() as usize;
                                    collect!(m);
                                    covered |= m;
                                }
                                eobrun -= 1;
                                let full = run_len == 0x7FFF || run_be > MAX_CORR_BITS - 64 + 1;
                                if eobrun == 0 {
                                    prev_end = Some(full);
                                } else {
                                    // libjpeg flushes as soon as too many bits are buffered
                                    flag!(run_be > MAX_CORR_BITS - 64 + 1, "ACr run too long");
                                    prev_end = None;
                                }
                            }
                            // apply: the i-th covered position (ascending) gets bit ncorr-1-i
                            let mut bad = false;
                            let mut i = ncorr;
                            while covered != 0 {
                                let pos = covered.trailing_zeros() as usize;
                                covered &= covered - 1;
                                i -= 1;
                                let bit = if i >= 64 { corr[0] >> (i - 64) } else { corr[1] >> i } & 1;
                                let c = blk[pos] as i32;
                                let upd = bit as i32 & (c & p1 == 0) as i32;
                                let nc = c + upd * if c >= 0 { p1 } else { -p1 };
                                bad |= nc.abs() > 32767;
                                blk[pos] = nc as i16;
                            }
                            if bad {
                                return err("coefficient out of range");
                            }
                        }
                    }
                }
            }
            if b.pos > data.len() {
                return err("scan data exhausted");
            }
        }
    }
    check_segment_end!(seg);
    flag!(pad_ones && pad_zeros, "mixed padding");
    if let (Some(w), true) = (why, std::env::var_os("JRC_DEBUG_CANON").is_some()) {
        eprintln!("canon: scan ss={} se={} ah={} al={}: {w}", s.ss, s.se, s.ah, s.al);
    }
    Ok(Canon { standard: why.is_none(), pad_zero: pad_zeros && !pad_ones, why })
}

// ---------------------------------------------------------------- encoding (jcphuff.c)

const MAX_CORR_BITS: usize = 1000;

/// Up to 64 bits, MSB-first (the correction bits of one block).
#[derive(Clone, Copy, Default)]
struct BitList {
    v: u64,
    n: u32,
}
impl BitList {
    #[inline(always)]
    fn push(&mut self, b: u32) {
        self.v = self.v << 1 | b as u64;
        self.n += 1;
    }
}

/// Bytes a block can produce at most: 64 symbols with extra / correction bits, plus one flush
/// of the pending EOB run's correction bits (< MAX_CORR_BITS). `encode_scan` reserves this per
/// block for a whole MCU row at a time, so the puts never need to check.
const BLOCK_BYTES: usize = 64 * 4 + MAX_CORR_BITS / 8 + 32;

/// Coefficient positions ss..=se as a zigzag bit mask.
#[inline(always)]
fn band_mask(ss: usize, se: usize) -> u64 {
    (u64::MAX >> (63 - se)) & (u64::MAX << ss)
}

thread_local! {
    /// correction bits of the blocks in a pending EOB run (at most 0x7FFF blocks)
    static BE: std::cell::RefCell<Vec<BitList>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Re-emit progressive scan `s` from final zigzag-ordered coefficients.
pub fn encode_scan(j: &Jpeg, s: &Scan, info: &ScanInfo, coefs: &[Coefs], out: &mut Vec<u8>) -> Result<(), Error> {
    out.clear();
    let kind = kind(s)?;
    let mut tabs = Vec::new();
    for sc in &s.comps {
        let spec = match kind {
            Kind::DcFirst => sc.dc.as_ref(),
            Kind::AcFirst | Kind::AcRefine => sc.ac.as_ref(),
            Kind::DcRefine => None,
        };
        tabs.push(match spec {
            Some(t) => EncTable::new(t).0,
            None if kind == Kind::DcRefine => [0; 256],
            None => return err("missing Huffman table"),
        });
    }
    let layout = jpeg::scan_layout(j, s);
    let mut buf = jpeg::take_scratch();
    let mut be = BE.with(|b| std::mem::take(&mut *b.borrow_mut()));
    be.resize(0x8000, BitList::default());
    let r = encode_blocks(s, info, coefs, kind, &layout, &tabs, &mut buf, &mut be, out);
    BE.with(|b| *b.borrow_mut() = be);
    jpeg::put_scratch(buf);
    r
}

/// The encoder proper. All hot state (bit buffer, EOB run) is in locals and the loop makes no
/// calls except the per-row buffer growth, so it stays in registers.
#[allow(clippy::too_many_arguments)]
fn encode_blocks(
    s: &Scan,
    info: &ScanInfo,
    coefs: &[Coefs],
    kind: Kind,
    layout: &[(usize, usize, usize)],
    tabs: &[[u32; 256]],
    buf: &mut Vec<u8>,
    be: &mut [BitList],
    out: &mut Vec<u8>,
) -> Result<(), Error> {
    let nmcu = s.mcux * s.mcuy;
    let ri = if s.ri > 0 { s.ri } else { nmcu };
    let (al, ss, se) = (s.al as u32, s.ss as usize, s.se as usize);
    let band = if ss > 0 { band_mask(ss, se) } else { 0 };
    let row_bytes = BLOCK_BYTES * s.mcux * layout.iter().map(|&(_, h, v)| h * v).sum::<usize>();
    let mut seg_ends = Vec::new();
    let mut pred = [0i32; 4];
    // MSB-first bit writer: `n` (< 8) pending bits in the low end of `acc`
    let (mut acc, mut n, mut pos) = (0u64, 0u32, 0usize);
    let mut ptr = buf.as_mut_ptr();
    let mut eobrun = 0u32;
    let mut be_len = 0usize; // blocks with buffered correction bits
    let mut be_bits = 0usize;
    macro_rules! put {
        ($len:expr, $bits:expr) => {{
            let (l, v) = ($len, $bits);
            acc = (acc << l) | v as u64;
            n += l;
            // SAFETY: >= BLOCK_BYTES are reserved per remaining block of the current MCU row
            unsafe { (ptr.add(pos) as *mut u64).write_unaligned((acc << (64 - n)).to_be()) };
            pos += (n >> 3) as usize;
            n &= 7;
        }};
    }
    macro_rules! symbol {
        ($t:expr, $sym:expr) => {{
            let e = $t[$sym as usize];
            if e == 0 {
                return err("symbol without Huffman code");
            }
            put!(e & 255, e >> 8);
        }};
    }
    macro_rules! bits {
        ($nb:expr, $v:expr) => {{
            let nb: u32 = $nb;
            if nb > 0 {
                put!(nb, ($v as u32) & ((1u32 << nb) - 1));
            }
        }};
    }
    macro_rules! emit_list {
        ($b:expr) => {{
            let b: BitList = $b;
            let mut k = b.n;
            while k > 0 {
                let take = k.min(24);
                put!(take, ((b.v >> (k - take)) & ((1 << take) - 1)) as u32);
                k -= take;
            }
        }};
    }
    macro_rules! emit_eobrun {
        ($t:expr) => {{
            if eobrun > 0 {
                let nbits = 31 - eobrun.leading_zeros();
                if nbits > 14 {
                    return err("EOB run too long");
                }
                symbol!($t, nbits << 4);
                bits!(nbits, eobrun);
                eobrun = 0;
                for i in 0..be_len {
                    emit_list!(be[i]);
                }
                be_len = 0;
                be_bits = 0;
            }
        }};
    }
    macro_rules! pad {
        () => {{
            if n > 0 {
                let p = 8 - n;
                put!(p, if info.pad_zero { 0 } else { (1u32 << p) - 1 });
            }
        }};
    }
    for m in 0..nmcu {
        if m % s.mcux == 0 && buf.len() < pos + row_bytes + 16 {
            buf.resize((buf.len() * 2).max(pos + row_bytes + 16), 0);
            ptr = buf.as_mut_ptr();
        }
        if m > 0 && m % ri == 0 {
            emit_eobrun!(&tabs[0]);
            pad!();
            seg_ends.push(pos);
            pred = [0; 4];
        }
        let (mx, my) = (m % s.mcux, m / s.mcux);
        for (si, &(ci, ch, cv)) in layout.iter().enumerate() {
            let o = &coefs[ci];
            let t = &tabs[si];
            for v in 0..cv {
                for h in 0..ch {
                    let (bx, by) = (mx * ch + h, my * cv + v);
                    if bx >= o.bw || by >= o.bh {
                        return err("block outside the component");
                    }
                    let bi = by * o.bw + bx;
                    let blk = &o.blocks[bi];
                    match kind {
                        Kind::DcFirst => {
                            let d = (blk[0] as i32) >> al;
                            let (sz, b) = size_bits(d - pred[si]);
                            pred[si] = d;
                            symbol!(t, sz);
                            bits!(sz, b);
                        }
                        Kind::DcRefine => put!(1, ((blk[0] as i32) >> al) as u32 & 1),
                        Kind::AcFirst => {
                            // nonzero (after the shift) coefficients in the band, via the mask
                            let mut mk = o.masks[bi] & band;
                            let mut last = ss - 1; // last coded position
                            while mk != 0 {
                                let k = mk.trailing_zeros() as usize;
                                mk &= mk - 1;
                                let c = blk[k] as i32;
                                let a = c.abs() >> al;
                                if a == 0 {
                                    continue;
                                }
                                let mut r = (k - last - 1) as u32;
                                emit_eobrun!(t);
                                while r > 15 {
                                    symbol!(t, 0xF0);
                                    r -= 16;
                                }
                                let nbits = 32 - a.leading_zeros();
                                symbol!(t, r << 4 | nbits);
                                bits!(nbits, if c < 0 { !a as u32 } else { a as u32 });
                                last = k;
                            }
                            if last < se {
                                eobrun += 1;
                                if eobrun == 0x7FFF {
                                    emit_eobrun!(t);
                                }
                            }
                        }
                        Kind::AcRefine => {
                            // Positions with |c| >> al == 0 count as zeros (r); == 1 are newly
                            // nonzero (a symbol each); > 1 ("history") contribute a correction
                            // bit and do not reset r. Classify the block once into masks, then
                            // walk only the newly nonzero positions; the history positions of a
                            // gap are handled in bulk unless the gap needs a ZRL (> 15 zeros).
                            let mk = o.masks[bi] & band;
                            let (mut hist, mut newm) = (0u64, 0u64);
                            let mut cbits = 0u64; // correction bits of the history positions, in order
                            let mut mm = mk;
                            while mm != 0 {
                                let k = mm.trailing_zeros();
                                mm &= mm - 1;
                                let a = (blk[k as usize] as i32).abs() >> al;
                                hist |= ((a > 1) as u64) << k;
                                newm |= ((a == 1) as u64) << k;
                                cbits = (cbits << (a > 1) as u32) | ((a > 1) & (a & 1 != 0)) as u64;
                            }
                            let zeros = band & !hist & !newm;
                            let eob = if newm == 0 { 0 } else { 63 - newm.leading_zeros() as usize };
                            let mut cleft = hist.count_ones(); // correction bits not yet taken
                            // the next c correction bits, MSB first
                            macro_rules! take_bits {
                                ($c:expr) => {{
                                    let c: u32 = $c;
                                    cleft -= c;
                                    BitList {
                                        v: if c == 0 { 0 } else { (cbits >> cleft) & (u64::MAX >> (64 - c)) },
                                        n: c,
                                    }
                                }};
                            }
                            let mut r = 0u32;
                            let mut br = BitList::default();
                            let mut prev = ss - 1;
                            let mut nm = newm;
                            while nm != 0 {
                                let pnew = nm.trailing_zeros() as usize;
                                nm &= nm - 1;
                                let range = below(pnew) & !below(prev + 1);
                                let z = (zeros & range).count_ones();
                                if z <= 15 {
                                    // no ZRL: the gap's correction bits go out after the symbol
                                    br = take_bits!((hist & range).count_ones());
                                    r = z;
                                } else {
                                    // exact per-position walk (rare)
                                    r = 0;
                                    let mut q = prev;
                                    let mut hm = hist & range;
                                    while hm != 0 {
                                        let k = hm.trailing_zeros() as usize;
                                        hm &= hm - 1;
                                        r += (zeros & below(k) & !below(q + 1)).count_ones();
                                        q = k;
                                        while r > 15 && k <= eob {
                                            emit_eobrun!(t);
                                            symbol!(t, 0xF0);
                                            r -= 16;
                                            emit_list!(br);
                                            br = BitList::default();
                                        }
                                        br.push(take_bits!(1).v as u32);
                                    }
                                    r += (zeros & range & !below(q + 1)).count_ones();
                                    while r > 15 {
                                        emit_eobrun!(t);
                                        symbol!(t, 0xF0);
                                        r -= 16;
                                        emit_list!(br);
                                        br = BitList::default();
                                    }
                                }
                                emit_eobrun!(t);
                                symbol!(t, r << 4 | 1);
                                put!(1, (blk[pnew] >= 0) as u32);
                                emit_list!(br);
                                br = BitList::default();
                                r = 0;
                                prev = pnew;
                            }
                            // after the last newly nonzero position: zeros and correction bits
                            // go to the EOB run (no ZRLs past `eob`)
                            let range = band & !below(prev + 1);
                            r += (zeros & range).count_ones();
                            let rest = take_bits!((hist & range).count_ones());
                            br = BitList { v: rest.v, n: rest.n };
                            let _ = cleft;
                            if r > 0 || br.n > 0 {
                                eobrun += 1;
                                if br.n > 0 {
                                    be[be_len] = br;
                                    be_len += 1;
                                    be_bits += br.n as usize;
                                }
                                if eobrun == 0x7FFF || be_bits > MAX_CORR_BITS - 64 + 1 {
                                    emit_eobrun!(t);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    if buf.len() < pos + 1024 {
        buf.resize(pos + 1024, 0);
        ptr = buf.as_mut_ptr();
    }
    emit_eobrun!(&tabs[0]);
    pad!();
    let _ = (ptr, n, eobrun, be_len, be_bits);
    for _ in 0..info.trailing_rst {
        seg_ends.push(pos);
    }
    out.reserve(pos + pos / 64 + seg_ends.len() * 2 + 16);
    let mut start = 0;
    for (r, &end) in seg_ends.iter().enumerate() {
        jpeg::stuff_into(&buf[start..end], out);
        out.extend_from_slice(&[0xFF, 0xD0 + (r as u8 & 7)]);
        start = end;
    }
    jpeg::stuff_into(&buf[start..pos], out);
    Ok(())
}

/// Decode all scans of a progressive JPEG `d` into zigzag-ordered coefficients with masks, and
/// determine for each scan whether `encode_scan` reproduces it (inline, see `decode_scan`). The
/// first scan that fails to decode and all later ones (which may depend on it) are marked `raw`,
/// as are scans not encoded the way `encode_scan` would.
///
/// With JRC_VERIFY_PROGRESSIVE set, every scan is also re-encoded and the inline verdict is
/// checked against the outcome (a scan judged reproducible that is not panics: test oracle).
/// Number of leading scans that form a valid scan script: every coefficient bit is coded once,
/// by a first scan (ah = 0) of a not yet coded position, then by refinements continuing exactly
/// where the previous scan stopped (ah = previous al). Re-emitting a scan from the final
/// coefficients relies on this; later scans are not decoded (stored raw).
fn valid_script_len(j: &Jpeg) -> usize {
    let mut bits = vec![[-1i32; 64]; j.comps.len()]; // per position: al of the last scan, -1 = none
    for (k, s) in j.scans.iter().enumerate() {
        if kind(s).is_err() {
            return k;
        }
        for sc in &s.comps {
            let b = &mut bits[sc.ci][s.ss as usize..=s.se as usize];
            let expect = if s.ah == 0 { -1 } else { s.ah as i32 };
            if b.iter().any(|&x| x != expect) {
                return k;
            }
            b.fill(s.al as i32);
        }
    }
    j.scans.len()
}

pub fn decode_all(j: &Jpeg, d: &[u8], out: &mut Vec<Coefs>, clean: &mut Clean) -> Result<Vec<ScanInfo>, Error> {
    let n = valid_script_len(j);
    let mut res: Vec<Option<(Canon, u32)>> = Vec::new();
    if rayon::current_num_threads() > 1 && n > 1 {
        res = decode_concurrent(j, n, d, out);
        // a failed scan: the outcome must be that of decoding in order up to the failure,
        // which the concurrent decoding may have overwritten
        if res.iter().any(|r| r.is_none()) {
            res.clear();
        }
    }
    if res.is_empty() {
        jpeg::alloc_coefs(j, out, true);
        let comps: Vec<CompPtr> = out.iter_mut().map(CompPtr::of).collect();
        for s in &j.scans[..n] {
            jpeg::destuff(&d[s.start..s.end], clean);
            let Ok(c) = decode_scan(j, s, clean, &comps, None) else { break };
            res.push(Some((c, trailing_rst(s, clean))));
        }
    }
    let mut infos = Vec::with_capacity(n);
    let mut whys = Vec::new();
    for (c, trailing_rst) in res.into_iter().map_while(|r| r) {
        whys.push(c.why);
        infos.push(ScanInfo {
            standard: c.standard,
            trailing_rst,
            pad_zero: c.pad_zero,
            truncated: None,
            raw: !c.standard,
        });
    }
    // scans after the first undecodable one stay raw
    infos.resize(j.scans.len(), ScanInfo { raw: true, ..Default::default() });
    // AC mask bits were maintained while decoding; set the DC bit
    for o in out.iter_mut() {
        for (blk, mask) in o.blocks.iter().zip(o.masks.iter_mut()) {
            *mask = (*mask & !1) | (blk[0] != 0) as u64;
            if (blk[0] as i32).abs() > DC_LIMIT {
                return err("DC out of range");
            }
        }
    }
    if std::env::var_os("JRC_VERIFY_PROGRESSIVE").is_some() {
        let mut buf = Vec::new();
        for (i, (s, info)) in j.scans.iter().zip(&infos).enumerate() {
            if info.raw && info.standard {
                continue;
            }
            let same = encode_scan(j, s, info, out, &mut buf).is_ok() && buf == d[s.start..s.end];
            if info.standard && !same {
                let orig = &d[s.start..s.end];
                let at = buf.iter().zip(orig).position(|(a, b)| a != b).unwrap_or(buf.len().min(orig.len()));
                panic!(
                    "progressive scan {i} (ss={} se={} ah={} al={} ri={}): judged reproducible but re-encodes differently: {} vs {} bytes, first difference at {at}",
                    s.ss,
                    s.se,
                    s.ah,
                    s.al,
                    s.ri,
                    buf.len(),
                    orig.len()
                );
            }
            if !info.raw || !same {
                continue;
            }
            // reproducible but judged otherwise: a missed opportunity, not an error
            eprintln!(
                "note: progressive scan {i} re-encodes identically but was judged non-canonical: {:?}",
                whys.get(i)
            );
        }
    }
    Ok(infos)
}

/// Number of RST markers after the last MCU of scan `s` (destuffed into `clean`).
fn trailing_rst(s: &Scan, clean: &Clean) -> u32 {
    let nmcu = s.mcux * s.mcuy;
    let nseg = nmcu.div_ceil(if s.ri > 0 { s.ri } else { nmcu });
    let data_len = clean.data.len() - PAD;
    let extra = clean.segments.len().saturating_sub(nseg);
    if extra > 0 && clean.segments[nseg..].iter().all(|&p| p == data_len) { extra as u32 } else { 0 }
}

thread_local! {
    static CLEAN: std::cell::RefCell<Clean> = std::cell::RefCell::new(Clean::default());
}

/// All scans decoded concurrently, pipelined by block rows (see `ScanSync`): each worker
/// takes the next scan in order, so a scan only ever waits for scans already being decoded.
/// Per scan: None if it failed to decode.
fn decode_concurrent(j: &Jpeg, n: usize, d: &[u8], out: &mut Vec<Coefs>) -> Vec<Option<(Canon, u32)>> {
    jpeg::alloc_coefs(j, out, true);
    let comps: Vec<CompPtr> = out.iter_mut().map(CompPtr::of).collect();
    let progress: Vec<[AtomicUsize; 4]> = (0..n).map(|_| Default::default()).collect();
    let next = AtomicUsize::new(0);
    let res: Vec<std::sync::Mutex<Option<(Canon, u32)>>> = (0..n).map(|_| Default::default()).collect();
    rayon::scope(|sc| {
        for _ in 0..rayon::current_num_threads().min(n) {
            sc.spawn(|_| {
                loop {
                    let k = next.fetch_add(1, Ordering::Relaxed);
                    if k >= n {
                        break;
                    }
                    let s = &j.scans[k];
                    let sync = ScanSync { progress: &progress, scan: k };
                    let r = CLEAN.with(|c| {
                        let clean = &mut *c.borrow_mut();
                        jpeg::destuff(&d[s.start..s.end], clean);
                        decode_scan(j, s, clean, &comps, Some(&sync)).ok().map(|c| (c, trailing_rst(s, clean)))
                    });
                    drop(sync);
                    *res[k].lock().unwrap() = r;
                }
            });
        }
    });
    res.into_iter().map(|m| m.into_inner().unwrap()).collect()
}

/// Per-scan timing (profiling aid): (description, decode seconds, encode seconds, scan bytes).
pub fn scan_times(j: &Jpeg, d: &[u8], iters: usize) -> Result<Vec<(String, f64, f64, usize)>, Error> {
    let mut coefs = Vec::new();
    let mut clean = Clean::default();
    let infos = decode_all(j, d, &mut coefs, &mut clean)?;
    let cleans: Vec<Clean> = j
        .scans
        .iter()
        .map(|s| {
            let mut c = Clean::default();
            jpeg::destuff(&d[s.start..s.end], &mut c);
            c
        })
        .collect();
    let mut td = vec![0f64; j.scans.len()];
    let mut te = vec![0f64; j.scans.len()];
    let mut tmp = Vec::new();
    let mut out = Vec::new();
    for _ in 0..iters {
        jpeg::alloc_coefs(j, &mut tmp, true);
        for (i, s) in j.scans.iter().enumerate() {
            let t0 = std::time::Instant::now();
            let comps: Vec<CompPtr> = tmp.iter_mut().map(CompPtr::of).collect();
            decode_scan(j, s, &cleans[i], &comps, None)?;
            td[i] += t0.elapsed().as_secs_f64();
        }
        for (i, (s, info)) in j.scans.iter().zip(&infos).enumerate() {
            let t0 = std::time::Instant::now();
            encode_scan(j, s, info, &coefs, &mut out)?;
            te[i] += t0.elapsed().as_secs_f64();
        }
    }
    Ok(j.scans
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let k = match kind(s) {
                Ok(Kind::DcFirst) => "DC1",
                Ok(Kind::DcRefine) => "DCr",
                Ok(Kind::AcFirst) => "AC1",
                Ok(Kind::AcRefine) => "ACr",
                Err(_) => "?",
            };
            let desc = format!("{k} comps={} ss={} se={} ah={} al={}", s.comps.len(), s.ss, s.se, s.ah, s.al);
            (desc, td[i] / iters as f64, te[i] / iters as f64, s.end - s.start)
        })
        .collect())
}
