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

/// Appends the index entry SQLite builds from a table row: columns `map` of the row
/// (whose payload is `row`), then the rowid. False if the row has no such columns.
pub fn predict(
    row: &[u8],
    rowid: i64,
    map: &[u32],
    fmt4: bool,
    cols: &mut Vec<(u64, u32)>,
    out: &mut Vec<u8>,
) -> bool {
    if !parse(row, cols) {
        return false;
    }
    let rst = canon_st(rowid, fmt4);
    let st_of = |c: u32| if c == ROWID { Some(rst) } else { cols.get(c as usize).map(|x| x.0) };
    let mut s = sqlite_len(rst) as u64;
    for &c in map {
        let Some(st) = st_of(c) else { return false };
        s += sqlite_len(st) as u64;
    }
    let mut vb = [0u8; 9];
    let n = put_sqlite(&mut vb, header_len(s));
    out.extend_from_slice(&vb[..n]);
    for &c in map.iter().chain(std::iter::once(&ROWID)) {
        let n = put_sqlite(&mut vb, st_of(c).unwrap());
        out.extend_from_slice(&vb[..n]);
    }
    let rb = rowid.to_be_bytes();
    for &c in map.iter().chain(std::iter::once(&ROWID)) {
        if c == ROWID {
            out.extend_from_slice(&rb[8 - INT_SIZE[rst as usize]..]);
        } else {
            let (st, off) = cols[c as usize];
            out.extend_from_slice(&row[off as usize..off as usize + st_size(st)]);
        }
    }
    true
}

// Value tags.
const T_NULL: u8 = 0;
const T_INT: u8 = 1;
const T_REAL: u8 = 2;
const T_TEXT: u8 = 3;
const T_BLOB: u8 = 4;
/// 16 + serial type: an integer with a serial type SQLite would not pick.
const T_ODD_INT: u8 = 16;

// Record codes.
const R_RAW: u64 = 0;
const R_REF: u64 = 1;
const R_COLS: u64 = 2;

#[derive(Default)]
pub struct ColEnc {
    pub tag: Vec<u8>,
    pub ints: Vec<i64>,
    pub real: Vec<u8>,
    pub len: Vec<u8>,
    pub bytes: Vec<u8>,
}

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
        for (c, &(st, off)) in self.cols.iter_mut().zip(cols.iter()) {
            let b = &pay[off as usize..off as usize + st_size(st)];
            match st {
                0 => c.tag.push(T_NULL),
                7 => {
                    c.tag.push(T_REAL);
                    c.real.extend_from_slice(b);
                }
                1..=9 => {
                    let v = int_value(st, b);
                    c.tag.push(if canon_st(v, fmt4) == st { T_INT } else { T_ODD_INT + st as u8 });
                    c.ints.push(v);
                }
                _ => {
                    c.tag.push(if st & 1 == 1 { T_TEXT } else { T_BLOB });
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
            let raw: usize = c.ints.iter().map(|&v| uv_len(zz(v))).sum();
            let mut prev = 0i64;
            let delta: usize = c
                .ints
                .iter()
                .map(|&v| {
                    let d = v.wrapping_sub(prev);
                    prev = v;
                    uv_len(zz(d))
                })
                .sum();
            let use_delta = delta < raw;
            tmp.clear();
            let mut prev = 0i64;
            for &v in &c.ints {
                put_sv(tmp, if use_delta { v.wrapping_sub(prev) } else { v });
                prev = v;
            }
            put_uv(dir, c.tag.len() as u64);
            put_uv(dir, use_delta as u64);
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
    delta: bool,
    prev: i64,
    real: Rd<'a>,
    len: Rd<'a>,
    bytes: Rd<'a>,
}

impl ColDec<'_> {
    #[inline]
    fn next_int(&mut self) -> Result<i64> {
        let d = self.int.sv()?;
        let v = if self.delta { self.prev.wrapping_add(d) } else { d };
        self.prev = v;
        Ok(v)
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
    pub order: Vec<u32>,
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
            let delta = dir.uv()? != 0;
            let int = int.sub(dir.usize()?)?;
            let real = real.sub(dir.usize()?)?;
            let len = len.sub(dir.usize()?)?;
            let bytes = bytes.sub(dir.usize()?)?;
            cols.push(ColDec { tag, int, delta, prev: 0, real, len, bytes });
        }
        Ok(GroupDec { rec, rowid, rawpay, cols })
    }

    /// Decodes all records (and rowids if `table`).
    pub fn decode(&mut self, table: bool, fmt4: bool, rc: Option<&RefCtx>) -> Result<Recs> {
        let mut r = Recs::default();
        r.buf.reserve(self.rawpay.b.len() + self.cols.iter().map(|c| c.bytes.b.len()).sum::<usize>());
        let mut vals: Vec<(u64, i64)> = Vec::new();
        let mut tcols = Vec::new();
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
                        .and_then(|i| rc.order.get(i))
                        .ok_or_else(corrupt)? as usize;
                    let (row, rowid) = (rc.table.get(row), rc.table.rowids[row]);
                    if !predict(row, rowid, rc.map, fmt4, &mut tcols, &mut r.buf) {
                        return Err(corrupt());
                    }
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
        for c in cols.iter_mut() {
            let t = c.tag.u8()?;
            let (st, v) = match t {
                T_NULL => (0, 0),
                T_INT => {
                    let v = c.next_int()?;
                    (canon_st(v, fmt4), v)
                }
                T_REAL => (7, 0),
                T_TEXT | T_BLOB => {
                    let n = c.len.uv()?;
                    if n > u32::MAX as u64 {
                        return Err(corrupt());
                    }
                    (12 + 2 * n + (t == T_TEXT) as u64, 0)
                }
                17..=25 => ((t - T_ODD_INT) as u64, c.next_int()?),
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
                7 => out.extend_from_slice(c.real.bytes(8)?),
                _ => out.extend_from_slice(c.bytes.bytes(st_size(st))?),
            }
        }
        Ok(())
    }
}
