//! Records: parsing them, and coding them by columns.
//!
//! A group (the records of one b-tree) is coded into its own streams: per record a code in
//! `rec` (0: raw payload, 1: predicted from a table, see `index`; n + 2: n columns), the
//! rowids of a table (deltas) or the ranks of predicted index entries (deltas against the
//! previous one plus one) in `rowid`, raw payloads in `rawpay`, and per column a tag per
//! value, integers, reals, text/blob lengths and bytes. A group decodes on its own (or with
//! its table), before any page is rebuilt.

use crate::index::ROWID;
use crate::varint::{Rd, get_sqlite, put_sqlite, put_sv, put_uv, sqlite_len, uv_len, zz};
use crate::{Result, corrupt};
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use xxhash_rust::xxh3::xxh3_64;

/// Integer content size by serial type 0..=9.
pub const INT_SIZE: [usize; 10] = [0, 1, 2, 3, 4, 6, 8, 8, 0, 0];

/// The serial type SQLite picks for an integer.
#[inline]
pub fn canon_st(v: i64, fmt4: bool) -> u64 {
    if fmt4 && (v == 0 || v == 1) {
        return 8 + v as u64;
    }
    let u = if v < 0 { !v } else { v } as u64;
    if u <= 0x7f {
        1
    } else if u <= 0x7fff {
        2
    } else if u <= 0x7f_ffff {
        3
    } else if u <= 0x7fff_ffff {
        4
    } else if u <= 0x7fff_ffff_ffff {
        5
    } else {
        6
    }
}

#[inline]
pub fn st_size(st: u64) -> usize {
    if st >= 12 { ((st - 12) / 2) as usize } else { INT_SIZE[st as usize] }
}

#[inline]
pub fn is_int(st: u64) -> bool {
    matches!(st, 1..=6 | 8 | 9)
}

/// Header length of a record whose serial types take `s` bytes.
#[inline]
pub fn header_len(s: u64) -> u64 {
    let k = sqlite_len(s + 1) as u64;
    if sqlite_len(s + k) as u64 != k { s + k + 1 } else { s + k }
}

/// Value of an integer column (serial type 1-6, 8, 9) whose content is `b`.
#[inline]
pub fn int_value(st: u64, b: &[u8]) -> i64 {
    match st {
        8 => 0,
        9 => 1,
        _ => {
            let mut v = (b[0] as i8) as i64;
            for &x in &b[1..] {
                v = (v << 8) | x as i64;
            }
            v
        }
    }
}

/// Parses a record as SQLite would write it: minimal varints, the header length it picks,
/// contents that add up to the payload. `cols` receives (serial type, content offset) per
/// column.
pub fn parse(pay: &[u8], cols: &mut Vec<(u64, u32)>) -> bool {
    cols.clear();
    let Some((hl, n)) = get_sqlite(pay) else { return false };
    if n != sqlite_len(hl) || hl > pay.len() as u64 {
        return false;
    }
    let hl = hl as usize;
    let mut q = n;
    let mut b = hl;
    while q < hl {
        let Some((st, n)) = get_sqlite(&pay[q..hl]) else { return false };
        if n != sqlite_len(st) || st == 10 || st == 11 || st > u32::MAX as u64 {
            return false;
        }
        q += n;
        cols.push((st, b as u32));
        b += st_size(st);
        if b > pay.len() {
            return false;
        }
    }
    header_len((hl - n) as u64) == hl as u64 && b == pay.len()
}

/// Appends the index entry SQLite builds from a table row (payload `row`): the indexed
/// columns (`map`, with their serial types and offsets `cv` as `index::order` found them),
/// then the rowid.
pub fn predict(row: &[u8], rowid: i64, map: &[u32], cv: &[(u64, u32)], fmt4: bool, out: &mut Vec<u8>) {
    let rst = canon_st(rowid, fmt4);
    let st_of = |j: usize| if map[j] == ROWID { rst } else { cv[j].0 };
    let mut s = sqlite_len(rst) as u64;
    for j in 0..map.len() {
        s += sqlite_len(st_of(j)) as u64;
    }
    let mut vb = [0u8; 9];
    let n = put_sqlite(&mut vb, header_len(s));
    out.extend_from_slice(&vb[..n]);
    for j in 0..map.len() {
        let n = put_sqlite(&mut vb, st_of(j));
        out.extend_from_slice(&vb[..n]);
    }
    let n = put_sqlite(&mut vb, rst);
    out.extend_from_slice(&vb[..n]);
    let rb = rowid.to_be_bytes();
    for j in 0..map.len() {
        if map[j] == ROWID {
            out.extend_from_slice(&rb[8 - INT_SIZE[rst as usize]..]);
        } else {
            let (st, off) = cv[j];
            out.extend_from_slice(&row[off as usize..off as usize + st_size(st)]);
        }
    }
    out.extend_from_slice(&rb[8 - INT_SIZE[rst as usize]..]);
}

// Value tags.
const T_NULL: u8 = 0;
const T_INT: u8 = 1;
const T_REAL: u8 = 2;
const T_TEXT: u8 = 3;
const T_BLOB: u8 = 4;
/// A real that is a float exactly (4 bytes in `real`).
const T_F32: u8 = 5;
/// Text or blob equal to an earlier one of the column (its id in `len`).
const T_TEXT_REP: u8 = 6;
const T_BLOB_REP: u8 = 7;
/// 16 + serial type: an integer with a serial type SQLite would not pick.
const T_ODD_INT: u8 = 16;
/// 32 + e: a real that is m / 10^e (m in the integers).
const T_DEC: u8 = 32;
const MAX_E: usize = 22;

const P10I: [i64; 19] = [
    1,
    10,
    100,
    1_000,
    10_000,
    100_000,
    1_000_000,
    10_000_000,
    100_000_000,
    1_000_000_000,
    10_000_000_000,
    100_000_000_000,
    1_000_000_000_000,
    10_000_000_000_000,
    100_000_000_000_000,
    1_000_000_000_000_000,
    10_000_000_000_000_000,
    100_000_000_000_000_000,
    1_000_000_000_000_000_000,
];

const P10: [f64; MAX_E + 1] = [
    1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16,
    1e17, 1e18, 1e19, 1e20, 1e21, 1e22,
];

/// A real as m / 10^e with the smallest e, if there is one (m below 2^53, so the division
/// is exact up to its one rounding).
#[inline]
fn decimal(bits: u64) -> Option<(i64, u8)> {
    let v = f64::from_bits(bits);
    if !v.is_finite() {
        return None;
    }
    for (e, &p) in P10.iter().enumerate() {
        let m = (v * p).round();
        if m.abs() >= 9007199254740992.0 {
            return None;
        }
        // Checked as the decoder computes it (an integer m loses the sign of -0.0).
        if ((m as i64) as f64 / p).to_bits() == bits {
            return Some((m as i64, e as u8));
        }
    }
    None
}

// Record codes.
const R_RAW: u64 = 0;
const R_REF: u64 = 1;
const R_COLS: u64 = 2;

#[derive(Default)]
pub struct ColEnc {
    pub tag: Vec<u8>,
    pub ints: Vec<i64>,
    /// The integer before each in its record (the previous column's), for the horizontal
    /// prediction.
    hpred: Vec<i64>,
    /// The decimal exponent of each (0 for integers): a column's decimals and integers are
    /// scaled to its largest exponent, so that deltas compare like with like.
    iexp: Vec<u8>,
    pub real: Vec<u8>,
    pub len: Vec<u8>,
    pub bytes: Vec<u8>,
    /// Distinct text/blob values so far: hash -> id, and (offset in `bytes`, length).
    seen: HashMap<u64, u32, BuildHasherDefault<IdHasher>>,
    distinct: Vec<(u32, u32)>,
}

/// For keys that are hashes already.
#[derive(Default)]
struct IdHasher(u64);

impl Hasher for IdHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, _: &[u8]) {
        unreachable!()
    }
    fn write_u64(&mut self, v: u64) {
        self.0 = v;
    }
}

/// Text and blob values this long or longer that appeared before in their column are
/// coded as the id of the earlier value.
const MIN_REP: usize = 2;

#[derive(Default)]
pub struct GroupEnc {
    pub rec: Vec<u8>,
    pub rowid: Vec<u8>,
    pub rawpay: Vec<u8>,
    pub cols: Vec<ColEnc>,
    prev_rowid: i64,
    prev_rank: i64,
}

impl GroupEnc {
    pub fn rowid(&mut self, r: i64) {
        put_sv(&mut self.rowid, r.wrapping_sub(self.prev_rowid));
        self.prev_rowid = r;
    }

    /// A record predicted from the row at `rank` in the table's index order.
    pub fn reference(&mut self, rank: i64) {
        put_uv(&mut self.rec, R_REF);
        put_sv(&mut self.rowid, rank - (self.prev_rank + 1));
        self.prev_rank = rank;
    }

    /// Codes a record by columns, or as raw payload if it is not one SQLite would write.
    pub fn record(&mut self, pay: &[u8], fmt4: bool, cols: &mut Vec<(u64, u32)>) {
        if !parse(pay, cols) {
            put_uv(&mut self.rec, R_RAW);
            put_uv(&mut self.rec, pay.len() as u64);
            self.rawpay.extend_from_slice(pay);
            return;
        }
        put_uv(&mut self.rec, cols.len() as u64 + R_COLS);
        if self.cols.len() < cols.len() {
            self.cols.resize_with(cols.len(), Default::default);
        }
        let mut last = 0i64;
        for (c, &(st, off)) in self.cols.iter_mut().zip(cols.iter()) {
            let b = &pay[off as usize..off as usize + st_size(st)];
            match st {
                0 => c.tag.push(T_NULL),
                7 => {
                    let bits = u64::from_be_bytes(b.try_into().unwrap());
                    let f = f64::from_bits(bits) as f32;
                    if let Some((m, e)) = decimal(bits) {
                        c.tag.push(T_DEC + e);
                        c.ints.push(m);
                        c.iexp.push(e);
                        c.hpred.push(last);
                        last = m;
                    } else if (f as f64).to_bits() == bits {
                        c.tag.push(T_F32);
                        c.real.extend_from_slice(&f.to_bits().to_be_bytes());
                    } else {
                        c.tag.push(T_REAL);
                        c.real.extend_from_slice(b);
                    }
                }
                1..=9 => {
                    let v = int_value(st, b);
                    c.tag.push(if canon_st(v, fmt4) == st { T_INT } else { T_ODD_INT + st as u8 });
                    c.ints.push(v);
                    c.iexp.push(0);
                    c.hpred.push(last);
                    last = v;
                }
                _ => {
                    let text = st & 1 == 1;
                    if b.len() >= MIN_REP && b.len() <= u32::MAX as usize {
                        let h = xxh3_64(b);
                        match c.seen.get(&h) {
                            Some(&id)
                                if {
                                    let (o, l) = c.distinct[id as usize];
                                    &c.bytes[o as usize..(o + l) as usize] == b
                                } =>
                            {
                                c.tag.push(if text { T_TEXT_REP } else { T_BLOB_REP });
                                put_uv(&mut c.len, (c.distinct.len() - 1 - id as usize) as u64);
                                continue;
                            }
                            _ => {
                                c.seen.insert(h, c.distinct.len() as u32);
                                c.distinct.push((c.bytes.len() as u32, b.len() as u32));
                            }
                        }
                    }
                    c.tag.push(if text { T_TEXT } else { T_BLOB });
                    put_uv(&mut c.len, b.len() as u64);
                    c.bytes.extend_from_slice(b);
                }
            }
        }
    }

    /// Per-column statistics (research aid).
    pub fn stats(&self) -> String {
        let mut o = format!("refs={}", self.rec.iter().filter(|&&c| c == R_REF as u8).count());
        for (j, c) in self.cols.iter().enumerate() {
            let mut h = [0usize; 32];
            for &t in &c.tag {
                h[t as usize & 31] += 1;
            }
            o += &format!(
                " | c{j} n{} i{} r{} t{} b{} int={} real={} len={} bytes={}",
                h[0],
                h[1] + h[17..26].iter().sum::<usize>(),
                h[2],
                h[3],
                h[4],
                c.ints.len() * 8,
                c.real.len(),
                c.len.len(),
                c.bytes.len()
            );
        }
        o
    }

    /// Appends the group's streams, and its directory entry.
    pub fn finish(&self, dir: &mut Vec<u8>, s: &mut [Vec<u8>; 8], tmp: &mut Vec<u8>) {
        let [rec, rowid, rawpay, tag, int, real, len, bytes] = s;
        put_uv(dir, self.rec.len() as u64);
        put_uv(dir, self.rowid.len() as u64);
        put_uv(dir, self.rawpay.len() as u64);
        put_uv(dir, self.cols.len() as u64);
        rec.extend_from_slice(&self.rec);
        rowid.extend_from_slice(&self.rowid);
        rawpay.extend_from_slice(&self.rawpay);
        for c in &self.cols {
            // Scale to the largest exponent if nothing overflows.
            let top = c.iexp.iter().copied().max().unwrap_or(0);
            let scaled: Option<Vec<i64>> = (top > 0)
                .then(|| {
                    c.ints
                        .iter()
                        .zip(&c.iexp)
                        .map(|(&v, &e)| P10I.get((top - e) as usize).and_then(|&p| v.checked_mul(p)))
                        .collect()
                })
                .flatten();
            // Integers as they are, against the column's previous one, or against the
            // previous integer of the record, scaled or not: whichever takes the fewest bytes.
            let cost = |ints: &[i64]| {
                let mut cost = [0usize; 3];
                let mut prev = 0i64;
                for (&v, &h) in ints.iter().zip(&c.hpred) {
                    cost[0] += uv_len(zz(v));
                    cost[1] += uv_len(zz(v.wrapping_sub(prev)));
                    cost[2] += uv_len(zz(v.wrapping_sub(h)));
                    prev = v;
                }
                (0..3).map(|m| (cost[m], m)).min().unwrap()
            };
            let (plain, mut mode) = cost(&c.ints);
            let mut scaled = scaled;
            if let Some(sc) = &scaled {
                let (k, m) = cost(sc);
                if k < plain {
                    mode = m;
                } else {
                    scaled = None;
                }
            }
            let ints = scaled.as_deref().unwrap_or(&c.ints);
            tmp.clear();
            let mut prev = 0i64;
            for (&v, &h) in ints.iter().zip(&c.hpred) {
                put_sv(tmp, v.wrapping_sub([0, prev, h][mode]));
                prev = v;
            }
            put_uv(dir, c.tag.len() as u64);
            put_uv(dir, mode as u64 + 3 * if scaled.is_some() { top as u64 + 1 } else { 0 });
            put_uv(dir, tmp.len() as u64);
            put_uv(dir, c.real.len() as u64);
            put_uv(dir, c.len.len() as u64);
            put_uv(dir, c.bytes.len() as u64);
            tag.extend_from_slice(&c.tag);
            int.extend_from_slice(tmp);
            real.extend_from_slice(&c.real);
            len.extend_from_slice(&c.len);
            bytes.extend_from_slice(&c.bytes);
        }
    }
}

pub struct ColDec<'a> {
    tag: Rd<'a>,
    int: Rd<'a>,
    mode: u8,
    /// The column's largest decimal exponent if scaled, else 0.
    scale: u8,
    prev: i64,
    real: Rd<'a>,
    len: Rd<'a>,
    bytes: Rd<'a>,
    distinct: Vec<(u32, u32)>,
}

impl ColDec<'_> {
    #[inline]
    fn next_int(&mut self, last: i64, e: u8) -> Result<i64> {
        let d = self.int.sv()?;
        let v = d.wrapping_add(match self.mode {
            0 => 0,
            1 => self.prev,
            _ => last,
        });
        self.prev = v;
        if self.scale == 0 {
            return Ok(v);
        }
        let p = *self.scale.checked_sub(e).and_then(|k| P10I.get(k as usize)).ok_or_else(corrupt)?;
        if v % p != 0 {
            return Err(corrupt());
        }
        Ok(v / p)
    }
}

pub struct GroupDec<'a> {
    rec: Rd<'a>,
    rowid: Rd<'a>,
    rawpay: Rd<'a>,
    cols: Vec<ColDec<'a>>,
}

/// The decoded records of a group, in order.
#[derive(Default)]
pub struct Recs {
    pub buf: Vec<u8>,
    pub ends: Vec<usize>,
    pub rowids: Vec<i64>,
}

impl Recs {
    #[inline]
    pub fn get(&self, i: usize) -> &[u8] {
        let s = if i == 0 { 0 } else { self.ends[i - 1] };
        &self.buf[s..self.ends[i]]
    }

    pub fn len(&self) -> usize {
        self.ends.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ends.is_empty()
    }
}

/// What a group's predicted records come from: its table's records, their order in the
/// index, and the table columns of the index.
pub struct RefCtx<'r> {
    pub table: &'r Recs,
    pub order: crate::index::Order,
    pub map: &'r [u32],
}

impl<'a> GroupDec<'a> {
    /// Takes the group's directory entry and streams from the cursors.
    pub fn new(dir: &mut Rd<'a>, s: &mut [Rd<'a>; 8]) -> Result<Self> {
        let [rec, rowid, rawpay, tag, int, real, len, bytes] = s;
        let rec = rec.sub(dir.usize()?)?;
        let rowid = rowid.sub(dir.usize()?)?;
        let rawpay = rawpay.sub(dir.usize()?)?;
        let nc = dir.usize()?;
        let mut cols = Vec::with_capacity(nc.min(1 << 16));
        for _ in 0..nc {
            let tag = tag.sub(dir.usize()?)?;
            let mode = dir.uv()?;
            if mode >= 3 * 24 {
                return Err(corrupt());
            }
            let scale = (mode / 3).saturating_sub(1) as u8;
            let mode = (mode % 3) as u8;
            let int = int.sub(dir.usize()?)?;
            let real = real.sub(dir.usize()?)?;
            let len = len.sub(dir.usize()?)?;
            let bytes = bytes.sub(dir.usize()?)?;
            cols.push(ColDec { tag, int, mode, scale, prev: 0, real, len, bytes, distinct: Vec::new() });
        }
        Ok(GroupDec { rec, rowid, rawpay, cols })
    }

    /// Decodes all records (and rowids if `table`).
    pub fn decode(&mut self, table: bool, fmt4: bool, rc: Option<&RefCtx>) -> Result<Recs> {
        let mut r = Recs::default();
        r.buf.reserve(self.rawpay.b.len() + self.cols.iter().map(|c| c.bytes.b.len()).sum::<usize>());
        let mut vals: Vec<(u64, i64)> = Vec::new();
        let (mut prev_rowid, mut prev_rank) = (0i64, 0i64);
        while !self.rec.at_end() {
            if table {
                prev_rowid = prev_rowid.wrapping_add(self.rowid.sv()?);
                r.rowids.push(prev_rowid);
            }
            let code = self.rec.uv()?;
            match code {
                R_RAW => {
                    let n = self.rec.usize()?;
                    r.buf.extend_from_slice(self.rawpay.bytes(n)?);
                }
                R_REF => {
                    let rc = rc.ok_or_else(corrupt)?;
                    let rank = prev_rank.wrapping_add(1).wrapping_add(self.rowid.sv()?);
                    prev_rank = rank;
                    let row = *usize::try_from(rank)
                        .ok()
                        .and_then(|i| rc.order.perm.get(i))
                        .ok_or_else(corrupt)? as usize;
                    if !rc.order.ok[row] {
                        return Err(corrupt());
                    }
                    let m = rc.map.len();
                    let cv = &rc.order.vals[row * m..(row + 1) * m];
                    predict(rc.table.get(row), rc.table.rowids[row], rc.map, cv, fmt4, &mut r.buf);
                }
                _ => self.columns((code - R_COLS) as usize, fmt4, &mut vals, &mut r.buf)?,
            }
            r.ends.push(r.buf.len());
        }
        Ok(r)
    }

    fn columns(
        &mut self,
        nc: usize,
        fmt4: bool,
        vals: &mut Vec<(u64, i64)>,
        out: &mut Vec<u8>,
    ) -> Result<()> {
        let cols = self.cols.get_mut(..nc).ok_or_else(corrupt)?;
        vals.clear();
        let mut s = 0u64;
        let mut last = 0i64;
        for c in cols.iter_mut() {
            let t = c.tag.u8()?;
            let (st, v) = match t {
                T_NULL => (0, 0),
                T_INT => {
                    let v = c.next_int(last, 0)?;
                    last = v;
                    (canon_st(v, fmt4), v)
                }
                T_REAL => (7, i64::from_be_bytes(c.real.bytes(8)?.try_into().unwrap())),
                T_F32 => {
                    let f = f32::from_bits(u32::from_be_bytes(c.real.bytes(4)?.try_into().unwrap()));
                    (7, (f as f64).to_bits() as i64)
                }
                T_DEC..=54 => {
                    let m = c.next_int(last, t - T_DEC)?;
                    last = m;
                    (7, (m as f64 / P10[(t - T_DEC) as usize]).to_bits() as i64)
                }
                T_TEXT | T_BLOB => {
                    let n = c.len.uv()?;
                    if n > u32::MAX as u64 {
                        return Err(corrupt());
                    }
                    (12 + 2 * n + (t == T_TEXT) as u64, -1)
                }
                T_TEXT_REP | T_BLOB_REP => {
                    let d = c.len.usize()?;
                    let id = c.distinct.len().checked_sub(d + 1).ok_or_else(corrupt)?;
                    let &(_, n) = c.distinct.get(id).ok_or_else(corrupt)?;
                    (12 + 2 * n as u64 + (t == T_TEXT_REP) as u64, id as i64)
                }
                17..=25 => {
                    let v = c.next_int(last, 0)?;
                    last = v;
                    ((t - T_ODD_INT) as u64, v)
                }
                _ => return Err(corrupt()),
            };
            s += sqlite_len(st) as u64;
            vals.push((st, v));
        }
        let mut b = [0u8; 9];
        let n = put_sqlite(&mut b, header_len(s));
        out.extend_from_slice(&b[..n]);
        for &(st, _) in vals.iter() {
            let n = put_sqlite(&mut b, st);
            out.extend_from_slice(&b[..n]);
        }
        for (c, &(st, v)) in cols.iter_mut().zip(vals.iter()) {
            match st {
                0 | 8 | 9 => {}
                1..=6 => out.extend_from_slice(&v.to_be_bytes()[8 - INT_SIZE[st as usize]..]),
                7 => out.extend_from_slice(&v.to_be_bytes()),
                _ if v >= 0 => {
                    let (o, n) = c.distinct[v as usize];
                    out.extend_from_slice(&c.bytes.b[o as usize..(o + n) as usize]);
                }
                _ => {
                    let n = st_size(st);
                    if n >= MIN_REP && n <= u32::MAX as usize {
                        c.distinct.push((c.bytes.p as u32, n as u32));
                    }
                    out.extend_from_slice(c.bytes.bytes(n)?);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimals() {
        for v in [0.1, 0.125, 12.99, -3.5, 1e-7, 123456.789, 0.30000000000000004, 1.0 / 3.0, 1e300, -0.0] {
            let bits = f64::to_bits(v);
            if let Some((m, e)) = decimal(bits) {
                assert_eq!((m as f64 / P10[e as usize]).to_bits(), bits, "{v}");
            }
        }
        assert_eq!(decimal(12.99f64.to_bits()), Some((1299, 2)));
        assert_eq!(decimal((-0.0f64).to_bits()), None);
    }

    #[test]
    fn records() {
        let mut cols = vec![];
        let mut out = vec![];
        // 3 columns: int 300, text "ab", NULL.
        let rec = [4u8, 2, 17, 0, 1, 44, b'a', b'b'];
        assert!(parse(&rec, &mut cols));
        assert_eq!(cols, vec![(2, 4), (17, 6), (0, 8)]);
        // Non-minimal header length, wrong total length.
        assert!(!parse(&[0x80, 4, 2, 17, 0, 1, 44, b'a', b'b'], &mut cols));
        assert!(!parse(&rec[..7], &mut cols));
        let cv = [cols[1], cols[0]];
        predict(&rec, 5, &[1, 0], &cv, true, &mut out);
        assert_eq!(out, [4u8, 17, 2, 1, b'a', b'b', 1, 44, 5]);
    }
}
