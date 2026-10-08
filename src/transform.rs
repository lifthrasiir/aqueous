//! The page-level transform: a database file is split into streams of like data, and rebuilt
//! from them bit for bit.
//!
//! Every page gets a kind: a b-tree page (its type byte), an overflow page claimed by a cell
//! of a b-tree page (rebuilt with that cell), an all-zero page, or a raw page (anything
//! else: freelist trunks, pointer maps, pages that fail any check). B-tree pages are coded
//! as fields:
//!
//! - the header (first freeblock, cell count, fragmented bytes, content start against the
//!   lowest cell, right child);
//! - each cell's offset, against the position right below the previous cell (where SQLite
//!   puts it when it builds a page);
//! - child pointers and interior keys, as deltas;
//! - records, column by column: pages are grouped into b-trees (connected by child
//!   pointers), and every b-tree has its own streams per column: a tag per value (NULL,
//!   integer, real, text, blob), integers (zigzag, as they are or as deltas, whichever is
//!   smaller per column), reals, text/blob lengths and bytes. Serial types, header lengths
//!   and payload lengths follow from the values;
//! - the record of a cell with overflow is put together from its overflow pages first, and
//!   the chain is coded as page number deltas;
//! - every byte of a page that no cell, header or cell pointer covers (free space,
//!   freeblocks, the database header, reserved bytes, the unused end of the last overflow
//!   page) goes to the filler stream as it is.
//!
//! Anything that would not be rebuilt exactly (non-minimal varints, a record whose header
//! or length does not add up, overlapping cells, broken overflow chains, ...) is not coded
//! as fields: a record is kept as raw payload, a page as raw page.

use crate::varint::{Rd, get_sqlite, put_sqlite, put_sv, put_uv, sqlite_len, uv_len, zz};
use crate::{Result, corrupt};

pub const K_RAW: u8 = 0;
pub const K_ZERO: u8 = 1;
pub const K_OVF: u8 = 3;

#[inline]
fn is_btree(t: u8) -> bool {
    matches!(t, 2 | 5 | 10 | 13)
}

#[inline]
fn be16(b: &[u8], i: usize) -> usize {
    u16::from_be_bytes([b[i], b[i + 1]]) as usize
}

#[inline]
fn be32(b: &[u8], i: usize) -> u32 {
    u32::from_be_bytes(b[i..i + 4].try_into().unwrap())
}

/// Page geometry and the database-wide facts the codings depend on.
#[derive(Clone, Copy, Debug)]
pub struct Geom {
    pub psize: usize,
    pub usable: usize,
    pub npages: usize,
    /// Schema format 4 and later code integers 0 and 1 as serial types 8 and 9.
    pub fmt4: bool,
}

impl Geom {
    pub fn from_file(f: &[u8]) -> Option<Geom> {
        if f.len() < 512 || &f[..16] != b"SQLite format 3\0" {
            return None;
        }
        let psize = match be16(f, 16) {
            1 => 65536,
            p if p.is_power_of_two() && (512..=32768).contains(&p) => p,
            _ => return None,
        };
        let usable = psize - f[20] as usize;
        if usable < 480 || f.len() < psize {
            return None;
        }
        let fmt4 = be32(f, 44) >= 4;
        Some(Geom { psize, usable, npages: f.len() / psize, fmt4 })
    }

    /// The part of a payload of `plen` bytes kept on the b-tree page.
    #[inline]
    pub fn local(&self, plen: u64, table_leaf: bool) -> usize {
        let u = self.usable;
        let x = if table_leaf { u - 35 } else { (u - 12) * 64 / 255 - 23 };
        if plen <= x as u64 {
            return plen as usize;
        }
        let m = (u - 12) * 32 / 255 - 23;
        let k = m + ((plen - m as u64) % (u as u64 - 4)) as usize;
        if k <= x { k } else { m }
    }

    #[inline]
    fn ovf_pages(&self, plen: u64, local: usize) -> u64 {
        (plen - local as u64).div_ceil(self.usable as u64 - 4)
    }
}

/// One cell, as parsed from its page.
#[derive(Clone, Copy, Default, Debug)]
struct Cell {
    off: usize,
    size: usize,
    /// Start of the payload on the page.
    pay: usize,
    local: usize,
    plen: u64,
    /// Rowid (table leaf) or key (table interior).
    key: u64,
    child: u32,
    /// First overflow page, 0 if none.
    ovf: u32,
}

/// Parses the cells of a b-tree page whose header is at `h`. Fails unless the page can be
/// rebuilt exactly from its fields and filler. `iv` receives the covered intervals, sorted.
fn parse_page(
    g: &Geom,
    pg: &[u8],
    h: usize,
    cells: &mut Vec<Cell>,
    iv: &mut Vec<(usize, usize)>,
) -> bool {
    cells.clear();
    iv.clear();
    let typ = pg[h];
    let end = g.usable;
    let interior = typ == 2 || typ == 5;
    let hl = if interior { 12 } else { 8 };
    if h + hl > end {
        return false;
    }
    let nc = be16(pg, h + 3);
    let ptr_end = h + hl + 2 * nc;
    if ptr_end > end {
        return false;
    }
    iv.push((h, ptr_end));
    let mut descending = true;
    let mut last = usize::MAX;
    for i in 0..nc {
        let off = be16(pg, h + hl + 2 * i);
        if off < ptr_end || off >= end {
            return false;
        }
        let mut c = Cell { off, ..Default::default() };
        let mut q = off;
        if interior {
            if q + 4 > end {
                return false;
            }
            c.child = be32(pg, q);
            q += 4;
        }
        let Some((v, n)) = get_sqlite(&pg[q..end]) else { return false };
        if n != sqlite_len(v) {
            return false;
        }
        q += n;
        if typ == 5 {
            c.key = v;
            c.size = q - off;
        } else {
            c.plen = v;
            if typ == 13 {
                let Some((k, n)) = get_sqlite(&pg[q..end]) else { return false };
                if n != sqlite_len(k) {
                    return false;
                }
                c.key = k;
                q += n;
            }
            c.pay = q;
            c.local = g.local(v, typ == 13);
            if c.local as u64 == v {
                c.size = q + c.local - off;
            } else {
                if g.ovf_pages(v, c.local) >= g.npages as u64 {
                    return false;
                }
                c.size = q + c.local + 4 - off;
                if off + c.size > end {
                    return false;
                }
                c.ovf = be32(pg, q + c.local);
            }
        }
        if off + c.size > end {
            return false;
        }
        descending &= off < last;
        last = off;
        cells.push(c);
        iv.push((off, off + c.size));
    }
    // The header and cell pointers come first: every cell starts past them.
    if descending {
        iv[1..].reverse();
    } else {
        iv[1..].sort_unstable();
    }
    iv.windows(2).all(|w| w[0].1 <= w[1].0)
}

/// Integer content size by serial type 0..=9.
const INT_SIZE: [usize; 10] = [0, 1, 2, 3, 4, 6, 8, 8, 0, 0];

/// The serial type SQLite picks for an integer.
#[inline]
fn canon_st(v: i64, fmt4: bool) -> u64 {
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
fn st_size(st: u64) -> u64 {
    if st >= 12 { (st - 12) / 2 } else { INT_SIZE[st as usize] as u64 }
}

/// Header length of a record whose serial types take `s` bytes.
#[inline]
fn header_len(s: u64) -> u64 {
    let k = sqlite_len(s + 1) as u64;
    if sqlite_len(s + k) as u64 != k { s + k + 1 } else { s + k }
}

// Value tags.
const T_NULL: u8 = 0;
const T_INT: u8 = 1;
const T_REAL: u8 = 2;
const T_TEXT: u8 = 3;
const T_BLOB: u8 = 4;
/// 16 + serial type: an integer with a serial type SQLite would not pick.
const T_ODD_INT: u8 = 16;

// Global streams.
pub const S_KIND: usize = 0;
pub const S_GRP: usize = 1;
pub const S_PHDR: usize = 2;
pub const S_CPTR: usize = 3;
pub const S_CHILD: usize = 4;
pub const S_IKEY: usize = 5;
pub const S_OVF: usize = 6;
pub const S_FILL: usize = 7;
pub const S_RAW: usize = 8;
pub const S_RAWPAY: usize = 9;
pub const S_DIR: usize = 10;
pub const S_REC: usize = 11;
pub const S_ROWID: usize = 12;
pub const S_TAG: usize = 13;
pub const S_INT: usize = 14;
pub const S_REAL: usize = 15;
pub const S_LEN: usize = 16;
pub const S_BYTES: usize = 17;
pub const NSTREAMS: usize = 18;

pub const STREAM_NAMES: [&str; NSTREAMS] = [
    "kind", "grp", "phdr", "cptr", "child", "ikey", "ovf", "fill", "raw", "rawpay", "dir", "rec",
    "rowid", "tag", "int", "real", "len", "bytes",
];

#[derive(Default)]
struct ColEnc {
    tag: Vec<u8>,
    ints: Vec<i64>,
    real: Vec<u8>,
    len: Vec<u8>,
    bytes: Vec<u8>,
}

#[derive(Default)]
struct GroupEnc {
    rec: Vec<u8>,
    rowid: Vec<u8>,
    prev_rowid: i64,
    cols: Vec<ColEnc>,
}

impl GroupEnc {
    /// Codes a record by columns, or as raw payload if it is not one SQLite would write.
    fn record(&mut self, pay: &[u8], fmt4: bool, sts: &mut Vec<u64>, rawpay: &mut Vec<u8>) {
        if !self.try_record(pay, fmt4, sts) {
            self.rec.push(0);
            put_uv(&mut self.rec, pay.len() as u64);
            rawpay.extend_from_slice(pay);
        }
    }

    fn try_record(&mut self, pay: &[u8], fmt4: bool, sts: &mut Vec<u64>) -> bool {
        sts.clear();
        let Some((hl, n)) = get_sqlite(pay) else { return false };
        if n != sqlite_len(hl) || hl > pay.len() as u64 {
            return false;
        }
        let hl = hl as usize;
        let mut q = n;
        let mut body = 0u64;
        while q < hl {
            let Some((st, n)) = get_sqlite(&pay[q..hl]) else { return false };
            if n != sqlite_len(st) || st == 10 || st == 11 {
                return false;
            }
            q += n;
            body += st_size(st);
            sts.push(st);
        }
        if header_len((hl - sqlite_len(hl as u64)) as u64) != hl as u64
            || hl as u64 + body != pay.len() as u64
        {
            return false;
        }
        put_uv(&mut self.rec, sts.len() as u64 + 1);
        if self.cols.len() < sts.len() {
            self.cols.resize_with(sts.len(), Default::default);
        }
        let mut b = hl;
        for (c, &st) in self.cols.iter_mut().zip(sts.iter()) {
            match st {
                0 => c.tag.push(T_NULL),
                1..=6 => {
                    let sz = INT_SIZE[st as usize];
                    let mut v = (pay[b] as i8) as i64;
                    for &x in &pay[b + 1..b + sz] {
                        v = (v << 8) | x as i64;
                    }
                    b += sz;
                    c.tag.push(if canon_st(v, fmt4) == st { T_INT } else { T_ODD_INT + st as u8 });
                    c.ints.push(v);
                }
                8 | 9 => {
                    let v = st as i64 - 8;
                    c.tag.push(if canon_st(v, fmt4) == st { T_INT } else { T_ODD_INT + st as u8 });
                    c.ints.push(v);
                }
                7 => {
                    c.tag.push(T_REAL);
                    c.real.extend_from_slice(&pay[b..b + 8]);
                    b += 8;
                }
                _ => {
                    let n = ((st - 12) / 2) as usize;
                    c.tag.push(if st & 1 == 1 { T_TEXT } else { T_BLOB });
                    put_uv(&mut c.len, n as u64);
                    c.bytes.extend_from_slice(&pay[b..b + n]);
                    b += n;
                }
            }
        }
        true
    }
}

pub struct Encoded {
    pub geom: Geom,
    pub streams: Vec<Vec<u8>>,
}

#[inline]
fn hdr_off(p: usize) -> usize {
    if p == 1 { 100 } else { 0 }
}

fn find(parent: &mut [u32], mut x: u32) -> u32 {
    while parent[x as usize] != x {
        let p = parent[x as usize];
        parent[x as usize] = parent[p as usize];
        x = p;
    }
    x
}

/// Splits a database file into streams. `None` if it is not one we can code.
pub fn encode(file: &[u8]) -> Option<Encoded> {
    let g = Geom::from_file(file)?;
    let (ps, n) = (g.psize, g.npages);
    let page = |p: usize| &file[(p - 1) * ps..p * ps];

    // Classify pages and claim overflow chains.
    let mut kind = vec![K_RAW; n + 1];
    let mut claimed = vec![false; n + 1];
    let mut cells = Vec::new();
    let mut iv = Vec::new();
    let mut mine = Vec::new();
    for p in 1..=n {
        if claimed[p] {
            continue;
        }
        let pg = page(p);
        let h = hdr_off(p);
        if !is_btree(pg[h]) {
            kind[p] = if pg.iter().all(|&b| b == 0) { K_ZERO } else { K_RAW };
            continue;
        }
        if !parse_page(&g, pg, h, &mut cells, &mut iv) {
            continue;
        }
        mine.clear();
        let mut ok = true;
        'cells: for c in &cells {
            if c.ovf == 0 && c.local as u64 == c.plen {
                continue;
            }
            let mut left = c.plen - c.local as u64;
            let mut q = c.ovf as usize;
            loop {
                if q < 2 || q > n || claimed[q] || is_btree(page(q)[0]) {
                    ok = false;
                    break 'cells;
                }
                claimed[q] = true;
                mine.push(q);
                left -= left.min(g.usable as u64 - 4);
                let next = be32(page(q), 0) as usize;
                if left == 0 {
                    if next != 0 {
                        ok = false;
                        break 'cells;
                    }
                    break;
                }
                q = next;
            }
        }
        if ok {
            kind[p] = pg[h];
        } else {
            for &q in &mine {
                claimed[q] = false;
            }
        }
    }
    for p in 1..=n {
        if claimed[p] {
            kind[p] = K_OVF;
        }
    }

    // Group b-tree pages into trees by their child pointers.
    let mut parent: Vec<u32> = (0..=n as u32).collect();
    for p in 1..=n {
        if kind[p] != 2 && kind[p] != 5 {
            continue;
        }
        let pg = page(p);
        let h = hdr_off(p);
        parse_page(&g, pg, h, &mut cells, &mut iv);
        let rc = be32(pg, h + 8);
        for c in cells.iter().map(|c| c.child).chain(std::iter::once(rc)) {
            let c = c as usize;
            if c >= 1 && c <= n && is_btree(kind[c]) {
                let (a, b) = (find(&mut parent, p as u32), find(&mut parent, c as u32));
                parent[a.max(b) as usize] = a.min(b);
            }
        }
    }
    let mut gid = vec![u32::MAX; n + 1];
    let mut groups: Vec<GroupEnc> = Vec::new();

    let mut s: Vec<Vec<u8>> = (0..NSTREAMS).map(|_| Vec::new()).collect();
    s[S_KIND].reserve(n);
    let mut sts = Vec::new();
    let mut gather = Vec::new();
    let mut rawpay = Vec::new();
    let (mut prev_child, mut prev_ikey, mut last_ovf) = (0i64, 0i64, 0i64);
    for p in 1..=n {
        let k = kind[p];
        s[S_KIND].push(k);
        let pg = page(p);
        if k == K_RAW {
            s[S_RAW].extend_from_slice(pg);
            continue;
        }
        if !is_btree(k) {
            continue;
        }
        let h = hdr_off(p);
        let root = find(&mut parent, p as u32) as usize;
        if gid[root] == u32::MAX {
            gid[root] = groups.len() as u32;
            groups.push(GroupEnc::default());
        }
        put_uv(&mut s[S_GRP], gid[root] as u64);
        let grp = &mut groups[gid[root] as usize];
        parse_page(&g, pg, h, &mut cells, &mut iv);
        put_uv(&mut s[S_PHDR], be16(pg, h + 1) as u64);
        put_uv(&mut s[S_PHDR], cells.len() as u64);
        put_uv(&mut s[S_PHDR], pg[h + 7] as u64);
        let mut pos = g.usable;
        let mut minoff = g.usable;
        for c in &cells {
            put_sv(&mut s[S_CPTR], c.off as i64 - (pos as i64 - c.size.max(4) as i64));
            pos = c.off;
            minoff = minoff.min(c.off);
            if k == 2 || k == 5 {
                put_sv(&mut s[S_CHILD], c.child as i64 - (prev_child + 1));
                prev_child = c.child as i64;
            }
            if k == 5 {
                put_sv(&mut s[S_IKEY], (c.key as i64).wrapping_sub(prev_ikey));
                prev_ikey = c.key as i64;
                continue;
            }
            if k == 13 {
                put_sv(&mut grp.rowid, (c.key as i64).wrapping_sub(grp.prev_rowid));
                grp.prev_rowid = c.key as i64;
            }
            if c.local as u64 == c.plen {
                grp.record(&pg[c.pay..c.pay + c.local], g.fmt4, &mut sts, &mut rawpay);
                continue;
            }
            // Put the payload together, code the chain.
            gather.clear();
            gather.extend_from_slice(&pg[c.pay..c.pay + c.local]);
            let mut left = c.plen as usize - c.local;
            let mut q = c.ovf as usize;
            while left > 0 {
                let op = page(q);
                let take = left.min(g.usable - 4);
                gather.extend_from_slice(&op[4..4 + take]);
                s[S_FILL].extend_from_slice(&op[4 + take..]);
                put_sv(&mut s[S_OVF], q as i64 - (last_ovf + 1));
                last_ovf = q as i64;
                left -= take;
                q = be32(op, 0) as usize;
            }
            grp.record(&gather, g.fmt4, &mut sts, &mut rawpay);
        }
        if k == 2 || k == 5 {
            let rc = be32(pg, h + 8) as i64;
            put_sv(&mut s[S_CHILD], rc - (prev_child + 1));
            prev_child = rc;
        }
        let cs = match be16(pg, h + 5) {
            0 => 65536,
            v => v,
        };
        put_sv(&mut s[S_PHDR], cs as i64 - minoff as i64);
        let mut cur = 0;
        for &(a, b) in &iv {
            s[S_FILL].extend_from_slice(&pg[cur..a]);
            cur = b;
        }
        s[S_FILL].extend_from_slice(&pg[cur..]);
    }
    s[S_RAW].extend_from_slice(&file[n * ps..]);
    s[S_RAWPAY] = rawpay;

    // Lay the groups' streams out one after another, with a directory of their lengths.
    let mut tmp = Vec::new();
    put_uv(&mut s[S_DIR], groups.len() as u64);
    for grp in &groups {
        put_uv(&mut s[S_DIR], grp.rec.len() as u64);
        put_uv(&mut s[S_DIR], grp.rowid.len() as u64);
        put_uv(&mut s[S_DIR], grp.cols.len() as u64);
        s[S_REC].extend_from_slice(&grp.rec);
        s[S_ROWID].extend_from_slice(&grp.rowid);
        for c in &grp.cols {
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
                put_sv(&mut tmp, if use_delta { v.wrapping_sub(prev) } else { v });
                prev = v;
            }
            let d = &mut s[S_DIR];
            put_uv(d, c.tag.len() as u64);
            put_uv(d, use_delta as u64);
            put_uv(d, tmp.len() as u64);
            put_uv(d, c.real.len() as u64);
            put_uv(d, c.len.len() as u64);
            put_uv(d, c.bytes.len() as u64);
            s[S_TAG].extend_from_slice(&c.tag);
            s[S_INT].extend_from_slice(&tmp);
            s[S_REAL].extend_from_slice(&c.real);
            s[S_LEN].extend_from_slice(&c.len);
            s[S_BYTES].extend_from_slice(&c.bytes);
        }
    }
    Some(Encoded { geom: g, streams: s })
}

struct ColDec<'a> {
    tag: Rd<'a>,
    int: Rd<'a>,
    delta: bool,
    prev: i64,
    real: Rd<'a>,
    len: Rd<'a>,
    bytes: Rd<'a>,
}

struct GroupDec<'a> {
    rec: Rd<'a>,
    rowid: Rd<'a>,
    prev_rowid: i64,
    cols: Vec<ColDec<'a>>,
}

impl GroupDec<'_> {
    /// Rebuilds a record (or raw payload) into `out`.
    fn record(
        &mut self,
        fmt4: bool,
        out: &mut Vec<u8>,
        vals: &mut Vec<(u64, i64)>,
        rawpay: &mut Rd,
    ) -> Result<()> {
        out.clear();
        let nc = self.rec.uv()?;
        if nc == 0 {
            let n = self.rec.usize()?;
            out.extend_from_slice(rawpay.bytes(n)?);
            return Ok(());
        }
        let nc = (nc - 1) as usize;
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
        let hl = header_len(s);
        let mut b = [0u8; 9];
        let n = put_sqlite(&mut b, hl);
        out.extend_from_slice(&b[..n]);
        for &(st, _) in vals.iter() {
            let n = put_sqlite(&mut b, st);
            out.extend_from_slice(&b[..n]);
        }
        for (c, &(st, v)) in cols.iter_mut().zip(vals.iter()) {
            match st {
                0 | 8 | 9 => {}
                1..=6 => {
                    let sz = INT_SIZE[st as usize];
                    out.extend_from_slice(&v.to_be_bytes()[8 - sz..]);
                }
                7 => out.extend_from_slice(c.real.bytes(8)?),
                _ => out.extend_from_slice(c.bytes.bytes(((st - 12) / 2) as usize)?),
            }
        }
        Ok(())
    }
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

/// Rebuilds the file from its streams. `len` is the original length.
pub fn decode(g: &Geom, len: usize, s: &[Vec<u8>]) -> Result<Vec<u8>> {
    let (ps, np) = (g.psize, g.npages);
    if s.len() != NSTREAMS || np == 0 || np.checked_mul(ps) != Some(len - len % ps) {
        return Err(corrupt());
    }
    let mut out = vec![0u8; len];
    let mut rd: Vec<Rd> = s.iter().map(|v| Rd::new(v)).collect();

    // Groups.
    let mut dir = rd[S_DIR];
    let ng = dir.usize()?;
    let mut groups = Vec::with_capacity(ng.min(1 << 20));
    for _ in 0..ng {
        let rec = rd[S_REC].sub(dir.usize()?)?;
        let rowid = rd[S_ROWID].sub(dir.usize()?)?;
        let nc = dir.usize()?;
        let mut cols = Vec::with_capacity(nc.min(1 << 16));
        for _ in 0..nc {
            let tag = rd[S_TAG].sub(dir.usize()?)?;
            let delta = dir.uv()? != 0;
            let int = rd[S_INT].sub(dir.usize()?)?;
            let real = rd[S_REAL].sub(dir.usize()?)?;
            let len = rd[S_LEN].sub(dir.usize()?)?;
            let bytes = rd[S_BYTES].sub(dir.usize()?)?;
            cols.push(ColDec { tag, int, delta, prev: 0, real, len, bytes });
        }
        groups.push(GroupDec { rec, rowid, prev_rowid: 0, cols });
    }

    let [kind, grp, phdr, cptr, child, ikey, ovf, fill, raw, rawpay, ..] = &mut rd[..] else {
        unreachable!()
    };
    let mut pay = Vec::new();
    let mut cell = Vec::new();
    let mut vals = Vec::new();
    let mut iv: Vec<(usize, usize)> = Vec::new();
    let mut chain: Vec<usize> = Vec::new();
    let (mut prev_child, mut prev_ikey, mut last_ovf) = (0i64, 0i64, 0i64);
    let u = g.usable;
    let mut vb = [0u8; 9];
    for p in 1..=np {
        let k = kind.u8()?;
        let base = (p - 1) * ps;
        match k {
            K_RAW => {
                out[base..base + ps].copy_from_slice(raw.bytes(ps)?);
                continue;
            }
            K_ZERO | K_OVF => continue,
            2 | 5 | 10 | 13 => {}
            _ => return Err(corrupt()),
        }
        let h = hdr_off(p);
        let grp = groups.get_mut(grp.usize()?).ok_or_else(corrupt)?;
        let fb = phdr.uv()?;
        let nc = phdr.usize()?;
        let frag = phdr.uv()?;
        if fb > 0xffff || nc > 0xffff || frag > 0xff {
            return Err(corrupt());
        }
        let interior = k == 2 || k == 5;
        let hl = if interior { 12 } else { 8 };
        let ptr_end = h + hl + 2 * nc;
        if ptr_end > u {
            return Err(corrupt());
        }
        {
            let pg = &mut out[base..base + ps];
            pg[h] = k;
            pg[h + 1..h + 3].copy_from_slice(&(fb as u16).to_be_bytes());
            pg[h + 3..h + 5].copy_from_slice(&(nc as u16).to_be_bytes());
            pg[h + 7] = frag as u8;
        }
        iv.clear();
        iv.push((h, ptr_end));
        let mut pos = u;
        let mut minoff = u;
        for i in 0..nc {
            cell.clear();
            if interior {
                let c = prev_child + 1 + child.sv()?;
                if !(0..=u32::MAX as i64).contains(&c) {
                    return Err(corrupt());
                }
                prev_child = c;
                cell.extend_from_slice(&(c as u32).to_be_bytes());
            }
            chain.clear();
            let mut local = 0;
            if k == 5 {
                let key = prev_ikey.wrapping_add(ikey.sv()?);
                prev_ikey = key;
                let n = put_sqlite(&mut vb, key as u64);
                cell.extend_from_slice(&vb[..n]);
            } else {
                let mut rowid = 0;
                if k == 13 {
                    rowid = grp.prev_rowid.wrapping_add(grp.rowid.sv()?);
                    grp.prev_rowid = rowid;
                }
                grp.record(g.fmt4, &mut pay, &mut vals, rawpay)?;
                let plen = pay.len() as u64;
                let n = put_sqlite(&mut vb, plen);
                cell.extend_from_slice(&vb[..n]);
                if k == 13 {
                    let n = put_sqlite(&mut vb, rowid as u64);
                    cell.extend_from_slice(&vb[..n]);
                }
                local = g.local(plen, k == 13);
                cell.extend_from_slice(&pay[..local]);
                if local < pay.len() {
                    for _ in 0..g.ovf_pages(plen, local) {
                        let q = last_ovf + 1 + ovf.sv()?;
                        if q < 2 || q > np as i64 {
                            return Err(corrupt());
                        }
                        last_ovf = q;
                        chain.push(q as usize);
                    }
                    cell.extend_from_slice(&(chain[0] as u32).to_be_bytes());
                }
            }
            let size = cell.len();
            let off = pos as i64 - size.max(4) as i64 + cptr.sv()?;
            if off < ptr_end as i64 || off as usize + size > u {
                return Err(corrupt());
            }
            let off = off as usize;
            out[base + off..base + off + size].copy_from_slice(&cell);
            out[base + h + hl + 2 * i..base + h + hl + 2 * i + 2]
                .copy_from_slice(&(off as u16).to_be_bytes());
            iv.push((off, off + size));
            pos = off;
            minoff = minoff.min(off);
            // Overflow pages.
            let mut at = local;
            for (j, &q) in chain.iter().enumerate() {
                let ob = (q - 1) * ps;
                let next = chain.get(j + 1).copied().unwrap_or(0) as u32;
                let take = (pay.len() - at).min(u - 4);
                let op = &mut out[ob..ob + ps];
                op[..4].copy_from_slice(&next.to_be_bytes());
                op[4..4 + take].copy_from_slice(&pay[at..at + take]);
                op[4 + take..].copy_from_slice(fill.bytes(ps - 4 - take)?);
                at += take;
            }
        }
        let pg = &mut out[base..base + ps];
        if interior {
            let c = prev_child + 1 + child.sv()?;
            if !(0..=u32::MAX as i64).contains(&c) {
                return Err(corrupt());
            }
            prev_child = c;
            pg[h + 8..h + 12].copy_from_slice(&(c as u32).to_be_bytes());
        }
        let cs = minoff as i64 + phdr.sv()?;
        if !(1..=65536).contains(&cs) {
            return Err(corrupt());
        }
        pg[h + 5..h + 7].copy_from_slice(&(cs as u16).to_be_bytes());
        iv.sort_unstable();
        let mut cur = 0;
        for &(a, b) in iv.iter() {
            if a < cur {
                return Err(corrupt());
            }
            pg[cur..a].copy_from_slice(fill.bytes(a - cur)?);
            cur = b;
        }
        pg[cur..].copy_from_slice(fill.bytes(ps - cur)?);
    }
    let tail = len - np * ps;
    out[np * ps..].copy_from_slice(raw.bytes(tail)?);
    Ok(out)
}
