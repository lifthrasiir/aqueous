//! Index b-trees predicted from their tables.
//!
//! An entry of an index on a rowid table is the indexed columns of a row followed by its
//! rowid. When the entries match their rows, the decoder rebuilds them from the table: it
//! sorts the table's rows by the indexed columns (as SQLite compares them with the BINARY
//! collation, ascending, then by rowid), and each entry is coded as its rank in that order,
//! against the previous entry's rank plus one. The leaves of an index hold runs of
//! consecutive entries, so most ranks cost nothing.

use crate::record::{int_value, is_int, parse, st_size};
use std::cmp::Ordering;

/// SQLite's order of an integer and a real.
fn int_real_cmp(i: i64, r: f64) -> Ordering {
    if r.is_nan() {
        return Ordering::Greater;
    }
    if r < -9223372036854775808.0 {
        return Ordering::Greater;
    }
    if r >= 9223372036854775808.0 {
        return Ordering::Less;
    }
    let y = r as i64;
    match i.cmp(&y) {
        Ordering::Equal => (i as f64).partial_cmp(&r).unwrap_or(Ordering::Equal),
        o => o,
    }
}

#[inline]
fn class(st: u64) -> u8 {
    match st {
        0 => 0,
        1..=9 => 1,
        _ => 2 + (st & 1 == 0) as u8,
    }
}

/// Compares two values as SQLite's BINARY collation does.
fn value_cmp(sa: u64, a: &[u8], sb: u64, b: &[u8]) -> Ordering {
    let (ca, cb) = (class(sa), class(sb));
    if ca != cb {
        return ca.cmp(&cb);
    }
    match ca {
        0 => Ordering::Equal,
        1 => match (sa == 7, sb == 7) {
            (false, false) => int_value(sa, a).cmp(&int_value(sb, b)),
            (true, true) => {
                let (x, y) = (real(a), real(b));
                x.partial_cmp(&y).unwrap_or_else(|| x.total_cmp(&y))
            }
            (false, true) => int_real_cmp(int_value(sa, a), real(b)),
            (true, false) => int_real_cmp(int_value(sb, b), real(a)).reverse(),
        },
        _ => a.cmp(b),
    }
}

#[inline]
fn real(b: &[u8]) -> f64 {
    f64::from_bits(u64::from_be_bytes(b.try_into().unwrap()))
}

/// A table column standing for the rowid (an INTEGER PRIMARY KEY column is stored as NULL).
pub const ROWID: u32 = u32::MAX;
pub const F_DESC: u8 = 1;
pub const F_NOCASE: u8 = 2;

/// The key of an index: per indexed column, the table column (or `ROWID`) and its order.
#[derive(Clone, Debug, Default)]
pub struct Key {
    pub cols: Vec<u32>,
    pub flags: Vec<u8>,
}

fn nocase_cmp(a: &[u8], b: &[u8]) -> Ordering {
    let fa = a.iter().map(|c| c.to_ascii_lowercase());
    let fb = b.iter().map(|c| c.to_ascii_lowercase());
    fa.cmp(fb)
}

/// Compares two values of an indexed column.
#[inline]
fn key_cmp(f: u8, sa: u64, a: &[u8], sb: u64, b: &[u8]) -> Ordering {
    let o = if f & F_NOCASE != 0 && sa >= 13 && sb >= 13 && sa & 1 == 1 && sb & 1 == 1 {
        nocase_cmp(a, b)
    } else {
        value_cmp(sa, a, sb, b)
    };
    if f & F_DESC != 0 { o.reverse() } else { o }
}

/// The rows of a table (payloads, rowids) in the order of an index with key `key`.
/// Encoder and decoder must agree on it exactly, so ties fall back to the row number.
/// The indexed columns of a table's rows, and their rowids: either pointing into the rows
/// (encoder, which keeps them), or copied out (decoder, which decodes the table twice
/// rather than keep it).
#[derive(Default)]
pub struct Keys<'a> {
    /// The table column of each indexed column (or `ROWID`).
    pub map: Vec<u32>,
    /// Per row and indexed column: (serial type, offset in the row or in `bytes`); offset
    /// u32::MAX for the rowid (in `rb`). A missing column reads as NULL.
    vals: Vec<(u32, u32)>,
    rows: &'a [&'a [u8]],
    bytes: Vec<u8>,
    rb: Vec<[u8; 8]>,
    /// Whether the row has all indexed columns.
    pub ok: Vec<bool>,
    pub rowids: std::borrow::Cow<'a, [i64]>,
}

impl<'a> Keys<'a> {
    /// To be filled with `push`.
    pub fn new(map: &[u32]) -> Self {
        Keys { map: map.to_vec(), ..Default::default() }
    }

    /// Over rows that outlive it.
    pub fn over(map: &[u32], rows: &'a [&'a [u8]], rowids: &'a [i64], cols: &mut Vec<(u64, u32)>) -> Self {
        let mut k = Keys { map: map.to_vec(), rows, rowids: std::borrow::Cow::Borrowed(rowids), ..Default::default() };
        let has_rowid = map.contains(&ROWID);
        for (&row, &rowid) in rows.iter().zip(rowids) {
            k.add(row, rowid, false, cols);
            if has_rowid {
                k.rb.push(rowid.to_be_bytes());
            }
        }
        k
    }

    pub fn len(&self) -> usize {
        self.ok.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ok.is_empty()
    }

    /// Adds a row (its payload and rowid), copying its indexed columns. `cols` is scratch.
    /// False if the copies outgrow 32-bit offsets.
    pub fn push(&mut self, row: &[u8], rowid: i64, cols: &mut Vec<(u64, u32)>) -> bool {
        self.add(row, rowid, true, cols);
        self.rowids.to_mut().push(rowid);
        if self.map.contains(&ROWID) {
            self.rb.push(rowid.to_be_bytes());
        }
        self.bytes.len() < u32::MAX as usize
    }

    fn add(&mut self, row: &[u8], _rowid: i64, copy: bool, cols: &mut Vec<(u64, u32)>) {
        let ok = parse(row, cols);
        self.ok.push(ok && self.map.iter().all(|&c| c == ROWID || (c as usize) < cols.len()));
        for &c in &self.map {
            match c {
                ROWID => self.vals.push((6, u32::MAX)),
                _ => match cols.get(c as usize) {
                    Some(&(st, off)) if ok => {
                        if copy {
                            let at = self.bytes.len() as u32;
                            self.bytes.extend_from_slice(&row[off as usize..off as usize + st_size(st)]);
                            self.vals.push((st as u32, at));
                        } else {
                            self.vals.push((st as u32, off));
                        }
                    }
                    _ => self.vals.push((0, 0)),
                },
            }
        }
    }

    /// Serial type and content of indexed column `j` of row `r`.
    #[inline]
    pub fn value(&self, r: usize, j: usize) -> (u64, &[u8]) {
        let (st, off) = self.vals[r * self.map.len() + j];
        if off == u32::MAX {
            return (6, &self.rb[r]);
        }
        let st = st as u64;
        let src = if self.rows.is_empty() { &self.bytes[..] } else { self.rows[r] };
        (st, &src[off as usize..off as usize + st_size(st)])
    }
}

/// A key above a row number in the low 32 bits.
trait Packed: Copy + Ord {
    fn byte(self, i: u32) -> u8;
    fn row(self) -> u32;
    fn key(self) -> u128;
}

impl Packed for u64 {
    #[inline]
    fn byte(self, i: u32) -> u8 {
        (self >> (32 + 8 * i)) as u8
    }
    #[inline]
    fn row(self) -> u32 {
        self as u32
    }
    #[inline]
    fn key(self) -> u128 {
        (self >> 32) as u128
    }
}

impl Packed for u128 {
    #[inline]
    fn byte(self, i: u32) -> u8 {
        (self >> (32 + 8 * i)) as u8
    }
    #[inline]
    fn row(self) -> u32 {
        self as u32
    }
    #[inline]
    fn key(self) -> u128 {
        self >> 32
    }
}

/// Sorts packed keys (rows in ascending order) by their `nb` key bytes: LSD radix sort.
fn sort_packed<T: Packed + Default>(v: &mut Vec<T>, nb: usize) {
    if v.len() < 512 {
        v.sort_unstable();
        return;
    }
    let mut tmp = vec![T::default(); v.len()];
    for i in 0..nb as u32 {
        let mut cnt = [0usize; 257];
        for &x in v.iter() {
            cnt[x.byte(i) as usize + 1] += 1;
        }
        for d in 0..256 {
            cnt[d + 1] += cnt[d];
        }
        for &x in v.iter() {
            let d = x.byte(i) as usize;
            tmp[cnt[d]] = x;
            cnt[d] += 1;
        }
        std::mem::swap(v, &mut tmp);
    }
}

/// The rows of sorted packed keys, runs of equal keys sorted by `full`.
fn fix_runs<T: Packed>(v: &[T], full: impl Fn(&u32, &u32) -> Ordering) -> Vec<u32> {
    let mut ix: Vec<u32> = v.iter().map(|x| x.row()).collect();
    let n = v.len();
    let mut i = 0;
    while i < n {
        let mut j = i + 1;
        while j < n && v[j].key() == v[i].key() {
            j += 1;
        }
        if j - i > 1 {
            ix[i..j].sort_unstable_by(&full);
        }
        i = j;
    }
    ix
}

/// Sorts `ix` (which comes in ascending order) by `k`, keeping ties in order: LSD radix
/// sort over the bytes that vary.
fn radix_sort(k: &mut Vec<u128>, ix: &mut Vec<u32>) {
    let n = k.len();
    let k0 = k.first().copied().unwrap_or(0);
    let diff = k.iter().fold(0u128, |a, &x| a | (x ^ k0));
    let bytes: Vec<u32> = (0..16).filter(|b| (diff >> (8 * b)) as u8 != 0).collect();
    if n < 512 || bytes.len() > 10 {
        let mut p: Vec<(u128, u32)> = k.iter().copied().zip(ix.iter().copied()).collect();
        p.sort_unstable();
        for (i, (a, b)) in p.into_iter().enumerate() {
            k[i] = a;
            ix[i] = b;
        }
        return;
    }
    let (mut tk, mut ti) = (vec![0u128; n], vec![0u32; n]);
    for &b in &bytes {
        let mut cnt = [0usize; 257];
        for &x in k.iter() {
            cnt[(x >> (8 * b)) as u8 as usize + 1] += 1;
        }
        for i in 0..256 {
            cnt[i + 1] += cnt[i];
        }
        for (&x, &i) in k.iter().zip(ix.iter()) {
            let d = (x >> (8 * b)) as u8 as usize;
            tk[cnt[d]] = x;
            ti[cnt[d]] = i;
            cnt[d] += 1;
        }
        std::mem::swap(k, &mut tk);
        std::mem::swap(ix, &mut ti);
    }
}

/// The rows of `keys` in the order of an index with key `key` (which `keys` was built
/// for). Encoder and decoder must agree on it exactly, so ties fall back to the row number.
pub fn order(keys: &Keys, key: &Key) -> Vec<u32> {
    let m = key.cols.len();
    let n = keys.len();
    let rowids = &keys.rowids;
    {
        let value = |r: usize, j: usize| keys.value(r, j);
        // A u128 prefix that orders like the key (ties are compared in full): 64 bits per
        // column. An integer column has an exact code; text and blobs their first 8 bytes.
        let st_of = |r: usize, j: usize| keys.vals[r * m + j].0 as u64;
        let code = |j: usize| -> Option<Box<dyn Fn(usize) -> u64 + '_>> {
            let f = key.flags[j];
            let inv = if f & F_DESC != 0 { u64::MAX } else { 0 };
            if (0..n).all(|r| is_int(st_of(r, j))) {
                return Some(Box::new(move |r| {
                    let (st, v) = value(r, j);
                    (int_value(st, v) as u64 ^ (1 << 63)) ^ inv
                }));
            }
            let text = (0..n).all(|r| class(st_of(r, j)) == 2);
            let blob = (0..n).all(|r| class(st_of(r, j)) == 3);
            if !text && !blob {
                return None;
            }
            let fold = text && f & F_NOCASE != 0;
            Some(Box::new(move |r| {
                let (_, b) = value(r, j);
                let mut p = [0u8; 8];
                let k = b.len().min(8);
                p[..k].copy_from_slice(&b[..k]);
                if fold {
                    p.make_ascii_lowercase();
                }
                u64::from_be_bytes(p) ^ inv
            }))
        };
        let exact0 = m > 0 && (0..n).all(|r| is_int(st_of(r, 0)));
        let hi = if m > 0 { code(0) } else { None };
        let lo: Option<Box<dyn Fn(usize) -> u64 + '_>> = if !exact0 {
            None
        } else if m == 1 {
            Some(Box::new(|r: usize| rowids[r] as u64 ^ (1 << 63)))
        } else {
            code(1)
        };
        let prefix = |r: usize| -> u128 {
            let h = hi.as_ref().map_or(0, |f| f(r)) as u128;
            let l = lo.as_ref().map_or(0, |f| f(r)) as u128;
            (h << 64) | l
        };
        let full = |x: &u32, y: &u32| {
            let (a, b) = (*x as usize, *y as usize);
            for j in 0..m {
                let (sa, va) = value(a, j);
                let (sb, vb) = value(b, j);
                let o = key_cmp(key.flags[j], sa, va, sb, vb);
                if o != Ordering::Equal {
                    return o;
                }
            }
            rowids[a].cmp(&rowids[b]).then(a.cmp(&b))
        };
        // Only the bytes of the prefix that vary matter: taken most significant first, they
        // order like it. Few of them pack with the row number into one u64 or u128.
        let k0 = if n > 0 { prefix(0) } else { 0 };
        let diff = (0..n).fold(0u128, |a, r| a | (prefix(r) ^ k0));
        let bytes: Vec<u32> = (0..16).rev().filter(|b| (diff >> (8 * b)) as u8 != 0).collect();
        let compact = |x: u128| bytes.iter().fold(0u128, |a, &b| (a << 8) | ((x >> (8 * b)) as u8) as u128);
        if bytes.len() <= 4 {
            let mut v: Vec<u64> = (0..n).map(|r| ((compact(prefix(r)) as u64) << 32) | r as u64).collect();
            sort_packed(&mut v, bytes.len());
            fix_runs(&v, full)
        } else if bytes.len() <= 12 {
            let mut v: Vec<u128> = (0..n).map(|r| (compact(prefix(r)) << 32) | r as u128).collect();
            sort_packed(&mut v, bytes.len());
            fix_runs(&v, full)
        } else {
            let mut k: Vec<u128> = (0..n).map(prefix).collect();
            let mut ix: Vec<u32> = (0..n as u32).collect();
            radix_sort(&mut k, &mut ix);
            let mut i = 0;
            while i < n {
                let mut j = i + 1;
                while j < n && k[j] == k[i] {
                    j += 1;
                }
                if j - i > 1 {
                    ix[i..j].sort_unstable_by(full);
                }
                i = j;
            }
            ix
        }
    }
}

/// Is `b` the ASCII string `s` in the database's text encoding (1 UTF-8, 2 UTF-16le,
/// 3 UTF-16be)?
fn text_is(b: &[u8], enc: u32, s: &str) -> bool {
    match enc {
        2 => b.len() == 2 * s.len() && b.chunks(2).zip(s.bytes()).all(|(p, c)| p == [c, 0]),
        3 => b.len() == 2 * s.len() && b.chunks(2).zip(s.bytes()).all(|(p, c)| p == [0, c]),
        _ => b == s.as_bytes(),
    }
}

/// (index root page, table root page) of every index in the schema, from the records of
/// sqlite_schema (type, name, tbl_name, rootpage, sql).
pub fn schema_indexes(schema: &[&[u8]], enc: u32) -> Vec<(u32, u32)> {
    let mut cols = Vec::new();
    let mut tables: Vec<(&[u8], u32)> = Vec::new();
    let mut indexes: Vec<(&[u8], u32)> = Vec::new();
    for rec in schema {
        if !parse(rec, &mut cols) || cols.len() < 4 {
            continue;
        }
        let get = |j: usize| {
            let (st, off) = cols[j];
            (st, &rec[off as usize..off as usize + st_size(st)])
        };
        let (tst, typ) = get(0);
        let (nst, tbl) = get(2);
        let (rst, root) = get(3);
        if tst < 13 || tst & 1 == 0 || nst < 13 || nst & 1 == 0 || !is_int(rst) {
            continue;
        }
        let root = int_value(rst, root);
        if !(1..=u32::MAX as i64).contains(&root) {
            continue;
        }
        if text_is(typ, enc, "table") {
            let (_, name) = get(1);
            tables.push((name, root as u32));
        } else if text_is(typ, enc, "index") {
            indexes.push((tbl, root as u32));
        }
    }
    indexes
        .iter()
        .filter_map(|&(tbl, ir)| tables.iter().find(|t| t.0 == tbl).map(|t| (ir, t.1)))
        .collect()
}

/// Finds the key of an index from its entries: the table column of every indexed column
/// (the first whose value matches in every sampled entry's row; `row_of` finds a row by
/// rowid), and the order of each (from adjacent entries, which are mostly in index order).
pub fn discover_key(
    entries: &[&[u8]],
    rows: &[&[u8]],
    rowids: &[i64],
    row_of: &dyn Fn(i64) -> Option<usize>,
) -> Option<Key> {
    let mut ec = Vec::new();
    let mut tc = Vec::new();
    // Matches per indexed column: table columns, and the rowid; over entries spread across
    // the index (its first entries have the smallest keys, which coincide more often).
    let mut hits: Vec<Vec<u32>> = Vec::new();
    let mut rowid_hits: Vec<u32> = Vec::new();
    let mut seen = 0u32;
    let step = (entries.len() / 64).max(1);
    for e in entries.iter().step_by(step) {
        if !parse(e, &mut ec) || ec.is_empty() {
            continue;
        }
        let (rst, roff) = *ec.last().unwrap();
        if !is_int(rst) {
            continue;
        }
        let rowid = int_value(rst, &e[roff as usize..roff as usize + st_size(rst)]);
        let Some(r) = row_of(rowid) else { continue };
        if !parse(rows[r], &mut tc) {
            continue;
        }
        let k = ec.len() - 1;
        if seen == 0 {
            hits = vec![Vec::new(); k];
            rowid_hits = vec![0; k];
        } else if hits.len() != k {
            continue;
        }
        seen += 1;
        for (j, hj) in hits.iter_mut().enumerate() {
            let (est, ev) = col(e, &ec, j);
            if is_int(est) && int_value(est, ev) == rowids[r] {
                rowid_hits[j] += 1;
            }
            if hj.len() < tc.len() {
                hj.resize(tc.len(), 0);
            }
            for (c, &(st, off)) in tc.iter().enumerate() {
                if st == est && &rows[r][off as usize..off as usize + st_size(st)] == ev {
                    hj[c] += 1;
                }
            }
        }
    }
    let cols = hits
        .iter()
        .zip(&rowid_hits)
        .map(|(hj, &rh)| {
            let (best, c) = hj
                .iter()
                .enumerate()
                .map(|(c, &h)| (h, c as u32))
                .chain(std::iter::once((rh, ROWID)))
                .max_by_key(|&(h, c)| (h, std::cmp::Reverse(c)))?;
            (best * 2 > seen).then_some(c)
        })
        .collect::<Option<Vec<u32>>>()?;
    let flags = discover_flags(entries, cols.len());
    Some(Key { cols, flags })
}

/// Direction and collation of each indexed column: the variant that adjacent entries
/// (where the columns before agree) contradict least.
fn col<'b>(b: &'b [u8], c: &[(u64, u32)], j: usize) -> (u64, &'b [u8]) {
    let (st, off) = c[j];
    (st, &b[off as usize..off as usize + st_size(st)])
}

fn discover_flags(entries: &[&[u8]], m: usize) -> Vec<u8> {
    let mut viol = vec![[0usize; 4]; m];
    let (mut pa, mut pb) = (Vec::new(), Vec::new());
    let mut prev: Option<&[u8]> = None;
    for &e in entries.iter().take(8192) {
        if !parse(e, &mut pb) || pb.len() <= m {
            prev = None;
            continue;
        }
        if let Some(p) = prev {
            for j in 0..m {
                let (sa, a) = col(p, &pa, j);
                let (sb, b) = col(e, &pb, j);
                if value_cmp(sa, a, sb, b) == Ordering::Equal {
                    continue;
                }
                for (f, n) in viol[j].iter_mut().enumerate() {
                    *n += (key_cmp(f as u8, sa, a, sb, b) == Ordering::Greater) as usize;
                }
                break;
            }
        }
        std::mem::swap(&mut pa, &mut pb);
        prev = Some(e);
    }
    viol.iter()
        .map(|v| (0..4).min_by_key(|&f| (v[f], f)).unwrap() as u8)
        .collect()
}
