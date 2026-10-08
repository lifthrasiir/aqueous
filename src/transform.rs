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
pub const S_DIR: usize = 9;
/// The group streams (see `record`), in this order.
pub const S_REC: usize = 10;
pub const S_BYTES: usize = 17;
pub const NSTREAMS: usize = 18;

pub const STREAM_NAMES: [&str; NSTREAMS] = [
    "kind", "grp", "phdr", "cptr", "child", "ikey", "ovf", "fill", "raw", "dir", "rec", "rowid",
    "rawpay", "tag", "int", "real", "len", "bytes",
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
                let row_of = row_lookup(&t.rowids);
                let (vi, vt) = (groups[gi as usize].view(), t.view());
                if let Some(key) = index::discover_key(&vi, &vt, &t.rowids, &row_of) {
                    refs[gi as usize] = Some((gt as usize, key));
                }
            }
        }
    }

    crate::mem::mark("enc: index keys");
    // Code the groups, freeing their payloads (and rowids) after their last use: a table's
    // with its last index.
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
    let mut cols = Vec::new();
    let mut pred = Vec::new();
    let mut gs: [Vec<Vec<u8>>; 8] = Default::default();
    put_uv(&mut s[S_DIR], groups.len() as u64);
    for i in 0..groups.len() {
        let gr = &groups[i];
        let pays_i = gr.view();
        let mut ge = GroupEnc::default();
        let d = &mut s[S_DIR];
        d.push(gr.table as u8);
        match &refs[i] {
            None => put_uv(d, 0),
            Some((t, key)) => {
                put_uv(d, *t as u64 + 1);
                put_uv(d, key.cols.len() as u64);
                for (&c, &f) in key.cols.iter().zip(&key.flags) {
                    put_uv(d, c.wrapping_add(1) as u64);
                    d.push(f);
                }
            }
        }
        for &r in &gr.rowids {
            ge.rowid(r);
        }
        if let Some((t, key)) = &refs[i] {
            let tg = &groups[*t];
            let pays_t = tg.view();
            let keys = index::Keys::over(&key.cols, &pays_t, &tg.rowids, &mut cols);
            let perm = index::order(&keys, key);
            let mut rank = vec![0u32; perm.len()];
            for (k, &r) in perm.iter().enumerate() {
                rank[r as usize] = k as u32;
            }
            drop(perm);
            let row_of = row_lookup(&tg.rowids);
            for &e in pays_i.iter() {
                // The entry's rowid, its row, and the entry the row predicts.
                let r = crate::record::parse(e, &mut cols)
                    .then(|| cols.last().copied())
                    .flatten()
                    .filter(|&(st, _)| is_int(st))
                    .and_then(|(st, off)| row_of(int_value(st, &e[off as usize..off as usize + st_size(st)])))
                    .filter(|&r| keys.ok[r]);
                if let Some(r) = r {
                    pred.clear();
                    predict(&keys, r, g.fmt4, &mut pred);
                    if pred == e {
                        ge.reference(rank[r] as i64);
                        continue;
                    }
                }
                ge.record(e, g.fmt4, &mut cols);
            }
        } else {
            for &e in pays_i.iter() {
                ge.record(e, g.fmt4, &mut cols);
            }
        }
        if std::env::var_os("SQLR_DUMP").is_some() {
            eprintln!(
                "G {i} first={} table={} recs={} ref={:?} {}",
                gr.first,
                gr.table,
                gr.len(),
                refs[i].as_ref().map(|r| (r.0, &r.1)),
                ge.stats()
            );
        }
        ge.finish(&mut s[S_DIR], &mut gs);
        drop(pays_i);
        for &x in &free_at[i] {
            groups[x].free();
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

/// Finds a row by rowid: binary search, over the rowids or a sorted copy.
fn row_lookup(rowids: &[i64]) -> impl Fn(i64) -> Option<usize> + '_ {
    let sorted = rowids.windows(2).all(|w| w[0] < w[1]);
    let mut idx: Vec<(i64, u32)> = Vec::new();
    if !sorted {
        idx = rowids.iter().enumerate().map(|(i, &r)| (r, i as u32)).collect();
        idx.sort_unstable();
    }
    move |r| {
        if sorted {
            rowids.binary_search(&r).ok()
        } else {
            idx.binary_search_by(|e| e.0.cmp(&r)).ok().map(|k| idx[k].1 as usize)
        }
    }
}

/// Where rebuilt pages go: every page (and the tail) exactly once, in no particular order.
trait Sink {
    fn put(&mut self, at: usize, b: &[u8]);
    /// An all-zero page.
    fn zeros(&mut self, at: usize, n: usize);
}

/// Into the output (zeroed when allocated).
struct Writer(Vec<u8>);

impl Sink for Writer {
    #[inline]
    fn put(&mut self, at: usize, b: &[u8]) {
        self.0[at..at + b.len()].copy_from_slice(b);
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
        if at % self.psize != 0 || self.seen.get(p) != Some(&false) {
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

/// Rebuilds the file from its streams. `len` is the original length.
pub fn decode(g: &Geom, len: usize, s: &[Vec<u8>]) -> Result<Vec<u8>> {
    let mut w = Writer(vec![0u8; len]);
    rebuild(g, len, s, &mut w)?;
    Ok(w.0)
}

/// Whether the streams rebuild `src` exactly, without allocating a copy of it.
pub fn verify(g: &Geom, src: &[u8], s: &[Vec<u8>]) -> bool {
    let np = g.npages;
    let mut c = Checker { src, psize: g.psize, seen: vec![false; np + 1], ok: true };
    if src.len() == np * g.psize {
        c.seen[np] = true;
    }
    rebuild(g, src.len(), s, &mut c).is_ok() && c.ok && c.seen.iter().all(|&x| x)
}

fn rebuild<S: Sink>(g: &Geom, len: usize, s: &[Vec<u8>], out: &mut S) -> Result<()> {
    let (ps, np) = (g.psize, g.npages);
    if s.len() != NSTREAMS || np == 0 || np.checked_mul(ps) != Some(len - len % ps) {
        return Err(corrupt());
    }
    let mut rd: Vec<Rd> = s.iter().map(|v| Rd::new(v)).collect();

    // Groups: directory, then records (the tables indexes refer to first).
    let mut dir = rd[S_DIR];
    let ng = dir.usize()?;
    let mut gstreams: [Rd; 8] = rd[S_REC..S_REC + 8].try_into().unwrap();
    let mut gds = Vec::with_capacity(ng.min(1 << 20));
    let mut meta = Vec::with_capacity(ng.min(1 << 20));
    for _ in 0..ng {
        let table = dir.u8()? != 0;
        let r = match dir.usize()? {
            0 => None,
            t => {
                let m = dir.usize()?;
                if m > 1 << 16 {
                    return Err(corrupt());
                }
                let mut key = index::Key::default();
                for _ in 0..m {
                    key.cols.push((dir.uv()? as u32).wrapping_sub(1));
                    key.flags.push(dir.u8()? & 3);
                }
                Some((t - 1, key))
            }
        };
        meta.push((table, r));
        gds.push(GroupDec::new(&mut dir, &mut gstreams)?);
    }
    // The indexed columns of the tables indexes are predicted from: a first pass over each
    // such table (on a copy of its cursors), for all its indexes at once. Then every group
    // decodes record by record as the pages need them.
    let mut ctxs: Vec<Option<RefCtx>> = (0..ng).map(|_| None).collect();
    let mut scratch = Vec::new();
    let mut cols = Vec::new();
    if meta.iter().any(|(_, r)| matches!(r, Some((t, _)) if *t >= ng)) {
        return Err(corrupt());
    }
    let mut by_table: Vec<Vec<usize>> = vec![Vec::new(); ng];
    for (i, (_, r)) in meta.iter().enumerate() {
        if let Some((t, _)) = r {
            by_table[*t].push(i);
        }
    }
    for (t, idx) in by_table.iter().enumerate() {
        if idx.is_empty() {
            continue;
        }
        if meta[t].1.is_some() || !meta[t].0 {
            return Err(corrupt());
        }
        let mut keys: Vec<index::Keys> =
            idx.iter().map(|&i| index::Keys::new(&meta[i].1.as_ref().unwrap().1.cols)).collect();
        let mut gd = gds[t].clone();
        while !gd.at_end() {
            scratch.clear();
            let rowid = gd.next(true, g.fmt4, None, &mut scratch)?;
            for k in &mut keys {
                if !k.push(&scratch, rowid, &mut cols) {
                    return Err(corrupt());
                }
            }
        }
        for (&i, keys) in idx.iter().zip(keys) {
            let perm = index::order(&keys, &meta[i].1.as_ref().unwrap().1);
            ctxs[i] = Some(RefCtx { keys, perm });
        }
    }
    crate::mem::mark("dec: tables, index orders");

    let mut pg = vec![0u8; ps];
    let mut op = vec![0u8; ps];
    let [kind, grp, phdr, cptr, child, ikey, ovf, fill, raw, ..] = &mut rd[..] else {
        unreachable!()
    };
    let mut cell = Vec::new();
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
                let c = prev_child + 1 + child.sv()?;
                if !(0..=u32::MAX as i64).contains(&c) {
                    return Err(corrupt());
                }
                prev_child = c;
                cell.extend_from_slice(&(c as u32).to_be_bytes());
            }
            chain.clear();
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
            let off = ends.decode(cptr.uv()?, size).ok_or_else(corrupt)?;
            if off < ptr_end || off + size > u {
                return Err(corrupt());
            }
            pg[off..off + size].copy_from_slice(&cell);
            pg[h + hl + 2 * i..h + hl + 2 * i + 2].copy_from_slice(&(off as u16).to_be_bytes());
            iv.push((off, off + size));
            minoff = minoff.min(off);
            // Overflow pages.
            let mut at = local;
            for (j, &q) in chain.iter().enumerate() {
                let ob = (q - 1) * ps;
                let next = chain.get(j + 1).copied().unwrap_or(0) as u32;
                let take = (pay.len() - at).min(u - 4);
                op[..4].copy_from_slice(&next.to_be_bytes());
                op[4..4 + take].copy_from_slice(&pay[at..at + take]);
                op[4 + take..].copy_from_slice(fill.bytes(ps - 4 - take)?);
                out.put(ob, &op);
                at += take;
            }
        }
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
    Ok(())
}
