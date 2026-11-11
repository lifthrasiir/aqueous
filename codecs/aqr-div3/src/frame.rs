//! The syntax of I and P frames, walked once for both directions (see `io`).
use crate::tables::*;
use alloc::vec;
use alloc::vec::Vec;
use aqr_walk::bits::{BitReader, BitWriter};
use aqr_walk::io::{Check, Dec, ESC, Enc, Io, Vlc};
use aqr_walk::model::Model;
use aqr_walk::rc::Coder;

pub use aqr_walk::Error as FrameError;

type R<T> = Result<T, FrameError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    I,
    P,
}

/// A table of AC symbols: the VLC, what each symbol means, and the escape-coded extensions.
struct AcTab {
    vlc: Vlc,
    info: &'static [(u8, u8, u8)],
    /// By last, then run: the highest level of the table.
    maxl: [[u8; 65]; 2],
    /// By last, then level: the highest run of the table (-1: none).
    maxr: [[i16; 128]; 2],
}

impl AcTab {
    fn new(codes: &'static [(u32, u8, u16)], info: &'static [(u8, u8, u8)]) -> AcTab {
        let mut maxl = [[0u8; 65]; 2];
        let mut maxr = [[-1i16; 128]; 2];
        for &(run, level, last) in info {
            let (r, l, k) = (run as usize, level as usize, last as usize);
            maxl[k][r] = maxl[k][r].max(level);
            if l < 128 {
                maxr[k][l] = maxr[k][l].max(run as i16);
            }
        }
        AcTab { vlc: Vlc::new(codes), info, maxl, maxr }
    }
}

/// The tables and the geometry: all the syntax walk reads.
struct Syntax {
    w: usize,
    h: usize,
    dc: [Vlc; 4],
    cbp_i: Vlc,
    mb_p: Vlc,
    mv: [Vlc; 2],
    intra: [AcTab; 3],
    inter: [AcTab; 3],
}

pub struct Codec {
    model: Model,
    syn: Syntax,
}

// Element ids, the top byte of a context.
const E_SKIP: u32 = 1 << 24;
const E_MBT: u32 = 2 << 24;
const E_MV: u32 = 3 << 24;
const E_AC: u32 = 4 << 24;
const E_DC: u32 = 5 << 24;
const E_AP: u32 = 6 << 24;
const E_CBPI: u32 = 7 << 24;
const E_ESC: u32 = 10 << 24;
const E_TAIL: u32 = 9 << 24;
const E_OK: u32 = 11 << 24;

fn d012<I: Io>(io: &mut I) -> R<u32> {
    if io.raw(1)? == 0 {
        return Ok(0);
    }
    Ok(1 + io.raw(1)?)
}

fn read_dc<I: Io>(io: &mut I, tab: &Vlc, ctx: u32) -> R<()> {
    let s = io.vlc(tab, E_DC | ctx)?;
    let level = if s == ESC { io.raw(8)? } else { s as u32 };
    if level != 0 {
        io.raw(1)?;
    }
    Ok(())
}

/// The AC symbols of one block. `kind`: 0 inter luma, 1 inter chroma, 2 intra luma, 3 intra chroma. `maxi`: the
/// highest scan index plus one (63 for intra blocks, which start after the DC; 64 for inter blocks).
fn ac_block<I: Io>(io: &mut I, t: &AcTab, kind: u32, maxi: u32) -> R<()> {
    let mut i = 0u32;
    let mut prev = 0u32;
    loop {
        let ctx = E_AC | kind | i.min(15) << 2 | prev.min(2) << 6;
        let s = io.vlc(&t.vlc, ctx)?;
        let (run, level, last);
        if s == ESC {
            if io.flag(E_ESC | kind)? == 1 {
                let s2 = io.vlc(&t.vlc, ctx | 1 << 8)?;
                if s2 == ESC {
                    return Err(FrameError::Parse);
                }
                let (r, l, la) = t.info[s2 as usize];
                run = r as u32;
                level = l as u32 + t.maxl[la as usize][r as usize] as u32;
                last = la;
            } else if io.flag(E_ESC | 4 | kind)? == 1 {
                let s2 = io.vlc(&t.vlc, ctx | 2 << 8)?;
                if s2 == ESC {
                    return Err(FrameError::Parse);
                }
                let (r, l, la) = t.info[s2 as usize];
                let m = t.maxr[la as usize][(l as usize).min(127)];
                if m < 0 {
                    return Err(FrameError::Parse);
                }
                run = r as u32 + m as u32;
                level = l as u32;
                last = la;
            } else {
                last = io.raw(1)? as u8;
                run = io.raw(6)?;
                let v = io.raw(8)?;
                level = if v >= 128 { 256 - v } else { v };
                i += run + 1;
                if i > maxi {
                    return Err(FrameError::Parse);
                }
                prev = level;
                if last != 0 {
                    return Ok(());
                }
                continue;
            }
            io.raw(1)?;
        } else {
            let (r, l, la) = t.info[s as usize];
            run = r as u32;
            level = l as u32;
            last = la;
            io.raw(1)?;
        }
        i += run + 1;
        if i > maxi {
            return Err(FrameError::Parse);
        }
        prev = level;
        if last != 0 {
            return Ok(());
        }
    }
}

struct Geom {
    mbw: usize,
    mbh: usize,
}

impl Syntax {
    fn new(width: usize, height: usize) -> Syntax {
        let a = |v: &'static [(u32, u8, u16)], i: &'static [(u8, u8, u8)]| AcTab::new(v, i);
        Syntax {
            w: width,
            h: height,
            dc: [Vlc::new(&DC_LUMA0), Vlc::new(&DC_LUMA1), Vlc::new(&DC_CHROMA0), Vlc::new(&DC_CHROMA1)],
            cbp_i: Vlc::new(&CBP_I),
            mb_p: Vlc::new(&MB_P),
            mv: [Vlc::new(&MV0_VLC), Vlc::new(&MV1_VLC)],
            intra: [
                a(&AC_INTRA0_VLC, &AC_INTRA0_INFO),
                a(&AC_INTRA1_VLC, &AC_INTRA1_INFO),
                a(&AC_INTRA2_VLC, &AC_INTRA2_INFO),
            ],
            inter: [a(&AC_B0_VLC, &AC_B0_INFO), a(&AC_B1_VLC, &AC_B1_INFO), a(&AC_B2_VLC, &AC_B2_INFO)],
        }
    }

    /// Walks a frame: returns its kind. The two header bits tell which syntax follows.
    fn walk<I: Io>(&self, io: &mut I) -> R<FrameKind> {
        let g = Geom { mbw: self.w.div_ceil(16), mbh: self.h.div_ceil(16) };
        let kind = io.raw(2)?;
        let k = match kind {
            0 => {
                self.walk_i(io, &g)?;
                FrameKind::I
            }
            1 => {
                self.walk_p(io, &g)?;
                FrameKind::P
            }
            _ => return Err(FrameError::Parse),
        };
        // What is left: the extension header and padding, coded bit by bit.
        let left = io.total_bits().checked_sub(io.done()).ok_or(FrameError::Parse)?;
        if left > if k == FrameKind::I { 40 } else { 16 } {
            return Err(FrameError::Parse);
        }
        for j in 0..left {
            io.flag(E_TAIL | j.min(63) as u32)?;
        }
        Ok(k)
    }

    // (the blocks of a macroblock by index, as the syntax has them)
    #[allow(clippy::needless_range_loop)]
    fn walk_i<I: Io>(&self, io: &mut I, g: &Geom) -> R<()> {
        io.raw(5)?; // q
        io.raw(5)?; // slice code
        let rlc = d012(io)? as usize;
        let rl = d012(io)? as usize;
        let dc = io.raw(1)? as usize;
        let stride = g.mbw * 2 + 2;
        let mut flags = vec![0u8; stride * (g.mbh * 2 + 2)];
        for my in 0..g.mbh {
            for mx in 0..g.mbw {
                let sym = io.vlc(&self.cbp_i, E_CBPI)? as u32;
                let mut cb = [0u8; 6];
                for i in 0..6 {
                    let mut bit = (sym >> (5 - i) & 1) as u8;
                    if i < 4 {
                        let x = mx * 2 + (i & 1) + 1;
                        let y = my * 2 + (i >> 1) + 1;
                        let a = flags[y * stride + x - 1];
                        let b = flags[(y - 1) * stride + x - 1];
                        let c = flags[(y - 1) * stride + x];
                        bit ^= if b == c { a } else { c };
                        flags[y * stride + x] = bit;
                    }
                    cb[i] = bit;
                }
                io.flag(E_AP)?;
                for i in 0..6 {
                    let dct: &Vlc = match (i < 4, dc) {
                        (true, 0) => &self.dc[0],
                        (true, _) => &self.dc[1],
                        (false, 0) => &self.dc[2],
                        (false, _) => &self.dc[3],
                    };
                    read_dc(io, dct, (i < 4) as u32)?;
                    if cb[i] != 0 {
                        if i < 4 {
                            ac_block(io, &self.intra[rl], 2, 63)?
                        } else {
                            ac_block(io, &self.inter[rlc], 3, 63)?
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn walk_p<I: Io>(&self, io: &mut I, g: &Geom) -> R<()> {
        io.raw(5)?; // q
        let use_skip = io.raw(1)?;
        let rl = d012(io)? as usize;
        let dc = io.raw(1)? as usize;
        let mvt = io.raw(1)? as usize;
        let mvtab: &Vlc = &self.mv[mvt];
        let mvvec: &'static [(i8, i8)] = if mvt == 0 { &MV0_VEC } else { &MV1_VEC };
        // Per macroblock: 0 skipped, 1 inter, 2 intra; and whether its vector is zero.
        let mut kind = vec![0u8; g.mbw * g.mbh];
        let mut mvz = vec![0u8; g.mbw * g.mbh];
        for my in 0..g.mbh {
            for mx in 0..g.mbw {
                let at = my * g.mbw + mx;
                let lk = if mx > 0 { kind[at - 1] } else { 0 } as u32;
                let uk = if my > 0 { kind[at - g.mbw] } else { 0 } as u32;
                let nctx = lk | uk << 2;
                if use_skip != 0 && io.flag(E_SKIP | nctx)? == 1 {
                    continue;
                }
                let sym = io.vlc(&self.mb_p, E_MBT | nctx)? as u32;
                let intra = sym & 64 != 0;
                let cb = |i: usize| sym >> (5 - i) & 1 != 0;
                if intra {
                    kind[at] = 2;
                    io.flag(E_AP)?;
                    for i in 0..6 {
                        let dct: &Vlc = match (i < 4, dc) {
                            (true, 0) => &self.dc[0],
                            (true, _) => &self.dc[1],
                            (false, 0) => &self.dc[2],
                            (false, _) => &self.dc[3],
                        };
                        read_dc(io, dct, (i < 4) as u32)?;
                        if cb(i) {
                            if i < 4 {
                                ac_block(io, &self.intra[rl], 2, 63)?
                            } else {
                                ac_block(io, &self.inter[rl], 3, 63)?
                            }
                        }
                    }
                } else {
                    kind[at] = 1;
                    let lz = if mx > 0 { mvz[at - 1] } else { 0 } as u32;
                    let uz = if my > 0 { mvz[at - g.mbw] } else { 0 } as u32;
                    let s = io.vlc(mvtab, E_MV | lz | uz << 2)?;
                    if s == ESC {
                        io.raw(12)?;
                        mvz[at] = 2;
                    } else {
                        let v = mvvec[s as usize];
                        mvz[at] = if v == (0, 0) { 1 } else { 2 };
                    }
                    for i in 0..6 {
                        if cb(i) {
                            ac_block(io, &self.inter[rl], (i >= 4) as u32, 64)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

impl Codec {
    pub fn new(width: usize, height: usize) -> Codec {
        Codec { model: Model::new(22), syn: Syntax::new(width, height) }
    }

    /// Forgets what the model has learned (the start of a chunk), in place.
    pub fn reset(&mut self) {
        self.model.reset();
    }

    /// Whether the frame can be coded: it must parse and leave only a short tail.
    pub fn check(&self, data: &[u8]) -> bool {
        let mut c = Check { r: BitReader::new(data) };
        self.syn.walk(&mut c).is_ok()
    }

    /// Codes a frame: a flag, then the elements (or the bytes, if the frame does not parse).
    pub fn encode<C: Coder>(&mut self, rc: &mut C, data: &[u8]) -> Option<FrameKind> {
        let ok = self.check(data);
        rc.bit(self.model.slot(E_OK, 0), ok as u32);
        if !ok {
            for &b in data {
                for k in (0..8).rev() {
                    rc.flat((b >> k & 1) as u32);
                }
            }
            return None;
        }
        let mut e = Enc { r: BitReader::new(data), rc, m: &mut self.model };
        self.syn.walk(&mut e).ok()
    }

    /// Decodes a frame of `len` bytes into `out` (its capacity is used: reserve `len` + 8 bytes to take no more).
    pub fn decode<C: Coder>(&mut self, rc: &mut C, len: usize, out: Vec<u8>) -> R<Vec<u8>> {
        let ok = rc.bit(self.model.slot(E_OK, 0), 0);
        if ok == 0 {
            let mut out = out;
            out.clear();
            for _ in 0..len {
                let mut v = 0u8;
                for _ in 0..8 {
                    v = v << 1 | rc.flat(0) as u8;
                }
                out.push(v);
            }
            return Ok(out);
        }
        let mut d = Dec { w: BitWriter::with_vec(out), total: len * 8, rc, m: &mut self.model };
        self.syn.walk(&mut d)?;
        let bits = d.w.bits();
        if bits != len * 8 {
            return Err(FrameError::Parse);
        }
        Ok(d.w.finish())
    }
}
