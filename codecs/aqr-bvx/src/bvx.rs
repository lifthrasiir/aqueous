//! Streams of bvx blocks: LZFSE (`bvx2`), LZVN (`bvxn`), stored (`bvx-`), and the end (`bvx$`)
//! (`FORMAT.md` §2, §3). A stream decodes into its data and its blocks, from which it is
//! written again ([`write`]).

use crate::fse::{self, BackReader, Table};
use crate::{Error, Result, corrupt};

pub const END: [u8; 4] = *b"bvx$";
pub const STORED: [u8; 4] = *b"bvx-";
pub const V2: [u8; 4] = *b"bvx2";
pub const LZVN: [u8; 4] = *b"bvxn";

pub const L_SYMS: usize = 20;
pub const M_SYMS: usize = 20;
pub const D_SYMS: usize = 64;
pub const LIT_SYMS: usize = 256;
pub const L_STATES: u32 = 64;
pub const M_STATES: u32 = 64;
pub const D_STATES: u32 = 256;
pub const LIT_STATES: u32 = 1024;

pub const L_EXTRA: [u8; L_SYMS] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 3, 5, 8];
pub const M_EXTRA: [u8; M_SYMS] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3, 5, 8, 11];
/// Extra bits of a distance's symbols: four symbols each of 0 to 15 bits.
pub const D_EXTRA: [u8; D_SYMS] = {
    let mut t = [0; D_SYMS];
    let mut i = 0;
    while i < D_SYMS {
        t[i] = (i / 4) as u8;
        i += 1;
    }
    t
};

/// The base values of the symbols of `extra` bits, each after the range of the one before.
const fn bases<const N: usize>(extra: &[u8; N]) -> [u32; N] {
    let mut t = [0; N];
    let mut i = 1;
    while i < N {
        t[i] = t[i - 1] + (1 << extra[i - 1]);
        i += 1;
    }
    t
}

pub const L_BASE: [u32; L_SYMS] = bases(&L_EXTRA);
pub const M_BASE: [u32; M_SYMS] = bases(&M_EXTRA);
pub const D_BASE: [u32; D_SYMS] = bases(&D_EXTRA);
pub const L_MAX: u32 = L_BASE[L_SYMS - 1] + (1 << L_EXTRA[L_SYMS - 1]) - 1;
pub const M_MAX: u32 = M_BASE[M_SYMS - 1] + (1 << M_EXTRA[M_SYMS - 1]) - 1;
pub const D_MAX: u32 = D_BASE[D_SYMS - 1] + (1 << D_EXTRA[D_SYMS - 1]) - 1;

/// The symbol of value `v` among `base` (the last whose base is at most `v`).
#[inline]
pub fn symbol(base: &[u32], v: u32) -> usize {
    base.partition_point(|&b| b <= v) - 1
}

/// The frequencies of an LZFSE block's four tables.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Freqs {
    pub l: [u16; L_SYMS],
    pub m: [u16; M_SYMS],
    pub d: [u16; D_SYMS],
    pub lit: [u16; LIT_SYMS],
}

impl Freqs {
    pub fn all(&self) -> impl Iterator<Item = u16> + '_ {
        self.l.iter().chain(&self.m).chain(&self.d).chain(&self.lit).copied()
    }
}

/// An LZFSE block (`bvx2`), as it was coded.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Lzfse {
    /// The literals, those of the triples and the padding to a multiple of 4.
    pub lits: Vec<u8>,
    /// The triples: literals, match length, distance (0: the one before).
    pub lmd: Vec<[u32; 3]>,
    pub freqs: Freqs,
    /// The states the literals' four coders start at, and the triples' three (the decoder's
    /// states after the last symbols).
    pub lit_start: [u32; 4],
    pub lmd_start: [u32; 3],
    /// Zero bits below the first symbol's in the literals' stream and in the triples'.
    pub lit_low: u32,
    pub lmd_low: u32,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Block {
    /// So many bytes of data stored.
    Stored(u32),
    Lzfse(Box<Lzfse>),
    /// An LZVN block: its data's length and payload.
    Lzvn(u32, Vec<u8>),
}

/// A stream decoded.
pub struct Stream {
    pub data: Vec<u8>,
    pub blocks: Vec<Block>,
    /// Bytes of the stream, up to its end block.
    pub used: usize,
}

fn le32(s: &[u8], p: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(s.get(p..p + 4).ok_or_else(corrupt)?.try_into().unwrap()))
}

fn le64(s: &[u8], p: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(s.get(p..p + 8).ok_or_else(corrupt)?.try_into().unwrap()))
}

/// Bits `at..at + n` of `x`.
#[inline]
fn field(x: u64, at: u32, n: u32) -> u32 {
    (x >> at & ((1 << n) - 1)) as u32
}

/// The stream at the start of `s` as its blocks' headers tell, without decoding them: its
/// bytes up to and with its end block, and its data's; None where they do not hold. Only
/// blocks that [`decode`] reads are taken (not LZFSE's first version).
pub fn extent(s: &[u8]) -> Option<(usize, u64)> {
    let (mut p, mut data) = (0usize, 0u64);
    loop {
        let magic: [u8; 4] = s.get(p..p + 4)?.try_into().unwrap();
        if magic == END {
            return (p > 0).then_some((p + 4, data));
        }
        let n = le32(s, p + 4).ok()? as u64;
        let len = match magic {
            STORED => 8 + n as usize,
            LZVN => 12 + le32(s, p + 8).ok()? as usize,
            V2 => {
                let (f0, f1, f2) = (le64(s, p + 8).ok()?, le64(s, p + 16).ok()?, le64(s, p + 24).ok()?);
                let header = field(f2, 0, 32) as usize;
                if header < 32 || f0 >> 63 != 0 || f1 >> 63 != 0 || f2 >> 62 != 0 {
                    return None;
                }
                header + field(f0, 20, 20) as usize + field(f1, 40, 20) as usize
            }
            _ => return None,
        };
        p = p.checked_add(len).filter(|&e| e <= s.len())?;
        data += n;
    }
}

/// Decodes the stream at the start of `s`, of at most `limit` bytes of data.
pub fn decode(s: &[u8], limit: usize) -> Result<Stream> {
    let mut data = Vec::new();
    let mut blocks = Vec::new();
    let mut p = 0;
    loop {
        let magic: [u8; 4] = s.get(p..p + 4).ok_or_else(corrupt)?.try_into().unwrap();
        let n = || le32(s, p + 4).map(|n| n as usize);
        match magic {
            END => return Ok(Stream { data, blocks, used: p + 4 }),
            STORED => {
                let n = n()?;
                let d = s.get(p + 8..p + 8 + n).ok_or_else(corrupt)?;
                if data.len() + n > limit {
                    return Err(Error("data beyond the limit"));
                }
                data.extend_from_slice(d);
                blocks.push(Block::Stored(n as u32));
                p += 8 + n;
            }
            V2 => {
                let (b, used) = decode_v2(&s[p..], &mut data, limit)?;
                blocks.push(Block::Lzfse(Box::new(b)));
                p += used;
            }
            LZVN => {
                let n_raw = n()?;
                let len = le32(s, p + 8)? as usize;
                let payload = s.get(p + 12..p + 12 + len).ok_or_else(corrupt)?;
                if data.len() + n_raw > limit {
                    return Err(Error("data beyond the limit"));
                }
                crate::lzvn::decode_onto(payload, &mut data, n_raw)?;
                blocks.push(Block::Lzvn(n_raw as u32, payload.to_vec()));
                p += 12 + len;
            }
            _ => return Err(Error("not a block")),
        }
    }
}

/// Decodes the LZFSE block at the start of `s` onto `data`: the block and its bytes.
fn decode_v2(s: &[u8], data: &mut Vec<u8>, limit: usize) -> Result<(Lzfse, usize)> {
    let n_raw = le32(s, 4)? as usize;
    let (f0, f1, f2) = (le64(s, 8)?, le64(s, 16)?, le64(s, 24)?);
    let n_lits = field(f0, 0, 20) as usize;
    let lit_bytes = field(f0, 20, 20) as usize;
    let n_lmd = field(f0, 40, 20) as usize;
    let lit_bits = field(f0, 60, 3);
    let lit_state = [field(f1, 0, 10), field(f1, 10, 10), field(f1, 20, 10), field(f1, 30, 10)];
    let lmd_bytes = field(f1, 40, 20) as usize;
    let lmd_bits = field(f1, 60, 3);
    let header = field(f2, 0, 32) as usize;
    let lmd_state = [field(f2, 32, 10), field(f2, 42, 10), field(f2, 52, 10)];
    // the four reserved bits, and literals in groups of four
    if f0 >> 63 != 0 || f1 >> 63 != 0 || f2 >> 62 != 0 || !n_lits.is_multiple_of(4) || header < 32 {
        return Err(corrupt());
    }
    if data.len() + n_raw > limit {
        return Err(Error("data beyond the limit"));
    }
    // the frequencies
    let h = s.get(32..header).ok_or_else(corrupt)?;
    let (mut acc, mut nb, mut at) = (0u64, 0u32, 0);
    let mut freqs = Freqs { l: [0; L_SYMS], m: [0; M_SYMS], d: [0; D_SYMS], lit: [0; LIT_SYMS] };
    for f in freqs.l.iter_mut().chain(&mut freqs.m).chain(&mut freqs.d).chain(&mut freqs.lit) {
        while nb < 14 && at < h.len() {
            acc |= (h[at] as u64) << nb;
            nb += 8;
            at += 1;
        }
        let (v, n) = fse::freq_decode(acc as u32);
        if n > nb {
            return Err(corrupt());
        }
        *f = v as u16;
        acc >>= n;
        nb -= n;
    }
    // the code's padding: zero bits, and no byte unread
    if acc != 0 || nb >= 8 {
        return Err(Error("header of another form"));
    }
    let lit_t = Table::new(&freqs.lit, LIT_STATES)?;
    let l_t = Table::new(&freqs.l, L_STATES)?;
    let m_t = Table::new(&freqs.m, M_STATES)?;
    let d_t = Table::new(&freqs.d, D_STATES)?;
    let lit_s = s.get(header..header + lit_bytes).ok_or_else(corrupt)?;
    let lmd_s = s.get(header + lit_bytes..header + lit_bytes + lmd_bytes).ok_or_else(corrupt)?;

    // literals: four coders in turn, the stream read from its end
    let unused = |b: u32| if b > 7 { Err(corrupt()) } else { Ok(7 - b) };
    let mut r = BackReader::new(lit_s, unused(lit_bits)?)?;
    let mut lst = lit_state;
    let mut lits = Vec::with_capacity(n_lits);
    for i in 0..n_lits {
        let x = &mut lst[i % 4];
        let &(k, sym, delta) = lit_t.dec.get(*x as usize).ok_or_else(corrupt)?;
        lits.push(sym as u8);
        *x = delta + r.read(k as u32)?;
    }
    if !zero_below(lit_s, r.pos) {
        return Err(Error("literals' stream of another form"));
    }
    let lit_low = r.pos as u32;

    let mut r = BackReader::new(lmd_s, unused(lmd_bits)?)?;
    let mut st = lmd_state;
    let mut lmd = Vec::with_capacity(n_lmd);
    let (mut li, mut dist, mut got) = (0, 0usize, 0);
    let start = data.len();
    for _ in 0..n_lmd {
        let mut v = [0; 3];
        for (i, (t, (base, extra))) in
            [(&l_t, (&L_BASE[..], &L_EXTRA[..])), (&m_t, (&M_BASE, &M_EXTRA)), (&d_t, (&D_BASE, &D_EXTRA))]
                .into_iter()
                .enumerate()
        {
            let &(k, sym, delta) = t.dec.get(st[i] as usize).ok_or_else(corrupt)?;
            let e = extra[sym as usize] as u32;
            let b = r.read(k as u32 + e)?;
            v[i] = base[sym as usize] + (b & ((1 << e) - 1));
            st[i] = delta + (b >> e);
        }
        let [l, m, d] = v.map(|x| x as usize);
        if d != 0 {
            dist = d;
        }
        if li + l > n_lits || got + l + m > n_raw || m > 0 && (dist == 0 || dist > data.len() + l) {
            return Err(corrupt());
        }
        data.extend_from_slice(&lits[li..li + l]);
        li += l;
        for _ in 0..m {
            data.push(data[data.len() - dist]);
        }
        got += l + m;
        lmd.push(v);
    }
    if got != n_raw || data.len() - start != n_raw {
        return Err(corrupt());
    }
    if !zero_below(lmd_s, r.pos) {
        return Err(Error("triples' stream of another form"));
    }
    let used = header + lit_bytes + lmd_bytes;
    let b = Lzfse { lits, lmd, freqs, lit_start: lst, lmd_start: st, lit_low, lmd_low: r.pos as u32 };
    Ok((b, used))
}

/// Whether the bits of `s` below `pos` are zeros.
fn zero_below(s: &[u8], pos: usize) -> bool {
    s[..pos / 8].iter().all(|&b| b == 0) && (pos.is_multiple_of(8) || s[pos / 8] & ((1 << (pos % 8)) - 1) == 0)
}

/// Writes the stream of `blocks` over `data` (which a stored block takes its bytes from).
pub fn write(data: &[u8], blocks: &[Block], out: &mut Vec<u8>) -> Result<()> {
    let mut at = 0;
    for b in blocks {
        write_block(data.get(at..).ok_or_else(corrupt)?, b, out)?;
        at += match b {
            Block::Stored(n) | Block::Lzvn(n, _) => *n as usize,
            Block::Lzfse(b) => b.lmd.iter().map(|t| (t[0] + t[1]) as usize).sum::<usize>(),
        };
    }
    out.extend_from_slice(&END);
    Ok(())
}

/// Writes a block whose data starts `data` (which a stored block takes its bytes from).
pub fn write_block(data: &[u8], b: &Block, out: &mut Vec<u8>) -> Result<()> {
    match b {
        Block::Stored(n) => {
            let d = data.get(..*n as usize).ok_or_else(corrupt)?;
            out.extend_from_slice(&STORED);
            out.extend_from_slice(&n.to_le_bytes());
            out.extend_from_slice(d);
        }
        Block::Lzvn(n, payload) => {
            out.extend_from_slice(&LZVN);
            out.extend_from_slice(&n.to_le_bytes());
            out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            out.extend_from_slice(payload);
        }
        Block::Lzfse(b) => write_v2(b, out)?,
    }
    Ok(())
}

/// Writes an LZFSE block.
pub fn write_v2(b: &Lzfse, out: &mut Vec<u8>) -> Result<()> {
    let f = &b.freqs;
    let lit_t = Table::new(&f.lit, LIT_STATES)?;
    let l_t = Table::new(&f.l, L_STATES)?;
    let m_t = Table::new(&f.m, M_STATES)?;
    let d_t = Table::new(&f.d, D_STATES)?;
    if !b.lits.len().is_multiple_of(4) || b.lits.len() >= 1 << 20 || b.lmd.len() >= 1 << 20 {
        return Err(corrupt());
    }

    let mut w = fse::Writer::default();
    zeros(&mut w, b.lit_low);
    let mut x = b.lit_start;
    for (i, &c) in b.lits.iter().enumerate().rev() {
        if !lit_t.has(c as usize) || x[i % 4] >= LIT_STATES {
            return Err(corrupt());
        }
        let (prev, v, n) = lit_t.encode(c as usize, x[i % 4]);
        w.put(v, n);
        x[i % 4] = prev;
    }
    let (lit_s, lit_unused) = w.finish();

    let mut w = fse::Writer::default();
    zeros(&mut w, b.lmd_low);
    let mut y = b.lmd_start;
    let tables = [
        (&l_t, &L_BASE[..], &L_EXTRA[..], L_STATES),
        (&m_t, &M_BASE, &M_EXTRA, M_STATES),
        (&d_t, &D_BASE, &D_EXTRA, D_STATES),
    ];
    for t in b.lmd.iter().rev() {
        for i in (0..3).rev() {
            let (table, base, extra, n) = tables[i];
            let s = symbol(base, t[i]);
            let e = extra[s] as u32;
            let low = t[i] - base[s];
            if !table.has(s) || low >> e != 0 || y[i] >= n {
                return Err(corrupt());
            }
            let (prev, v, k) = table.encode(s, y[i]);
            w.put(v << e | low, k + e);
            y[i] = prev;
        }
    }
    let (lmd_s, lmd_unused) = w.finish();

    let mut h = fse::Writer::default();
    for v in f.all() {
        let (c, n) = fse::freq_code(v as u32);
        h.put(c, n);
    }
    let (h, _) = h.finish();
    let n_raw: u32 = b.lmd.iter().map(|t| t[0] + t[1]).sum();
    let header = 32 + h.len() as u64;
    let f0 =
        b.lits.len() as u64 | (lit_s.len() as u64) << 20 | (b.lmd.len() as u64) << 40 | ((7 - lit_unused) as u64) << 60;
    let f1 = x[0] as u64
        | (x[1] as u64) << 10
        | (x[2] as u64) << 20
        | (x[3] as u64) << 30
        | (lmd_s.len() as u64) << 40
        | ((7 - lmd_unused) as u64) << 60;
    let f2 = header | (y[0] as u64) << 32 | (y[1] as u64) << 42 | (y[2] as u64) << 52;
    if lit_s.len() >= 1 << 20 || lmd_s.len() >= 1 << 20 {
        return Err(corrupt());
    }
    out.extend_from_slice(&V2);
    out.extend_from_slice(&n_raw.to_le_bytes());
    for f in [f0, f1, f2] {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out.extend_from_slice(&h);
    out.extend_from_slice(&lit_s);
    out.extend_from_slice(&lmd_s);
    Ok(())
}

fn zeros(w: &mut fse::Writer, mut n: u32) {
    while n > 0 {
        let k = n.min(32);
        w.put(0, k);
        n -= k;
    }
}
