//! Where the blobs of a database's records are in its file: each blob as pieces of the file (the
//! part of its record on its b-tree page, then on overflow pages), for every table and index the
//! schema has, so that what a blob holds (a compressor's frame, say) can be made again in place.

use std::ops::Range;

use crate::page::{Geom, T_INDEX_INTERIOR, T_INDEX_LEAF, T_TABLE_INTERIOR, T_TABLE_LEAF, be16, be32, hdr_off};
use crate::varint::get_sqlite;

/// A blob of a record: the pieces of the file it is, in order; of which record (its b-tree's root
/// page, and its place among the records walked) and column; and the record's first columns where
/// they are integers ([`KEY`] of them, else 0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blob {
    pub pieces: Vec<Range<usize>>,
    pub root: u32,
    pub record: u32,
    pub column: u16,
    pub key: [i64; KEY],
}

/// The record's first columns told of its blobs ([`Blob::key`]).
pub const KEY: usize = 3;

impl Blob {
    pub fn len(&self) -> usize {
        self.pieces.iter().map(|r| r.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.pieces.iter().all(|r| r.is_empty())
    }

    /// Its bytes, of the file `f`.
    pub fn bytes(&self, f: &[u8]) -> Vec<u8> {
        let mut v = Vec::with_capacity(self.len());
        for r in &self.pieces {
            v.extend_from_slice(&f[r.clone()]);
        }
        v
    }
}

/// The pieces of a payload in the file.
struct Payload {
    pieces: Vec<Range<usize>>,
}

impl Payload {
    /// The bytes `at..at + n` of the payload as pieces of the file.
    fn range(&self, at: usize, n: usize) -> Vec<Range<usize>> {
        let (mut out, mut acc) = (Vec::new(), 0);
        for r in &self.pieces {
            let (lo, hi) = (at.max(acc), (at + n).min(acc + r.len()));
            if lo < hi {
                out.push(r.start + lo - acc..r.start + hi - acc);
            }
            acc += r.len();
            if acc >= at + n {
                break;
            }
        }
        out
    }

    /// Up to `n` bytes from `at`.
    fn read(&self, f: &[u8], at: usize, n: usize) -> Vec<u8> {
        self.range(at, n).iter().flat_map(|r| f[r.clone()].iter().copied()).collect()
    }
}

struct Walk<'a> {
    f: &'a [u8],
    g: Geom,
    seen: Vec<bool>,
    out: Vec<Blob>,
    min: usize,
    want: &'a dyn Fn(&[u8]) -> bool,
    /// The root pages of the schema's tables and indexes, when walking the schema.
    roots: Option<Vec<u32>>,
    /// Pages left to walk.
    budget: usize,
    /// The b-tree walked, and the records walked so far.
    root: u32,
    records: u32,
}

impl<'a> Walk<'a> {
    fn page(&mut self, pg: u32) -> Option<&'a [u8]> {
        let i = (pg as usize).checked_sub(1).filter(|&i| i < self.g.npages)?;
        if self.budget == 0 || std::mem::replace(&mut self.seen[i], true) {
            return None;
        }
        self.budget -= 1;
        let f: &'a [u8] = self.f;
        Some(&f[i * self.g.psize..(i + 1) * self.g.psize])
    }

    fn tree(&mut self, root: u32) {
        self.root = root;
        let mut stack = vec![root];
        while let Some(pg) = stack.pop() {
            let base = (pg as usize).wrapping_sub(1).wrapping_mul(self.g.psize);
            let Some(p) = self.page(pg) else { continue };
            let h = hdr_off(pg as usize);
            let (typ, u) = (p[h], self.g.usable);
            let interior = typ == T_INDEX_INTERIOR || typ == T_TABLE_INTERIOR;
            if !(interior || typ == T_INDEX_LEAF || typ == T_TABLE_LEAF) || h + 12 > u {
                continue;
            }
            let hl = if interior { 12 } else { 8 };
            let n = be16(p, h + 3);
            if h + hl + 2 * n > u {
                continue;
            }
            if interior {
                stack.push(be32(p, h + 8));
            }
            for i in 0..n {
                let off = be16(p, h + hl + 2 * i);
                if off >= u {
                    continue;
                }
                let mut q = off;
                if interior {
                    if q + 4 > u {
                        continue;
                    }
                    stack.push(be32(p, q));
                    q += 4;
                }
                if typ == T_TABLE_INTERIOR {
                    continue;
                }
                let Some((plen, k)) = get_sqlite(&p[q..u]) else { continue };
                q += k;
                if typ == T_TABLE_LEAF {
                    let Some((_, k)) = get_sqlite(&p[q..u]) else { continue };
                    q += k;
                }
                let local = self.g.local(plen, typ == T_TABLE_LEAF);
                if q + local > u {
                    continue;
                }
                let mut pay = Payload { pieces: Vec::new() };
                pay.pieces.push(base + q..base + q + local);
                if (local as u64) < plen {
                    if q + local + 4 > u {
                        continue;
                    }
                    let mut next = be32(p, q + local);
                    let mut left = (plen - local as u64) as usize;
                    while left > 0 {
                        let ob = (next as usize).wrapping_sub(1).wrapping_mul(self.g.psize);
                        let Some(o) = self.page(next) else { break };
                        let m = left.min(u - 4);
                        next = be32(o, 0);
                        pay.pieces.push(ob + 4..ob + 4 + m);
                        left -= m;
                    }
                    if left > 0 {
                        continue;
                    }
                }
                self.record(&pay);
            }
        }
    }

    /// The blobs of a record (and, of the schema's, its root page).
    fn record(&mut self, pay: &Payload) {
        let head = pay.read(self.f, 0, 9);
        let Some((hl, mut q)) = get_sqlite(&head) else { return };
        let hl = hl as usize;
        if hl > 1 << 16 {
            return;
        }
        let header = pay.read(self.f, 0, hl);
        if header.len() < hl {
            return;
        }
        let mut at = hl;
        let mut column = 0;
        let mut key = [0i64; KEY];
        let record = self.records;
        self.records += 1;
        while q < hl {
            let Some((s, k)) = get_sqlite(&header[q..]) else { return };
            q += k;
            let n = match s {
                0 | 8 | 9 | 10 | 11 => 0,
                1..=4 => s as usize,
                5 => 6,
                6 | 7 => 8,
                _ => (s as usize - 12) / 2,
            };
            // (an integer of 1 to 8 bytes, or 0 and 1 of no bytes)
            let int = match s {
                1..=6 => {
                    let b = pay.read(self.f, at, n);
                    Some(b.iter().fold(0i64, |v, &x| v << 8 | x as i64) << (64 - 8 * n) >> (64 - 8 * n))
                }
                8 | 9 => Some(s as i64 - 8),
                _ => None,
            };
            if column < KEY {
                key[column] = int.unwrap_or(0);
            }
            if let Some(roots) = &mut self.roots
                && column == 3
                && let Some(v) = int.filter(|_| (1..=4).contains(&s))
                && v > 0
            {
                roots.push(v as u32);
            }
            if s >= 12 && s % 2 == 0 && n >= self.min && self.roots.is_none() {
                let start = pay.read(self.f, at, 16);
                if (self.want)(&start) {
                    let (root, column) = (self.root, column as u16);
                    self.out.push(Blob { pieces: pay.range(at, n), root, record, column, key });
                }
            }
            at += n;
            column += 1;
        }
    }
}

/// The blobs of at least `min` bytes of the records of `f`, a database, that `want` takes of
/// their first bytes (up to 16): in the order of their tables and indexes in the schema, of the
/// pages of each b-tree as they are walked. A page is walked once; what cannot be read is left
/// out. None where `f` is not a database.
pub fn blobs(f: &[u8], min: usize, want: &dyn Fn(&[u8]) -> bool) -> Option<Vec<Blob>> {
    Some(walk(f, min, want, usize::MAX)?.0)
}

/// [`blobs`] of the first `pages` pages walked at most: the blobs and the pages walked.
pub fn sample(f: &[u8], min: usize, want: &dyn Fn(&[u8]) -> bool, pages: usize) -> Option<(Vec<Blob>, usize)> {
    walk(f, min, want, pages)
}

fn walk(f: &[u8], min: usize, want: &dyn Fn(&[u8]) -> bool, pages: usize) -> Option<(Vec<Blob>, usize)> {
    let g = Geom::from_file(f)?;
    let mut w = Walk {
        f,
        g,
        seen: vec![false; g.npages],
        out: Vec::new(),
        min: min.max(1),
        want,
        roots: Some(Vec::new()),
        budget: pages,
        root: 1,
        records: 0,
    };
    w.tree(1);
    let roots = w.roots.take().unwrap_or_default();
    for r in roots {
        w.tree(r);
    }
    Some((w.out, pages - w.budget))
}
