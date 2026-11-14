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
/// (for merging): ids for them, as the sections' mappings have them.
#[derive(Debug, Default, Clone)]
pub struct States {
    pub bytes: Vec<u8>,
    pub at: Vec<(u32, u32)>,
    pub opaque: Vec<bool>,
    pub air: Vec<bool>,
    index: std::collections::HashMap<Vec<u8>, u32>,
}

/// The suffix of the air's states.
const AIR: &[u8] = b"_DH-BSW_AIR";

impl States {
    pub fn id(&mut self, s: &[u8]) -> u32 {
        if let Some(&i) = self.index.get(s) {
            return i;
        }
        let i = self.at.len() as u32;
        self.at.push((self.bytes.len() as u32, s.len() as u32));
        self.bytes.extend_from_slice(s);
        self.opaque.push(false);
        self.air.push(s.ends_with(AIR));
        self.index.insert(s.to_vec(), i);
        i
    }

    /// The id of `s`, where it has one.
    pub fn find(&self, s: &[u8]) -> Option<u32> {
        self.index.get(s).copied()
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

/// A section: its 4096 columns (their points' ids of [`States`]), its generation steps and its
/// compression modes.
#[derive(Debug, Clone, Default)]
pub struct Section {
    pub cols: Vec<Vec<Point>>,
    pub generation: Vec<u8>,
    pub compression: Vec<u8>,
}

/// The section of its blobs' contents (in the order of [`Blob::ALL`]), its ids those of `states`
/// (those it has not added).
pub fn section(blobs: &[&[u8]; 8], states: &mut States) -> Result<Section> {
    for r in mapping(blobs[7])? {
        states.id(&blobs[7][r]);
    }
    section_of(blobs, states)
}

/// [`section`] of states that have all of its mapping's (else corrupt).
pub fn section_of(blobs: &[&[u8]; 8], states: &States) -> Result<Section> {
    let m = mapping(blobs[7])?;
    let ids: Vec<u32> =
        m.iter().map(|r| states.find(&blobs[7][r.clone()]).ok_or_else(corrupt)).collect::<Result<_>>()?;
    let mut cols = vec![Vec::new(); COLUMNS];
    for (k, blob) in Blob::ALL[..5].iter().enumerate() {
        let idx: Vec<usize> = blob.columns().collect();
        let c = parse(blobs[k], idx.len())?;
        for (j, &i) in idx.iter().enumerate() {
            cols[i] = c
                .column(j)
                .iter()
                .map(|q| Ok(Point { id: *ids.get(q.id as usize).ok_or_else(corrupt)?, ..*q }))
                .collect::<Result<_>>()?;
        }
    }
    let (g, c) = (blobs[5], blobs[6]);
    if g.len() != COLUMNS || c.len() != COLUMNS {
        return Err(corrupt());
    }
    Ok(Section { cols, generation: g.to_vec(), compression: c.to_vec() })
}

/// The merge of a parent's column of the 2 by 2 columns `cols` of its child: of each run between
/// the points' tops and bottoms (of any of the four), the state most of the four have at its
/// middle (air not counted; of a tie the first), the light the average of those of that state
/// (else of all four); runs of the same state and light joined. From the top down.
pub fn merge(cols: [&[Point]; 4], states: &States, out: &mut Vec<Point>) {
    const NONE: u32 = u32::MAX;
    let start = out.len();
    let mut ys: Vec<i32> = Vec::with_capacity(64);
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
    let mut run = Vec::new();
    for w in ys.windows(2) {
        let (lo, hi) = (w[0], w[1]);
        let h = (hi - lo) as u32;
        let sy = lo + (h / 2) as i32;
        let mut ids = [NONE; 4];
        let mut bl = [0u32; 4];
        let mut sk = [0u32; 4];
        for i in 0..4 {
            while cur[i] >= 0 {
                let p = &cols[i][cur[i] as usize];
                if sy >= p.top() {
                    cur[i] -= 1;
                } else if sy >= p.bottom {
                    ids[i] = p.id;
                    bl[i] = (p.light >> 4) as u32;
                    sk[i] = (p.light & 15) as u32;
                    break;
                } else {
                    break;
                }
            }
        }
        // the most common of those not air
        let air = |id: u32| id != NONE && states.air[id as usize];
        let mut c = [0u32; 4];
        for i in 0..4 {
            if air(ids[i]) {
                continue;
            }
            let x = ids[i];
            let k = if x == ids[0] {
                0
            } else if x == ids[1] {
                1
            } else if x == ids[2] {
                2
            } else {
                3
            };
            c[k] += 1;
        }
        let m = *c.iter().max().unwrap();
        let win = ids[(0..4).find(|&k| c[k] == m).unwrap()];
        let avg = |v: &[u32; 4]| {
            let (mut s, mut n) = (0, 0);
            for i in 0..4 {
                if ids[i] == win {
                    s += v[i];
                    n += 1;
                }
            }
            s.checked_div(n).unwrap_or(v.iter().sum::<u32>() / 4)
        };
        let (b, s) = (avg(&bl) as u8, avg(&sk) as u8);
        if acc == 0 {
            last = (win, b, s);
            low = lo;
            acc = h;
        } else if (win, b, s) != last {
            run.push(Point { id: last.0, height: acc, bottom: low, light: last.1 << 4 | last.2 });
            last = (win, b, s);
            low = lo;
            acc = h;
        } else {
            acc += h;
        }
    }
    if acc > 0 {
        run.push(Point { id: last.0, height: acc, bottom: low, light: last.1 << 4 | last.2 });
    }
    if run.len() >= 2 && run[0].bottom < run[run.len() - 1].bottom {
        run.reverse();
    }
    out.truncate(start);
    out.extend(run);
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

/// The column at `x`, `z` of `grid` culled: each point (from the bottom up, not the bottom nor the
/// top one) opaque between opaque ones above and below, whose four neighbours' points beside it
/// are all opaque, joined into the point above. A column at the section's side is not.
fn cull(grid: &mut [Vec<Point>], x: usize, z: usize, opaque: &[bool]) {
    let op = |id: u32| id != u32::MAX && opaque[id as usize];
    let mut nb: [usize; 4] = [0; 4];
    for (k, (dx, dz)) in [(1isize, 0isize), (-1, 0), (0, 1), (0, -1)].into_iter().enumerate() {
        let (xx, zz) = (x as isize + dx, z as isize + dz);
        if !(0..W as isize).contains(&xx) || !(0..W as isize).contains(&zz) {
            return;
        }
        let i = xx as usize * W + zz as usize;
        if grid[i].is_empty() {
            return;
        }
        nb[k] = i;
    }
    let me = x * W + z;
    let mut idx = nb.map(|i| grid[i].len() as isize - 1);
    let mut ci = grid[me].len() as isize - 1;
    while ci >= 0 {
        let c = ci as usize;
        let col = &grid[me];
        let p = col[c];
        if !op(p.id) || c == 0 || !op(col[c - 1].id) || c + 1 >= col.len() || !op(col[c + 1].id) {
            ci -= 1;
            continue;
        }
        let mut ok = true;
        for k in 0..4 {
            match hidden_by(&p, &grid[nb[k]], idx[k], opaque) {
                Ok(r) => idx[k] = r,
                Err(r) => {
                    idx[k] = r;
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            let col = &mut grid[me];
            col.remove(c);
            let a = &mut col[c - 1];
            a.height += p.height;
            a.bottom = p.bottom;
        }
        ci -= 1;
    }
}

/// The quadrant of a parent that its child `k` makes: its 32 by 32 columns merged (`x * 32 + z`),
/// and their generation steps (the least of the four) and compression modes (the most).
pub fn quadrant(k: &Section, states: &States) -> Section {
    let mut cols = vec![Vec::new(); 32 * 32];
    let (mut generation, mut compression) = (vec![0; 32 * 32], vec![0; 32 * 32]);
    for rx in 0..32 {
        for rz in 0..32 {
            let (cx, cz) = (rx * 2, rz * 2);
            let ii = [cx * W + cz, cx * W + cz + 1, (cx + 1) * W + cz, (cx + 1) * W + cz + 1];
            let mut out = Vec::new();
            merge(ii.map(|i| &k.cols[i][..]), states, &mut out);
            cols[rx * 32 + rz] = out;
            generation[rx * 32 + rz] = ii.iter().map(|&i| k.generation[i]).min().unwrap();
            compression[rx * 32 + rz] = ii.iter().map(|&i| k.compression[i]).max().unwrap();
        }
    }
    Section { cols, generation, compression }
}

/// The parent of its four quadrants (`[a][b]` of the child at `2x + a`, `2z + b`), culled.
pub fn parent(q: [[&Section; 2]; 2], states: &States) -> Section {
    let mut s =
        Section { cols: vec![Vec::new(); COLUMNS], generation: vec![0; COLUMNS], compression: vec![0; COLUMNS] };
    for x in 0..W {
        for z in 0..W {
            let k = q[x / 32][z / 32];
            let j = (x % 32) * 32 + z % 32;
            s.cols[x * W + z] = k.cols[j].clone();
            s.generation[x * W + z] = k.generation[j];
            s.compression[x * W + z] = k.compression[j];
        }
    }
    for x in 0..W {
        for z in 0..W {
            if s.cols[x * W + z].len() > 1 {
                cull(&mut s.cols, x, z, &states.opaque);
            }
        }
    }
    s
}

/// A section's blobs (in the order of [`Blob::ALL`]): its mapping the states of its points in the
/// order of their first use (columns `x * 64 + z`, each from the top). None where a point is of
/// no state.
pub fn blobs(s: &Section, states: &States) -> Option<[Vec<u8>; 8]> {
    let mut local = std::collections::HashMap::new();
    let mut order = Vec::new();
    for c in &s.cols {
        for p in c {
            if p.id == u32::MAX {
                return None;
            }
            local.entry(p.id).or_insert_with(|| {
                order.push(p.id);
                order.len() as u32 - 1
            });
        }
    }
    let cols: Vec<Vec<Point>> =
        s.cols.iter().map(|c| c.iter().map(|p| Point { id: local[&p.id], ..*p }).collect()).collect();
    let mut out: [Vec<u8>; 8] = Default::default();
    for (k, blob) in Blob::ALL[..5].iter().enumerate() {
        write(blob.columns().map(|i| &cols[i][..]), &mut out[k]);
    }
    out[5] = s.generation.clone();
    out[6] = s.compression.clone();
    let m = &mut out[7];
    m.extend((order.len() as i32).to_be_bytes());
    for id in order {
        let b = states.get(id);
        m.extend((b.len() as u16).to_be_bytes());
        m.extend_from_slice(b);
    }
    Some(out)
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
        let mut out = Vec::new();
        merge([&stone, &stone, &dirt_on_stone, &air], &st, &mut out);
        assert_eq!(out, [p(0, 12, 0)]);
        out.clear();
        merge([&dirt_on_stone, &dirt_on_stone, &stone, &air], &st, &mut out);
        assert_eq!(out, [p(1, 2, 10), p(0, 10, 0)]);
    }

    /// A point between opaque ones, of opaque neighbours on all four sides, is joined into the one
    /// above; at a side of the section, or beside air, it is not.
    #[test]
    fn culled() {
        let mut st = states();
        st.opaque[0] = true;
        st.opaque[1] = true;
        let col = vec![p(0, 2, 6), p(1, 2, 4), p(0, 4, 0)];
        let mut grid = vec![vec![p(0, 8, 0)]; COLUMNS];
        grid[10 * W + 10] = col.clone();
        cull(&mut grid, 10, 10, &st.opaque);
        assert_eq!(grid[10 * W + 10], [p(0, 4, 4), p(0, 4, 0)]);
        grid[0] = col.clone();
        cull(&mut grid, 0, 0, &st.opaque);
        assert_eq!(grid[0], col);
        grid[20 * W + 20] = col.clone();
        grid[21 * W + 20] = vec![p(2, 8, 0)];
        cull(&mut grid, 20, 20, &st.opaque);
        assert_eq!(grid[20 * W + 20], col);
    }
}
