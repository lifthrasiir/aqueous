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
//! - records: pages are grouped into b-trees (connected by child pointers), and the records
//!   of every b-tree form a group, coded by columns (see `record`); index entries can be
//!   predicted from their table (see `index`). The record of a cell with overflow is put
//!   together from its overflow pages first, and the chain is coded as page number deltas;
//! - every byte of a page that no cell, header or cell pointer covers (free space,
//!   freeblocks, the database header, reserved bytes, the unused end of the last overflow
//!   page) goes to the filler stream as it is.
//!
//! Anything that would not be rebuilt exactly (non-minimal varints, a record whose header
//! or length does not add up, overlapping cells, broken overflow chains, ...) is not coded
//! as fields: a record is kept as raw payload, a page as raw page.
//!
//! Decoding first decodes every group's records (tables before the indexes predicted from
//! them), then rebuilds the pages in file order.

use crate::index;
use crate::page::{Cell, Ends, Geom, K_OVF, K_RAW, K_ZERO, be16, be32, hdr_off, is_btree, parse_page};
use crate::record::{GroupDec, GroupEnc, RefCtx, int_value, is_int, predict, st_size};
use crate::varint::{Rd, put_sqlite, put_sv, put_uv};
use crate::{Result, corrupt};

// The streams, in order: each is read with the help of those before it
// only (FORMAT.md).
pub const S_KIND: usize = 0;
pub const S_RAW: usize = 1;
pub const S_DIR: usize = 2;
/// The group streams (see `record`), in this order.
pub const S_REC: usize = 3;
pub const S_BYTES: usize = 10;
pub const S_GRP: usize = 11;
pub const S_PHDR: usize = 12;
pub const S_CHILD: usize = 13;
pub const S_IKEY: usize = 14;
pub const S_CPTR: usize = 15;
pub const S_OVF: usize = 16;
pub const S_FILL: usize = 17;
pub const NSTREAMS: usize = 18;

pub const STREAM_NAMES: [&str; NSTREAMS] = [
    "kind", "raw", "dir", "rec", "rowid", "rawpay", "tag", "int", "real", "len", "bytes", "grp", "phdr", "child",
    "ikey", "cptr", "ovf", "fill",
];

pub struct Encoded {
    pub geom: Geom,
    pub streams: Vec<Vec<u8>>,
}

fn find(parent: &mut [u32], mut x: u32) -> u32 {
    while parent[x as usize] != x {
        let p = parent[x as usize];
        parent[x as usize] = parent[p as usize];
        x = p;
    }
    x
}

/// The records of a group, as the encoder collects them.
#[derive(Default)]
struct GroupRecs<'a> {
    table: bool,
    first: usize,
    /// Payloads on their page; empty for those with overflow, which are put together in
    /// `arena`: (record, start, length) in `fix`.
    pays: Vec<&'a [u8]>,
    arena: Vec<u8>,
    fix: Vec<(u32, usize, usize)>,
    rowids: Vec<i64>,
}

impl<'a> GroupRecs<'a> {
    /// All payloads (a copy of the list only if some have overflow).
    fn view(&self) -> std::borrow::Cow<'_, [&[u8]]> {
        if self.fix.is_empty() {
            return std::borrow::Cow::Borrowed(&self.pays);
        }
        let mut v: Vec<&[u8]> = self.pays.clone();
        for &(j, start, len) in &self.fix {
            v[j as usize] = &self.arena[start..start + len];
        }
        std::borrow::Cow::Owned(v)
    }

    fn len(&self) -> usize {
        self.pays.len()
    }

    fn free(&mut self) {
        self.pays = Vec::new();
        self.arena = Vec::new();
        self.fix = Vec::new();
        self.rowids = Vec::new();
    }
}

/// Splits a database file into streams. `None` if it is not one we can code.
pub fn encode(file: &[u8]) -> Option<Encoded> {
    encode_threads(file, 1)
}

/// `encode` with `threads` threads (the groups are coded in parallel): the same streams.
pub fn encode_threads(file: &[u8], threads: usize) -> Option<Encoded> {
    let g = Geom::from_file(file)?;
    let (ps, n) = (g.psize, g.npages);
    let page = |p: usize| &file[(p - 1) * ps..p * ps];

    // Classify pages and claim overflow chains.
    let mut kind = vec![K_RAW; n + 1];
    let mut claimed = vec![false; n + 1];
    let mut cells: Vec<Cell> = Vec::new();
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
    crate::mem::mark("enc: classify");

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
    // A group per tree and kind of record (table rows, index entries).
    let mut gid_t = vec![u32::MAX; n + 1];
    let mut gid_i = vec![u32::MAX; n + 1];
    let mut groups: Vec<GroupRecs> = Vec::new();

    let mut s: Vec<Vec<u8>> = (0..NSTREAMS).map(|_| Vec::new()).collect();
    s[S_KIND].reserve(n);
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
        let base = (p - 1) * ps;
        let mut grp = None;
        if k != 5 {
            let root = find(&mut parent, p as u32) as usize;
            let gid = if k == 13 { &mut gid_t[root] } else { &mut gid_i[root] };
            if *gid == u32::MAX {
                *gid = groups.len() as u32;
                groups.push(GroupRecs { table: k == 13, first: p, ..Default::default() });
            }
            put_uv(&mut s[S_GRP], *gid as u64);
            grp = Some(&mut groups[*gid as usize]);
        }
        parse_page(&g, pg, h, &mut cells, &mut iv);
        put_uv(&mut s[S_PHDR], be16(pg, h + 1) as u64);
        put_uv(&mut s[S_PHDR], cells.len() as u64);
        put_uv(&mut s[S_PHDR], pg[h + 7] as u64);
        let mut ends = Ends::new(g.usable);
        let mut minoff = g.usable;
        for c in &cells {
            put_uv(&mut s[S_CPTR], ends.encode(c.off, c.size));
            minoff = minoff.min(c.off);
            if k == 2 || k == 5 {
                put_sv(&mut s[S_CHILD], c.child as i64 - (prev_child + 1));
                prev_child = c.child as i64;
            }
            let Some(grp) = grp.as_mut() else {
                put_sv(&mut s[S_IKEY], (c.key as i64).wrapping_sub(prev_ikey));
                prev_ikey = c.key as i64;
                continue;
            };
            if k == 13 {
                grp.rowids.push(c.key as i64);
            }
            if c.local as u64 == c.plen {
                grp.pays.push(&file[base + c.pay..base + c.pay + c.local]);
                continue;
            }
            // Put the payload together, code the chain.
            let start = grp.arena.len();
            grp.arena.extend_from_slice(&pg[c.pay..c.pay + c.local]);
            let mut left = c.plen as usize - c.local;
            let mut q = c.ovf as usize;
            while left > 0 {
                let op = page(q);
                let take = left.min(g.usable - 4);
                grp.arena.extend_from_slice(&op[4..4 + take]);
                s[S_FILL].extend_from_slice(&op[4 + take..]);
                put_sv(&mut s[S_OVF], q as i64 - (last_ovf + 1));
                last_ovf = q as i64;
                left -= take;
                q = be32(op, 0) as usize;
            }
            grp.fix.push((grp.pays.len() as u32, start, c.plen as usize));
            grp.pays.push(&[]);
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
    crate::mem::mark("enc: pages");

    // Indexes predicted from their tables: (table group, columns) per index group.
    let mut refs: Vec<Option<(usize, index::Key)>> = (0..groups.len()).map(|_| None).collect();
    let schema_root = find(&mut parent, 1) as usize;
    if kind[1] == 13 || kind[1] == 5 {
        let sg = gid_t[schema_root];
        if sg != u32::MAX {
            let enc = u32::from_be_bytes(file[56..60].try_into().unwrap());
            let schema = groups[sg as usize].view();
            for (ir, tr) in index::schema_indexes(&schema, enc) {
                let (ir, tr) = (ir as usize, tr as usize);
                if ir > n || tr > n || !is_btree(kind[ir]) || !is_btree(kind[tr]) {
                    continue;
                }
                let gi = gid_i[find(&mut parent, ir as u32) as usize];
                let gt = gid_t[find(&mut parent, tr as u32) as usize];
                if gi == u32::MAX || gt == u32::MAX || refs[gi as usize].is_some() {
                    continue;
                }
                let t = &groups[gt as usize];
                let rows = Rows::new(&t.rowids);
                let (vi, vt) = (groups[gi as usize].view(), t.view());
                if let Some(key) = index::discover_key(&vi, &vt, &t.rowids, &|r| rows.of(r).next()) {
                    refs[gi as usize] = Some((gt as usize, key));
                }
            }
        }
    }

    crate::mem::mark("enc: index keys");
    // Code the groups, freeing their payloads (and rowids) after their last use: a table's
    // with its last index.
    let mut gs: [Vec<Vec<u8>>; 8] = Default::default();
    put_uv(&mut s[S_DIR], groups.len() as u64);
    if threads > 1 && groups.len() > 1 {
        code_groups_par(&g, &mut groups, &refs, threads, &mut s[S_DIR], &mut gs);
    } else {
        let mut last_use: Vec<usize> = (0..groups.len()).collect();
        for (i, r) in refs.iter().enumerate() {
            if let Some((t, _)) = r {
                last_use[*t] = last_use[*t].max(i);
            }
        }
        let mut free_at: Vec<Vec<usize>> = vec![Vec::new(); groups.len()];
        for (x, &i) in last_use.iter().enumerate() {
            free_at[i].push(x);
        }
        let mut sc = Scratch::default();
        for i in 0..groups.len() {
            let r = refs[i].as_ref().map(|r| (r.0, &groups[r.0], &r.1));
            code_group(&g, i, &groups[i], r, &mut sc, &mut s[S_DIR], &mut gs);
            for &x in &free_at[i] {
                groups[x].free();
            }
        }
    }
    drop(groups);
    crate::mem::mark("enc: groups");
    // The pieces put together (exactly sized, each freed once copied).
    for (j, pieces) in gs.into_iter().enumerate() {
        s[S_REC + j] = if pieces.len() == 1 {
            pieces.into_iter().next().unwrap()
        } else {
            let mut v = Vec::with_capacity(pieces.iter().map(|p| p.len()).sum());
            for p in pieces {
                v.extend_from_slice(&p);
            }
            v
        };
    }
    Some(Encoded { geom: g, streams: s })
}

/// Buffers `code_group` reuses.
#[derive(Default)]
struct Scratch {
    cols: Vec<(u64, u32)>,
    pred: Vec<u8>,
}

/// Codes group `i` (`gr`; `r` if its records are predicted from a table: its number, the
/// group and the index key): its
/// directory entry goes to `d`, its streams to `gs` as pieces.
#[allow(clippy::too_many_arguments)]
fn code_group(
    g: &Geom,
    i: usize,
    gr: &GroupRecs,
    r: Option<(usize, &GroupRecs, &index::Key)>,
    sc: &mut Scratch,
    d: &mut Vec<u8>,
    gs: &mut [Vec<Vec<u8>>; 8],
) {
    let Scratch { cols, pred } = sc;
    let pays_i = gr.view();
    let mut ge = GroupEnc::default();
    d.push(gr.table as u8);
    match r {
        Some((t, _, key)) => {
            put_uv(d, t as u64 + 1);
            put_uv(d, key.cols.len() as u64);
            for (&c, &f) in key.cols.iter().zip(&key.flags) {
                put_uv(d, c.wrapping_add(1) as u64);
                d.push(f);
            }
        }
        None => put_uv(d, 0),
    }
    for &r in &gr.rowids {
        ge.rowid(r);
    }
    if let Some((_, tg, key)) = r {
        let pays_t = tg.view();
        let keys = index::Keys::over(&key.cols, &pays_t, &tg.rowids, cols);
        let perm = index::order(&keys, key);
        let mut rank = vec![0u32; perm.len()];
        for (k, &r) in perm.iter().enumerate() {
            rank[r as usize] = k as u32;
        }
        drop(perm);
        let rows = Rows::new(&tg.rowids);
        'entries: for &e in pays_i.iter() {
            // The entry's rowid, and the first of its rows (several in a broken table) that
            // predicts the entry.
            let rowid = crate::record::parse(e, cols)
                .then(|| cols.last().copied())
                .flatten()
                .filter(|&(st, _)| is_int(st))
                .map(|(st, off)| int_value(st, &e[off as usize..off as usize + st_size(st)]));
            for r in rowid.into_iter().flat_map(|rowid| rows.of(rowid)) {
                if !keys.ok[r] {
                    continue;
                }
                pred.clear();
                predict(&keys, r, g.fmt4, pred);
                if *pred == e {
                    ge.reference(rank[r] as i64);
                    continue 'entries;
                }
            }
            ge.record(e, g.fmt4, cols);
        }
    } else {
        for &e in pays_i.iter() {
            ge.record(e, g.fmt4, cols);
        }
    }
    if std::env::var_os("SQLR_DUMP").is_some() {
        eprintln!(
            "G {i} first={} table={} recs={} ref={:?} {}",
            gr.first,
            gr.table,
            gr.len(),
            r.map(|r| r.2),
            ge.stats()
        );
    }
    ge.finish(d, gs);
}

/// `code_group` for all groups on several threads: each group makes its own directory
/// entry and pieces, put together in group order. A group is freed when it and the indexes
/// predicted from it are done.
fn code_groups_par(
    g: &Geom,
    groups: &mut Vec<GroupRecs>,
    refs: &[Option<(usize, index::Key)>],
    threads: usize,
    dir: &mut Vec<u8>,
    gs: &mut [Vec<Vec<u8>>; 8],
) {
    use std::sync::RwLock;
    use std::sync::atomic::{
        AtomicUsize,
        Ordering::{AcqRel, Relaxed},
    };
    let n = groups.len();
    // The group's own use, and one per index predicted from it.
    let users: Vec<AtomicUsize> = (0..n).map(|_| AtomicUsize::new(1)).collect();
    for (t, _) in refs.iter().flatten() {
        users[*t].fetch_add(1, Relaxed);
    }
    let cost = |i: usize| groups[i].len() + refs[i].as_ref().map_or(0, |r| groups[r.0].len());
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(cost(i)));
    let locks: Vec<RwLock<GroupRecs>> = std::mem::take(groups).into_iter().map(RwLock::new).collect();
    let release = |x: usize| {
        if users[x].fetch_sub(1, AcqRel) == 1 {
            locks[x].write().unwrap().free();
        }
    };
    let res = crate::par::map(threads, n, &order, Scratch::default, |sc, i| {
        let (mut d, mut gp): (Vec<u8>, [Vec<Vec<u8>>; 8]) = Default::default();
        {
            let gr = locks[i].read().unwrap();
            let tg = refs[i].as_ref().map(|r| locks[r.0].read().unwrap());
            let r = refs[i].as_ref().zip(tg.as_deref()).map(|(r, tg)| (r.0, tg, &r.1));
            code_group(g, i, &gr, r, sc, &mut d, &mut gp);
        }
        release(i);
        if let Some((t, _)) = &refs[i] {
            release(*t);
        }
        (d, gp)
    });
    for (d, gp) in res.into_iter().map(|x| x.unwrap()) {
        dir.extend_from_slice(&d);
        for (to, from) in gs.iter_mut().zip(gp) {
            to.extend(from);
        }
    }
}

/// Finds rows by rowid: binary search, over the rowids or a sorted copy.
struct Rows<'a> {
    rowids: &'a [i64],
    /// (rowid, row) sorted, if the rowids are not increasing.
    idx: Vec<(i64, u32)>,
}

impl<'a> Rows<'a> {
    fn new(rowids: &'a [i64]) -> Self {
        let mut idx: Vec<(i64, u32)> = Vec::new();
        if !rowids.windows(2).all(|w| w[0] < w[1]) {
            idx = rowids.iter().enumerate().map(|(i, &r)| (r, i as u32)).collect();
            idx.sort_unstable();
        }
        Rows { rowids, idx }
    }

    /// The rows with rowid `r`, in row order.
    fn of(&self, r: i64) -> impl Iterator<Item = usize> + '_ {
        let (single, run) = if self.idx.is_empty() {
            (self.rowids.binary_search(&r).ok(), &[][..])
        } else {
            let k = self.idx.partition_point(|e| e.0 < r);
            let n = self.idx[k..].iter().take_while(|e| e.0 == r).count();
            (None, &self.idx[k..k + n])
        };
        single.into_iter().chain(run.iter().map(|e| e.1 as usize))
    }
}

/// Where rebuilt pages go: every page (and the tail) exactly once, in no particular order.
trait Sink {
    fn put(&mut self, at: usize, b: &[u8]);
    /// An all-zero page.
    fn zeros(&mut self, at: usize, n: usize);
}

/// Into the output of `len` bytes, zeroed as it is allocated: at the first page written, so
/// after the first passes (whose tasks, on WebAssembly, copy the memory as it is then).
struct Writer {
    out: Vec<u8>,
    len: usize,
}

impl Writer {
    #[inline]
    fn out(&mut self) -> &mut Vec<u8> {
        if self.out.len() != self.len {
            self.alloc();
        }
        &mut self.out
    }

    #[cold]
    #[inline(never)]
    fn alloc(&mut self) {
        self.out = vec![0; self.len];
    }
}

impl Sink for Writer {
    #[inline]
    fn put(&mut self, at: usize, b: &[u8]) {
        self.out()[at..at + b.len()].copy_from_slice(b);
    }
    fn zeros(&mut self, _: usize, _: usize) {}
}

/// Compares with the original instead, and checks that every page comes exactly once.
struct Checker<'a> {
    src: &'a [u8],
    psize: usize,
    seen: Vec<bool>,
    ok: bool,
}

impl Checker<'_> {
    #[inline]
    fn see(&mut self, at: usize, n: usize) -> Option<&[u8]> {
        let p = at / self.psize;
        if !at.is_multiple_of(self.psize) || self.seen.get(p) != Some(&false) {
            self.ok = false;
            return None;
        }
        self.seen[p] = true;
        let s = self.src.get(at..at + n);
        if s.is_none() {
            self.ok = false;
        }
        s
    }
}

impl Sink for Checker<'_> {
    #[inline]
    fn put(&mut self, at: usize, b: &[u8]) {
        if self.see(at, b.len()).is_some_and(|s| s != b) {
            self.ok = false;
        }
    }
    fn zeros(&mut self, at: usize, n: usize) {
        if self.see(at, n).is_some_and(|s| s.iter().any(|&x| x != 0)) {
            self.ok = false;
        }
    }
}

/// Rebuilds the file from its streams (of the sizes `sizes`). `len` is the original length.
pub fn decode(g: &Geom, len: usize, s: &[Vec<u8>], sizes: &Sizes) -> Result<Vec<u8>> {
    decode_threads(g, len, s, sizes, 1)
}

/// `decode` with `threads` threads (the first pass over the tables and indexes).
pub fn decode_threads(g: &Geom, len: usize, s: &[impl AsRef<[u8]>], sizes: &Sizes, threads: usize) -> Result<Vec<u8>> {
    // The output is no larger than the streams describe: a kind per page, the tail in `raw`
    // (checked before it is allocated).
    if s.len() != NSTREAMS || s[S_KIND].as_ref().len() != g.npages || len % g.psize > s[S_RAW].as_ref().len() {
        return Err(corrupt());
    }
    let mut w = Writer { out: Vec::new(), len };
    rebuild(g, len, s, &mut w, threads, Some(sizes), None)?;
    w.out();
    Ok(w.out)
}

/// Whether the streams rebuild `src` exactly (without allocating a copy of it), and the sizes
/// that decoding takes ([`Sizes`]), as far as it went.
pub fn check(g: &Geom, src: &[u8], s: &[Vec<u8>]) -> (bool, Sizes) {
    let np = g.npages;
    let mut c = Checker { src, psize: g.psize, seen: vec![false; np + 1], ok: true };
    if src.len() == np * g.psize {
        c.seen[np] = true;
    }
    let mut z = Sizes::default();
    let ok = rebuild(g, src.len(), s, &mut c, 1, None, Some(&mut z)).is_ok() && c.ok && c.seen.iter().all(|&x| x);
    (ok, z)
}

/// The sizes of [`check`] where the streams rebuild `src` exactly; else None.
pub fn verify(g: &Geom, src: &[u8], s: &[Vec<u8>]) -> Option<Sizes> {
    let (ok, z) = check(g, src, s);
    ok.then_some(z)
}

/// What decoding takes that the streams do not tell, as sqlite-recons 2's params give it (after
/// the geometry): the most bytes of a record, the most columns of a row of a table indexes are
/// predicted from, and for each index predicted (in the order of the groups) the bytes of the
/// columns its keys copy. Decoding holds to it; the encoder's check ([`verify`]) finds it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Sizes {
    pub record: u64,
    pub cols: u64,
    pub keys: Vec<u64>,
}

/// The indexes of each table (as `starts`, by table, into `list`) and the tables that have any.
struct ByTable {
    starts: Vec<u32>,
    list: Vec<u32>,
    tables: Vec<u32>,
}

impl ByTable {
    fn of(meta: &[(bool, Option<(usize, index::Key)>)]) -> Result<ByTable> {
        let ng = meta.len();
        // (the counts by table, then where each table's start)
        let mut starts = vec![0u32; ng + 1];
        for (_, r) in meta {
            if let Some((t, _)) = r {
                *starts.get_mut(*t).filter(|_| *t < ng).ok_or_else(corrupt)? += 1;
            }
        }
        let mut sum = 0;
        for s in &mut starts {
            (*s, sum) = (sum, sum + *s);
        }
        let mut list = vec![u32::MAX; sum as usize];
        let mut at = starts.clone();
        for (i, (_, r)) in meta.iter().enumerate() {
            if let Some((t, _)) = r {
                list[at[*t] as usize] = i as u32;
                at[*t] += 1;
            }
        }
        drop(at);
        let ntab = (0..ng).filter(|&t| starts[t] < starts[t + 1]).count();
        let mut tables = Vec::with_capacity(ntab);
        tables.extend((0..ng as u32).filter(|&t| starts[t as usize] < starts[t as usize + 1]));
        Ok(ByTable { starts, list, tables })
    }

    fn of_table(&self, t: usize) -> &[u32] {
        &self.list[self.starts[t] as usize..self.starts[t + 1] as usize]
    }
}

/// The sizes of [`Keys`](index::Keys) of index `i` (the `k`-th predicted) on table `t`: its
/// columns, rows, bytes copied, whether the rowid is indexed.
fn key_shape(
    meta: &[(bool, Option<(usize, index::Key)>)],
    gds: &[GroupDec],
    i: usize,
    bytes: u64,
) -> Result<(usize, usize, usize, bool)> {
    let (t, key) = meta[i].1.as_ref().ok_or_else(corrupt)?;
    let n = gds[*t].records()?;
    let b = usize::try_from(bytes).map_err(|_| corrupt())?;
    Ok((key.cols.len(), n, b, key.cols.contains(&index::ROWID)))
}

/// The next overflow page of a chain, against the last one.
fn next_ovf(ovf: &mut Rd, last: &mut i64, np: usize) -> Result<usize> {
    let q = (*last + 1).checked_add(ovf.sv()?).ok_or_else(corrupt)?;
    if q < 2 || q > np as i64 {
        return Err(corrupt());
    }
    *last = q;
    Ok(q as usize)
}

/// Rebuilds the file onto `out`: of the sizes `given` (the module's, which bound it), or finding
/// them into `found` (the encoder's check).
/// A group's directory entry but its decoder: whether it is a table's, and the table and key
/// of an index predicted from it.
type Meta = (bool, Option<(usize, index::Key)>);

/// The `ng` groups of the directory `dir` (after their count) over the streams `rd`: their
/// decoders and entries, the decoders' parts covering their streams exactly.
fn read_groups<'a>(dir: &mut Rd<'a>, rd: &[Rd<'a>], ng: usize) -> Result<(Vec<GroupDec<'a>>, Vec<Meta>)> {
    let mut gstreams: [Rd; 8] = rd[S_REC..S_REC + 8].try_into().unwrap();
    let mut gds = Vec::with_capacity(ng);
    let mut meta = Vec::with_capacity(ng);
    for _ in 0..ng {
        let table = dir.u8()? != 0;
        let r = match dir.usize()? {
            0 => None,
            t => {
                let m = dir.usize()?;
                if m > 1 << 16 {
                    return Err(corrupt());
                }
                let mut key = index::Key { cols: Vec::with_capacity(m), flags: Vec::with_capacity(m) };
                for _ in 0..m {
                    let c = u32::try_from(dir.uv()?).map_err(|_| corrupt())?;
                    let f = dir.u8()?;
                    if f > 3 {
                        return Err(corrupt());
                    }
                    key.cols.push(c.wrapping_sub(1));
                    key.flags.push(f);
                }
                Some((t - 1, key))
            }
        };
        // Only index entries are predicted.
        if table && r.is_some() {
            return Err(corrupt());
        }
        meta.push((table, r));
        gds.push(GroupDec::new(dir, &mut gstreams)?);
    }
    if !dir.at_end() || gstreams.iter().any(|r| !r.at_end()) {
        return Err(corrupt());
    }
    Ok((gds, meta))
}

/// Bytes of [`aqr_api::stack`] of the wasm32 sizes of what `rebuild` holds: a stream's reader,
/// a group's decoder, a column's, a group's entry, a group's context, an index's keys.
const RD_BYTES: u64 = 12;
const GROUP_BYTES: u64 = 80;
const COL_BYTES: u64 = 88;
const META_BYTES: u64 = 32;
const CTX_BYTES: u64 = 96;
const KEYS_BYTES: u64 = 84;
#[cfg(target_arch = "wasm32")]
const _: () = assert!(
    size_of::<Rd>() == RD_BYTES as usize
        && size_of::<GroupDec>() == GROUP_BYTES as usize
        && size_of::<crate::record::ColDec>() == COL_BYTES as usize
        && size_of::<Meta>() == META_BYTES as usize
        && size_of::<Option<RefCtx>>() == CTX_BYTES as usize
        && size_of::<index::Keys>() == KEYS_BYTES as usize
);

/// The memory plan of decoding on WebAssembly ([`aqr_api::stack`]), of geometry `g`, a file
/// of `len` bytes, the streams `s` and the sizes `z`, given the instance's memory as it starts
/// (`initial` bytes, whole pages) and `held` bytes of the stack taken before: the most the
/// instance takes and the most a task does, in bytes. As
/// `rebuild` takes it: the readers, the groups' decoders and entries, their contexts, the
/// indexes by table and a record's buffers; then the tables' scans as tasks, the instance
/// reserving every index's keys and taking each scan's; then the indexes' orders as tasks, the
/// instance reserving and taking each; then the pages' buffers and the file.
pub fn plan(g: &Geom, len: u64, s: &[&[u8]], z: &Sizes, initial: u64, held: u64) -> Option<(u64, u64)> {
    use aqr_api::stack::block;
    let pages = |n: u64| n.div_ceil(1 << 16) << 16;
    if s.len() != NSTREAMS {
        return None;
    }
    let rd: Vec<Rd> = s.iter().map(|v| Rd::new(v)).collect();
    let mut dir = rd[S_DIR];
    let ng = dir.usize().ok()?;
    let (gds, meta) = read_groups(&mut dir, &rd, ng).ok()?;
    let by = ByTable::of(&meta).ok()?;
    let n = |v: u64| usize::try_from(v).ok();
    let (rec, cols) = (n(z.record)?, n(z.cols)?);
    let decoder = |gd: &GroupDec| {
        let (nc, distinct, nv) = gd.room();
        block(COL_BYTES * nc as u64) + distinct.map(|d| block(8 * d as u64)).sum::<u64>() + block(16 * nv as u64)
    };
    let ng64 = ng as u64;
    let mut top =
        initial + held + block(RD_BYTES * NSTREAMS as u64) + block(GROUP_BYTES * ng64) + block(META_BYTES * ng64);
    for ((_, r), gd) in meta.iter().zip(&gds) {
        if let Some((_, key)) = r {
            top += block(4 * key.cols.len() as u64) + block(key.cols.len() as u64);
        }
        top += decoder(gd);
    }
    top += block(CTX_BYTES * ng64)
        + block(4 * (ng64 + 1))
        + block(4 * by.list.len() as u64)
        + block(4 * by.tables.len() as u64)
        + block(rec as u64)
        + block(16 * cols as u64);
    // (each index's keys: its shape, in the order of the groups)
    let mut k = 0;
    let mut shapes = vec![None; ng];
    for (i, (_, r)) in meta.iter().enumerate() {
        if r.is_some() {
            shapes[i] = Some(key_shape(&meta, &gds, i, *z.keys.get(k)?).ok()?);
            k += 1;
        }
    }
    if k != z.keys.len() {
        return None;
    }
    let keys_plan = |i: usize| shapes[i].map_or(0, |(m, n, b, rowid)| index::Keys::plan(m, n, b, rowid));
    let written = |i: usize| shapes[i].map_or(0, |(m, n, b, rowid)| index::Keys::written(m, n, b, rowid) as u64);
    let (mut end, mut most) = (pages(top), 0);
    if !by.tables.is_empty() {
        let scans: Vec<u64> = by
            .tables
            .iter()
            .map(|&t| {
                let idx = by.of_table(t as usize);
                let w: u64 = idx.iter().map(|&i| written(i as usize)).sum();
                let own = block(16 * cols as u64)
                    + block(rec as u64)
                    + block(w)
                    + block(KEYS_BYTES * idx.len() as u64)
                    + idx.iter().map(|&i| keys_plan(i as usize)).sum::<u64>()
                    + decoder(&gds[t as usize]);
                end.max(pages(top + own))
            })
            .collect();
        let joined = by
            .tables
            .iter()
            .map(|&t| block(by.of_table(t as usize).iter().map(|&i| written(i as usize)).sum()))
            .max()
            .unwrap_or(0);
        top += block(by.tables.len() as u64) + (0..ng).map(keys_plan).sum::<u64>();
        end = end.max(pages(top + joined));
        most = most.max(scans.into_iter().max().unwrap_or(0));
        // the orders
        let idxs: Vec<usize> = (0..ng).filter(|&i| shapes[i].is_some()).collect();
        top += block(4 * idxs.len() as u64);
        end = end.max(pages(top));
        let rows = |i: usize| shapes[i].map_or(0, |s| s.1) as u64;
        let orders: Vec<u64> = idxs
            .iter()
            .map(|&i| {
                let own = 2 * block(4 * rows(i)) + index::OrderBufs::plan(rows(i) as usize);
                end.max(pages(top + own))
            })
            .collect();
        let joined = idxs.iter().map(|&i| block(4 * rows(i))).max().unwrap_or(0);
        top += block(idxs.len() as u64)
            + block(12 * idxs.len() as u64)
            + idxs.iter().map(|&i| block(4 * rows(i))).sum::<u64>();
        end = end.max(pages(top + joined));
        most = most.max(orders.into_iter().max().unwrap_or(0));
    }
    let ps = g.psize as u64;
    // (the spans of a page: two usizes each, on wasm32)
    top += 2 * block(ps) + block(ps + 32) + block(8 * (ps / 2 + 2)) + block(len);
    Some((end.max(pages(top)), most))
}

fn rebuild<S: Sink>(
    g: &Geom,
    len: usize,
    s: &[impl AsRef<[u8]>],
    out: &mut S,
    threads: usize,
    given: Option<&Sizes>,
    mut found: Option<&mut Sizes>,
) -> Result<()> {
    let (ps, np) = (g.psize, g.npages);
    // A kind per page (which also bounds the output by the streams).
    if s.len() != NSTREAMS || np == 0 || np.checked_mul(ps) != Some(len - len % ps) || s[S_KIND].as_ref().len() != np {
        return Err(corrupt());
    }
    let mut rd: Vec<Rd> = s.iter().map(|v| Rd::new(v.as_ref())).collect();

    // Groups: directory, then records (the tables indexes refer to first).
    let mut dir = rd[S_DIR];
    let ng = dir.usize()?;
    // (a group takes a byte of the directory at least)
    if ng > s[S_DIR].as_ref().len() {
        return Err(corrupt());
    }
    let (mut gds, meta) = read_groups(&mut dir, &rd, ng)?;
    // The indexed columns of the tables indexes are predicted from: a first pass over each
    // such table (on a copy of its cursors), for all its indexes at once. Then every group
    // decodes record by record as the pages need them.
    if meta.iter().any(|(_, r)| matches!(r, Some((t, _)) if *t >= ng)) {
        return Err(corrupt());
    }
    let mut ctxs: Vec<Option<RefCtx>> = (0..ng).map(|_| None).collect();
    let by = ByTable::of(&meta)?;
    for (t, _) in meta.iter().enumerate() {
        if !by.of_table(t).is_empty() && (meta[t].1.is_some() || !meta[t].0) {
            return Err(corrupt());
        }
    }
    // (the bytes of each index's keys, in the order of the groups)
    let nidx = meta.iter().filter(|(_, r)| r.is_some()).count();
    let key_bytes = |i: usize| -> u64 {
        let k = meta[..i].iter().filter(|(_, r)| r.is_some()).count();
        given.map_or(0, |z| z.keys[k])
    };
    let (rec_cap, cols_cap) = match given {
        Some(z) => {
            if z.keys.len() != nidx {
                return Err(corrupt());
            }
            let n = |v: u64| usize::try_from(v).map_err(|_| corrupt());
            (n(z.record)?, n(z.cols)?)
        }
        None => (0, 0),
    };
    let mut scratch = Vec::with_capacity(rec_cap);
    let mut cols = Vec::with_capacity(cols_cap);
    let bounded = given.is_some();
    // The first pass over a table, for all its indexes at once: their keys (of the room the
    // sizes give, where they are given) written onto `out` (taken first: below the rest).
    #[allow(clippy::type_complexity)]
    let scan = |t: usize,
                cols: &mut Vec<(u64, u32)>,
                scratch: &mut Vec<u8>|
     -> Result<(Vec<u8>, Vec<index::Keys<'static>>, usize, usize)> {
        let idx = by.of_table(t);
        let mut size = 0;
        for &i in idx {
            let (m, n, b, rowid) = key_shape(&meta, &gds, i as usize, key_bytes(i as usize))?;
            size += index::Keys::written(m, n, b, rowid);
        }
        let mut written = Vec::with_capacity(if bounded { size } else { 0 });
        let mut keys: Vec<index::Keys> = Vec::with_capacity(idx.len());
        for &i in idx {
            let (_, n, b, _) = key_shape(&meta, &gds, i as usize, key_bytes(i as usize))?;
            let map = &meta[i as usize].1.as_ref().unwrap().1.cols;
            keys.push(if bounded { index::Keys::with_capacity(map, n, b) } else { index::Keys::new(map) });
        }
        let mut gd = gds[t].clone_reserved();
        let (mut most_rec, mut most_cols) = (0, 0);
        while !gd.at_end() {
            scratch.clear();
            let rowid = gd.next(true, g.fmt4, None, scratch)?;
            if bounded && scratch.len() > rec_cap {
                return Err(corrupt());
            }
            for k in &mut keys {
                if !k.push(scratch, rowid, cols) {
                    return Err(corrupt());
                }
                most_cols = most_cols.max(cols.len());
            }
            most_rec = most_rec.max(scratch.len());
        }
        if bounded {
            keys.iter().for_each(|k| k.write(&mut written));
        }
        drop(gd);
        Ok((written, keys, most_rec, most_cols))
    };
    let parallel = threads > 1 && !by.tables.is_empty() && bounded;
    if parallel {
        // Tables apart, then every index's order apart (as tasks on WebAssembly): each result,
        // written, read into what is reserved for it once the tasks are spawned.
        let nt = by.tables.len();
        let ctxs_c = std::cell::RefCell::new(&mut ctxs);
        // (as tasks even alone: what a scan frees in the instance would be holes, a column each)
        aqr_common::pool::par_each_apart(
            nt,
            threads,
            1,
            |k| {
                let mut sc = (Vec::with_capacity(cols_cap), Vec::with_capacity(rec_cap));
                match scan(by.tables[k] as usize, &mut sc.0, &mut sc.1) {
                    Ok((written, mut keys, ..)) => {
                        // (the keys given back from the last, then the buffers)
                        while keys.pop().is_some() {}
                        drop(keys);
                        written
                    }
                    Err(_) => Vec::new(),
                }
            },
            || {
                let mut ctxs = ctxs_c.borrow_mut();
                for i in 0..ng {
                    if meta[i].1.is_some()
                        && let Ok((_, n, b, _)) = key_shape(&meta, &gds, i, key_bytes(i))
                    {
                        let map = &meta[i].1.as_ref().unwrap().1.cols;
                        ctxs[i] = Some(RefCtx { keys: index::Keys::with_capacity(map, n, b), perm: Vec::new() });
                    }
                }
            },
            |k, d| {
                let mut ctxs = ctxs_c.borrow_mut();
                let mut at = 0;
                for &i in by.of_table(by.tables[k] as usize) {
                    let (m, n, b, rowid) = key_shape(&meta, &gds, i as usize, key_bytes(i as usize))?;
                    let w = index::Keys::written(m, n, b, rowid);
                    let keys = &mut ctxs[i as usize].as_mut().ok_or_else(corrupt)?.keys;
                    if !keys.read(d.get(at..at + w).ok_or_else(corrupt)?) {
                        return Err(corrupt());
                    }
                    at += w;
                }
                if at != d.len() { Err(corrupt()) } else { Ok(()) }
            },
        )?;
        let mut idxs = Vec::with_capacity(nidx);
        idxs.extend((0..ng).filter(|&i| ctxs[i].is_some()));
        let ctxs_ref = &ctxs;
        let perms_c = std::cell::RefCell::new(Vec::<Vec<u32>>::new());
        aqr_common::pool::par_each_apart(
            idxs.len(),
            threads,
            1,
            |k| {
                let i = idxs[k];
                let keys = &ctxs_ref[i].as_ref().unwrap().keys;
                let mut d = Vec::with_capacity(4 * keys.len());
                let mut bufs = index::OrderBufs::with_capacity(keys.len());
                let mut perm = Vec::with_capacity(keys.len());
                index::order_in(keys, &meta[i].1.as_ref().unwrap().1, &mut bufs, &mut perm);
                perm.iter().for_each(|r| d.extend_from_slice(&r.to_le_bytes()));
                d
            },
            || {
                let mut perms = perms_c.borrow_mut();
                perms.reserve_exact(idxs.len());
                for &i in &idxs {
                    perms.push(Vec::with_capacity(ctxs_ref[i].as_ref().unwrap().keys.ok.capacity()));
                }
            },
            |k, d| {
                let mut perms = perms_c.borrow_mut();
                let p = &mut perms[k];
                if d.len() != 4 * p.capacity() {
                    return Err(corrupt());
                }
                p.extend(d.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes(*c)));
                Ok(())
            },
        )?;
        for (&i, perm) in idxs.iter().zip(perms_c.into_inner()) {
            ctxs[i].as_mut().unwrap().perm = perm;
        }
    } else {
        for &t in &by.tables {
            let (_, keys, most_rec, most_cols) = scan(t as usize, &mut cols, &mut scratch)?;
            if let Some(z) = found.as_deref_mut() {
                z.record = z.record.max(most_rec as u64);
                z.cols = z.cols.max(most_cols as u64);
            }
            for (&i, keys) in by.of_table(t as usize).iter().zip(keys) {
                if let Some(z) = found.as_deref_mut() {
                    z.keys.push(keys.bytes_len() as u64);
                }
                let perm = index::order(&keys, &meta[i as usize].1.as_ref().unwrap().1);
                ctxs[i as usize] = Some(RefCtx { keys, perm });
            }
        }
        if let Some(z) = found.as_deref_mut() {
            // (in the order of the groups, rather than of the tables)
            let mut order: Vec<usize> =
                by.tables.iter().flat_map(|&t| by.of_table(t as usize)).map(|&i| i as usize).collect();
            let found = std::mem::take(&mut z.keys);
            let mut pairs: Vec<(usize, u64)> = order.drain(..).zip(found).collect();
            pairs.sort_unstable();
            z.keys = pairs.into_iter().map(|p| p.1).collect();
        }
    }
    crate::mem::mark("dec: tables, index orders");

    let mut pg = vec![0u8; ps];
    let mut op = vec![0u8; ps];
    let [kind, raw, _, _, _, _, _, _, _, _, _, grp, phdr, child, ikey, cptr, ovf, fill] = &mut rd[..] else {
        unreachable!()
    };
    // (a cell is its child, two varints, what is on the page and its first overflow page: no
    // more than the page; a page holds fewer cells than half its bytes)
    let mut cell = Vec::with_capacity(ps + 32);
    let mut iv: Vec<(usize, usize)> = Vec::with_capacity(ps / 2 + 2);
    let (mut prev_child, mut prev_ikey, mut last_ovf) = (0i64, 0i64, 0i64);
    let u = g.usable;
    let mut vb = [0u8; 9];
    for p in 1..=np {
        let k = kind.u8()?;
        let base = (p - 1) * ps;
        match k {
            K_RAW => {
                out.put(base, raw.bytes(ps)?);
                continue;
            }
            K_ZERO => {
                out.zeros(base, ps);
                continue;
            }
            K_OVF => continue,
            2 | 5 | 10 | 13 => {}
            _ => return Err(corrupt()),
        }
        let h = hdr_off(p);
        let gi = if k != 5 {
            let gi = grp.usize()?;
            if gi >= ng || meta[gi].0 != (k == 13) {
                return Err(corrupt());
            }
            gi
        } else {
            usize::MAX
        };
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
            pg[h] = k;
            pg[h + 1..h + 3].copy_from_slice(&(fb as u16).to_be_bytes());
            pg[h + 3..h + 5].copy_from_slice(&(nc as u16).to_be_bytes());
            pg[h + 7] = frag as u8;
        }
        iv.clear();
        iv.push((h, ptr_end));
        let mut ends = Ends::new(u);
        let mut minoff = u;
        for i in 0..nc {
            cell.clear();
            if interior {
                let c = (prev_child + 1).checked_add(child.sv()?).ok_or_else(corrupt)?;
                if !(0..=u32::MAX as i64).contains(&c) {
                    return Err(corrupt());
                }
                prev_child = c;
                cell.extend_from_slice(&(c as u32).to_be_bytes());
            }
            // (the cell's overflow pages: how many, and the first, each next one read as its page
            // is written)
            let (mut novf, mut first) = (0, 0usize);
            let mut local = 0;
            let mut pay: &[u8] = &[];
            if k == 5 {
                let key = prev_ikey.wrapping_add(ikey.sv()?);
                prev_ikey = key;
                let n = put_sqlite(&mut vb, key as u64);
                cell.extend_from_slice(&vb[..n]);
            } else {
                scratch.clear();
                let rowid = gds[gi].next(k == 13, g.fmt4, ctxs[gi].as_ref(), &mut scratch)?;
                if given.is_some() && scratch.len() > rec_cap {
                    return Err(corrupt());
                }
                if let Some(z) = found.as_deref_mut() {
                    z.record = z.record.max(scratch.len() as u64);
                }
                let p = &scratch[..];
                pay = p;
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
                    novf = g.ovf_pages(plen, local);
                    first = next_ovf(ovf, &mut last_ovf, np)?;
                    cell.extend_from_slice(&(first as u32).to_be_bytes());
                }
            }
            let size = cell.len();
            let off = ends.decode(cptr.uv()?, size).ok_or_else(corrupt)?;
            if off < ptr_end || off + size > u {
                return Err(corrupt());
            }
            pg[off..off + size].copy_from_slice(&cell);
            pg[h + hl + 2 * i..h + hl + 2 * i + 2].copy_from_slice(&(off as u16).to_be_bytes());
            iv.push((off, off + size));
            minoff = minoff.min(off);
            // Overflow pages.
            let (mut at, mut q) = (local, first);
            for j in 0..novf {
                let ob = (q - 1) * ps;
                let next = if j + 1 < novf { next_ovf(ovf, &mut last_ovf, np)? } else { 0 };
                let take = (pay.len() - at).min(u - 4);
                op[..4].copy_from_slice(&(next as u32).to_be_bytes());
                op[4..4 + take].copy_from_slice(&pay[at..at + take]);
                op[4 + take..].copy_from_slice(fill.bytes(ps - 4 - take)?);
                out.put(ob, &op);
                at += take;
                q = next;
            }
        }
        if interior {
            let c = (prev_child + 1).checked_add(child.sv()?).ok_or_else(corrupt)?;
            if !(0..=u32::MAX as i64).contains(&c) {
                return Err(corrupt());
            }
            prev_child = c;
            pg[h + 8..h + 12].copy_from_slice(&(c as u32).to_be_bytes());
        }
        let cs = (minoff as i64).checked_add(phdr.sv()?).ok_or_else(corrupt)?;
        if !(1..=65536).contains(&cs) {
            return Err(corrupt());
        }
        pg[h + 5..h + 7].copy_from_slice(&(cs as u16).to_be_bytes());
        iv.sort_unstable();
        let mut at = 0;
        for &(a, b) in iv.iter() {
            if a < at {
                return Err(corrupt());
            }
            pg[at..a].copy_from_slice(fill.bytes(a - at)?);
            at = b;
        }
        pg[at..].copy_from_slice(fill.bytes(ps - at)?);
        out.put(base, &pg);
    }
    crate::mem::mark("dec: pages");
    let tail = len - np * ps;
    if tail > 0 {
        out.put(np * ps, raw.bytes(tail)?);
    }
    // Every stream used up, every record of every group placed.
    let used = [&*kind, &*raw, &*grp, &*phdr, &*child, &*ikey, &*cptr, &*ovf, &*fill];
    if used.iter().any(|r| !r.at_end()) || gds.iter().any(|gd| !gd.finished()) {
        return Err(corrupt());
    }
    // (in WebAssembly the decoders, keys and orders are left as they are: the module ends with
    // the file, below which a stack would keep each as a hole, more than it keeps)
    #[cfg(target_family = "wasm")]
    std::mem::forget((gds, meta, ctxs, by));
    Ok(())
}
