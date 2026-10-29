//! aqr-sqlite as a module of entry formulas (see aqr-formula): the page geometry as
//! parameters, the 18 streams of the transform as raw input slots (FORMAT.md, "Formula
//! module").

use crate::index::ROWID;
use crate::page::K_OVF;
use crate::record::{MIN_REP, R_RAW, T_BLOB, T_BLOB_REP, T_TEXT, T_TEXT_REP};
use crate::transform::{self, NSTREAMS, S_DIR, S_GRP, S_KIND, S_REC, STREAM_NAMES};
use crate::varint::{Rd, put_uv};
use aqr_formula::{Formula, Module, Node, PAGE, Usage};

pub use crate::module::decode;

pub const MODULE: Module = Module::new("sqlite-recons", 1).with_memory(memory);

/// Task slots of the module (its bin's `whole!(slots 8, ..)`).
const SLOTS: usize = 8;

/// A group of records, as its directory entry and streams say.
struct Group {
    /// The table the index is on, and the indexed columns (`ROWID` or a table column).
    index: Option<(usize, Vec<u32>)>,
    /// Per column, its largest value and the size of all its values (see [`column`]).
    cols: Vec<(u64, u64)>,
    rows: u64,
    /// Values of the column tags (every value of the group has one).
    values: u64,
    /// The records kept as raw payloads: the largest, and all together.
    raw_max: u64,
    raw_sum: u64,
    /// Leaf pages (of a table).
    leaves: u64,
}

impl Group {
    /// The most bytes of a record coded by columns or kept raw: a header, and per column a
    /// serial type, its value (8 bytes, or the largest text or blob).
    fn record(&self) -> u64 {
        self.raw_max.max(16 + self.cols.iter().map(|c| 18 + c.0.max(8)).sum::<u64>())
    }

    /// The most columns of a record that is parsed.
    fn width(&self) -> u64 {
        self.raw_max.max(self.cols.len() as u64)
    }
}

/// The largest value of a column and the sizes of all its values together, from its tags and
/// lengths: 8 bytes for those that are not text or blobs (a text or blob is of the next length,
/// or a repeat: the one of the distinct ones that many places back, the distinct ones being
/// those of `MIN_REP` bytes at least).
fn column(mut tag: Rd, mut len: Rd) -> Option<(u64, u64)> {
    let (mut distinct, mut most, mut sum) = (Vec::new(), 0, 0);
    while !tag.at_end() {
        let size = match tag.u8().ok()? {
            T_TEXT | T_BLOB => {
                let n = len.uv().ok()?;
                if n >= MIN_REP as u64 {
                    distinct.push(n);
                }
                n
            }
            T_TEXT_REP | T_BLOB_REP => {
                *distinct.len().checked_sub(len.usize().ok()?.checked_add(1)?).and_then(|i| distinct.get(i))?
            }
            _ => 8,
        };
        most = most.max(size);
        sum += size;
    }
    Some((most, sum))
}

/// The directory (as `transform::rebuild` reads it), the records of each group and the pages'
/// kinds: the groups, and the overflow pages.
fn groups(s: &[&[u8]]) -> Option<(Vec<Group>, u64)> {
    let mut dir = Rd::new(s[S_DIR]);
    let ng = dir.usize().ok().filter(|&n| n <= dir.b.len())?;
    let mut gs: [Rd; 8] = std::array::from_fn(|i| Rd::new(s[S_REC + i]));
    let mut out = Vec::with_capacity(ng);
    for _ in 0..ng {
        dir.u8().ok()?;
        let index = match dir.usize().ok()? {
            0 => None,
            t => {
                let m = dir.usize().ok().filter(|&m| m <= 1 << 16)?;
                let mut cols = Vec::with_capacity(m);
                for _ in 0..m {
                    cols.push(u32::try_from(dir.uv().ok()?).ok()?.wrapping_sub(1));
                    dir.u8().ok()?;
                }
                Some((t - 1, cols))
            }
        };
        // (the order of `GroupDec::new`)
        let mut rec = gs[0].sub(dir.usize().ok()?).ok()?;
        gs[1].sub(dir.usize().ok()?).ok()?;
        gs[2].sub(dir.usize().ok()?).ok()?;
        let nc = dir.usize().ok().filter(|&n| n <= dir.b.len())?;
        let (mut cols, mut values) = (Vec::with_capacity(nc), 0);
        for _ in 0..nc {
            let tag = gs[3].sub(dir.usize().ok()?).ok()?;
            values += tag.b.len() as u64;
            dir.uv().ok()?;
            gs[4].sub(dir.usize().ok()?).ok()?;
            gs[5].sub(dir.usize().ok()?).ok()?;
            let len = gs[6].sub(dir.usize().ok()?).ok()?;
            gs[7].sub(dir.usize().ok()?).ok()?;
            cols.push(column(tag, len)?);
        }
        let mut g = Group { index, cols, rows: 0, values, raw_max: 0, raw_sum: 0, leaves: 0 };
        while !rec.at_end() {
            g.rows += 1;
            // (a raw record is followed by its length)
            if rec.uv().ok()? == R_RAW {
                let n = rec.usize().ok()? as u64;
                g.raw_max = g.raw_max.max(n);
                g.raw_sum += n;
            }
        }
        out.push(g);
    }
    let (mut kind, mut grp, mut overflow) = (Rd::new(s[S_KIND]), Rd::new(s[S_GRP]), 0);
    while !kind.at_end() {
        match kind.u8().ok()? {
            K_OVF => overflow += 1,
            k @ (2 | 10 | 13) => {
                let g = grp.usize().ok().filter(|&g| g < ng)?;
                out[g].leaves += (k == 13) as u64;
            }
            _ => {}
        }
    }
    Some((out, overflow))
}

/// The most bytes that a vector of bytes of `n`, pushed to from empty (capacity doubling from 8),
/// holds at once: the old capacity and the new, at the last growth.
fn grown_by_pushes(n: u64) -> u64 {
    if n <= 8 { 16 } else { 3 * (n.next_power_of_two() / 2) }
}

/// The memory of the module, on 32 bits, from the directory and the streams of the records
/// (the tags and lengths of each column are followed, for the sizes of its values). `n` is the
/// records of a table, `m` the indexed columns of an index.
///
/// - `W`, the instance before its first task: the inputs, and the decoders, the metadata and
///   the slots of the contexts (400 bytes a group, 96 a column, 16 an indexed column).
/// - The keys of an index (`Keys`) per row: 8 bytes a column (serial type and place), a flag,
///   the rowid (8), its bytes (8) if indexed; then the bytes copied, the least of `m` times the
///   table's payload (its leaf pages and all overflow pages) and the sizes of the indexed
///   columns' values (8 bytes for what is not a text or a blob) and of the raw records, for each
///   column. Pushed to, the vectors are twice that, and one more is held while it grows.
/// - A task of the first pass (a table with indexes) holds, on its copy of `W`, the group's
///   decoder, its largest record (three times), the columns of a parsed record (16 bytes,
///   three times), the table of values it repeats (8 bytes each, three times) and the keys of
///   its indexes; then those and the bytes they are put in (at the last growth, the old
///   capacity and the new). The spawner (with two tables at least) takes them in at their sizes
///   from bytes of the same sizes (freed, but not taken to be reused). Alone, the first pass
///   runs in the spawner, without the bytes.
/// - A task of the second pass (an index) holds `44 n` bytes (the sort of `index::order` at its
///   largest: keys and rows, and a second set for the radix passes; and the order). The
///   spawner takes the orders (4 `n`) likewise. One index is sorted in the spawner. Without
///   tasks (one thread) all keys stay at their grown size, and a table's scan leaves its
///   record and columns behind.
/// - Last the pages: the output, two pages, a cell, the intervals (8 bytes, three times,
///   at most half a page of them), the chain of an overflow (4 bytes, three times) and the
///   largest record (three times).
///
/// The peak is the largest of those: the spawner and at most [`SLOTS`] tasks, the largest ones
/// (a task starts as another ends), each on its copy of `W` as it was at the spawn, and a page
/// of slack for each large vector.
fn memory(u: &Usage<'_>) -> Option<u64> {
    let s: [&[u8]; NSTREAMS] = u.all_contents()?;
    let (gs, overflow) = groups(&s)?;
    let mut r = Rd::new(u.params);
    let (psize, usable) = (r.usize().ok()? as u64, r.usize().ok()? as u64);
    let len = u.output();
    let ng = gs.len() as u64;
    let w = u.base()
        + 216
        + 1024
        + gs.iter()
            .map(|g| 400 + 96 * g.cols.len() as u64 + 16 * g.index.as_ref().map_or(0, |i| i.1.len() as u64))
            .sum::<u64>();
    // the indexes of each table
    let mut by_table: Vec<Vec<usize>> = vec![Vec::new(); gs.len()];
    for (i, g) in gs.iter().enumerate() {
        if let Some((t, _)) = &g.index {
            by_table.get_mut(*t)?.push(i);
        }
    }
    let pad = |x: u64| x.min(PAGE);
    // the bytes of the keys (their vectors' contents) of an index on table `t`
    let keys = |t: usize, i: usize| -> Option<(u64, u64, u64)> {
        let (g, tg) = (&gs[i], &gs[t]);
        let cols = &g.index.as_ref()?.1;
        let (n, m) = (tg.rows, cols.len() as u64);
        let rowid = cols.contains(&ROWID) as u64;
        let payload = len.min((tg.leaves + overflow) * psize);
        let columns: u64 =
            cols.iter().map(|&c| tg.cols.get(c as usize).filter(|_| c != ROWID).map_or(0, |c| c.1)).sum();
        let copied = (m * payload).min(columns + m * tg.raw_sum);
        let bytes = 4 * m + 8 * n * m + 9 * n + 8 * n * rowid + copied;
        // (the largest vector, and the pages of slack the large ones take)
        let pads = pad(8 * n * m) + pad(n) + (1 + rowid) * pad(8 * n) + pad(copied);
        Some((bytes, (8 * n * m).max(8 * n).max(copied), pads))
    };
    let tables: Vec<usize> = (0..gs.len()).filter(|&t| !by_table[t].is_empty()).collect();
    let mut first = Vec::new();
    let (mut kept, mut grown, mut scan, mut sorted, mut perms) = (0u64, 0u64, 0u64, Vec::new(), 0u64);
    let mut indexes = Vec::new();
    for &t in &tables {
        let tg = &gs[t];
        let (mut total, mut most, mut pads) = (0, 0, 0);
        for &i in &by_table[t] {
            let (bytes, vec, slack) = keys(t, i)?;
            total += bytes;
            pads += slack;
            most = most.max(vec);
            indexes.push(tg.rows);
            sorted.push(44 * tg.rows + 4 * pad(8 * tg.rows));
            perms += 4 * tg.rows;
        }
        let extra = 96 * tg.cols.len() as u64 + 80 + 3 * tg.record() + 48 * tg.width() + 24 * tg.values;
        let building = 2 * total + most + extra;
        let sent = total + 64 * by_table[t].len() as u64;
        let giving = 2 * total + grown_by_pushes(sent) + 8 * tg.rows;
        let pads = 2 * pads + pad(3 * tg.record()) + pad(48 * tg.width()) + pad(24 * tg.values) + 2 * pad(sent);
        first.push((building.max(giving) + pads, sent));
        kept += total;
        grown += 2 * total;
        scan = scan.max(extra + most);
    }
    let longest = gs.iter().map(|g| g.record()).max().unwrap_or(0);
    let cells = 3 * (psize + 16) + 12 * psize + 12 * overflow.min(longest / usable.saturating_sub(4).max(1) + 1);
    let pages = len + 2 * psize + cells + 3 * longest + 2 * PAGE;
    // the largest tasks at once, each on `at`
    let top = |mut v: Vec<u64>, at: u64| -> u64 {
        v.sort_unstable_by(|a, b| b.cmp(a));
        v.iter().take(SLOTS).map(|t| at + t).sum()
    };
    let slack = (2 * ng + 8) * PAGE / 16;
    // the first pass
    let stale = first.iter().map(|f| f.1).sum::<u64>();
    let (pass1, held) = if first.len() >= 2 {
        let sizes = first.iter().map(|f| f.0).collect();
        (w + kept + stale + top(sizes, w) + slack, kept + stale)
    } else {
        // (in the spawner)
        let own = first.iter().map(|f| f.0).max().unwrap_or(0);
        (w + own + slack, grown)
    };
    // the second
    let at = w + held;
    let pass2 = if indexes.len() >= 2 {
        at + 2 * perms + top(sorted.clone(), at) + slack
    } else {
        at + sorted.first().copied().unwrap_or(0) + slack
    };
    // without tasks: all keys, grown, and the orders sorted one at a time
    let serial = w + grown + perms + scan + sorted.iter().copied().max().unwrap_or(0) + slack;
    let last = w + held.max(grown) + 2 * perms + scan + pages + slack;
    Some(pass1.max(pass2).max(serial).max(last))
}

#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Rebuild the file from the streams and compare (no formula on a mismatch).
    pub verify: bool,
    /// No formula if the transform panics (off to find such bugs).
    pub catch: bool,
    /// Threads (1: everything on the calling thread). The streams are the same whatever the
    /// number.
    pub threads: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options { verify: true, catch: true, threads: 1 }
    }
}

/// The formula of `input`, or `None` where the file is not a database that the transform
/// can code.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    encode_with(input, &Options { threads, ..Default::default() })
}

/// `encode` with `opt`.
pub fn encode_with(input: &[u8], opt: &Options) -> Option<Formula> {
    let enc = if opt.catch {
        std::panic::catch_unwind(|| transform::encode_threads(input, opt.threads)).ok().flatten()
    } else {
        transform::encode_threads(input, opt.threads)
    }?;
    if opt.verify {
        let run = || transform::verify(&enc.geom, input, &enc.streams);
        let ok = if opt.catch {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)).unwrap_or(false)
        } else {
            run()
        };
        if !ok {
            return None;
        }
    }
    let mut params = Vec::new();
    put_uv(&mut params, enc.geom.psize as u64);
    put_uv(&mut params, enc.geom.usable as u64);
    params.push(enc.geom.fmt4 as u8);
    let mut f = Formula::default();
    let inputs =
        enc.streams.into_iter().enumerate().map(|(i, s)| f.push(format!("sqlr/{}", STREAM_NAMES[i]), s)).collect();
    f.root = Node::apply(MODULE, params, inputs, input.len() as u64);
    Some(f)
}
