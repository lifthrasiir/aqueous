//! Progressive JPEG (SOF2, Huffman): decoding of all scans into the final coefficients, and
//! bit-exact re-emission of each scan from those coefficients.
//!
//! Decoding follows libjpeg's jdphuff.c, encoding follows jcphuff.c (EOBRUN flushed at 0x7FFF,
//! correction bits buffered up to MAX_CORR_BITS, ZRLs folded into EOB runs where possible),
//! which is what libjpeg, libjpeg-turbo and mozjpeg produce. Encoders that decide differently
//! are detected at compression time by re-encoding every scan and comparing with the original.

use crate::jpeg::{self, err, extend, size_bits, BitOut, Bits, Clean, Coefs, EncTable, Error, HuffSpec, Jpeg, Scan, ScanInfo, DC_LIMIT, PAD};

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
                return self.vals.get((c + self.valoff[l as usize]) as usize).map(|&s| s as u32).ok_or(Error("bad Huffman code".into()));
            }
        }
        err("bad Huffman code")
    }
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

fn decode_scan(j: &Jpeg, s: &Scan, clean: &Clean, out: &mut [Coefs]) -> Result<(), Error> {
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
    let layout = jpeg::scan_layout(j, s);
    let nmcu = s.mcux * s.mcuy;
    let ri = if s.ri > 0 { s.ri } else { nmcu };
    let data = &clean.data[..];
    let mut b = Bits { buf: 0, cnt: 0, pos: 0, data };
    let (al, ss, se) = (s.al as u32, s.ss as usize, s.se as usize);
    let (p1, m1) = (1i32 << al, -1i32 << al);
    let mut pred = [0i32; 4];
    let mut eobrun = 0u32;
    let mut seg = 0;
    for m in 0..nmcu {
        if m > 0 && m % ri == 0 {
            seg += 1;
            let p = *clean.segments.get(seg).ok_or(Error("missing RST".into()))?;
            b = Bits { buf: 0, cnt: 0, pos: p, data };
            pred = [0; 4];
            eobrun = 0;
        }
        let (mx, my) = (m % s.mcux, m / s.mcux);
        for (si, &(ci, ch, cv)) in layout.iter().enumerate() {
            let o = &mut out[ci];
            for v in 0..cv {
                for h in 0..ch {
                    let (bx, by) = (mx * ch + h, my * cv + v);
                    if bx >= o.bw || by >= o.bh {
                        return err("block outside the component");
                    }
                    let blk = &mut o.blocks[by * o.bw + bx];
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
                                continue;
                            }
                            let d = dec[si].as_ref().unwrap();
                            let mut k = ss;
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
                                } else if r == 15 {
                                    k += 15;
                                } else {
                                    b.refill();
                                    eobrun = (1 << r) + b.get(r) - 1;
                                    break;
                                }
                                k += 1;
                            }
                        }
                        Kind::AcRefine => {
                            let d = dec[si].as_ref().unwrap();
                            let mut k = ss;
                            if eobrun == 0 {
                                while k <= se {
                                    b.refill();
                                    let sym = d.decode(&mut b)?;
                                    let (mut r, sz) = ((sym >> 4) as i32, sym & 15);
                                    let mut val = 0i32;
                                    if sz != 0 {
                                        if sz != 1 {
                                            return err("bad refinement symbol");
                                        }
                                        val = if b.get(1) != 0 { p1 } else { m1 };
                                    } else if r != 15 {
                                        eobrun = (1 << r) + b.get(r as u32);
                                        break;
                                    }
                                    // skip r zero-history coefficients, refining nonzero ones
                                    while k <= se {
                                        let c = blk[k] as i32;
                                        if c != 0 {
                                            b.refill();
                                            if b.get(1) != 0 && c & p1 == 0 {
                                                blk[k] = store(c + if c >= 0 { p1 } else { m1 })?;
                                            }
                                        } else {
                                            r -= 1;
                                            if r < 0 {
                                                break;
                                            }
                                        }
                                        k += 1;
                                    }
                                    if val != 0 {
                                        if k > se {
                                            return err("coefficient index overflow");
                                        }
                                        blk[k] = val as i16;
                                    }
                                    k += 1;
                                }
                            }
                            if eobrun > 0 {
                                while k <= se {
                                    let c = blk[k] as i32;
                                    if c != 0 {
                                        b.refill();
                                        if b.get(1) != 0 && c & p1 == 0 {
                                            blk[k] = store(c + if c >= 0 { p1 } else { m1 })?;
                                        }
                                    }
                                    k += 1;
                                }
                                eobrun -= 1;
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
    let _ = PAD;
    Ok(())
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

struct Enc<'a> {
    w: BitOut,
    t: &'a [u32; 256], // AC (or DC) table of the current component
    eobrun: u32,
    be: Vec<BitList>, // correction bits of the blocks in the pending EOB run
    be_n: usize,
}

/// Bytes a block can produce at most (64 symbols + extra/correction bits), see `Enc::reserve`.
const BLOCK_BYTES: usize = 64 * 4 + 16;

impl Enc<'_> {
    /// The puts below do not reserve: `encode_blocks` reserves BLOCK_BYTES per block and
    /// `emit_eobrun` reserves for the buffered correction bits.
    #[inline(always)]
    fn symbol(&mut self, sym: u32) -> Result<(), Error> {
        let e = self.t[sym as usize];
        if e == 0 {
            return err("symbol without Huffman code");
        }
        self.w.put(e & 255, e >> 8);
        Ok(())
    }
    #[inline(always)]
    fn bits(&mut self, n: u32, v: u32) {
        if n > 0 {
            self.w.put(n, v & ((1 << n) - 1));
        }
    }
    #[inline(always)]
    fn emit_list(&mut self, b: BitList) {
        let mut n = b.n;
        while n > 0 {
            let take = n.min(24);
            self.w.put(take, ((b.v >> (n - take)) & ((1 << take) - 1)) as u32);
            n -= take;
        }
    }
    fn emit_eobrun(&mut self) -> Result<(), Error> {
        if self.eobrun > 0 {
            let nbits = 31 - self.eobrun.leading_zeros();
            if nbits > 14 {
                return err("EOB run too long");
            }
            self.w.reserve(8 + self.be_n / 8 + 8);
            self.symbol(nbits << 4)?;
            let r = self.eobrun;
            self.bits(nbits, r);
            self.eobrun = 0;
            for i in 0..self.be.len() {
                let b = self.be[i];
                self.emit_list(b);
            }
            self.be.clear();
            self.be_n = 0;
        }
        Ok(())
    }
}

/// Coefficient positions ss..=se as a zigzag bit mask.
#[inline(always)]
fn band_mask(ss: usize, se: usize) -> u64 {
    (u64::MAX >> (63 - se)) & (u64::MAX << ss)
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
    let mut e = Enc { w: BitOut { acc: 0, n: 0, buf: jpeg::take_scratch(), pos: 0 }, t: &tabs[0], eobrun: 0, be: Vec::new(), be_n: 0 };
    let r = encode_blocks(s, info, coefs, out, kind, &layout, &tabs, &mut e);
    jpeg::put_scratch(std::mem::take(&mut e.w.buf));
    r
}

#[allow(clippy::too_many_arguments)]
fn encode_blocks<'a>(
    s: &Scan,
    info: &ScanInfo,
    coefs: &[Coefs],
    out: &mut Vec<u8>,
    kind: Kind,
    layout: &[(usize, usize, usize)],
    tabs: &'a [[u32; 256]],
    e: &mut Enc<'a>,
) -> Result<(), Error> {
    let nmcu = s.mcux * s.mcuy;
    let ri = if s.ri > 0 { s.ri } else { nmcu };
    let (al, ss, se) = (s.al as u32, s.ss as usize, s.se as usize);
    let mut seg_ends = Vec::new();
    let mut pred = [0i32; 4];
    let band = if ss > 0 { band_mask(ss, se) } else { 0 };
    for m in 0..nmcu {
        if m > 0 && m % ri == 0 {
            e.emit_eobrun()?;
            e.w.reserve(16);
            e.w.pad(info.pad_zero);
            seg_ends.push(e.w.pos);
            pred = [0; 4];
        }
        let (mx, my) = (m % s.mcux, m / s.mcux);
        for (si, &(ci, ch, cv)) in layout.iter().enumerate() {
            let o = &coefs[ci];
            e.t = &tabs[si];
            for v in 0..cv {
                for h in 0..ch {
                    let (bx, by) = (mx * ch + h, my * cv + v);
                    if bx >= o.bw || by >= o.bh {
                        return err("block outside the component");
                    }
                    let bi = by * o.bw + bx;
                    let blk = &o.blocks[bi];
                    e.w.reserve(BLOCK_BYTES);
                    match kind {
                        Kind::DcFirst => {
                            let t = (blk[0] as i32) >> al;
                            let (sz, bits) = size_bits(t - pred[si]);
                            pred[si] = t;
                            e.symbol(sz)?;
                            e.bits(sz, bits);
                        }
                        Kind::DcRefine => e.bits(1, ((blk[0] as i32) >> al) as u32 & 1),
                        Kind::AcFirst if o.masks[bi] & band == 0 => {
                            // empty band: just extends the EOB run
                            e.eobrun += 1;
                            if e.eobrun == 0x7FFF {
                                e.emit_eobrun()?;
                            }
                        }
                        Kind::AcRefine if o.masks[bi] & band == 0 => {
                            // empty band, no correction bits
                            e.eobrun += 1;
                            if e.eobrun == 0x7FFF {
                                e.emit_eobrun()?;
                            }
                        }
                        Kind::AcFirst => {
                            // nonzero (after the shift) coefficients in the band, via the mask
                            let mut mk = o.masks[bi] & band;
                            let mut last = ss - 1; // last coded position
                            while mk != 0 {
                                let k = mk.trailing_zeros() as usize;
                                mk &= mk - 1;
                                let c = blk[k] as i32;
                                let t = c.abs() >> al;
                                if t == 0 {
                                    continue;
                                }
                                let mut r = (k - last - 1) as u32;
                                e.emit_eobrun()?;
                                while r > 15 {
                                    e.symbol(0xF0)?;
                                    r -= 16;
                                }
                                let nbits = 32 - t.leading_zeros();
                                e.symbol(r << 4 | nbits)?;
                                e.bits(nbits, if c < 0 { !t as u32 } else { t as u32 });
                                last = k;
                            }
                            if last < se {
                                e.eobrun += 1;
                                if e.eobrun == 0x7FFF {
                                    e.emit_eobrun()?;
                                }
                            }
                        }
                        Kind::AcRefine => {
                            // Positions with |c| >> al == 0 count as zeros (r); == 1 are newly
                            // nonzero; > 1 contribute a correction bit and do not reset r.
                            let mk = o.masks[bi] & band;
                            let mut eob = 0;
                            let mut mm = mk;
                            while mm != 0 {
                                let k = mm.trailing_zeros() as usize;
                                mm &= mm - 1;
                                if (blk[k] as i32).abs() >> al == 1 {
                                    eob = k;
                                }
                            }
                            let mut r = 0u32;
                            let mut br = BitList::default();
                            let mut prev = ss - 1;
                            let mut mm = mk;
                            while mm != 0 {
                                let k = mm.trailing_zeros() as usize;
                                mm &= mm - 1;
                                let t = (blk[k] as i32).abs() >> al;
                                if t == 0 {
                                    continue;
                                }
                                r += (k - prev - 1) as u32;
                                prev = k;
                                while r > 15 && k <= eob {
                                    e.emit_eobrun()?;
                                    e.symbol(0xF0)?;
                                    r -= 16;
                                    e.emit_list(br);
                                    br = BitList::default();
                                }
                                if t > 1 {
                                    br.push((t & 1) as u32);
                                    continue;
                                }
                                e.emit_eobrun()?;
                                e.symbol(r << 4 | 1)?;
                                e.bits(1, (blk[k] >= 0) as u32);
                                e.emit_list(br);
                                br = BitList::default();
                                r = 0;
                            }
                            r += (se - prev) as u32;
                            if r > 0 || br.n > 0 {
                                e.eobrun += 1;
                                e.be.push(br);
                                e.be_n += br.n as usize;
                                if e.eobrun == 0x7FFF || e.be_n > MAX_CORR_BITS - 64 + 1 {
                                    e.emit_eobrun()?;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    e.emit_eobrun()?;
    e.w.reserve(16);
    e.w.pad(info.pad_zero);
    for _ in 0..info.trailing_rst {
        seg_ends.push(e.w.pos);
    }
    let w = &e.w;
    out.reserve(w.pos + w.pos / 64 + seg_ends.len() * 2 + 16);
    let mut start = 0;
    for (r, &end) in seg_ends.iter().enumerate() {
        jpeg::stuff_into(&w.buf[start..end], out);
        out.extend_from_slice(&[0xFF, 0xD0 + (r as u8 & 7)]);
        start = end;
    }
    jpeg::stuff_into(&w.buf[start..w.pos], out);
    Ok(())
}

/// Decode all scans of a progressive JPEG `d` into zigzag-ordered coefficients with masks, and
/// determine for each scan whether `encode_scan` reproduces it (by re-encoding it). The first
/// scan that fails to decode and all later ones (which may depend on it) are marked `raw`, as
/// are scans that do not re-encode identically.
pub fn decode_all(j: &Jpeg, d: &[u8], out: &mut Vec<Coefs>, clean: &mut Clean) -> Result<Vec<ScanInfo>, Error> {
    jpeg::alloc_coefs(j, out, true);
    let mut trailing = Vec::with_capacity(j.scans.len());
    for s in &j.scans {
        jpeg::destuff(&d[s.start..s.end], clean);
        if decode_scan(j, s, clean, out).is_err() {
            break;
        }
        let nmcu = s.mcux * s.mcuy;
        let nseg = nmcu.div_ceil(if s.ri > 0 { s.ri } else { nmcu });
        let data_len = clean.data.len() - PAD;
        let extra = clean.segments.len().saturating_sub(nseg);
        trailing.push(if extra > 0 && clean.segments[nseg..].iter().all(|&p| p == data_len) { extra as u32 } else { 0 });
    }
    for o in out.iter_mut() {
        for (blk, mask) in o.blocks.iter().zip(o.masks.iter_mut()) {
            *mask = blk.iter().enumerate().fold(0u64, |m, (k, &v)| m | ((v != 0) as u64) << k);
            if (blk[0] as i32).abs() > DC_LIMIT {
                return err("DC out of range");
            }
        }
    }
    // verify: re-encode every decoded scan (with 1- or 0-padding) and compare
    let mut infos = Vec::with_capacity(j.scans.len());
    let mut buf = Vec::new();
    for (i, s) in j.scans.iter().enumerate() {
        let Some(&t) = trailing.get(i) else {
            infos.push(ScanInfo { raw: true, ..Default::default() });
            continue;
        };
        let orig = &d[s.start..s.end];
        let mut info = ScanInfo { standard: false, trailing_rst: t, pad_zero: false, truncated: None, raw: false };
        for pad_zero in [false, true] {
            info.pad_zero = pad_zero;
            if encode_scan(j, s, &info, out, &mut buf).is_ok() && buf == orig {
                info.standard = true;
                break;
            }
        }
        info.raw = !info.standard;
        infos.push(info);
    }
    Ok(infos)
}
