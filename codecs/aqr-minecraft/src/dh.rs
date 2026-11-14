//! Distant Horizons' LODs (its database's table `FullData`): a section of 64 by 64 columns, each
//! column points from the top down (a block state, a height, a bottom, light), and the coarser
//! levels made of the finer: a parent's column is the merge of the 2 by 2 columns of its child
//! below it, then culled of the points hidden by opaque neighbours. Written from what the data
//! shows (Distant Horizons' code, LGPL, was read for reference only).
//!
//! A section's blobs: `Data`, its columns but the edges (x and z from 1 to 62); the four sides
//! (`NorthAdjData` z 0, `SouthAdjData` z 63, `EastAdjData` x 63, `WestAdjData` x 0, each a line of
//! 64 columns); `ColumnGenerationStep` and `ColumnWorldCompressionMode`, a byte for each column;
//! `Mapping`, the block states its points' ids are of.

use crate::{Result, corrupt};

/// Columns along a side of a section.
pub const W: usize = 64;
/// Columns of a section.
pub const COLUMNS: usize = W * W;

/// The blobs of a section, in the order of [`Blob`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blob {
    Data,
    North,
    South,
    East,
    West,
    Generation,
    Compression,
    Mapping,
}

impl Blob {
    pub const ALL: [Blob; 8] = [
        Blob::Data,
        Blob::North,
        Blob::South,
        Blob::East,
        Blob::West,
        Blob::Generation,
        Blob::Compression,
        Blob::Mapping,
    ];

    /// The columns of a blob of columns, `x` from `x0` to `x1` and `z` from `z0` to `z1` (`x`
    /// outer).
    pub fn range(self) -> Option<(usize, usize, usize, usize)> {
        Some(match self {
            Blob::Data => (1, W - 1, 1, W - 1),
            Blob::North => (0, W, 0, 1),
            Blob::South => (0, W, W - 1, W),
            Blob::East => (W - 1, W, 0, W),
            Blob::West => (0, 1, 0, W),
            _ => return None,
        })
    }

    /// The indices (`x * 64 + z`) of its columns, in order.
    pub fn columns(self) -> impl Iterator<Item = usize> + Clone {
        let (x0, x1, z0, z1) = self.range().unwrap_or((0, 0, 0, 0));
        (x0..x1).flat_map(move |x| (z0..z1).map(move |z| x * W + z))
    }
}

/// A point of a column: its block state (an id of the section's mapping, or of the states of a
/// file: [`States`]), its height, the Y of its bottom, and its light (block light high, sky light
/// low).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Point {
    pub id: u32,
    pub height: u32,
    pub bottom: i32,
    pub light: u8,
}

impl Point {
    fn top(&self) -> i32 {
        self.bottom + self.height as i32
    }
}

/// Columns, their points one after another (`at[i]..at[i + 1]` the `i`-th's).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Columns {
    pub at: Vec<u32>,
    pub points: Vec<Point>,
}

impl Columns {
    pub fn len(&self) -> usize {
        self.at.len().saturating_sub(1)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn column(&self, i: usize) -> &[Point] {
        &self.points[self.at[i] as usize..self.at[i + 1] as usize]
    }
}

fn varint(b: &[u8], p: &mut usize) -> Result<u64> {
    aqr_common::varint::get(b, p).map_err(|_| corrupt())
}

fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn unzigzag(v: u64) -> i64 {
    (v >> 1) as i64 ^ -((v & 1) as i64)
}

/// A blob of `n` columns parsed: for each column its count, then for each point its id with two
/// flags (a bottom not at the top of the point below, light not zero), then the heights, then
/// of those flagged the bottoms (against the point before's bottom less the height), then the
/// light, a byte each, of those flagged.
pub fn parse(b: &[u8], n: usize) -> Result<Columns> {
    let mut p = 0;
    let mut at = Vec::with_capacity(n + 1);
    at.push(0u32);
    for _ in 0..n {
        let k = varint(b, &mut p)?;
        let last = *at.last().unwrap() as u64;
        at.push(u32::try_from(last + k).map_err(|_| corrupt()).and_then(|v| match v <= 1 << 24 {
            true => Ok(v),
            false => Err(corrupt()),
        })?);
    }
    let total = *at.last().unwrap() as usize;
    let mut points = vec![Point::default(); total];
    let mut flags = vec![0u8; total];
    for (q, f) in points.iter_mut().zip(&mut flags) {
        let e = varint(b, &mut p)?;
        q.id = u32::try_from(e >> 2).map_err(|_| corrupt())?;
        *f = (e & 3) as u8;
    }
    for q in &mut points {
        q.height = u32::try_from(varint(b, &mut p)?).map_err(|_| corrupt())?;
    }
    let mut prev = 0i64;
    for (q, &f) in points.iter_mut().zip(&flags) {
        let err = if f & 1 != 0 { unzigzag(varint(b, &mut p)?) } else { 0 };
        let v = prev - q.height as i64 + err;
        q.bottom = i32::try_from(v).map_err(|_| corrupt())?;
        prev = v;
    }
    for (q, &f) in points.iter_mut().zip(&flags) {
        if f & 2 != 0 {
            q.light = *b.get(p).ok_or_else(corrupt)?;
            p += 1;
        }
    }
    match p == b.len() {
        true => Ok(Columns { at, points }),
        false => Err(corrupt()),
    }
}

/// The blob of the columns `cols` (each a slice of points), as [`parse`] reads it.
pub fn write<'a>(cols: impl Iterator<Item = &'a [Point]> + Clone, out: &mut Vec<u8>) {
    use aqr_common::varint::put;
    for c in cols.clone() {
        put(out, c.len() as u64);
    }
    let mut prev = 0i64;
    for q in cols.clone().flatten() {
        let err = q.bottom as i64 - (prev - q.height as i64);
        prev = q.bottom as i64;
        put(out, (q.id as u64) << 2 | ((q.light != 0) as u64) << 1 | (err != 0) as u64);
    }
    for q in cols.clone().flatten() {
        put(out, q.height as u64);
    }
    let mut prev = 0i64;
    for q in cols.clone().flatten() {
        let err = q.bottom as i64 - (prev - q.height as i64);
        prev = q.bottom as i64;
        if err != 0 {
            put(out, zigzag(err));
        }
    }
    for q in cols.flatten() {
        if q.light != 0 {
            out.push(q.light);
        }
    }
}

/// A mapping's entries (a count, then each a length of 2 bytes and its bytes), as ranges of `b`.
pub fn mapping(b: &[u8]) -> Result<Vec<std::ops::Range<usize>>> {
    let n = i32::from_be_bytes(b.get(..4).ok_or_else(corrupt)?.try_into().unwrap());
    let n = usize::try_from(n).map_err(|_| corrupt())?;
    let mut p = 4;
    let mut v = Vec::with_capacity(n.min(b.len() / 2));
    for _ in 0..n {
        let l = u16::from_be_bytes(b.get(p..p + 2).ok_or_else(corrupt)?.try_into().unwrap()) as usize;
        let r = p + 2..p + 2 + l;
        if r.end > b.len() {
            return Err(corrupt());
        }
        v.push(r);
        p += 2 + l;
    }
    match p == b.len() {
        true => Ok(v),
        false => Err(corrupt()),
    }
}

/// The block states of a file, each its bytes and whether it is opaque (for culling) and air
/// (for merging): ids for them, as the sections' mappings have them. Its index is a table of
/// open addressing, grown where it is made too small for what it takes
/// ([`States::with_capacity`]).
#[derive(Debug, Default, Clone)]
pub struct States {
    pub bytes: Vec<u8>,
    pub at: Vec<(u32, u32)>,
    pub opaque: Vec<bool>,
    pub air: Vec<bool>,
    /// Ids plus one, by their hashes (0 none).
    table: Vec<u32>,
}

/// The suffix of the air's states.
const AIR: &[u8] = b"_DH-BSW_AIR";

fn fnv(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &x| (h ^ x as u64).wrapping_mul(0x100_0000_01b3))
}

impl States {
    /// For `n` states of `bytes` bytes in all, made at once.
    pub fn with_capacity(n: usize, bytes: usize) -> States {
        let mut s = States {
            bytes: Vec::with_capacity(bytes),
            at: Vec::with_capacity(n),
            opaque: Vec::with_capacity(n),
            air: Vec::with_capacity(n),
            table: Vec::new(),
        };
        s.table.resize(Self::slots(n), 0);
        s
    }

    /// Slots of the table for `n` states: a power of two of at least twice as many.
    pub fn slots(n: usize) -> usize {
        (2 * n).next_power_of_two().max(16)
    }

    /// The slot of `s`: its id's, or the empty one where it would be.
    fn slot(&self, s: &[u8]) -> usize {
        let mask = self.table.len() - 1;
        let mut i = fnv(s) as usize & mask;
        loop {
            match self.table[i] {
                0 => return i,
                id if self.get(id - 1) == s => return i,
                _ => i = (i + 1) & mask,
            }
        }
    }

    pub fn id(&mut self, s: &[u8]) -> u32 {
        if self.table.is_empty() || 2 * (self.at.len() + 1) > self.table.len() {
            let n = Self::slots(self.at.len() + 1);
            let old = std::mem::replace(&mut self.table, vec![0; n]);
            for id in old.into_iter().filter(|&id| id != 0) {
                let i = self.slot(self.get(id - 1));
                self.table[i] = id;
            }
        }
        let i = self.slot(s);
        if self.table[i] != 0 {
            return self.table[i] - 1;
        }
        let id = self.at.len() as u32;
        self.at.push((self.bytes.len() as u32, s.len() as u32));
        self.bytes.extend_from_slice(s);
        self.opaque.push(false);
        self.air.push(s.ends_with(AIR));
        self.table[i] = id + 1;
        id
    }

    /// Of no states, its buffers kept.
    pub fn clear(&mut self) {
        self.bytes.clear();
        self.at.clear();
        self.opaque.clear();
        self.air.clear();
        self.table.fill(0);
    }

    /// The id of `s`, where it has one.
    pub fn find(&self, s: &[u8]) -> Option<u32> {
        if self.table.is_empty() {
            return None;
        }
        self.table[self.slot(s)].checked_sub(1)
    }

    pub fn get(&self, i: u32) -> &[u8] {
        let (a, n) = self.at[i as usize];
        &self.bytes[a as usize..(a + n) as usize]
    }

    pub fn len(&self) -> usize {
        self.at.len()
    }

    pub fn is_empty(&self) -> bool {
        self.at.is_empty()
    }
}

/// Columns of points, each its start in `pts` and its length: a column is written whole (its
/// points after the others'), and made shorter where it is (culled).
#[derive(Debug, Clone, Default)]
pub struct Grid {
    start: Vec<u32>,
    len: Vec<u16>,
    pts: Vec<Point>,
    /// The most points it may take, and of a column (0: no bound): past them, corrupt.
    limit: usize,
    column: usize,
}

impl Grid {
    /// Of `columns` columns and `points` points at most, `column` of them in a column, made at
    /// once.
    pub fn with_capacity(columns: usize, points: usize, column: usize) -> Grid {
        let mut g = Grid {
            start: Vec::with_capacity(columns),
            len: Vec::with_capacity(columns),
            pts: Vec::with_capacity(points),
            limit: points,
            column,
        };
        g.clear(columns);
        g
    }

    /// Whether `n` more points fit.
    fn room(&self, n: usize) -> bool {
        self.limit == 0 || self.pts.len() + n <= self.limit
    }

    /// Of `n` columns, none of points; its buffers kept.
    pub fn clear(&mut self, n: usize) {
        self.start.clear();
        self.start.resize(n, 0);
        self.len.clear();
        self.len.resize(n, 0);
        self.pts.clear();
    }

    pub fn columns(&self) -> usize {
        self.len.len()
    }

    pub fn col(&self, i: usize) -> &[Point] {
        let s = self.start[i] as usize;
        &self.pts[s..s + self.len[i] as usize]
    }

    /// The points written, those of columns written again too (a section's corners: of two sides).
    pub fn written(&self) -> usize {
        self.pts.len()
    }

    /// The points of every column.
    pub fn points(&self) -> usize {
        self.len.iter().map(|&n| n as usize).sum()
    }

    /// Column `i` made of the points pushed by `f` (at most 65535).
    fn set(&mut self, i: usize, f: impl FnOnce(&mut Vec<Point>)) -> Result<()> {
        let s = self.pts.len();
        f(&mut self.pts);
        let n = self.pts.len() - s;
        self.start[i] = s as u32;
        self.len[i] = u16::try_from(n).map_err(|_| corrupt())?;
        Ok(())
    }

    /// Point `c` of column `i` taken out, joined into the one above it.
    fn join_up(&mut self, i: usize, c: usize) {
        let (s, n) = (self.start[i] as usize, self.len[i] as usize);
        let col = &mut self.pts[s..s + n];
        let p = col[c];
        col.copy_within(c + 1.., c);
        let a = &mut col[c - 1];
        a.height += p.height;
        a.bottom = p.bottom;
        self.len[i] -= 1;
    }
}

/// A section: its columns (their points' ids of [`States`]), its generation steps and its
/// compression modes, a byte for each column.
#[derive(Debug, Clone, Default)]
pub struct Section {
    pub grid: Grid,
    pub generation: Vec<u8>,
    pub compression: Vec<u8>,
}

impl Section {
    /// Of `columns` columns, `points` points at most (`column` in a column), made at once.
    pub fn with_capacity(columns: usize, points: usize, column: usize) -> Section {
        Section {
            grid: Grid::with_capacity(columns, points, column),
            generation: Vec::with_capacity(columns),
            compression: Vec::with_capacity(columns),
        }
    }
}

/// A section of the columns `cols` (of 4096), its generation steps and compression modes none.
pub fn section_from(cols: &[Vec<Point>]) -> Section {
    let mut s = Section::default();
    s.grid.clear(cols.len());
    for (i, c) in cols.iter().enumerate() {
        let _ = s.grid.set(i, |pts| pts.extend_from_slice(c));
    }
    s
}

/// Buffers of the walks of sections, kept from one to the next.
#[derive(Debug, Clone, Default)]
pub struct Work {
    ys: Vec<i32>,
    flags: Vec<u8>,
    ids: Vec<u32>,
    /// Of each state, its id in the section's mapping being made, where its stamp is the one now.
    local: Vec<u32>,
    stamp: Vec<u32>,
    now: u32,
    order: Vec<u32>,
    pts: Vec<Point>,
    at: Vec<usize>,
    mapping: Vec<u8>,
}

impl Work {
    /// For sections of `points` points at most (`column` in a column), and `states` states of
    /// `bytes` bytes, made at once.
    pub fn with_capacity(points: usize, column: usize, states: usize, bytes: usize) -> Work {
        let mut w = Work {
            ys: Vec::with_capacity(8 * column),
            flags: Vec::with_capacity(points),
            ids: Vec::with_capacity(states),
            local: Vec::with_capacity(states),
            stamp: Vec::with_capacity(states),
            now: 0,
            order: Vec::with_capacity(states),
            pts: Vec::with_capacity(points),
            at: Vec::with_capacity(COLUMNS + 1),
            mapping: Vec::with_capacity(4 + 2 * states + bytes),
        };
        w.stamp.resize(states, 0);
        w.local.resize(states, 0);
        w
    }

    /// The bytes it takes, of those parameters: its buffers, in WebAssembly (of 32-bit pointers).
    pub fn memory(points: usize, column: usize, states: usize, bytes: usize, block: impl Fn(u64) -> u64) -> u64 {
        let (p, s) = (points as u64, states as u64);
        block(32 * column as u64)
            + block(p)
            + 4 * block(4 * s)
            + block(16 * p)
            + block(4 * (COLUMNS as u64 + 1))
            + block(4 + 2 * s + bytes as u64)
    }
}

/// The columns `cols` of a blob parsed into `g` (as [`parse`] reads it), their ids by `ids`.
fn parse_into(
    b: &[u8],
    cols: impl Iterator<Item = usize> + Clone,
    g: &mut Grid,
    ids: &[u32],
    w: &mut Work,
) -> Result<()> {
    let mut p = 0;
    let first = g.pts.len();
    // the counts: each column's place in the points
    let mut at = first;
    for i in cols.clone() {
        let k = usize::try_from(varint(b, &mut p)?).map_err(|_| corrupt())?;
        if k > u16::MAX as usize || k > b.len() || g.column != 0 && k > g.column {
            return Err(corrupt());
        }
        g.start[i] = at as u32;
        g.len[i] = k as u16;
        at += k;
    }
    let n = at - first;
    if n > b.len() || !g.room(n) {
        return Err(corrupt());
    }
    g.pts.resize(at, Point::default());
    w.flags.clear();
    w.flags.resize(n, 0);
    let pts = &mut g.pts[first..];
    for (q, f) in pts.iter_mut().zip(&mut w.flags) {
        let e = varint(b, &mut p)?;
        q.id = *ids.get(usize::try_from(e >> 2).map_err(|_| corrupt())?).ok_or_else(corrupt)?;
        *f = (e & 3) as u8;
    }
    for q in pts.iter_mut() {
        q.height = u32::try_from(varint(b, &mut p)?).map_err(|_| corrupt())?;
    }
    let mut prev = 0i64;
    for (q, &f) in pts.iter_mut().zip(&w.flags) {
        let err = if f & 1 != 0 { unzigzag(varint(b, &mut p)?) } else { 0 };
        let v = prev - q.height as i64 + err;
        q.bottom = i32::try_from(v).map_err(|_| corrupt())?;
        prev = v;
    }
    for (q, &f) in pts.iter_mut().zip(&w.flags) {
        if f & 2 != 0 {
            q.light = *b.get(p).ok_or_else(corrupt)?;
            p += 1;
        }
    }
    match p == b.len() {
        true => Ok(()),
        false => Err(corrupt()),
    }
}

/// The section of its blobs' contents (in the order of [`Blob::ALL`]) into `out`, its ids those
/// of `states` (those it has not, added).
pub fn section(blobs: &[&[u8]; 8], states: &mut States, w: &mut Work, out: &mut Section) -> Result<()> {
    for r in mapping(blobs[7])? {
        states.id(&blobs[7][r]);
    }
    section_of(blobs, states, w, out)
}

/// [`section`] of states that have all of its mapping's (else corrupt).
pub fn section_of(blobs: &[&[u8]; 8], states: &States, w: &mut Work, out: &mut Section) -> Result<()> {
    let mut ids = std::mem::take(&mut w.ids);
    ids.clear();
    for r in mapping(blobs[7])? {
        ids.push(states.find(&blobs[7][r]).ok_or_else(corrupt)?);
    }
    out.grid.clear(COLUMNS);
    let mut r = Ok(());
    for (k, blob) in Blob::ALL[..5].iter().enumerate() {
        r = r.and_then(|_| parse_into(blobs[k], blob.columns(), &mut out.grid, &ids, w));
    }
    w.ids = ids;
    r?;
    let (g, c) = (blobs[5], blobs[6]);
    if g.len() != COLUMNS || c.len() != COLUMNS {
        return Err(corrupt());
    }
    out.generation.clear();
    out.generation.extend_from_slice(g);
    out.compression.clear();
    out.compression.extend_from_slice(c);
    Ok(())
}

/// The merge of a parent's column of the 2 by 2 columns `cols` of its child, pushed to `out`: of
/// each run between the points' tops and bottoms (of any of the four), the state most of the four
/// have at its middle (air not counted; of a tie the first; none where a column has no point
/// there), the light the average of those of that state (else of all four); runs of the same
/// state and light joined. From the top down.
pub fn merge(cols: [&[Point]; 4], states: &States, ys: &mut Vec<i32>, out: &mut Vec<Point>) {
    const NONE: u32 = u32::MAX;
    let start = out.len();
    ys.clear();
    for c in cols {
        for p in c {
            ys.push(p.bottom);
            ys.push(p.top());
        }
    }
    if ys.is_empty() {
        return;
    }
    ys.sort_unstable();
    ys.dedup();
    let mut cur = cols.map(|c| c.len() as isize - 1);
    let mut acc = 0u32;
    let mut last = (NONE, 0u8, 0u8);
    let mut low = 0i32;
    for w in ys.windows(2) {
        let (lo, hi) = (w[0], w[1]);
        let h = (hi - lo) as u32;
        let sy = lo + (h / 2) as i32;
        let mut ids = [NONE; 4];
        let mut light = [0u8; 4];
        for i in 0..4 {
            while cur[i] >= 0 {
                let p = &cols[i][cur[i] as usize];
                if sy >= p.top() {
                    cur[i] -= 1;
                } else if sy >= p.bottom {
                    ids[i] = p.id;
                    light[i] = p.light;
                    break;
                } else {
                    break;
                }
            }
        }
        // the most common of those not air
        let mut c = [0u32; 4];
        for i in 0..4 {
            let x = ids[i];
            if x != NONE && states.air[x as usize] {
                continue;
            }
            let k = (0..3).find(|&k| ids[k] == x).unwrap_or(3);
            c[k] += 1;
        }
        let m = *c.iter().max().unwrap();
        let win = ids[(0..4).find(|&k| c[k] == m).unwrap()];
        let avg = |shift: u32| {
            let (mut s, mut n, mut all) = (0u32, 0u32, 0u32);
            for i in 0..4 {
                let v = (light[i] >> shift & 15) as u32;
                all += v;
                if ids[i] == win {
                    s += v;
                    n += 1;
                }
            }
            s.checked_div(n).unwrap_or(all / 4) as u8
        };
        let (b, s) = (avg(4), avg(0));
        if acc == 0 {
            last = (win, b, s);
            low = lo;
            acc = h;
        } else if (win, b, s) != last {
            out.push(Point { id: last.0, height: acc, bottom: low, light: last.1 << 4 | last.2 });
            last = (win, b, s);
            low = lo;
            acc = h;
        } else {
            acc += h;
        }
    }
    if acc > 0 {
        out.push(Point { id: last.0, height: acc, bottom: low, light: last.1 << 4 | last.2 });
    }
    let run = &mut out[start..];
    if run.len() >= 2 && run[0].bottom < run[run.len() - 1].bottom {
        run.reverse();
    }
}

/// Whether the points of `adj` from index `ai` down (from the bottom up) that overlap `pt` are all
/// opaque: `Ok(index)` where they are (the index of the one reaching its top), `Err(index)` (where
/// to go on from) where not.
fn hidden_by(pt: &Point, adj: &[Point], mut ai: isize, opaque: &[bool]) -> std::result::Result<isize, isize> {
    if adj.is_empty() {
        return Err(ai);
    }
    let (bc, tc) = (pt.bottom, pt.top());
    while ai >= 0 {
        let a = &adj[ai as usize];
        if a.top() <= bc {
            ai -= 1;
            continue;
        }
        if a.id == u32::MAX || !opaque[a.id as usize] {
            return Err(ai);
        }
        if a.top() >= tc {
            return Ok(ai);
        }
        ai -= 1;
    }
    Err(ai)
}

/// The column at `x`, `z` of `g` culled: each point (from the bottom up, not the bottom nor the
/// top one) opaque between opaque ones above and below, whose four neighbours' points beside it
/// are all opaque, joined into the point above. A column at the section's side is not.
fn cull(g: &mut Grid, x: usize, z: usize, opaque: &[bool]) {
    let op = |id: u32| id != u32::MAX && opaque[id as usize];
    let mut nb: [usize; 4] = [0; 4];
    for (k, (dx, dz)) in [(1isize, 0isize), (-1, 0), (0, 1), (0, -1)].into_iter().enumerate() {
        let (xx, zz) = (x as isize + dx, z as isize + dz);
        if !(0..W as isize).contains(&xx) || !(0..W as isize).contains(&zz) {
            return;
        }
        let i = xx as usize * W + zz as usize;
        if g.len[i] == 0 {
            return;
        }
        nb[k] = i;
    }
    let me = x * W + z;
    let mut idx = nb.map(|i| g.len[i] as isize - 1);
    let mut ci = g.len[me] as isize - 1;
    while ci >= 0 {
        let c = ci as usize;
        let col = g.col(me);
        let p = col[c];
        if !op(p.id) || c == 0 || !op(col[c - 1].id) || c + 1 >= col.len() || !op(col[c + 1].id) {
            ci -= 1;
            continue;
        }
        let mut ok = true;
        for k in 0..4 {
            match hidden_by(&p, g.col(nb[k]), idx[k], opaque) {
                Ok(r) => idx[k] = r,
                Err(r) => {
                    idx[k] = r;
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            g.join_up(me, c);
        }
        ci -= 1;
    }
}

/// The quadrant of a parent that its child `k` makes, into `out`: its 32 by 32 columns merged
/// (`x * 32 + z`), and their generation steps (the least of the four) and compression modes (the
/// most).
pub fn quadrant(k: &Section, states: &States, w: &mut Work, out: &mut Section) -> Result<()> {
    out.grid.clear(32 * 32);
    out.generation.clear();
    out.compression.clear();
    for rx in 0..32 {
        for rz in 0..32 {
            let (cx, cz) = (rx * 2, rz * 2);
            let ii = [cx * W + cz, cx * W + cz + 1, (cx + 1) * W + cz, (cx + 1) * W + cz + 1];
            let ys = &mut w.ys;
            // (a merge makes a point less than the tops and bottoms of the four, at most)
            if !out.grid.room(2 * ii.iter().map(|&i| k.grid.len[i] as usize).sum::<usize>()) {
                return Err(corrupt());
            }
            out.grid.set(rx * 32 + rz, |pts| merge(ii.map(|i| k.grid.col(i)), states, ys, pts))?;
            out.generation.push(ii.iter().map(|&i| k.generation[i]).min().unwrap());
            out.compression.push(ii.iter().map(|&i| k.compression[i]).max().unwrap());
        }
    }
    Ok(())
}

/// The parent of its four quadrants (`[a][b]` of the child at `2x + a`, `2z + b`) into `out`,
/// culled.
pub fn parent(q: [[&Section; 2]; 2], states: &States, out: &mut Section) -> Result<()> {
    out.grid.clear(COLUMNS);
    out.generation.clear();
    out.compression.clear();
    for x in 0..W {
        for z in 0..W {
            let k = q[x / 32][z / 32];
            let j = (x % 32) * 32 + z % 32;
            let c = k.grid.col(j);
            if !out.grid.room(c.len()) {
                return Err(corrupt());
            }
            out.grid.set(x * W + z, |pts| pts.extend_from_slice(c))?;
            out.generation.push(k.generation[j]);
            out.compression.push(k.compression[j]);
        }
    }
    for x in 0..W {
        for z in 0..W {
            if out.grid.len[x * W + z] > 1 {
                cull(&mut out.grid, x, z, &states.opaque);
            }
        }
    }
    Ok(())
}

/// A section's blobs (in the order of [`Blob::ALL`]) into `out`: its mapping the states of its
/// points in the order of their first use (columns `x * 64 + z`, each from the top). False where
/// a point is of no state.
pub fn blobs(s: &Section, states: &States, w: &mut Work, out: &mut [Vec<u8>; 8]) -> bool {
    if !local_ids(&s.grid, states, w) {
        return false;
    }
    for (k, blob) in Blob::ALL[..5].iter().enumerate() {
        // (the columns' points of the mapping's ids)
        w.pts.clear();
        w.at.clear();
        w.at.push(0);
        for i in blob.columns() {
            w.pts.extend(s.grid.col(i).iter().map(|p| Point { id: w.local[p.id as usize], ..*p }));
            w.at.push(w.pts.len());
        }
        out[k].clear();
        write(w.at.windows(2).map(|r| &w.pts[r[0]..r[1]]), &mut out[k]);
    }
    out[5].clear();
    out[5].extend_from_slice(&s.generation);
    out[6].clear();
    out[6].extend_from_slice(&s.compression);
    out[7].clear();
    write_mapping(&w.order, states, &mut out[7]);
    true
}

/// The ids of the points of `cols` of `g` in the order of their first use, into `w.order`, and
/// each state's place among them into `w.local`: false where a point is of no state.
fn local_ids(g: &Grid, states: &States, w: &mut Work) -> bool {
    w.now = w.now.wrapping_add(1);
    if w.now == 0 || w.stamp.len() < states.len() {
        w.stamp.clear();
        w.stamp.resize(states.len().max(w.stamp.capacity()), 0);
        w.local.resize(w.stamp.len(), 0);
        w.now = 1;
    }
    w.order.clear();
    for i in 0..g.columns() {
        for p in g.col(i) {
            if p.id == u32::MAX {
                return false;
            }
            let id = p.id as usize;
            if w.stamp[id] != w.now {
                w.stamp[id] = w.now;
                w.local[id] = w.order.len() as u32;
                w.order.push(p.id);
            }
        }
    }
    true
}

/// The mapping of `w.order`'s states appended to `m`.
fn write_mapping(order: &[u32], states: &States, m: &mut Vec<u8>) {
    m.extend((order.len() as i32).to_be_bytes());
    for &id in order {
        let b = states.get(id);
        m.extend((b.len() as u16).to_be_bytes());
        m.extend_from_slice(b);
    }
}

/// A quadrant (of 1024 columns, [`quadrant`]) as bytes, appended to `out`: a varint of the
/// bytes of its mapping (of its states in the order of their first use), the mapping, its columns
/// (as [`write`] writes them, of the mapping's ids), then its generation steps and compression
/// modes, 1024 bytes each. False where a point is of no state.
pub fn quadrant_bytes(q: &Section, states: &States, w: &mut Work, out: &mut Vec<u8>) -> bool {
    if !local_ids(&q.grid, states, w) {
        return false;
    }
    let mut m = std::mem::take(&mut w.mapping);
    m.clear();
    write_mapping(&w.order, states, &mut m);
    aqr_common::varint::put(out, m.len() as u64);
    out.extend_from_slice(&m);
    w.mapping = m;
    w.pts.clear();
    w.at.clear();
    w.at.push(0);
    for i in 0..q.grid.columns() {
        w.pts.extend(q.grid.col(i).iter().map(|p| Point { id: w.local[p.id as usize], ..*p }));
        w.at.push(w.pts.len());
    }
    write(w.at.windows(2).map(|r| &w.pts[r[0]..r[1]]), out);
    out.extend_from_slice(&q.generation);
    out.extend_from_slice(&q.compression);
    true
}

/// A quadrant of [`quadrant_bytes`] into `out`, its states added to `states`.
pub fn quadrant_of(b: &[u8], states: &mut States, w: &mut Work, out: &mut Section) -> Result<()> {
    const N: usize = 32 * 32;
    let mut p = 0;
    let n = usize::try_from(varint(b, &mut p)?).map_err(|_| corrupt())?;
    let m = b.get(p..).and_then(|r| r.get(..n)).ok_or_else(corrupt)?;
    let mut ids = std::mem::take(&mut w.ids);
    ids.clear();
    for r in mapping(m)? {
        ids.push(states.id(&m[r]));
    }
    p += n;
    let rest = b.len().checked_sub(2 * N).filter(|&e| e >= p).ok_or_else(corrupt)?;
    out.grid.clear(N);
    let r = parse_into(&b[p..rest], 0..N, &mut out.grid, &ids, w);
    w.ids = ids;
    r?;
    out.generation.clear();
    out.generation.extend_from_slice(&b[rest..rest + N]);
    out.compression.clear();
    out.compression.extend_from_slice(&b[rest + N..]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rng(s: &mut u64) -> u64 {
        *s ^= *s << 13;
        *s ^= *s >> 7;
        *s ^= *s << 17;
        *s
    }

    /// Columns of points from the top down, some with gaps between them and some with light.
    fn columns(n: usize, seed: u64) -> Vec<Vec<Point>> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                let mut y = 200 - (rng(&mut s) % 40) as i32;
                (0..rng(&mut s) % 6)
                    .map(|_| {
                        let h = 1 + (rng(&mut s) % 30) as u32;
                        y -= h as i32 + if rng(&mut s).is_multiple_of(4) { 3 } else { 0 };
                        let light = if rng(&mut s).is_multiple_of(3) { 0 } else { (rng(&mut s) % 256) as u8 };
                        Point { id: (rng(&mut s) % 9) as u32, height: h, bottom: y, light }
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn written_as_parsed() {
        for (n, seed) in [(3844, 1), (64, 2), (1, 3)] {
            let cols = columns(n, seed);
            let mut b = Vec::new();
            write(cols.iter().map(|c| &c[..]), &mut b);
            let c = parse(&b, n).unwrap();
            assert!((0..n).all(|i| c.column(i) == &cols[i][..]));
            let mut again = Vec::new();
            write((0..n).map(|i| c.column(i)), &mut again);
            assert_eq!(again, b);
            assert!(parse(&b[..b.len() - 1], n).is_err() || b.is_empty());
        }
    }

    fn states() -> States {
        let mut s = States::default();
        for n in [&b"minecraft:stone"[..], b"minecraft:dirt", b"minecraft:air_DH-BSW_AIR"] {
            s.id(n);
        }
        s
    }

    fn p(id: u32, height: u32, bottom: i32) -> Point {
        Point { id, height, bottom, light: 0x0f }
    }

    /// Of four columns, the state most of them have at each run (air not counted), runs of the
    /// same joined, from the top down.
    #[test]
    fn merged() {
        let st = states();
        let stone = [p(0, 12, 0)];
        let dirt_on_stone = [p(1, 2, 10), p(0, 10, 0)];
        let air = [p(2, 12, 0)];
        let (mut out, mut ys) = (Vec::new(), Vec::new());
        merge([&stone, &stone, &dirt_on_stone, &air], &st, &mut ys, &mut out);
        assert_eq!(out, [p(0, 12, 0)]);
        out.clear();
        merge([&dirt_on_stone, &dirt_on_stone, &stone, &air], &st, &mut ys, &mut out);
        assert_eq!(out, [p(1, 2, 10), p(0, 10, 0)]);
    }

    /// A point between opaque ones, of opaque neighbours on all four sides, is joined into the one
    /// above; at a side of the section, or beside air, it is not.
    #[test]
    fn culled() {
        let mut st = states();
        st.opaque[0] = true;
        st.opaque[1] = true;
        let col = [p(0, 2, 6), p(1, 2, 4), p(0, 4, 0)];
        let grid = |cols: &[(usize, &[Point])]| {
            let mut g = Grid::default();
            g.clear(COLUMNS);
            let stone = [p(0, 8, 0)];
            for i in 0..COLUMNS {
                let c = cols.iter().find(|c| c.0 == i).map_or(&stone[..], |c| c.1);
                g.set(i, |pts| pts.extend_from_slice(c)).unwrap();
            }
            g
        };
        let mut g = grid(&[(10 * W + 10, &col)]);
        cull(&mut g, 10, 10, &st.opaque);
        assert_eq!(g.col(10 * W + 10), [p(0, 4, 4), p(0, 4, 0)]);
        let mut g = grid(&[(0, &col)]);
        cull(&mut g, 0, 0, &st.opaque);
        assert_eq!(g.col(0), col);
        let mut g = grid(&[(20 * W + 20, &col), (21 * W + 20, &[p(2, 8, 0)])]);
        cull(&mut g, 20, 20, &st.opaque);
        assert_eq!(g.col(20 * W + 20), col);
    }
}
