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
/// A table's rows in index order, and their indexed columns.
pub struct Order {
    pub perm: Vec<u32>,
    /// Per row and indexed column: (serial type, content offset); u32::MAX for the rowid.
    pub vals: Vec<(u64, u32)>,
    /// Whether the row has all indexed columns (else they read as NULL).
    pub ok: Vec<bool>,
}

/// Sorts by key, then by position (`keys` come in position order): LSD radix sort over
/// the bytes that vary.
fn radix_sort(keys: &mut Vec<(u128, u32)>) {
    let n = keys.len();
    if n < 512 {
        keys.sort_unstable();
        return;
    }
    let k0 = keys[0].0;
    let diff = keys.iter().fold(0u128, |a, k| a | (k.0 ^ k0));
    let bytes: Vec<u32> = (0..16).filter(|b| (diff >> (8 * b)) as u8 != 0).collect();
    if bytes.len() > 10 {
        keys.sort_unstable();
        return;
    }
    let mut tmp = keys.clone();
    for &b in &bytes {
        let mut cnt = [0usize; 257];
        for k in keys.iter() {
            cnt[(k.0 >> (8 * b)) as u8 as usize + 1] += 1;
        }
        for i in 0..256 {
            cnt[i + 1] += cnt[i];
        }
        for k in keys.iter() {
            let d = (k.0 >> (8 * b)) as u8 as usize;
            tmp[cnt[d]] = *k;
            cnt[d] += 1;
        }
        std::mem::swap(keys, &mut tmp);
    }
}

pub fn order(rows: &[&[u8]], rowids: &[i64], key: &Key) -> Order {
    let m = key.cols.len();
    let n = rows.len();
    let rb: Vec<[u8; 8]> = rowids.iter().map(|r| r.to_be_bytes()).collect();
    // (serial type, content offset) of the indexed columns, row by row; NULL when missing,
    // an offset of u32::MAX for the rowid.
    let mut vals: Vec<(u64, u32)> = Vec::with_capacity(n * m);
    let mut oks = Vec::with_capacity(n);
    let mut cols = Vec::new();
    for row in rows {
        let ok = parse(row, &mut cols);
        oks.push(ok && key.cols.iter().all(|&c| c == ROWID || (c as usize) < cols.len()));
        vals.extend(key.cols.iter().map(|&c| match c {
            ROWID => (6, u32::MAX),
            _ if ok => cols.get(c as usize).copied().unwrap_or((0, 0)),
            _ => (0, 0),
        }));
    }
    let val = |r: usize, j: usize| -> (u64, &[u8]) {
        let (st, off) = vals[r * m + j];
        if off == u32::MAX {
            (6, &rb[r][..])
        } else {
            (st, &rows[r][off as usize..off as usize + st_size(st)])
        }
    };
    // A u128 prefix that orders like the key (ties are compared in full): 64 bits per column.
    // An integer column has an exact code; text and blobs their first 8 bytes.
    let code = |j: usize| -> Option<Box<dyn Fn(usize) -> u64 + '_>> {
        let f = key.flags[j];
        let inv = if f & F_DESC != 0 { u64::MAX } else { 0 };
        if (0..n).all(|r| is_int(vals[r * m + j].0)) {
            return Some(Box::new(move |r| {
                let (st, b) = val(r, j);
                (int_value(st, b) as u64 ^ (1 << 63)) ^ inv
            }));
        }
        let text = (0..n).all(|r| class(vals[r * m + j].0) == 2);
        let blob = (0..n).all(|r| class(vals[r * m + j].0) == 3);
        if !text && !blob {
            return None;
        }
        let fold = text && f & F_NOCASE != 0;
        Some(Box::new(move |r| {
            let (_, b) = val(r, j);
            let mut p = [0u8; 8];
            let k = b.len().min(8);
            p[..k].copy_from_slice(&b[..k]);
            if fold {
                p.make_ascii_lowercase();
            }
            u64::from_be_bytes(p) ^ inv
        }))
    };
    let exact0 = m > 0 && (0..n).all(|r| is_int(vals[r * m].0));
    let hi = if m > 0 { code(0) } else { None };
    let lo: Option<Box<dyn Fn(usize) -> u64 + '_>> = if !exact0 {
        None
    } else if m == 1 {
        Some(Box::new(|r: usize| rowids[r] as u64 ^ (1 << 63)))
    } else {
        code(1)
    };
    let mut keys: Vec<(u128, u32)> = (0..n)
        .map(|r| {
            let h = hi.as_ref().map_or(0, |f| f(r)) as u128;
            let l = lo.as_ref().map_or(0, |f| f(r)) as u128;
            ((h << 64) | l, r as u32)
        })
        .collect();
    // Sort by prefix, then runs of equal prefixes in full.
    radix_sort(&mut keys);
    let full = |x: &(u128, u32), y: &(u128, u32)| {
        let (a, b) = (x.1 as usize, y.1 as usize);
        for j in 0..m {
            let (sa, va) = val(a, j);
            let (sb, vb) = val(b, j);
            let o = key_cmp(key.flags[j], sa, va, sb, vb);
            if o != Ordering::Equal {
                return o;
            }
        }
        rowids[a].cmp(&rowids[b]).then(a.cmp(&b))
    };
    let mut i = 0;
    while i < n {
        let mut j = i + 1;
        while j < n && keys[j].0 == keys[i].0 {
            j += 1;
        }
        if j - i > 1 {
            keys[i..j].sort_unstable_by(full);
        }
        i = j;
    }
    let perm = keys.into_iter().map(|k| k.1).collect();
    drop((hi, lo));
    Order { perm, vals, ok: oks }
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
