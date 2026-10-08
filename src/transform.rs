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
use crate::page::{Cell, Geom, K_OVF, K_RAW, K_ZERO, be16, be32, hdr_off, is_btree, parse_page};
use crate::record::{GroupDec, GroupEnc, RefCtx, Recs, int_value, is_int, predict, st_size};
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
struct GroupRecs {
    table: bool,
    first: usize,
    /// (offset, length, in the arena rather than the file)
    recs: Vec<(usize, usize, bool)>,
    rowids: Vec<i64>,
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
    let mut arena = Vec::new();
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
            let Some(grp) = grp.as_mut() else {
                put_sv(&mut s[S_IKEY], (c.key as i64).wrapping_sub(prev_ikey));
                prev_ikey = c.key as i64;
                continue;
            };
            if k == 13 {
                grp.rowids.push(c.key as i64);
            }
            if c.local as u64 == c.plen {
                grp.recs.push((base + c.pay, c.local, false));
                continue;
            }
            // Put the payload together, code the chain.
            let start = arena.len();
            arena.extend_from_slice(&pg[c.pay..c.pay + c.local]);
            let mut left = c.plen as usize - c.local;
            let mut q = c.ovf as usize;
            while left > 0 {
                let op = page(q);
                let take = left.min(g.usable - 4);
                arena.extend_from_slice(&op[4..4 + take]);
                s[S_FILL].extend_from_slice(&op[4 + take..]);
                put_sv(&mut s[S_OVF], q as i64 - (last_ovf + 1));
                last_ovf = q as i64;
                left -= take;
                q = be32(op, 0) as usize;
            }
            grp.recs.push((start, c.plen as usize, true));
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

    let pays: Vec<Vec<&[u8]>> = groups
        .iter()
        .map(|gr| {
            gr.recs
                .iter()
                .map(|&(o, l, a)| if a { &arena[o..o + l] } else { &file[o..o + l] })
                .collect()
        })
        .collect();

    // Indexes predicted from their tables: (table group, columns) per index group.
    let mut refs: Vec<Option<(usize, index::Key)>> = (0..groups.len()).map(|_| None).collect();
    let schema_root = find(&mut parent, 1) as usize;
    if kind[1] == 13 || kind[1] == 5 {
        let sg = gid_t[schema_root];
        if sg != u32::MAX {
            let enc = u32::from_be_bytes(file[56..60].try_into().unwrap());
            for (ir, tr) in index::schema_indexes(&pays[sg as usize], enc) {
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
                if let Some(key) =
                    index::discover_key(&pays[gi as usize], &pays[gt as usize], &t.rowids, &row_of)
                {
                    refs[gi as usize] = Some((gt as usize, key));
                }
            }
        }
    }

    // Code the groups.
    let mut cols = Vec::new();
    let mut tmp = Vec::new();
    let mut pred = Vec::new();
    let mut gs: [Vec<u8>; 8] = Default::default();
    put_uv(&mut s[S_DIR], groups.len() as u64);
    for (i, gr) in groups.iter().enumerate() {
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
            let map = &key.cols;
            let tg = &groups[*t];
            let order = index::order(&pays[*t], &tg.rowids, key);
            let mut rank = vec![0u32; order.len()];
            for (k, &r) in order.iter().enumerate() {
                rank[r as usize] = k as u32;
            }
            let row_of = row_lookup(&tg.rowids);
            for &e in &pays[i] {
                // The entry's rowid, its row, and the entry the row predicts.
                let r = crate::record::parse(e, &mut cols)
                    .then(|| cols.last().copied())
                    .flatten()
                    .filter(|&(st, _)| is_int(st))
                    .and_then(|(st, off)| row_of(int_value(st, &e[off as usize..off as usize + st_size(st)])));
                if let Some(r) = r {
                    pred.clear();
                    if predict(pays[*t][r], tg.rowids[r], map, g.fmt4, &mut cols, &mut pred)
                        && pred == e
                    {
                        ge.reference(rank[r] as i64);
                        continue;
                    }
                }
                ge.record(e, g.fmt4, &mut cols);
            }
        } else {
            for &e in &pays[i] {
                ge.record(e, g.fmt4, &mut cols);
            }
        }
        if std::env::var_os("SQLR_DUMP").is_some() {
            eprintln!(
                "G {i} first={} table={} recs={} ref={:?} {}",
                gr.first,
                gr.table,
                gr.recs.len(),
                refs[i].as_ref().map(|r| (r.0, &r.1)),
                ge.stats()
            );
        }
        ge.finish(&mut s[S_DIR], &mut gs, &mut tmp);
    }
    for (j, v) in gs.into_iter().enumerate() {
        s[S_REC + j] = v;
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

/// Rebuilds the file from its streams. `len` is the original length.
pub fn decode(g: &Geom, len: usize, s: &[Vec<u8>]) -> Result<Vec<u8>> {
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
    let mut recs: Vec<Recs> = (0..ng).map(|_| Recs::default()).collect();
    for (i, gd) in gds.iter_mut().enumerate() {
        if meta[i].1.is_none() {
            recs[i] = gd.decode(meta[i].0, g.fmt4, None)?;
        }
    }
    for (i, gd) in gds.iter_mut().enumerate() {
        if let Some((t, key)) = &meta[i].1 {
            if *t >= ng || meta[*t].1.is_some() || !meta[*t].0 {
                return Err(corrupt());
            }
            let tr = &recs[*t];
            let rows: Vec<&[u8]> = (0..tr.len()).map(|k| tr.get(k)).collect();
            let order = index::order(&rows, &tr.rowids, key);
            let rc = RefCtx { table: tr, order, map: &key.cols };
            recs[i] = gd.decode(meta[i].0, g.fmt4, Some(&rc))?;
        }
    }

    let mut out = vec![0u8; len];
    let [kind, grp, phdr, cptr, child, ikey, ovf, fill, raw, ..] = &mut rd[..] else {
        unreachable!()
    };
    let mut cur = vec![0usize; ng];
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
                out[base..base + ps].copy_from_slice(raw.bytes(ps)?);
                continue;
            }
            K_ZERO | K_OVF => continue,
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
            let mut pay: &[u8] = &[];
            if k == 5 {
                let key = prev_ikey.wrapping_add(ikey.sv()?);
                prev_ikey = key;
                let n = put_sqlite(&mut vb, key as u64);
                cell.extend_from_slice(&vb[..n]);
            } else {
                let r = &recs[gi];
                let j = cur[gi];
                if j >= r.len() {
                    return Err(corrupt());
                }
                cur[gi] += 1;
                pay = r.get(j);
                let plen = pay.len() as u64;
                let n = put_sqlite(&mut vb, plen);
                cell.extend_from_slice(&vb[..n]);
                if k == 13 {
                    let n = put_sqlite(&mut vb, r.rowids[j] as u64);
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
        let mut at = 0;
        for &(a, b) in iv.iter() {
            if a < at {
                return Err(corrupt());
            }
            pg[at..a].copy_from_slice(fill.bytes(a - at)?);
            at = b;
        }
        pg[at..].copy_from_slice(fill.bytes(ps - at)?);
    }
    let tail = len - np * ps;
    out[np * ps..].copy_from_slice(raw.bytes(tail)?);
    Ok(out)
}
