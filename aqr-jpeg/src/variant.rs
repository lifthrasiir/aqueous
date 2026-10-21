//! Test support: re-encode a JPEG's scans with *valid but non-canonical* choices.
//!
//! Every stream produced here decodes to exactly the same coefficients as the original (checked
//! by `jrc mutenc`), but differs from what libjpeg would write in some encoder decision that a
//! recompressor must notice (or reproduce): EOB-run splitting and limits, correction-bit buffer
//! limits, ZRLs that libjpeg would fold into an EOB, padding bits, fill bytes before markers,
//! trailing restart markers. Feeding these through compress / decompress checks that the
//! "is this scan reproducible?" decisions have no blind spots.
//!
//! Deliberately simple (per position, per bit); not used by the codec.

use crate::jpeg::{self, BitOut, Clean, Coefs, EncTable, Error, Jpeg, Order, Scan, size_bits};
use crate::progressive;

/// xorshift64*
pub struct Rng(pub u64);
impl Rng {
    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n.max(1)
    }
    fn chance(&mut self, p_percent: u64) -> bool {
        self.below(100) < p_percent
    }
}

/// Knobs of one variant scan (each scan draws its own).
#[derive(Debug, Clone, Copy)]
struct Knobs {
    /// probability (%) of trailing ZRLs before EOB where possible
    trailing_zrl: u64,
    /// padding: 0 = 1-bits, 1 = 0-bits, 2 = random bits (per segment)
    pad: u8,
    /// probability (%) of an extra 0xFF fill byte before each RST marker
    fill: u64,
    /// extra RST markers after the last MCU
    trailing_rst: u32,
    /// progressive: probability (%) of flushing a pending EOB run after a block
    eob_flush: u64,
    /// progressive: longest EOB run
    eob_max: u32,
    /// progressive: correction-bit buffer limit (libjpeg: 937)
    be_max: usize,
}

impl Knobs {
    fn draw(r: &mut Rng) -> Knobs {
        // half of the time a knob is off, otherwise a random probability
        let off = |r: &mut Rng| if r.chance(50) { 0 } else { 1 + r.below(100) };
        Knobs {
            trailing_zrl: off(r),
            pad: if r.chance(60) { 0 } else { 1 + r.below(2) as u8 },
            fill: off(r),
            trailing_rst: if r.chance(80) { 0 } else { 1 + r.below(2) as u32 },
            eob_flush: off(r),
            eob_max: if r.chance(60) { 0x7FFF } else { 1 + r.below(0x7FFF) as u32 },
            be_max: if r.chance(60) { 937 } else { r.below(3000) as usize },
        }
    }
}

struct W<'a> {
    w: BitOut,
    rng: &'a mut Rng,
    k: Knobs,
}
impl W<'_> {
    fn put(&mut self, len: u32, bits: u32) {
        if len > 0 {
            self.w.reserve(16);
            self.w.put(len, bits & ((1u64 << len) - 1) as u32);
        }
    }
    fn sym(&mut self, t: &[u32; 256], s: u32) -> Result<(), Error> {
        let e = t[s as usize];
        if e == 0 {
            return Err(Error("symbol without code".into()));
        }
        self.put(e & 255, e >> 8);
        Ok(())
    }
    fn pad(&mut self) {
        if self.w.n > 0 {
            let p = 8 - self.w.n;
            let bits = match self.k.pad {
                0 => (1 << p) - 1,
                1 => 0,
                _ => self.rng.below(1 << p) as u32,
            };
            self.put(p, bits);
        }
    }
}

/// Stuff, insert RST markers (with optional fill bytes) and trailing RSTs.
fn finish(mut w: W, mut seg_ends: Vec<usize>, out: &mut Vec<u8>) {
    w.pad();
    for _ in 0..w.k.trailing_rst {
        seg_ends.push(w.w.pos);
    }
    let mut start = 0;
    for (r, &end) in seg_ends.iter().enumerate() {
        jpeg::stuff_into(&w.w.buf[start..end], out);
        if w.rng.chance(w.k.fill) {
            out.push(0xFF);
        }
        out.extend_from_slice(&[0xFF, 0xD0 + (r as u8 & 7)]);
        start = end;
    }
    jpeg::stuff_into(&w.w.buf[start..w.w.pos], out);
}

/// Per scan component: its DC and AC encoding tables (zeros where not used).
type Codes = Vec<([u32; 256], [u32; 256])>;

fn tables(s: &Scan, dc: bool, ac: bool) -> Result<Codes, Error> {
    s.comps
        .iter()
        .map(|sc| {
            let d = if dc { EncTable::new(sc.dc.as_ref().ok_or(Error("no DC table".into()))?).0 } else { [0; 256] };
            let a = if ac { EncTable::new(sc.ac.as_ref().ok_or(Error("no AC table".into()))?).0 } else { [0; 256] };
            Ok((d, a))
        })
        .collect()
}

/// Iterate the blocks of a scan in MCU order: f(mcu index, scan component index, block).
fn for_blocks(
    j: &Jpeg,
    s: &Scan,
    coefs: &[Coefs],
    mut f: impl FnMut(usize, usize, &[i16; 64]) -> Result<(), Error>,
) -> Result<(), Error> {
    let layout = jpeg::scan_layout(j, s);
    for m in 0..s.mcux * s.mcuy {
        let (mx, my) = (m % s.mcux, m / s.mcux);
        for (si, &(ci, ch, cv)) in layout.iter().enumerate() {
            let o = &coefs[ci];
            for v in 0..cv {
                for h in 0..ch {
                    let (bx, by) = (mx * ch + h, my * cv + v);
                    f(m, si, &o.blocks[by * o.bw + bx])?;
                }
            }
        }
    }
    Ok(())
}

fn seq_scan(j: &Jpeg, s: &Scan, coefs: &[Coefs], w: &mut W, seg_ends: &mut Vec<usize>) -> Result<(), Error> {
    let tabs = tables(s, true, true)?;
    let nmcu = s.mcux * s.mcuy;
    let ri = if s.ri > 0 { s.ri } else { nmcu };
    let mut pred = [0i32; 4];
    let mut last_m = usize::MAX;
    for_blocks(j, s, coefs, |m, si, blk| {
        if m != last_m && m > 0 && m % ri == 0 {
            w.pad();
            seg_ends.push(w.w.pos);
            pred = [0; 4];
        }
        last_m = m;
        let (dt, at) = (&tabs[si].0, &tabs[si].1);
        let (sz, bits) = size_bits(blk[0] as i32 - pred[si]);
        pred[si] = blk[0] as i32;
        w.sym(dt, sz)?;
        w.put(sz, bits);
        let mut run = 0u32;
        let mut k = 1;
        while k < 64 {
            let v = blk[k] as i32;
            if v == 0 {
                run += 1;
            } else {
                while run >= 16 {
                    w.sym(at, 0xF0)?;
                    run -= 16;
                }
                let (sz, bits) = size_bits(v);
                w.sym(at, run << 4 | sz)?;
                w.put(sz, bits);
                run = 0;
            }
            k += 1;
        }
        if run > 0 {
            // non-canonical: ZRLs for (some of) the trailing zeros before the EOB
            if run >= 16 && at[0xF0] != 0 && w.rng.chance(w.k.trailing_zrl) {
                let z = 1 + w.rng.below((run / 16) as u64) as u32;
                for _ in 0..z {
                    w.sym(at, 0xF0)?;
                }
                run -= 16 * z;
            }
            if run > 0 {
                w.sym(at, 0x00)?;
            }
        }
        Ok(())
    })
}

fn prog_scan(j: &Jpeg, s: &Scan, coefs: &[Coefs], w: &mut W, seg_ends: &mut Vec<usize>) -> Result<(), Error> {
    let (ss, se, al) = (s.ss as usize, s.se as usize, s.al as u32);
    let refine = s.ah != 0;
    let dc = ss == 0;
    let tabs = tables(s, dc && !refine, !dc)?;
    let nmcu = s.mcux * s.mcuy;
    let ri = if s.ri > 0 { s.ri } else { nmcu };
    let mut pred = [0i32; 4];
    let mut eobrun = 0u32;
    let mut be: Vec<u8> = Vec::new(); // buffered correction bits of the EOB run
    let mut last_m = usize::MAX;
    // emit the pending EOB run (AC scans have a single component: table 0)
    fn flush(w: &mut W, at: &[u32; 256], eobrun: &mut u32, be: &mut Vec<u8>) -> Result<(), Error> {
        if *eobrun > 0 {
            let nbits = 31 - eobrun.leading_zeros();
            w.sym(at, nbits << 4)?;
            w.put(nbits, *eobrun);
            *eobrun = 0;
            for &b in be.iter() {
                w.put(1, b as u32);
            }
            be.clear();
        }
        Ok(())
    }
    let at0 = tabs[0].1;
    for_blocks(j, s, coefs, |m, si, blk| {
        if m != last_m && m > 0 && m % ri == 0 {
            flush(w, &at0, &mut eobrun, &mut be)?;
            w.pad();
            seg_ends.push(w.w.pos);
            pred = [0; 4];
        }
        last_m = m;
        if dc {
            if refine {
                w.put(1, ((blk[0] as i32) >> al) as u32 & 1);
            } else {
                let t = (blk[0] as i32) >> al;
                let (sz, bits) = size_bits(t - pred[si]);
                pred[si] = t;
                w.sym(&tabs[si].0, sz)?;
                w.put(sz, bits);
            }
            return Ok(());
        }
        let at = &tabs[0].1;
        let mut blk_in_run = false; // this block's tail is covered by the EOB run
        if !refine {
            let mut r = 0u32;
            for k in ss..=se {
                let c = blk[k] as i32;
                let a = c.abs() >> al;
                if a == 0 {
                    r += 1;
                    continue;
                }
                flush(w, at, &mut eobrun, &mut be)?;
                while r > 15 {
                    w.sym(at, 0xF0)?;
                    r -= 16;
                }
                let nb = 32 - a.leading_zeros();
                w.sym(at, r << 4 | nb)?;
                w.put(nb, if c < 0 { !a as u32 } else { a as u32 });
                r = 0;
            }
            if r > 0 {
                // non-canonical: ZRLs for some trailing zeros (the rest goes to the EOB run)
                if r >= 16 && at[0xF0] != 0 && w.rng.chance(w.k.trailing_zrl) {
                    flush(w, at, &mut eobrun, &mut be)?;
                    let z = 1 + w.rng.below((r / 16) as u64) as u32;
                    for _ in 0..z {
                        w.sym(at, 0xF0)?;
                    }
                    r -= 16 * z;
                }
                if r > 0 {
                    eobrun += 1;
                    blk_in_run = true;
                }
            }
        } else {
            let absv: Vec<i32> = (0..64).map(|k| (blk[k] as i32).abs() >> al).collect();
            let eob = (ss..=se).rev().find(|&k| absv[k] == 1).unwrap_or(0);
            // ZRLs past the last new coefficient (only if the table has a ZRL code)
            let relaxed = at[0xF0] != 0 && w.rng.chance(w.k.trailing_zrl);
            let mut r = 0u32;
            let mut br: Vec<u8> = Vec::new();
            for k in ss..=se {
                let t = absv[k];
                if t == 0 {
                    r += 1;
                    continue;
                }
                while r > 15 && (k <= eob || relaxed) {
                    flush(w, at, &mut eobrun, &mut be)?;
                    w.sym(at, 0xF0)?;
                    r -= 16;
                    for &b in &br {
                        w.put(1, b as u32);
                    }
                    br.clear();
                }
                if t > 1 {
                    br.push((t & 1) as u8);
                    continue;
                }
                flush(w, at, &mut eobrun, &mut be)?;
                w.sym(at, r << 4 | 1)?;
                w.put(1, (blk[k] >= 0) as u32);
                for &b in &br {
                    w.put(1, b as u32);
                }
                br.clear();
                r = 0;
            }
            if r > 0 || !br.is_empty() {
                eobrun += 1;
                be.extend_from_slice(&br);
                blk_in_run = true;
            }
        }
        // flush early (non-canonically) only if the table can code this run length; the run
        // continues otherwise, up to libjpeg's own limits
        let codable = eobrun > 0 && at[((31 - eobrun.leading_zeros()) << 4) as usize] != 0;
        let early = codable && (eobrun >= w.k.eob_max || be.len() > w.k.be_max || w.rng.chance(w.k.eob_flush));
        if blk_in_run && (early || eobrun == 0x7FFF || be.len() > 937) {
            flush(w, at, &mut eobrun, &mut be)?;
        }
        Ok(())
    })?;
    if !dc {
        flush(w, &at0, &mut eobrun, &mut be)?;
    }
    Ok(())
}

/// Decode `d` and re-encode all of its scans with randomly drawn non-canonical choices.
/// Returns the new file and a description of the choices, or None if `d` is not fully decodable.
pub fn reencode(d: &[u8], rng: &mut Rng) -> Option<(Vec<u8>, String)> {
    let j = jpeg::parse(d).ok()?;
    let mut coefs = Vec::new();
    let mut clean = Clean::default();
    let infos = if j.progressive {
        progressive::decode_all(&j, d, &mut coefs, &mut clean).ok()?
    } else {
        jpeg::decode_all(&j, d, Order::Zigzag, &mut coefs, &mut clean).ok()?
    };
    if infos.iter().any(|i| i.raw || i.truncated.is_some()) {
        return None;
    }
    let mut out = Vec::with_capacity(d.len() + 1024);
    let mut desc = String::new();
    let mut p = 0;
    for s in &j.scans {
        out.extend_from_slice(&d[p..s.start]);
        p = s.end;
        let k = Knobs::draw(rng);
        desc += &format!("[{k:?}]");
        let mut w = W { w: BitOut { acc: 0, n: 0, buf: vec![0; 1 << 16], pos: 0 }, rng, k };
        let mut seg_ends = Vec::new();
        let r = if j.progressive {
            prog_scan(&j, s, &coefs, &mut w, &mut seg_ends)
        } else {
            seq_scan(&j, s, &coefs, &mut w, &mut seg_ends)
        };
        r.ok()?;
        finish(w, seg_ends, &mut out);
    }
    out.extend_from_slice(&d[p..]);
    Some((out, desc))
}

/// Coefficients of a JPEG (for checking that a variant decodes identically).
pub fn coefficients(d: &[u8]) -> Option<Vec<Coefs>> {
    let j = jpeg::parse(d).ok()?;
    let mut coefs = Vec::new();
    let mut clean = Clean::default();
    if j.progressive {
        progressive::decode_all(&j, d, &mut coefs, &mut clean).ok()?;
    } else {
        jpeg::decode_all(&j, d, Order::Zigzag, &mut coefs, &mut clean).ok()?;
    }
    Some(coefs)
}
