//! AC-3 (ATSC A/52) syncframes walked down to their quantized mantissas, once for both directions (see `io`): the side
//! information, the exponents and the mantissas are coded under small contexts; the checksums are derived, not coded.
//! The bit allocation (7.2) is run on the exponents to know how many bits each mantissa has.
use crate::ac3_tables::*;
use alloc::boxed::Box;
use alloc::vec::Vec;
use aqr_walk::Error as FrameError;
use aqr_walk::bits::{BitReader, BitWriter};
use aqr_walk::io::{Check, Dec, Enc, Io};
use aqr_walk::model::Model;
use aqr_walk::rc::Coder;

type R<T> = Result<T, FrameError>;

const NFCHANS: [usize; 8] = [2, 1, 2, 3, 3, 4, 4, 5];
const MANT_BITS: [u32; 16] = [0, 5, 7, 3, 7, 4, 5, 6, 7, 8, 9, 10, 11, 12, 14, 16];

/// Syncframe length in bytes from its fscod and frmsizecod.
pub fn frame_bytes(fscod: u32, frmsizecod: u32) -> Option<usize> {
    if fscod >= 3 || frmsizecod >= 38 {
        return None;
    }
    Some(FRAME_WORDS[frmsizecod as usize][fscod as usize] as usize * 2)
}

/// The length of the syncframe at the start of `d` (its header must be well formed), if it is one.
pub fn frame_len(d: &[u8]) -> Option<usize> {
    if d.len() < 5 || d[0] != 0x0B || d[1] != 0x77 {
        return None;
    }
    frame_bytes((d[4] >> 6) as u32, (d[4] & 0x3f) as u32)
}

const POLY: u16 = 0x8005;

fn crc_byte(mut crc: u16, byte: u8) -> u16 {
    crc ^= (byte as u16) << 8;
    for _ in 0..8 {
        crc = if crc & 0x8000 != 0 { crc << 1 ^ POLY } else { crc << 1 };
    }
    crc
}

fn crc16(d: &[u8]) -> u16 {
    d.iter().fold(0, |c, &b| crc_byte(c, b))
}

/// One bit of the CRC register stepped back (the register before the bit was shifted in, the data bit being xored
/// before the shifts of a byte).
fn crc_unstep(s: u16) -> u16 {
    let top = s & 1;
    (s ^ if top != 0 { POLY } else { 0 }) >> 1 | top << 15
}

/// The 16-bit `c` whose two bytes, from a zero register, leave the register `s`.
fn crc_head_solve(mut s: u16) -> u16 {
    // the register after two bytes is linear in them: a basis with one pivot bit each, from the images of the unit vectors
    let mut basis = [(0u16, 0u16); 16];
    for i in 0..16 {
        let mut m = 1u16 << i;
        let mut v = crc16(&[(m >> 8) as u8, m as u8]);
        for bit in (0..16).rev() {
            if v >> bit & 1 == 0 {
                continue;
            }
            if basis[bit].0 != 0 {
                v ^= basis[bit].0;
                m ^= basis[bit].1;
            } else {
                basis[bit] = (v, m);
                break;
            }
        }
    }
    let mut c = 0u16;
    for bit in (0..16).rev() {
        if s >> bit & 1 != 0 {
            s ^= basis[bit].0;
            c ^= basis[bit].1;
        }
    }
    c
}

/// The value of crc1 such that the CRC of the bytes from the syncword's end to the 5/8 point is zero.
fn crc1_of(frame: &[u8]) -> u16 {
    let words = frame.len() / 2;
    let end = ((words >> 1) + (words >> 3)) * 2;
    // the register after crc1's two bytes must be the one from which the rest leads to zero: stepped back from 0
    let mut s = 0u16;
    for &byte in frame[4..end].iter().rev() {
        for _ in 0..8 {
            s = crc_unstep(s);
        }
        s ^= (byte as u16) << 8;
    }
    crc_head_solve(s)
}

/// Whether the checksums of a whole syncframe are right.
pub fn crcs_ok(frame: &[u8]) -> bool {
    let words = frame.len() / 2;
    let n58 = ((words >> 1) + (words >> 3)) * 2;
    crc16(&frame[2..n58]) == 0 && crc16(&frame[2..]) == 0
}

/// Sets the checksums of a syncframe whose other bits are written: crc1 first, as crc2 covers it.
fn set_crcs(frame: &mut [u8]) {
    let n = frame.len();
    let c1 = crc1_of(frame);
    frame[2] = (c1 >> 8) as u8;
    frame[3] = c1 as u8;
    let c2 = crc16(&frame[2..n - 2]);
    frame[n - 2] = (c2 >> 8) as u8;
    frame[n - 1] = c2 as u8;
}

fn logadd(a: i32, b: i32) -> i32 {
    let c = a - b;
    let addr = ((c.unsigned_abs() >> 1).min(255)) as usize;
    if c >= 0 { a + LATAB[addr] } else { b + LATAB[addr] }
}

fn calc_lowcomp(mut a: i32, b0: i32, b1: i32, bin: usize) -> i32 {
    if bin < 7 {
        if b0 + 256 == b1 {
            a = 384;
        } else if b0 > b1 {
            a = (a - 64).max(0);
        }
    } else if bin < 20 {
        if b0 + 256 == b1 {
            a = 320;
        } else if b0 > b1 {
            a = (a - 64).max(0);
        }
    } else {
        a = (a - 128).max(0);
    }
    a
}

/// Band of each bin.
fn masktab(bin: usize) -> usize {
    // (the bands are contiguous: a short search is enough and keeps the table out of the module)
    let mut b = 0;
    while b < 49 && (BNDTAB[b + 1] as usize) <= bin {
        b += 1;
    }
    b
}

struct Alloc {
    sdecay: i32,
    fdecay: i32,
    sgain: i32,
    dbknee: i32,
    floor: i32,
    fscod: usize,
}

/// The bit allocation pointers of bins `start..end` from their exponents (`exp` by bin).
#[allow(clippy::too_many_arguments)]
fn bit_alloc(
    a: &Alloc,
    exp: &[u8; 256],
    start: usize,
    end: usize,
    fgain: i32,
    snroffset: i32,
    fastleak0: i32,
    slowleak0: i32,
    delta: &[(u8, u8, u8)],
    bap: &mut [u8; 256],
) {
    let mut psd = [0i32; 256];
    for b in start..end {
        psd[b] = 3072 - ((exp[b] as i32) << 7);
    }
    let mut bndpsd = [0i32; 50];
    let mut j = start;
    let mut k = masktab(start);
    loop {
        let lastbin = (BNDTAB[k] as usize + BNDSZ[k] as usize).min(end);
        bndpsd[k] = psd[j];
        j += 1;
        while j < lastbin {
            bndpsd[k] = logadd(bndpsd[k], psd[j]);
            j += 1;
        }
        k += 1;
        if end <= lastbin {
            break;
        }
    }
    let bndstrt = masktab(start);
    let bndend = masktab(end - 1) + 1;
    let mut excite = [0i32; 50];
    let (mut fastleak, mut slowleak);
    let begin;
    if bndstrt == 0 {
        let mut lowcomp = 0;
        lowcomp = calc_lowcomp(lowcomp, bndpsd[0], bndpsd[1], 0);
        excite[0] = bndpsd[0] - fgain - lowcomp;
        lowcomp = calc_lowcomp(lowcomp, bndpsd[1], bndpsd[2], 1);
        excite[1] = bndpsd[1] - fgain - lowcomp;
        let mut b7 = 7;
        fastleak = 0;
        slowleak = 0;
        for bin in 2..7 {
            if bndend != 7 || bin != 6 {
                lowcomp = calc_lowcomp(lowcomp, bndpsd[bin], bndpsd[bin + 1], bin);
            }
            fastleak = bndpsd[bin] - fgain;
            slowleak = bndpsd[bin] - a.sgain;
            excite[bin] = fastleak - lowcomp;
            if (bndend != 7 || bin != 6) && bndpsd[bin] <= bndpsd[bin + 1] {
                b7 = bin + 1;
                break;
            }
        }
        for bin in b7..bndend.min(22) {
            if bndend != 7 || bin != 6 {
                lowcomp = calc_lowcomp(lowcomp, bndpsd[bin], bndpsd[bin + 1], bin);
            }
            fastleak -= a.fdecay;
            fastleak = fastleak.max(bndpsd[bin] - fgain);
            slowleak -= a.sdecay;
            slowleak = slowleak.max(bndpsd[bin] - a.sgain);
            excite[bin] = (fastleak - lowcomp).max(slowleak);
        }
        begin = 22;
    } else {
        begin = bndstrt;
        fastleak = fastleak0;
        slowleak = slowleak0;
    }
    for bin in begin..bndend {
        fastleak -= a.fdecay;
        fastleak = fastleak.max(bndpsd[bin] - fgain);
        slowleak -= a.sdecay;
        slowleak = slowleak.max(bndpsd[bin] - a.sgain);
        excite[bin] = fastleak.max(slowleak);
    }
    let mut mask = [0i32; 50];
    for bin in bndstrt..bndend {
        if bndpsd[bin] < a.dbknee {
            excite[bin] += (a.dbknee - bndpsd[bin]) >> 2;
        }
        mask[bin] = excite[bin].max(HTH[bin][a.fscod]);
    }
    let mut band = 0usize;
    for &(off, len, ba) in delta {
        band += off as usize;
        let d = if ba >= 4 { (ba as i32 - 3) << 7 } else { (ba as i32 - 4) << 7 };
        for _ in 0..len {
            if band >= bndstrt && band < bndend {
                mask[band] += d;
            }
            band += 1;
        }
    }
    let mut i = start;
    let mut j = masktab(start);
    loop {
        let lastbin = (BNDTAB[j] as usize + BNDSZ[j] as usize).min(end);
        mask[j] -= snroffset;
        mask[j] -= a.floor;
        if mask[j] < 0 {
            mask[j] = 0;
        }
        mask[j] &= 0x1fe0;
        mask[j] += a.floor;
        while i < lastbin {
            let address = ((psd[i] - mask[j]) >> 5).clamp(0, 63);
            bap[i] = BAPTAB[address as usize];
            i += 1;
        }
        j += 1;
        if end <= lastbin {
            break;
        }
    }
}

/// Exponents of a channel from its absolute first exponent and `ngrps` groups of three differences (7 bits each).
/// `out[bin]` is set for the fbw and lfe channels (`at` = 0: list entry 0 is the exponent of bin 0); for the coupling
/// channel list entry n + 1 is the exponent of bin `at + n`.
#[allow(clippy::too_many_arguments)]
fn read_exps<I: Io>(
    io: &mut I,
    last: &mut Ctx,
    kind: u32,
    absexp: u32,
    ngrps: usize,
    grpsize: usize,
    out: &mut [u8; 256],
    at: usize,
) -> R<()> {
    let mut prev = absexp as i32;
    if at == 0 {
        out[0] = absexp as u8;
    }
    let mut idx = 1usize;
    let mut pg = 0u32;
    for g in 0..ngrps {
        let above = last.grp[kind as usize][g.min(63)] as u32 / 25;
        let ctx = 0x4000 + kind * 64 + (pg / 25).min(4) + above.min(4) * 5;
        let v = io.field(7, ctx)?;
        if v >= 125 {
            return Err(FrameError::Parse);
        }
        last.grp[kind as usize][g.min(63)] = v as u8;
        pg = v;
        for d in [v as i32 / 25 - 2, v as i32 % 25 / 5 - 2, v as i32 % 5 - 2] {
            prev += d;
            if !(0..=24).contains(&prev) {
                return Err(FrameError::Parse);
            }
            for _ in 0..grpsize {
                let bin = if at == 0 { idx } else { at + idx - 1 };
                if bin < 256 {
                    out[bin] = prev as u8;
                }
                idx += 1;
            }
        }
    }
    Ok(())
}

/// What the contexts of the coding remember: the previous value of each field, the last exponent groups of each channel,
/// the last mantissa of each bap.
#[derive(Clone)]
pub struct Ctx {
    field: [u32; 192],
    grp: [[u8; 64]; 7],
    mant: [u32; 16],
}

impl Default for Ctx {
    fn default() -> Self {
        Ctx { field: [0; 192], grp: [[0; 64]; 7], mant: [0; 16] }
    }
}

fn fld<I: Io>(io: &mut I, last: &mut Ctx, id: usize, n: u32, blk0: bool) -> R<u32> {
    let ctx = (id as u32) << 20 ^ last.field[id].wrapping_mul(0x9E37_79B1) ^ blk0 as u32;
    let v = io.field(n, ctx)?;
    last.field[id] = v;
    Ok(v)
}

/// Bytes of the state of a block while a frame is walked.
pub const BLK_BYTES: usize = core::mem::size_of::<Blk>();

#[derive(Clone)]
struct Blk {
    cplinu: bool,
    chincpl: [bool; 5],
    phsflginu: bool,
    cplbegf: u32,
    cplendf: u32,
    ncplbnd: usize,
    cplexpstr: u32,
    chexpstr: [u32; 5],
    lfeexpstr: u32,
    chbwcod: [u32; 5],
    exps: [[u8; 256]; 5],
    cplexp: [u8; 256],
    lfeexp: [u8; 256],
    sdcycod: u32,
    fdcycod: u32,
    sgaincod: u32,
    dbpbcod: u32,
    floorcod: u32,
    csnroffst: u32,
    cplfsnroffst: u32,
    cplfgaincod: u32,
    fsnroffst: [u32; 5],
    fgaincod: [u32; 5],
    lfefsnroffst: u32,
    lfefgaincod: u32,
    cplfleak: u32,
    cplsleak: u32,
    deltbae: [u32; 5],
    delta: [[(u8, u8, u8); 8]; 5],
    ndelta: [usize; 5],
    cpldeltbae: u32,
    cpldelta: [(u8, u8, u8); 8],
    ncpldelta: usize,
}

impl Blk {
    fn new() -> Box<Blk> {
        Box::new(Blk {
            cplinu: false,
            chincpl: [false; 5],
            phsflginu: false,
            cplbegf: 0,
            cplendf: 0,
            ncplbnd: 0,
            cplexpstr: 0,
            chexpstr: [0; 5],
            lfeexpstr: 0,
            chbwcod: [0; 5],
            exps: [[0; 256]; 5],
            cplexp: [0; 256],
            lfeexp: [0; 256],
            sdcycod: 0,
            fdcycod: 0,
            sgaincod: 0,
            dbpbcod: 0,
            floorcod: 0,
            csnroffst: 0,
            cplfsnroffst: 0,
            cplfgaincod: 0,
            fsnroffst: [0; 5],
            fgaincod: [0; 5],
            lfefsnroffst: 0,
            lfefgaincod: 0,
            cplfleak: 0,
            cplsleak: 0,
            deltbae: [0; 5],
            delta: [[(0, 0, 0); 8]; 5],
            ndelta: [0; 5],
            cpldeltbae: 0,
            cpldelta: [(0, 0, 0); 8],
            ncpldelta: 0,
        })
    }
}

/// Walks a syncframe: returns its length in bytes. The reader (or writer) holds exactly the frame, except when
/// decompressing, where the length comes from the header just coded.
// (the channels and bins by index, as the syntax of A/52's Section 5.4 has them)
#[allow(clippy::needless_range_loop)]
pub fn walk<I: Io>(io: &mut I, last: &mut Ctx) -> R<usize> {
    io.expect(0x0B77, 16)?;
    io.derived(16)?;
    let fscod = fld(io, last, 0, 2, false)?;
    let frm = fld(io, last, 1, 6, false)?;
    let bytes = frame_bytes(fscod, frm).ok_or(FrameError::Parse)?;
    let total = bytes * 8;
    if fld(io, last, 2, 5, false)? != 8 {
        return Err(FrameError::Parse);
    }
    fld(io, last, 3, 3, false)?;
    let acmod = fld(io, last, 4, 3, false)? as usize;
    if acmod & 1 != 0 && acmod != 1 {
        fld(io, last, 5, 2, false)?;
    }
    if acmod & 4 != 0 {
        fld(io, last, 6, 2, false)?;
    }
    if acmod == 2 {
        fld(io, last, 7, 2, false)?;
    }
    let lfeon = fld(io, last, 8, 1, false)? != 0;
    fld(io, last, 9, 5, false)?;
    if fld(io, last, 10, 1, false)? != 0 {
        fld(io, last, 11, 8, false)?;
    }
    if fld(io, last, 12, 1, false)? != 0 {
        fld(io, last, 13, 8, false)?;
    }
    if fld(io, last, 14, 1, false)? != 0 {
        fld(io, last, 15, 5, false)?;
        fld(io, last, 16, 2, false)?;
    }
    if acmod == 0 {
        fld(io, last, 17, 5, false)?;
        if fld(io, last, 18, 1, false)? != 0 {
            fld(io, last, 19, 8, false)?;
        }
        if fld(io, last, 20, 1, false)? != 0 {
            fld(io, last, 21, 8, false)?;
        }
        if fld(io, last, 22, 1, false)? != 0 {
            fld(io, last, 23, 5, false)?;
            fld(io, last, 24, 2, false)?;
        }
    }
    fld(io, last, 25, 2, false)?; // copyrightb, origbs
    if fld(io, last, 26, 1, false)? != 0 {
        fld(io, last, 27, 14, false)?;
    }
    if fld(io, last, 28, 1, false)? != 0 {
        fld(io, last, 29, 14, false)?;
    }
    if fld(io, last, 30, 1, false)? != 0 {
        let n = fld(io, last, 31, 6, false)?;
        for _ in 0..=n {
            io.field(8, 0x3000)?;
        }
    }
    let nf = NFCHANS[acmod];
    let mut st = Blk::new();
    let alloc_fscod = fscod as usize;
    for blk in 0..6 {
        let b0 = blk == 0;
        for ch in 0..nf {
            fld(io, last, 40 + ch, 1, b0)?;
        }
        for ch in 0..nf {
            fld(io, last, 45 + ch, 1, b0)?;
        }
        if fld(io, last, 50, 1, b0)? != 0 {
            fld(io, last, 51, 8, b0)?;
        }
        if acmod == 0 && fld(io, last, 52, 1, b0)? != 0 {
            fld(io, last, 53, 8, b0)?;
        }
        if fld(io, last, 54, 1, b0)? != 0 {
            st.cplinu = fld(io, last, 55, 1, b0)? != 0;
            if st.cplinu {
                for ch in 0..nf {
                    st.chincpl[ch] = fld(io, last, 56 + ch, 1, b0)? != 0;
                }
                st.phsflginu = if acmod == 2 { fld(io, last, 61, 1, b0)? != 0 } else { false };
                st.cplbegf = fld(io, last, 62, 4, b0)?;
                st.cplendf = fld(io, last, 63, 4, b0)?;
                let ncplsubnd = 3 + st.cplendf as i32 - st.cplbegf as i32;
                if ncplsubnd <= 0 {
                    return Err(FrameError::Parse);
                }
                let mut sum = 0;
                for _ in 1..ncplsubnd {
                    sum += fld(io, last, 64, 1, b0)? as i32;
                }
                st.ncplbnd = (ncplsubnd - sum) as usize;
            } else {
                st.chincpl = [false; 5];
            }
        }
        if st.cplinu {
            let mut cplcoe = [false; 5];
            for ch in 0..nf {
                if st.chincpl[ch] {
                    cplcoe[ch] = fld(io, last, 65, 1, b0)? != 0;
                    if cplcoe[ch] {
                        fld(io, last, 66, 2, b0)?;
                        for _ in 0..st.ncplbnd {
                            fld(io, last, 67, 4, b0)?;
                            fld(io, last, 68, 4, b0)?;
                        }
                    }
                }
            }
            if acmod == 2 && st.phsflginu && (cplcoe[0] || cplcoe[1]) {
                for _ in 0..st.ncplbnd {
                    fld(io, last, 69, 1, b0)?;
                }
            }
        }
        if acmod == 2 && fld(io, last, 70, 1, b0)? != 0 {
            let cb = st.cplbegf;
            let n = if cb > 2 || !st.cplinu {
                4
            } else if cb > 0 {
                3
            } else {
                2
            };
            for _ in 0..n {
                fld(io, last, 71, 1, b0)?;
            }
        }
        if st.cplinu {
            st.cplexpstr = fld(io, last, 72, 2, b0)?;
        }
        for ch in 0..nf {
            st.chexpstr[ch] = fld(io, last, 73 + ch, 2, b0)?;
        }
        if lfeon {
            st.lfeexpstr = fld(io, last, 78, 1, b0)?;
        }
        let cplstrtmant = 37 + 12 * st.cplbegf as usize;
        let cplendmant = 37 + 12 * (st.cplendf as usize + 3);
        for ch in 0..nf {
            if st.chexpstr[ch] != 0 && !st.chincpl[ch] {
                st.chbwcod[ch] = fld(io, last, 79, 6, b0)?;
                if st.chbwcod[ch] > 60 {
                    return Err(FrameError::Parse);
                }
            }
        }
        let mut endmant = [0usize; 5];
        for ch in 0..nf {
            endmant[ch] =
                if st.cplinu && st.chincpl[ch] { cplstrtmant } else { 37 + 3 * (st.chbwcod[ch] as usize + 12) };
        }
        if st.cplinu && st.cplexpstr != 0 {
            let g = [0, 1, 2, 4][st.cplexpstr as usize];
            if cplendmant > 256 || cplendmant <= cplstrtmant {
                return Err(FrameError::Parse);
            }
            let ngrps = (cplendmant - cplstrtmant) / (3 * g);
            let a = fld(io, last, 80, 4, b0)?;
            read_exps(io, last, 5, a << 1, ngrps, g, &mut st.cplexp, cplstrtmant)?;
        }
        for ch in 0..nf {
            if st.chexpstr[ch] != 0 {
                let g = [0, 1, 2, 4][st.chexpstr[ch] as usize];
                let nch = match st.chexpstr[ch] {
                    1 => (endmant[ch] - 1) / 3,
                    2 => (endmant[ch] - 1 + 3) / 6,
                    _ => (endmant[ch] - 1 + 9) / 12,
                };
                let a = fld(io, last, 81 + ch, 4, b0)?;
                read_exps(io, last, ch as u32, a, nch, g, &mut st.exps[ch], 0)?;
                fld(io, last, 86 + ch, 2, b0)?;
            }
        }
        if lfeon && st.lfeexpstr != 0 {
            let a = fld(io, last, 91, 4, b0)?;
            read_exps(io, last, 6, a, 2, 1, &mut st.lfeexp, 0)?;
        }
        if fld(io, last, 92, 1, b0)? != 0 {
            st.sdcycod = fld(io, last, 93, 2, b0)?;
            st.fdcycod = fld(io, last, 94, 2, b0)?;
            st.sgaincod = fld(io, last, 95, 2, b0)?;
            st.dbpbcod = fld(io, last, 96, 2, b0)?;
            st.floorcod = fld(io, last, 97, 3, b0)?;
        }
        if fld(io, last, 98, 1, b0)? != 0 {
            st.csnroffst = fld(io, last, 99, 6, b0)?;
            if st.cplinu {
                st.cplfsnroffst = fld(io, last, 100, 4, b0)?;
                st.cplfgaincod = fld(io, last, 101, 3, b0)?;
            }
            for ch in 0..nf {
                st.fsnroffst[ch] = fld(io, last, 102 + ch, 4, b0)?;
                st.fgaincod[ch] = fld(io, last, 107 + ch, 3, b0)?;
            }
            if lfeon {
                st.lfefsnroffst = fld(io, last, 112, 4, b0)?;
                st.lfefgaincod = fld(io, last, 113, 3, b0)?;
            }
        }
        if st.cplinu && fld(io, last, 114, 1, b0)? != 0 {
            st.cplfleak = fld(io, last, 115, 3, b0)?;
            st.cplsleak = fld(io, last, 116, 3, b0)?;
        }
        if fld(io, last, 117, 1, b0)? != 0 {
            if st.cplinu {
                st.cpldeltbae = fld(io, last, 118, 2, b0)?;
            }
            for ch in 0..nf {
                st.deltbae[ch] = fld(io, last, 119, 2, b0)?;
            }
            if st.cplinu && st.cpldeltbae == 1 {
                let n = fld(io, last, 120, 3, b0)? as usize;
                st.ncpldelta = n + 1;
                for s in 0..=n {
                    st.cpldelta[s] = (
                        fld(io, last, 121, 5, b0)? as u8,
                        fld(io, last, 122, 4, b0)? as u8,
                        fld(io, last, 123, 3, b0)? as u8,
                    );
                }
            }
            for ch in 0..nf {
                if st.deltbae[ch] == 1 {
                    let n = fld(io, last, 124, 3, b0)? as usize;
                    st.ndelta[ch] = n + 1;
                    for s in 0..=n {
                        st.delta[ch][s] = (
                            fld(io, last, 125, 5, b0)? as u8,
                            fld(io, last, 126, 4, b0)? as u8,
                            fld(io, last, 127, 3, b0)? as u8,
                        );
                    }
                }
            }
        }
        if fld(io, last, 128, 1, b0)? != 0 {
            let n = fld(io, last, 129, 9, b0)?;
            for _ in 0..n {
                io.field(8, 0x3001)?;
            }
        }
        // bit allocation
        let al = Alloc {
            sdecay: SLOWDEC[st.sdcycod as usize],
            fdecay: FASTDEC[st.fdcycod as usize],
            sgain: SLOWGAIN[st.sgaincod as usize],
            dbknee: DBPBTAB[st.dbpbcod as usize],
            floor: FLOORTAB[st.floorcod as usize],
            fscod: alloc_fscod,
        };
        let allzero = st.csnroffst == 0
            && st.cplfsnroffst == 0
            && st.lfefsnroffst == 0
            && st.fsnroffst[..nf].iter().all(|&x| x == 0);
        let mut baps = [[0u8; 256]; 5];
        let mut cplbap = [0u8; 256];
        let mut lfebap = [0u8; 256];
        if !allzero {
            for ch in 0..nf {
                let snr = (((st.csnroffst as i32 - 15) << 4) + st.fsnroffst[ch] as i32) << 2;
                let dl: &[(u8, u8, u8)] = if st.deltbae[ch] <= 1 { &st.delta[ch][..st.ndelta[ch]] } else { &[] };
                bit_alloc(
                    &al,
                    &st.exps[ch],
                    0,
                    endmant[ch],
                    FASTGAIN[st.fgaincod[ch] as usize],
                    snr,
                    0,
                    0,
                    dl,
                    &mut baps[ch],
                );
            }
            if st.cplinu {
                let snr = (((st.csnroffst as i32 - 15) << 4) + st.cplfsnroffst as i32) << 2;
                let dl: &[(u8, u8, u8)] = if st.cpldeltbae <= 1 { &st.cpldelta[..st.ncpldelta] } else { &[] };
                bit_alloc(
                    &al,
                    &st.cplexp,
                    cplstrtmant,
                    cplendmant,
                    FASTGAIN[st.cplfgaincod as usize],
                    snr,
                    ((st.cplfleak << 8) + 768) as i32,
                    ((st.cplsleak << 8) + 768) as i32,
                    dl,
                    &mut cplbap,
                );
            }
            if lfeon {
                let snr = (((st.csnroffst as i32 - 15) << 4) + st.lfefsnroffst as i32) << 2;
                bit_alloc(&al, &st.lfeexp, 0, 7, FASTGAIN[st.lfefgaincod as usize], snr, 0, 0, &[], &mut lfebap);
            }
        }
        // mantissas
        let mut pend = [0u8; 5];
        let mut mant = |io: &mut I, last: &mut Ctx, bap: u8| -> R<()> {
            let bap = bap as usize;
            match bap {
                0 => {}
                1 | 2 | 4 => {
                    if pend[bap] == 0 {
                        let ctx = 0x5000 + bap as u32 * 64 + (last.mant[bap] & 63);
                        last.mant[bap] = io.field(MANT_BITS[bap], ctx)?;
                        pend[bap] = [0, 2, 2, 0, 1][bap];
                    } else {
                        pend[bap] -= 1;
                    }
                }
                _ => {
                    let ctx = 0x5000 + bap as u32 * 64 + (last.mant[bap] >> MANT_BITS[bap].saturating_sub(3));
                    last.mant[bap] = io.field(MANT_BITS[bap], ctx)?;
                }
            }
            Ok(())
        };
        let mut got = false;
        for ch in 0..nf {
            for bin in 0..endmant[ch] {
                mant(io, last, baps[ch][bin])?;
            }
            if st.cplinu && st.chincpl[ch] && !got {
                for bin in cplstrtmant..cplendmant {
                    mant(io, last, cplbap[bin])?;
                }
                got = true;
            }
        }
        if lfeon {
            for bin in 0..7 {
                mant(io, last, lfebap[bin])?;
            }
        }
    }
    // the padding, then auxdatae (none), crcrsv and crc2
    let used = io.done();
    let pad = total.checked_sub(used + 18).ok_or(FrameError::Parse)?;
    if pad > 32 {
        return Err(FrameError::Parse);
    }
    io.expect(0, pad as u32)?;
    io.expect(0, 1)?;
    io.expect(0, 1)?;
    io.derived(16)?;
    Ok(bytes)
}

/// Whether the syncframe at the start of `d` can be coded (`d` is exactly the frame).
pub fn check(d: &[u8]) -> bool {
    if d.len() < 8 || !d.len().is_multiple_of(2) || !crcs_ok(d) {
        return false;
    }
    let mut c = Check { r: BitReader::new(d) };
    let mut ctx = Ctx::default();
    matches!(walk(&mut c, &mut ctx), Ok(n) if n == d.len())
}

/// Codes the syncframe `d` (checked by [`check`]).
pub fn encode<C: Coder>(rc: &mut C, m: &mut Model, last: &mut Ctx, d: &[u8]) -> R<()> {
    let mut e = Enc { r: BitReader::new(d), rc, m };
    walk(&mut e, last).map(|_| ())
}

/// Decodes a syncframe, its length from its header.
pub fn decode<C: Coder>(rc: &mut C, m: &mut Model, last: &mut Ctx, out: Vec<u8>) -> R<Vec<u8>> {
    let mut d = Dec { w: BitWriter::with_vec(out), total: 0, rc, m };
    let n = walk(&mut d, last)?;
    let mut v = d.w.finish();
    if v.len() != n {
        return Err(FrameError::Parse);
    }
    set_crcs(&mut v);
    Ok(v)
}
