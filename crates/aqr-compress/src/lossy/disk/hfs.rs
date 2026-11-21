//! HFS+ and HFSX (Apple's TN1150): the free bytes of a volume are the allocation blocks whose bit
//! in the allocation file is clear (the most significant bit of a byte first), among the volume's
//! total blocks. What HFS+ writes there are the remains of deleted files.
//!
//! The volume is trusted only where it parses and is consistent: the signature `H+` or `HX`, a
//! power-of-two block size, the allocation file covering a bit for each block, no free block in
//! the extents of the special files (the allocation, extents overflow, catalog, attributes and
//! startup files, with their extents in the extents overflow B-tree where more than eight) nor in
//! any extent of the overflow file, the count of free blocks of the header the bitmap's. A volume
//! not unmounted cleanly, marked inconsistent, or journaled with a journal that holds
//! transactions (`start != end`: its replay would write blocks) is kept as it is. Blocks that
//! meet the first 1536 bytes (boot blocks and the volume header), the alternate volume header
//! and the last 512 bytes are never free. File tails (the bytes after a fork's logical size in
//! its last block) are not free: the block is allocated.

use std::ops::Range;

const UNMOUNTED: u32 = 1 << 8;
const INCONSISTENT: u32 = 1 << 11;
const JOURNALED: u32 = 1 << 13;

fn be16(d: &[u8], p: usize) -> usize {
    u16::from_be_bytes([d[p], d[p + 1]]) as usize
}

fn be32(d: &[u8], p: usize) -> usize {
    u32::from_be_bytes([d[p], d[p + 1], d[p + 2], d[p + 3]]) as usize
}

fn be64(d: &[u8], p: usize) -> u64 {
    u64::from_be_bytes(d[p..p + 8].try_into().unwrap())
}

/// A fork's data in the header: logical size, total blocks, and the (up to 8) extents.
struct Fork {
    size: u64,
    blocks: usize,
    extents: Vec<(usize, usize)>,
}

fn fork(h: &[u8], p: usize) -> Fork {
    let extents =
        (0..8).map(|i| (be32(h, p + 16 + i * 8), be32(h, p + 20 + i * 8))).take_while(|&(_, n)| n != 0).collect();
    Fork { size: be64(h, p), blocks: be32(h, p + 12), extents }
}

/// One record of the extents overflow file: the file, fork type, first block, and its extents.
struct Overflow {
    id: usize,
    fork: u8,
    start: usize,
    extents: Vec<(usize, usize)>,
}

struct Vol<'a> {
    d: &'a [u8],
    bs: usize,
    total: usize,
}

impl Vol<'_> {
    /// The bytes of a file by its extents, up to `size`.
    fn read(&self, extents: &[(usize, usize)], size: usize) -> Result<Vec<u8>, String> {
        let mut out = Vec::with_capacity(size.min(self.d.len()));
        for &(s, n) in extents {
            if s.checked_add(n).is_none_or(|e| e > self.total) {
                return Err("an extent out of the volume".into());
            }
            out.extend_from_slice(&self.d[s * self.bs..(s + n) * self.bs]);
        }
        if out.len() < size {
            return Err("a file shorter than its size".into());
        }
        out.truncate(size);
        Ok(out)
    }

    /// The records of the extents overflow file (its B-tree's leaves), `f` being its fork.
    fn overflow(&self, f: &Fork) -> Result<Vec<Overflow>, String> {
        if f.size == 0 {
            return Ok(Vec::new());
        }
        let covered: usize = f.extents.iter().map(|e| e.1).sum();
        if covered < f.blocks {
            return Err("an extents overflow file with extents of its own overflowing".into());
        }
        let size = usize::try_from(f.size).map_err(|_| "an extents overflow file too large")?;
        let t = self.read(&f.extents, size)?;
        if t.len() < 512 || t[8] != 1 {
            return Err("extents overflow file without a header node".into());
        }
        let node = be16(&t, 14 + 18);
        let (first, nodes) = (be32(&t, 14 + 10), be32(&t, 14 + 22));
        if !node.is_power_of_two() || !(512..=32768).contains(&node) || nodes * node > t.len() {
            return Err("extents overflow file's nodes out of it".into());
        }
        let mut out = Vec::new();
        let mut at = first;
        let mut seen = 0;
        while at != 0 {
            seen += 1;
            if at >= nodes || seen > nodes {
                return Err("extents overflow file's leaves out of it or in a loop".into());
            }
            let n = &t[at * node..(at + 1) * node];
            if n[8] != 0xff {
                return Err("extents overflow file's leaf not a leaf".into());
            }
            for i in 0..be16(n, 10) {
                let o = be16(n, node - 2 * (i + 1));
                if o < 14 || o + 2 + 10 + 64 > node - 2 * (i + 1) || be16(n, o) != 10 {
                    return Err("extents overflow file's record malformed".into());
                }
                let extents = (0..8)
                    .map(|j| (be32(n, o + 12 + j * 8), be32(n, o + 16 + j * 8)))
                    .take_while(|&(_, c)| c != 0)
                    .collect();
                out.push(Overflow { id: be32(n, o + 4), fork: n[o + 2], start: be32(n, o + 8), extents });
            }
            at = be32(n, 0);
        }
        Ok(out)
    }
}

/// All the extents of the data fork of file `id`: the header's, then the overflow file's, each
/// where the one before ends; all of its `blocks`.
fn full(f: &Fork, id: usize, over: &[Overflow]) -> Result<Vec<(usize, usize)>, String> {
    let mut out = f.extents.clone();
    let mut n: usize = out.iter().map(|e| e.1).sum();
    if n < f.blocks {
        let mut recs: Vec<&Overflow> = over.iter().filter(|o| o.id == id && o.fork == 0).collect();
        recs.sort_by_key(|o| o.start);
        for o in recs {
            if o.start == n {
                out.extend(&o.extents);
                n += o.extents.iter().map(|e| e.1).sum::<usize>();
            }
        }
        if n < f.blocks {
            return Err(format!("file {id}'s extents not all found"));
        }
    }
    Ok(out)
}

/// The journal of a journaled volume must be empty.
fn journal_empty(v: &Vol, info_block: usize) -> Result<(), String> {
    let at = info_block.checked_mul(v.bs).filter(|&a| a + 52 <= v.d.len());
    let Some(at) = at else { return Err("the journal info block out of the volume".into()) };
    let (flags, offset, size) = (be32(v.d, at), be64(v.d, at + 36), be64(v.d, at + 44));
    if flags & 2 != 0 {
        return Err("a journal on another device".into());
    }
    if flags & 1 == 0 || flags & 4 != 0 {
        return Err("a journal not in the volume or yet to be set up".into());
    }
    let (offset, size) = (usize::try_from(offset).unwrap_or(usize::MAX), usize::try_from(size).unwrap_or(usize::MAX));
    if offset.checked_add(size).is_none_or(|e| e > v.d.len()) || size < 512 || offset % 512 != 0 {
        return Err("the journal out of the volume".into());
    }
    let h = &v.d[offset..offset + 44];
    // (in the byte order of the machine that wrote it)
    let le = match (u32::from_le_bytes(h[4..8].try_into().unwrap()), u32::from_be_bytes(h[4..8].try_into().unwrap())) {
        (0x12345678, _) => true,
        (_, 0x12345678) => false,
        _ => return Err("the journal header's byte order unknown".into()),
    };
    let q = |p: usize| {
        let b: [u8; 8] = h[p..p + 8].try_into().unwrap();
        if le { u64::from_le_bytes(b) } else { u64::from_be_bytes(b) }
    };
    if q(8) != q(16) {
        return Err("the journal not empty".into());
    }
    Ok(())
}

/// The free bytes of the HFS+ or HFSX volume `d`.
pub(super) fn free(d: &[u8]) -> Result<Vec<Range<usize>>, String> {
    if d.len() < 1024 + 512 {
        return Err("shorter than a volume header".into());
    }
    let h = &d[1024..1536];
    if h[0] != b'H' || !matches!(h[1], b'+' | b'X') || be16(h, 2) != if h[1] == b'+' { 4 } else { 5 } {
        return Err("not an HFS+ or HFSX volume header".into());
    }
    let attrs = be32(h, 4) as u32;
    if attrs & UNMOUNTED == 0 {
        return Err("not unmounted cleanly".into());
    }
    if attrs & INCONSISTENT != 0 {
        return Err("marked inconsistent".into());
    }
    let (bs, total, free_blocks) = (be32(h, 40), be32(h, 44), be32(h, 48));
    if !bs.is_power_of_two() || bs < 512 || total == 0 || total.checked_mul(bs).is_none_or(|n| n > d.len()) {
        return Err("sizes inconsistent".into());
    }
    let v = Vol { d, bs, total };
    let (alloc, ext, cat, attr, start) = (fork(h, 112), fork(h, 192), fork(h, 272), fork(h, 352), fork(h, 432));
    let over = v.overflow(&ext)?;
    let mut special: Vec<(usize, usize)> = Vec::new();
    for (id, f) in [(6, &alloc), (3, &ext), (4, &cat), (8, &attr), (7, &start)] {
        if f.blocks != 0 {
            special.extend(full(f, id, &over)?);
        }
    }
    // (bad blocks are marked used; the extents of any file of the overflow are used too)
    special.extend(over.iter().flat_map(|o| o.extents.iter().copied()));
    if attrs & JOURNALED != 0 {
        let info = be32(h, 12);
        journal_empty(&v, info)?;
        special.push((info, 1));
    }
    // The bitmap.
    let need = total.div_ceil(8);
    let bits = v.read(&full(&alloc, 6, &over)?, need).map_err(|e| format!("allocation file: {e}"))?;
    if alloc.size < need as u64 {
        return Err("allocation file too small for its blocks".into());
    }
    let used = |b: usize| bits[b / 8] & (0x80 >> (b % 8)) != 0;
    if (0..total).filter(|&b| !used(b)).count() != free_blocks {
        return Err("the header's free blocks not the bitmap's".into());
    }
    for &(s, n) in &special {
        if s.checked_add(n).is_none_or(|e| e > total) || (s..s + n).any(|b| !used(b)) {
            return Err("a block of a special file marked free".into());
        }
    }
    // The first 1536 bytes, the alternate header, the last 512 bytes.
    let end = total * bs;
    let kept = |b: usize| {
        let r = b * bs..(b + 1) * bs;
        let meets = |k: Range<usize>| r.start < k.end && k.start < r.end;
        meets(0..1536) || meets(end - 1024..end) || meets(d.len() - 512..d.len()) || meets(d.len() - 1024..d.len())
    };
    let mut out: Vec<Range<usize>> = Vec::new();
    for b in (0..total).filter(|&b| !used(b) && !kept(b)) {
        let r = b * bs..(b + 1) * bs;
        match out.last_mut() {
            Some(l) if l.end == r.start => l.end = r.end,
            _ => out.push(r),
        }
    }
    Ok(out)
}
