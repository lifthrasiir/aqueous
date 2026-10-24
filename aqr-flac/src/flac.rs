//! FLAC frames: parsing into an exact representation and writing it back bit for bit.

use crate::bits::{BitReader, FastWriter};

pub use aqr_raw::audio::frame::*;

pub fn crc8(d: &[u8]) -> u8 {
    static T: std::sync::OnceLock<[u8; 256]> = std::sync::OnceLock::new();
    let t = T.get_or_init(|| {
        let mut t = [0u8; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let mut c = i as u8;
            for _ in 0..8 {
                c = if c & 0x80 != 0 { c << 1 ^ 0x07 } else { c << 1 };
            }
            *e = c;
        }
        t
    });
    d.iter().fold(0u8, |c, &b| t[(c ^ b) as usize])
}

/// CRC-16 (polynomial 0x8005, MSB first), slicing by 8.
pub fn crc16(d: &[u8]) -> u16 {
    static T: std::sync::OnceLock<[[u16; 256]; 8]> = std::sync::OnceLock::new();
    let t = T.get_or_init(|| {
        let mut t = [[0u16; 256]; 8];
        for i in 0..256 {
            let mut c = (i as u16) << 8;
            for _ in 0..8 {
                c = if c & 0x8000 != 0 { c << 1 ^ 0x8005 } else { c << 1 };
            }
            t[0][i] = c;
        }
        for i in 0..256 {
            for k in 1..8 {
                let p = t[k - 1][i];
                t[k][i] = p << 8 ^ t[0][(p >> 8) as usize];
            }
        }
        t
    });
    let mut c = 0u16;
    let mut ch = d.chunks_exact(8);
    for b in &mut ch {
        let hi = c ^ u16::from_be_bytes([b[0], b[1]]);
        c = t[7][(hi >> 8) as usize]
            ^ t[6][(hi & 0xff) as usize]
            ^ t[5][b[2] as usize]
            ^ t[4][b[3] as usize]
            ^ t[3][b[4] as usize]
            ^ t[2][b[5] as usize]
            ^ t[1][b[6] as usize]
            ^ t[0][b[7] as usize];
    }
    for &b in ch.remainder() {
        c = c << 8 ^ t[0][((c >> 8) as u8 ^ b) as usize];
    }
    c
}

/// Block size of a header code (0 when it is stored after the coded number or reserved).
fn block_size_code(c: u8) -> usize {
    match c {
        1 => 192,
        2..=5 => 576 << (c - 2),
        8..=15 => 256 << (c - 8),
        _ => 0,
    }
}

/// Parses a frame header at `d[p..]`. Returns (header length without CRC-8, block size,
/// channel assignment, sample size).
pub fn parse_header(d: &[u8], p: usize, si_bps: u32) -> Option<(usize, usize, u8, u32)> {
    let h = d.get(p..p + 16.min(d.len() - p))?;
    if h.len() < 6 || h[0] != 0xff || h[1] & 0xfe != 0xf8 {
        return None;
    }
    let bs_code = h[2] >> 4;
    let sr_code = h[2] & 15;
    let assign = h[3] >> 4;
    let ss_code = (h[3] >> 1) & 7;
    if bs_code == 0 || sr_code == 15 || assign > 10 || ss_code == 3 || h[3] & 1 != 0 {
        return None;
    }
    let bps = match ss_code {
        0 => si_bps,
        1 => 8,
        2 => 12,
        4 => 16,
        5 => 20,
        6 => 24,
        _ => 32,
    };
    if !(4..=32).contains(&bps) {
        return None;
    }
    // Coded number.
    let b = h[4];
    let extra = match b.leading_ones() {
        0 => 0,
        1 | 8 => return None,
        n => n as usize - 1,
    };
    let mut q = 5;
    for _ in 0..extra {
        if *h.get(q)? & 0xc0 != 0x80 {
            return None;
        }
        q += 1;
    }
    let mut block = block_size_code(bs_code);
    if bs_code == 6 {
        block = *h.get(q)? as usize + 1;
        q += 1;
    } else if bs_code == 7 {
        block = u16::from_be_bytes([*h.get(q)?, *h.get(q + 1)?]) as usize + 1;
        q += 2;
    }
    q += match sr_code {
        12 => 1,
        13 | 14 => 2,
        _ => 0,
    };
    if *h.get(q)? != crc8(&h[..q]) {
        return None;
    }
    Some((q, block, assign, bps))
}

#[inline(always)]
fn sext(v: u64, n: u32) -> i64 {
    if n == 0 { 0 } else { ((v << (64 - n)) as i64) >> (64 - n) }
}

/// Reads `n` (0..=33) bits as a signed value.
#[inline(always)]
fn get_signed(r: &mut BitReader, n: u32) -> i64 {
    if n > 32 {
        let hi = r.get(n - 32) as u64;
        let lo = r.get(32) as u64;
        sext(hi << 32 | lo, n)
    } else {
        sext(r.get(n) as u64, n)
    }
}

/// The 64 bits at bit `pos` of `data`, left aligned. `pos / 8 + 8 <= data.len()`.
#[inline(always)]
fn window_at(data: &[u8], pos: usize) -> u64 {
    debug_assert!(pos / 8 + 8 <= data.len());
    // SAFETY: the caller keeps pos / 8 + 8 within the data.
    let w = u64::from_be(unsafe { (data.as_ptr().add(pos >> 3) as *const u64).read_unaligned() });
    w << (pos & 7)
}

/// One Rice code starting at the top of `w`: (zigzag value, length); the value is only
/// right if the length is at most 57.
#[inline(always)]
fn rice_one(w: u64, k: u32) -> (u64, u32) {
    let lz = w.leading_zeros();
    let u = (lz as u64) << k | (w.wrapping_shl(lz) << 1 >> 1 >> (63 - k));
    (u, lz + 1 + k)
}

/// Reads one Rice code with a unary part of any length (slow path).
/// Returns the value and the new position.
#[cold]
#[inline(never)]
fn rice_long(data: &[u8], mut pos: usize, k: u32, limit: usize) -> Option<(u64, usize)> {
    let mut q = 0u64;
    loop {
        if pos > limit || q >> 32 != 0 {
            return None;
        }
        let lz = window_at(data, pos).leading_zeros().min(56);
        q += lz as u64;
        pos += lz as usize;
        if lz < 56 {
            break;
        }
    }
    pos += 1;
    if pos > limit {
        return None;
    }
    let low = if k == 0 { 0 } else { window_at(data, pos) >> (64 - k) };
    Some((q << k | low, pos + k as usize))
}

#[inline(always)]
fn unzigzag(u: u64) -> i32 {
    let u = u as u32;
    (u >> 1) as i32 ^ -((u & 1) as i32)
}

/// Reads Rice-coded residuals with parameter `k` into `out`. False if a value does not
/// fit 32 bits or the reader passes `limit` (at most `limit / 8 + 8 <= data.len()`).
///
/// Two codes are decoded from each 64-bit window; the second is kept when it fits, without
/// a branch.
#[inline(never)]
fn read_rice(r: &mut BitReader, k: u32, out: &mut [i32], limit: usize) -> bool {
    let mut pos = r.pos;
    let data = r.data();
    let n = out.len();
    let mut over = 0u64;
    let mut i = 0;
    while i + 1 < n {
        if pos > limit {
            return false;
        }
        let w = window_at(data, pos);
        let (u1, l1) = rice_one(w, k);
        if l1 > 57 {
            let Some((u, p)) = rice_long(data, pos, k, limit) else { return false };
            pos = p;
            over |= u;
            out[i] = unzigzag(u);
            i += 1;
            continue;
        }
        // Up to two more codes from the same window, kept if they fit.
        let (u2, l2) = rice_one(w << l1, k);
        let l12 = l1 + l2;
        let (u3, l3) = rice_one(w.wrapping_shl(l12), k);
        let fits2 = l12 <= 57 && i + 2 < n + 1;
        let fits3 = fits2 && l12 + l3 <= 57 && i + 3 <= n;
        out[i] = unzigzag(u1);
        out[i + 1] = unzigzag(u2);
        if i + 2 < n {
            out[i + 2] = unzigzag(u3);
        }
        over |= u1 | if fits2 { u2 } else { 0 } | if fits3 { u3 } else { 0 };
        i += 1 + fits2 as usize + fits3 as usize;
        pos += (if fits3 {
            l12 + l3
        } else if fits2 {
            l12
        } else {
            l1
        }) as usize;
    }
    if i < n {
        if pos > limit {
            return false;
        }
        let (u, l) = rice_one(window_at(data, pos), k);
        let u = if l > 57 {
            let Some((u, p)) = rice_long(data, pos, k, limit) else { return false };
            pos = p;
            u
        } else {
            pos += l as usize;
            u
        };
        over |= u;
        out[i] = unzigzag(u);
    }
    r.pos = pos;
    over >> 32 == 0 && pos <= limit
}

/// Parses the frame at `d[p..]` (`d` padded, see [`crate::bits::padded`]; `end` is the
/// length without padding) into `f`. Returns the end of the frame. The CRC-16 must match
/// and the padding bits must be zero.
pub fn parse_frame(d: &[u8], p: usize, end: usize, si_bps: u32, f: &mut Frame) -> Option<usize> {
    let (hl, block, assign, bps) = parse_header(&d[..end], p, si_bps)?;
    f.header.clear();
    f.header.extend_from_slice(&d[p..p + hl]);
    f.block_size = block;
    f.assign = assign;
    f.channels = if assign < 8 { assign as usize + 1 } else { 2 };
    f.bps = bps;
    f.sub.resize(f.channels, Subframe::default());
    f.res.resize(f.channels, Vec::new());
    f.verbatim.clear();
    let limit = end * 8;
    let mut r = BitReader::new(d, (p + hl + 1) * 8);
    for ch in 0..f.channels {
        let side = matches!((assign, ch), (8, 1) | (9, 0) | (10, 1));
        let s = &mut f.sub[ch];
        let res = &mut f.res[ch];
        let x = r.get(8);
        if x & 0x80 != 0 {
            return None;
        }
        s.typ = (x >> 1) as u8 & 63;
        s.wasted = 0;
        if x & 1 != 0 {
            let w = r.window();
            let lz = w.leading_zeros();
            if lz >= 32 {
                return None;
            }
            r.skip(lz + 1);
            s.wasted = lz as u8 + 1;
        }
        let full = bps + side as u32;
        if s.wasted as u32 >= full {
            return None;
        }
        let bits = full - s.wasted as u32;
        s.bits = bits as u8;
        s.order = 0;
        s.params.clear();
        res.clear();
        match s.typ {
            SUB_CONSTANT => s.warm[0] = get_signed(&mut r, bits),
            SUB_VERBATIM => {
                for _ in 0..block {
                    f.verbatim.push(get_signed(&mut r, bits));
                }
            }
            t if (SUB_FIXED..=SUB_FIXED + 4).contains(&t) || t >= SUB_LPC => {
                let order = if t >= SUB_LPC { t - SUB_LPC + 1 } else { t - SUB_FIXED } as usize;
                if order > block {
                    return None;
                }
                s.order = order as u8;
                for i in 0..order {
                    s.warm[i] = get_signed(&mut r, bits);
                }
                if t >= SUB_LPC {
                    let pr = r.get(4);
                    if pr == 15 {
                        return None;
                    }
                    s.precision = pr as u8 + 1;
                    s.shift = sext(r.get(5) as u64, 5) as i8;
                    for i in 0..order {
                        s.coefs[i] = get_signed(&mut r, pr + 1) as i32;
                    }
                }
                let m = r.get(2);
                if m > 1 {
                    return None;
                }
                s.method = m as u8;
                s.porder = r.get(4) as u8;
                let np = 1usize << s.porder;
                if block % np != 0 || block / np < order {
                    return None;
                }
                let pbits = 4 + m;
                let esc = (1u32 << pbits) - 1;
                res.resize(block - order, 0);
                let mut at = 0;
                for i in 0..np {
                    let n = block / np - if i == 0 { order } else { 0 };
                    let k = r.get(pbits);
                    let out = &mut res[at..at + n];
                    if k == esc {
                        let nb = r.get(5);
                        s.params.push(ESCAPE | nb as u8);
                        for o in out.iter_mut() {
                            *o = sext(r.get(nb) as u64, nb) as i32;
                        }
                    } else {
                        s.params.push(k as u8);
                        if !read_rice(&mut r, k, out, limit) {
                            return None;
                        }
                    }
                    at += n;
                    if r.pos > limit {
                        return None;
                    }
                }
            }
            _ => return None,
        }
        if r.pos > limit {
            return None;
        }
    }
    // Zero padding, then the CRC-16.
    let pad = (8 - r.pos % 8) % 8;
    if pad > 0 && r.get(pad as u32) != 0 {
        return None;
    }
    let fe = r.pos / 8 + 2;
    if fe > end || crc16(&d[p..fe - 2]) != u16::from_be_bytes([d[fe - 2], d[fe - 1]]) {
        return None;
    }
    Some(fe)
}

/// Writes `n` (0..=33) bits of `v`.
#[inline(always)]
fn put_signed(w: &mut FastWriter, v: i64, n: u32) {
    if n > 0 {
        w.put(v as u64 & (u64::MAX >> (64 - n)), n);
    }
}

#[inline(always)]
/// False if the long codes (over 56 bits) would take more than `budget` bits, which is
/// reduced by what they take.
fn write_rice(w: &mut FastWriter, k: u32, res: &[i32], budget: &mut u64) -> bool {
    w.reserve(res.len() * 8 + 8);
    let mut l = w.local();
    for &v in res {
        let u = ((v << 1) ^ (v >> 31)) as u32 as u64;
        let q = u >> k;
        if q + 1 + k as u64 <= 56 {
            l.put(u & ((1u64 << k) - 1) | 1 << k, q as u32 + 1 + k);
        } else {
            w.put_back(l);
            if q + 1 + k as u64 > *budget {
                return false;
            }
            *budget -= q + 1 + k as u64;
            let mut q = q;
            while q > 0 {
                let n = q.min(32);
                w.reserve(8);
                w.put(0, n as u32);
                q -= n;
            }
            w.reserve(8);
            w.put(u & ((1u64 << k) - 1) | 1 << k, 1 + k);
            w.reserve(res.len() * 8 + 8);
            l = w.local();
        }
    }
    w.put_back(l);
    true
}

/// Appends the frame's bytes to `out`; false (with `out` unchanged) if they would be more
/// than about `max_bytes` (bounds the work on corrupted input: a residual can take up to
/// 2^32 bits).
pub fn write_frame(f: &Frame, out: &mut Vec<u8>, max_bytes: usize) -> bool {
    write_frame_bits(f, out, (max_bytes as u64).saturating_mul(8)).is_some()
}

/// [`write_frame`] with the budget in bits of long codes (Rice codes over 56 bits), the
/// only ones it counts; returns what is left of it. So it fails exactly when the frame's
/// long codes take more than `budget` bits.
#[inline]
pub fn write_frame_bits(f: &Frame, out: &mut Vec<u8>, mut budget: u64) -> Option<u64> {
    let start = out.len();
    let mut w = FastWriter::with_capacity(f.block_size * f.channels * 4 + 64);
    for &b in &f.header {
        w.reserve(8);
        w.put(b as u64, 8);
    }
    w.reserve(8);
    w.put(crc8(&f.header) as u64, 8);
    let mut vi = 0;
    for ch in 0..f.channels {
        let s = &f.sub[ch];
        let bits = s.bits as u32;
        w.reserve(16);
        w.put((s.typ as u64) << 1 | (s.wasted > 0) as u64, 8);
        if s.wasted > 0 {
            w.put(1, s.wasted as u32);
        }
        match s.typ {
            SUB_CONSTANT => put_signed(&mut w, s.warm[0], bits),
            SUB_VERBATIM => {
                w.reserve(f.block_size * 5 + 8);
                for &v in &f.verbatim[vi..vi + f.block_size] {
                    put_signed(&mut w, v, bits);
                }
                vi += f.block_size;
            }
            _ => {
                let order = s.order as usize;
                w.reserve(order * 10 + 16);
                for &v in &s.warm[..order] {
                    put_signed(&mut w, v, bits);
                }
                if s.typ >= SUB_LPC {
                    w.put(s.precision as u64 - 1, 4);
                    w.put(s.shift as u64 & 31, 5);
                    for &c in &s.coefs[..order] {
                        put_signed(&mut w, c as i64, s.precision as u32);
                    }
                }
                w.put((s.method as u64) << 4 | s.porder as u64, 6);
                let np = 1usize << s.porder;
                let pbits = 4 + s.method as u32;
                let res = &f.res[ch];
                let mut at = 0;
                for (i, &k) in s.params.iter().enumerate() {
                    let n = f.block_size / np - if i == 0 { order } else { 0 };
                    let part = &res[at..at + n];
                    w.reserve(8);
                    if k & ESCAPE != 0 {
                        let nb = (k & 31) as u32;
                        w.put((1u64 << pbits) - 1, pbits);
                        w.put(nb as u64, 5);
                        w.reserve(n * 5 + 8);
                        for &v in part {
                            put_signed(&mut w, v as i64, nb);
                        }
                    } else {
                        w.put(k as u64, pbits);
                        if !write_rice(&mut w, k as u32, part, &mut budget) {
                            return None;
                        }
                    }
                    at += n;
                }
            }
        }
    }
    let bytes = w.finish();
    out.extend_from_slice(&bytes);
    let c = crc16(&out[start..]);
    out.extend_from_slice(&c.to_be_bytes());
    Some(budget)
}

/// Stream info needed to parse frames.
#[derive(Clone, Copy, Debug, Default)]
pub struct StreamInfo {
    pub bps: u32,
    pub channels: u32,
    pub sample_rate: u32,
}

/// Finds "fLaC" (possibly after an ID3v2 tag) and walks the metadata blocks. Returns the
/// stream info and the offset of the first byte after the metadata.
pub fn parse_metadata(d: &[u8]) -> Option<(StreamInfo, usize)> {
    let mut p = 0;
    if d.len() >= 10 && &d[..3] == b"ID3" {
        let sz = d[6..10].iter().fold(0usize, |a, &b| a << 7 | (b & 0x7f) as usize);
        p = 10 + sz + if d[5] & 0x10 != 0 { 10 } else { 0 };
    }
    if d.get(p..p + 4)? != b"fLaC" {
        return None;
    }
    p += 4;
    let mut si = None;
    loop {
        let h = d.get(p..p + 4)?;
        let last = h[0] & 0x80 != 0;
        let typ = h[0] & 0x7f;
        let len = (h[1] as usize) << 16 | (h[2] as usize) << 8 | h[3] as usize;
        let body = d.get(p + 4..p + 4 + len)?;
        if typ == 0 && len >= 18 {
            let x = u64::from_be_bytes(body[10..18].try_into().unwrap());
            si = Some(StreamInfo {
                sample_rate: (x >> 44) as u32,
                channels: ((x >> 41) & 7) as u32 + 1,
                bps: ((x >> 36) & 31) as u32 + 1,
            });
        }
        p += 4 + len;
        if last {
            break;
        }
    }
    Some((si?, p))
}
