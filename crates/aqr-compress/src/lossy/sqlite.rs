//! SQLite databases (<https://www.sqlite.org/fileformat2.html>): `slack`, the bytes SQLite never
//! reads zeroed in place: the unallocated space of b-tree pages, freeblocks' bodies and
//! fragments, freelist pages (a trunk keeps its leaf numbers), the tail of an overflow chain's
//! last page. What SQLite writes there are the remains of deleted rows (`secure_delete` off).
//! The header, the reserved bytes at the end of every page (checksums, encryption), pointer-map
//! pages and the lock-byte page are kept. The file's length stays.
//!
//! Every page must be accounted for by one structure only (b-tree, overflow chain, freelist); a
//! file where that does not hold, or the header or a b-tree page does not parse, is kept as it is.

use std::ops::Range;

use crate::Options;

/// What claims a page.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Claim {
    None,
    Special,
    Free,
    Tree,
    Overflow,
}

struct Db<'a> {
    d: &'a [u8],
    size: usize,
    usable: usize,
    pages: usize,
    claim: Vec<Claim>,
    /// Ranges (of the file) to zero.
    zero: Vec<Range<usize>>,
}

fn be16(d: &[u8], p: usize) -> usize {
    u16::from_be_bytes([d[p], d[p + 1]]) as usize
}

fn be32(d: &[u8], p: usize) -> usize {
    u32::from_be_bytes([d[p], d[p + 1], d[p + 2], d[p + 3]]) as usize
}

fn varint(d: &[u8], mut p: usize) -> Option<(u64, usize)> {
    let mut v = 0u64;
    for i in 0..9 {
        let b = *d.get(p)?;
        p += 1;
        if i == 8 {
            return Some((v << 8 | b as u64, p));
        }
        v = v << 7 | (b & 0x7f) as u64;
        if b & 0x80 == 0 {
            return Some((v, p));
        }
    }
    None
}

pub(super) fn slack(d: &[u8], _: &Options) -> Result<Option<Vec<u8>>, String> {
    if d.len() < 100 || &d[..16] != b"SQLite format 3\0" {
        return Err("not a SQLite 3 database".into());
    }
    let ps = be16(d, 16);
    let size = match ps {
        1 => 65536,
        512..=32768 if ps.is_power_of_two() => ps,
        _ => return Err("invalid page size".into()),
    };
    let usable = size - d[20] as usize;
    if usable < 480 {
        return Err("invalid reserved space".into());
    }
    if !(1..=2).contains(&d[18]) || !(1..=2).contains(&d[19]) || d[21] != 64 || d[22] != 32 || d[23] != 32 {
        return Err("unknown database format".into());
    }
    if !d.len().is_multiple_of(size) || d.len() < size {
        return Err("length is not of whole pages".into());
    }
    let pages = d.len() / size;
    if be32(d, 28) != 0 && be32(d, 24) == be32(d, 92) && be32(d, 28) != pages {
        return Err("size in the header is not the file's".into());
    }
    let mut db = Db { d, size, usable, pages, claim: vec![Claim::None; pages + 1], zero: Vec::new() };
    let lock = 0x4000_0000 / size + 1;
    let auto = be32(d, 52) != 0;
    if pages >= lock {
        if auto {
            return Err("auto-vacuum database over 1 GiB".into());
        }
        db.claim[lock] = Claim::Special;
    }
    if auto {
        let j = usable / 5 + 1;
        for p in (2..=pages).step_by(j) {
            db.claim[p] = Claim::Special;
        }
    }
    db.freelist()?;
    let mut chains = Vec::new();
    for p in 1..=pages {
        if db.claim[p] != Claim::None {
            continue;
        }
        let h = if p == 1 { 100 } else { 0 };
        if matches!(d[(p - 1) * size + h], 2 | 5 | 10 | 13) {
            db.claim[p] = Claim::Tree;
            db.tree(p, h, &mut chains).map_err(|e| format!("page {p}: {e}"))?;
        } else if p == 1 {
            return Err("page 1 is not a b-tree".into());
        }
    }
    for (first, rest) in chains {
        db.chain(first, rest)?;
    }
    if db.zero.iter().all(|r| d[r.clone()].iter().all(|&b| b == 0)) {
        return Ok(None);
    }
    let mut r = d.to_vec();
    for z in db.zero {
        r[z].fill(0);
    }
    Ok(Some(r))
}

impl Db<'_> {
    fn page(&self, p: usize) -> &[u8] {
        &self.d[(p - 1) * self.size..p * self.size]
    }

    fn take(&mut self, p: usize, by: Claim) -> Result<(), String> {
        if p < 2 || p > self.pages {
            return Err(format!("page {p} out of the file"));
        }
        if self.claim[p] != Claim::None {
            return Err(format!("page {p} is claimed twice"));
        }
        self.claim[p] = by;
        Ok(())
    }

    fn zero_page(&mut self, p: usize, within: Range<usize>) {
        let at = (p - 1) * self.size;
        self.zero.push(at + within.start..at + within.end);
    }

    fn freelist(&mut self) -> Result<(), String> {
        let (mut t, total) = (be32(self.d, 32), be32(self.d, 36));
        let mut seen = 0;
        while t != 0 {
            self.take(t, Claim::Free)?;
            seen += 1;
            let (next, k) = (be32(self.page(t), 0), be32(self.page(t), 4));
            if k > self.usable / 4 - 2 {
                return Err("too many leaves on a freelist trunk".into());
            }
            for i in 0..k {
                let leaf = be32(self.page(t), 8 + 4 * i);
                self.take(leaf, Claim::Free)?;
                self.zero_page(leaf, 0..self.usable);
            }
            seen += k;
            self.zero_page(t, 8 + 4 * k..self.usable);
            t = next;
        }
        if seen != total {
            return Err("freelist is not of the pages counted".into());
        }
        Ok(())
    }

    /// The b-tree page `p` (its header at `h`): its free space zeroed, its overflow chains
    /// pushed to `chains` (first page, bytes held in the chain).
    fn tree(&mut self, p: usize, h: usize, chains: &mut Vec<(usize, usize)>) -> Result<(), String> {
        let u = self.usable;
        let d: &[u8] = self.d;
        let pg = &d[(p - 1) * self.size..p * self.size];
        let mut zeros: Vec<Range<usize>> = Vec::new();
        let t = pg[h];
        let hdr = if t >= 10 { 8 } else { 12 };
        let n = be16(pg, h + 3);
        let arr_end = h + hdr + 2 * n;
        let content = match be16(pg, h + 5) {
            0 => 65536,
            c => c,
        };
        if arr_end > u || content < arr_end || content > u {
            return Err("cell area does not fit".into());
        }
        // (usable payload: X, the most held in a cell; M, the least of one spilling)
        let x = if t == 13 { u - 35 } else { (u - 12) * 64 / 255 - 23 };
        let m = (u - 12) * 32 / 255 - 23;
        // (what is kept, and what takes the space)
        let mut keep: Vec<Range<usize>> = Vec::with_capacity(n + 8);
        keep.push(0..arr_end);
        let mut taken: Vec<Range<usize>> = Vec::new();
        for i in 0..n {
            let c = be16(pg, h + hdr + 2 * i);
            if c < content || c >= u {
                return Err("cell out of the content area".into());
            }
            let mut q = c;
            if t < 10 {
                q += 4;
            }
            let rowid = t == 5 || t == 13;
            let mut size = 0usize;
            if t != 5 {
                let (v, e) = varint(pg, q).ok_or("cell does not parse")?;
                if v > i32::MAX as u64 {
                    return Err("payload too long".into());
                }
                size = v as usize;
                q = e;
            }
            if rowid {
                q = varint(pg, q).ok_or("cell does not parse")?.1;
            }
            let mut spill = None;
            if t != 5 {
                let local = if size <= x {
                    size
                } else {
                    let k = m + (size - m) % (u - 4);
                    if k <= x { k } else { m }
                };
                q += local;
                if size > local {
                    if q + 4 > u {
                        return Err("cell does not fit".into());
                    }
                    spill = Some((be32(pg, q), size - local));
                    q += 4;
                }
            }
            if q > u {
                return Err("cell does not fit".into());
            }
            keep.push(c..q);
            taken.push(c..q);
            if let Some(s) = spill {
                chains.push(s);
            }
        }
        let mut f = be16(pg, h + 1);
        let mut at = 0;
        while f != 0 {
            if f < content || f < at || f + 4 > u {
                return Err("freeblock out of place".into());
            }
            let sz = be16(pg, f + 2);
            if sz < 4 || f + sz > u {
                return Err("freeblock out of place".into());
            }
            keep.push(f..f + 4);
            taken.push(f..f + sz);
            at = f + sz;
            f = be16(pg, f);
        }
        taken.sort_by_key(|r| r.start);
        if taken.windows(2).any(|w| w[0].end > w[1].start) {
            return Err("cells and freeblocks overlap".into());
        }
        keep.sort_by_key(|r| r.start);
        let mut at = arr_end;
        for k in keep.iter().skip(1) {
            if k.start > at {
                zeros.push(at..k.start);
            }
            at = at.max(k.end);
        }
        if at < u {
            zeros.push(at..u);
        }
        for z in zeros {
            self.zero_page(p, z);
        }
        Ok(())
    }

    /// An overflow chain from page `p` holding `rest` bytes of a payload.
    fn chain(&mut self, mut p: usize, mut rest: usize) -> Result<(), String> {
        let room = self.usable - 4;
        loop {
            self.take(p, Claim::Overflow)?;
            let next = be32(self.page(p), 0);
            if rest <= room {
                if next != 0 {
                    return Err("overflow chain longer than its payload".into());
                }
                self.zero_page(p, 4 + rest..self.usable);
                return Ok(());
            }
            rest -= room;
            p = next;
        }
    }
}
