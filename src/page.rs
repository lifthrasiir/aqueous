//! Database geometry and b-tree page parsing.

use crate::varint::{get_sqlite, sqlite_len};

pub const K_RAW: u8 = 0;
pub const K_ZERO: u8 = 1;
pub const K_OVF: u8 = 3;

#[inline]
pub fn is_btree(t: u8) -> bool {
    matches!(t, 2 | 5 | 10 | 13)
}

#[inline]
pub fn be16(b: &[u8], i: usize) -> usize {
    u16::from_be_bytes([b[i], b[i + 1]]) as usize
}

#[inline]
pub fn be32(b: &[u8], i: usize) -> u32 {
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
    pub fn ovf_pages(&self, plen: u64, local: usize) -> u64 {
        (plen - local as u64).div_ceil(self.usable as u64 - 4)
    }
}

/// One cell, as parsed from its page.
#[derive(Clone, Copy, Default, Debug)]
pub struct Cell {
    pub off: usize,
    pub size: usize,
    /// Start of the payload on the page.
    pub pay: usize,
    pub local: usize,
    pub plen: u64,
    /// Rowid (table leaf) or key (table interior).
    pub key: u64,
    pub child: u32,
    /// First overflow page, 0 if none.
    pub ovf: u32,
}

/// Parses the cells of a b-tree page whose header is at `h`. Fails unless the page can be
/// rebuilt exactly from its fields and filler. `iv` receives the covered intervals, sorted.
pub fn parse_page(
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

#[inline]
pub fn hdr_off(p: usize) -> usize {
    if p == 1 { 100 } else { 0 }
}
