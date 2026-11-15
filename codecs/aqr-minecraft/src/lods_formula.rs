//! Distant Horizons' database as a formula: its blobs' frames made again by aqr-zstd's
//! `zstd-frames`, of contents that `dh-lods` ([`MODULE`]) makes, the parents' of their children
//! where the children come before them (FORMAT.md, §8).

use std::collections::HashMap;
use std::ops::Range;

use aqr_formula::{Formula, Kind, Module, Node};
use aqr_sqlite::blobs::Blob as SqlBlob;
use aqr_zstd::frames::Pool;

use crate::dh::{self, Section, States, Work};
use crate::lods::{DERIVED, OBSERVED, PLAIN, Params, SHIFT};

/// Distant Horizons' contents made of their steps, stored bytes and opaque states, a part module.
pub const MODULE: Module = Module::new("dh-lods", 1).with_plan(memory);

fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    crate::lods::plan(u.initial(), u.params, *u.inputs.get(2)?)
}

/// The columns of `FullData` that are a section's blobs, in the order of [`dh::Blob::ALL`].
pub const COLUMNS: [u16; 8] = [5, 15, 16, 17, 18, 6, 7, 8];

/// What the pool may hold at once (blobs of children to come, quadrants kept, parents' blobs to
/// put), and the pool at most: within the bound of an application (256 MiB compressing).
const HELD: u64 = 96 << 20;
const POOL: u64 = 160 << 20;

/// Contents made and checked at once, about: their bytes.
const BATCH: usize = 64 << 20;

/// A row of `FullData` of all its blobs' frames taken: its key and its frames.
struct Row {
    key: [i64; 3],
    frames: [usize; 8],
    /// Its contents' places: the first and the last; and its contents' bytes.
    first: usize,
    last: usize,
    lens: [u64; 8],
}

impl Row {
    fn bytes(&self) -> u64 {
        self.lens.iter().sum()
    }
}

/// What became of a row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Role {
    #[default]
    Plain,
    /// Its blobs kept as they come, for its parent's quadrant.
    Observed,
    /// Made of its quadrants (each of a child's row, or stored); its own quadrant kept where its
    /// parent is made of it.
    Derived { sources: [Option<usize>; 4], child: bool },
}

/// The contents of the frames `order` of `input`, decompressed on threads.
fn contents(input: &[u8], frames: &[Vec<Range<usize>>], order: &[usize], threads: usize) -> Vec<Vec<u8>> {
    const GROUP: usize = 64;
    let groups = aqr_common::pool::par_map(order.len().div_ceil(GROUP), threads, |g| {
        let mut dec = zstd::bulk::Decompressor::new().ok();
        let mut frame = Vec::new();
        order[g * GROUP..(g * GROUP + GROUP).min(order.len())]
            .iter()
            .map(|&i| {
                frame.clear();
                frames[i].iter().for_each(|r| frame.extend_from_slice(&input[r.clone()]));
                dec.as_mut()
                    .and_then(|d| d.decompress(&frame, aqr_zstd::compress::CONTENT_MAX).ok())
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
    });
    groups.into_iter().flatten().collect()
}

/// What the analysis of a row tells: its section's points and longest column, its quadrant's
/// bytes; of a parent, whether it is made again exactly, and its points before culling.
#[derive(Debug, Clone, Copy, Default)]
struct Facts {
    points: usize,
    column: usize,
    quadrant: usize,
    quadrant_points: usize,
    exact: bool,
    merged: usize,
}

/// The buffers of a worker's analysis.
#[derive(Default)]
struct Bufs {
    w: Work,
    kid: Section,
    quads: [Section; 4],
    parent: Section,
    blobs: [Vec<u8>; 8],
    bytes: Vec<u8>,
}

/// The formula of the Distant Horizons database `input` of the blobs `blobs` (aqr-sqlite's), or
/// None where it has no parent to make of its children.
pub fn encode(input: &[u8], blobs: &[SqlBlob], threads: usize) -> Option<Formula> {
    let frames: Vec<Vec<Range<usize>>> = blobs.iter().map(|b| b.pieces.clone()).collect();
    // the rows of all eight blobs, by their keys
    let mut by_key: HashMap<[i64; 3], [Option<usize>; 8]> = HashMap::new();
    for (i, b) in blobs.iter().enumerate() {
        if let Some(k) = COLUMNS.iter().position(|&c| c == b.column) {
            let e = by_key.entry(b.key).or_default();
            if e[k].replace(i).is_some() {
                by_key.get_mut(&b.key).unwrap()[k] = None;
            }
        }
    }
    let child = |p: [i64; 3], q: usize| [p[0] - 1, 2 * p[1] + (q / 2) as i64, 2 * p[2] + (q % 2) as i64];
    let any = by_key.keys().any(|&p| p[0] > 0 && (0..4).all(|q| by_key.contains_key(&child(p, q))));
    if !any {
        return None;
    }
    aqr_zstd::formula::frames_in_by(input, &frames, 3, threads, &mut |taken, f| {
        lods(input, &frames, &by_key, taken, threads, f)
    })
}

fn lods(
    input: &[u8],
    frames: &[Vec<Range<usize>>],
    by_key: &HashMap<[i64; 3], [Option<usize>; 8]>,
    taken: &aqr_zstd::formula::Taken,
    threads: usize,
    f: &mut Formula,
) -> Option<Node> {
    let n = taken.order.len();
    let mut place = vec![usize::MAX; frames.len()];
    for (i, &fr) in taken.order.iter().enumerate() {
        place[fr] = i;
    }
    // the rows of all eight taken
    let mut rows: Vec<Row> = Vec::new();
    for (&key, fr) in by_key {
        let Some(fr) = fr.iter().map(|&x| x.filter(|&x| place[x] != usize::MAX)).collect::<Option<Vec<_>>>() else {
            continue;
        };
        let fr: [usize; 8] = fr.try_into().unwrap();
        let first = fr.iter().map(|&x| place[x]).min().unwrap();
        let last = fr.iter().map(|&x| place[x]).max().unwrap();
        let lens = fr.map(|x| taken.lens[place[x]]);
        rows.push(Row { key, frames: fr, first, last, lens });
    }
    rows.sort_by_key(|r| r.first);
    let index: HashMap<[i64; 3], usize> = rows.iter().enumerate().map(|(i, r)| (r.key, i)).collect();
    let child = |p: [i64; 3], q: usize| [p[0] - 1, 2 * p[1] + (q / 2) as i64, 2 * p[2] + (q % 2) as i64];
    let kids: Vec<Option<[usize; 4]>> = rows
        .iter()
        .map(|r| match r.key[0] > 0 {
            true => (0..4)
                .map(|q| index.get(&child(r.key, q)).copied())
                .collect::<Option<Vec<_>>>()
                .map(|v| v.try_into().unwrap()),
            false => None,
        })
        .collect();
    // the states of every mapping
    let content_of = |r: &Row| contents(input, frames, &r.frames, 1);
    let mut states = States::default();
    {
        // (each group's states, each once: not the mappings, which are some 2-5% of the contents)
        const GROUP: usize = 256;
        let groups = aqr_common::pool::par_map(rows.len().div_ceil(GROUP), threads, |g| {
            let mut own = States::default();
            let ok = rows[g * GROUP..(g * GROUP + GROUP).min(rows.len())].iter().all(|r| {
                let m = contents(input, frames, &[r.frames[7]], 1).pop().unwrap_or_default();
                dh::mapping(&m)
                    .map(|rs| {
                        rs.into_iter().for_each(|x| {
                            own.id(&m[x]);
                        })
                    })
                    .is_ok()
            });
            ok.then_some(own)
        });
        for own in groups {
            let own = own?;
            for i in 0..own.len() as u32 {
                states.id(own.get(i));
            }
        }
    }
    let section = |r: &Row, b: &mut Bufs, states: &States| -> Option<Vec<Vec<u8>>> {
        let c = content_of(r);
        let s: [&[u8]; 8] = std::array::from_fn(|k| &c[k][..]);
        dh::section_of(&s, states, &mut b.w, &mut b.kid).ok()?;
        Some(c)
    };
    // the opaque states: of the parents' merged columns, the points not in the parents' columns
    let parents: Vec<usize> = (0..rows.len()).filter(|&i| kids[i].is_some()).collect();
    let culled: Vec<Vec<u32>> = aqr_common::pool::par_map(parents.len().div_ceil(16), threads, |g| {
        let mut b = Bufs::default();
        let mut out = Vec::new();
        for &p in &parents[g * 16..(g * 16 + 16).min(parents.len())] {
            for (q, &c) in kids[p].unwrap().iter().enumerate() {
                if section(&rows[c], &mut b, &states).is_none()
                    || dh::quadrant(&b.kid, &states, &mut b.w, &mut b.quads[q]).is_err()
                {
                    break;
                }
            }
            if section(&rows[p], &mut b, &states).is_none() {
                continue;
            }
            for x in 0..dh::W {
                for z in 0..dh::W {
                    let col = b.kid.grid.col(x * dh::W + z);
                    for pt in b.quads[x / 32 * 2 + z / 32].grid.col((x % 32) * 32 + z % 32) {
                        let top = pt.bottom + pt.height as i32;
                        if pt.id != u32::MAX && !col.iter().any(|q| q.bottom + q.height as i32 == top) {
                            out.push(pt.id);
                        }
                    }
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    });
    for &c in culled.iter().flatten() {
        states.opaque[c as usize] = true;
    }
    // each row's facts: its section's, its quadrant's, and of a parent whether it is made again
    let facts: Vec<Facts> = aqr_common::pool::par_map(rows.len().div_ceil(16), threads, |g| {
        let mut b = Bufs::default();
        (g * 16..(g * 16 + 16).min(rows.len()))
            .map(|i| {
                let mut fa = Facts::default();
                let Some(actual) = section(&rows[i], &mut b, &states) else { return fa };
                fa.points = b.kid.grid.written();
                fa.column = (0..dh::COLUMNS).map(|c| b.kid.grid.col(c).len()).max().unwrap_or(0);
                if dh::quadrant(&b.kid, &states, &mut b.w, &mut b.quads[0]).is_ok() {
                    b.bytes.clear();
                    if dh::quadrant_bytes(&b.quads[0], &states, &mut b.w, &mut b.bytes) {
                        fa.quadrant = b.bytes.len();
                        fa.quadrant_points = b.quads[0].grid.points();
                        let q = &b.quads[0].grid;
                        fa.column = fa.column.max((0..q.columns()).map(|c| q.col(c).len()).max().unwrap_or(0));
                    }
                }
                if let Some(k) = kids[i] {
                    let mut ok = true;
                    for (q, &c) in k.iter().enumerate() {
                        ok &= section(&rows[c], &mut b, &states).is_some()
                            && dh::quadrant(&b.kid, &states, &mut b.w, &mut b.quads[q]).is_ok();
                    }
                    if ok {
                        fa.merged = b.quads.iter().map(|q| q.grid.points()).sum();
                        let [q0, q1, q2, q3] = &b.quads;
                        fa.exact = dh::parent([[q0, q1], [q2, q3]], &states, &mut b.parent).is_ok()
                            && dh::blobs(&b.parent, &states, &mut b.w, &mut b.blobs)
                            && (0..8).all(|k| b.blobs[k] == actual[k]);
                    }
                }
                fa
            })
            .collect::<Vec<_>>()
    })
    .into_iter()
    .flatten()
    .collect();
    // the roles: each exact parent made at its first content, of the children done before it
    let mut roles = vec![Role::Plain; rows.len()];
    let mut parent_of = vec![None::<usize>; rows.len()];
    for &p in &parents {
        for &c in &kids[p].unwrap() {
            parent_of[c] = Some(p);
        }
    }
    // (the parents in the order they are made, so that a child made before is known)
    let mut by_first: Vec<usize> = parents.iter().copied().filter(|&p| facts[p].exact).collect();
    by_first.sort_by_key(|&p| rows[p].first);
    for &p in &by_first {
        let mut sources = [None; 4];
        for (q, &c) in kids[p].unwrap().iter().enumerate() {
            let ready = match roles[c] {
                Role::Derived { .. } => rows[c].first < rows[p].first,
                _ => rows[c].last < rows[p].first,
            };
            if ready && facts[c].quadrant > 0 {
                sources[q] = Some(c);
            }
        }
        if sources.iter().all(|s| s.is_none()) {
            continue;
        }
        for c in sources.iter().flatten() {
            match &mut roles[*c] {
                Role::Derived { child, .. } => *child = true,
                r => *r = Role::Observed,
            }
        }
        roles[p] = Role::Derived { sources, child: false };
    }
    // what the pool holds, within HELD: the children kept longest let go first (their quadrants
    // stored instead), and parents whose blobs wait longest made no more
    let mut budget = HELD;
    let sim = loop {
        settle(&rows, &facts, &mut roles, &parent_of, budget);
        let s = simulate(&rows, &facts, &roles, taken, &place);
        if s.most <= POOL || budget < 1 << 20 {
            break s;
        }
        budget /= 2;
    };
    if !roles.iter().any(|r| matches!(r, Role::Derived { .. })) {
        return None;
    }
    // the steps and the stored bytes, in the order of the contents
    let mut row_of = vec![usize::MAX; frames.len()];
    let mut blob_of = vec![0u8; frames.len()];
    for (i, r) in rows.iter().enumerate() {
        for (k, &fr) in r.frames.iter().enumerate() {
            row_of[fr] = i;
            blob_of[fr] = k as u8;
        }
    }
    let mut steps = Vec::new();
    let mut stored = aqr_formula::store::Writer::new(None);
    let mut started = vec![false; rows.len()];
    let mut got = vec![0u8; rows.len()];
    let put = aqr_common::varint::put;
    let mut at = 0;
    while at < n {
        let mut end = at;
        let mut bytes = 0u64;
        while end < n && bytes < BATCH as u64 {
            bytes += taken.lens[end];
            end += 1;
        }
        let batch = &taken.order[at..end];
        let made = contents(input, frames, batch, threads);
        // the quadrants stored of the parents made in the batch
        let derive_here: Vec<(usize, usize)> = batch
            .iter()
            .filter_map(|&fr| {
                let r = row_of[fr];
                (r != usize::MAX && place[fr] == rows[r].first).then_some(r)
            })
            .filter_map(|r| match roles[r] {
                Role::Derived { sources, .. } => {
                    Some(sources.iter().enumerate().filter(|s| s.1.is_none()).map(|(q, _)| (r, q)).collect::<Vec<_>>())
                }
                _ => None,
            })
            .flatten()
            .collect();
        let quads: Vec<Option<Vec<u8>>> = aqr_common::pool::par_map(derive_here.len(), threads, |j| {
            let (p, q) = derive_here[j];
            let c = kids[p].unwrap()[q];
            let mut b = Bufs::default();
            section(&rows[c], &mut b, &states)?;
            dh::quadrant(&b.kid, &states, &mut b.w, &mut b.quads[0]).ok()?;
            let mut out = Vec::new();
            dh::quadrant_bytes(&b.quads[0], &states, &mut b.w, &mut out).then_some(out)
        });
        let mut quads: HashMap<(usize, usize), Vec<u8>> =
            derive_here.into_iter().zip(quads).map(|(k, v)| v.map(|v| (k, v))).collect::<Option<_>>()?;
        for (j, &fr) in batch.iter().enumerate() {
            let r = row_of[fr];
            let k = blob_of[fr] as u64;
            match if r != usize::MAX { roles[r] } else { Role::Plain } {
                Role::Plain => {
                    put(&mut steps, (made[j].len() as u64) << SHIFT | PLAIN);
                    stored.write(&made[j]);
                }
                Role::Observed => {
                    put(&mut steps, (made[j].len() as u64) << SHIFT | OBSERVED);
                    put(&mut steps, r as u64);
                    put(&mut steps, k);
                    got[r] |= 1 << k;
                    if got[r] == 0xFF {
                        put(&mut steps, facts[r].quadrant as u64);
                    }
                    stored.write(&made[j]);
                }
                Role::Derived { sources, child } => {
                    put(&mut steps, DERIVED);
                    put(&mut steps, r as u64);
                    put(&mut steps, k);
                    if !std::mem::replace(&mut started[r], true) {
                        for (q, s) in sources.iter().enumerate() {
                            put(&mut steps, s.map_or(0, |c| c as u64 + 1));
                            if s.is_none() {
                                let b = quads.remove(&(r, q))?;
                                let mut v = Vec::new();
                                put(&mut v, b.len() as u64);
                                stored.write(&v);
                                stored.write(&b);
                            }
                        }
                        put(&mut steps, child as u64);
                    }
                }
            }
        }
        at = end;
    }
    // the opaque states' list
    let mut list = Vec::new();
    let opaque: Vec<u32> = (0..states.len() as u32).filter(|&i| states.opaque[i as usize]).collect();
    list.extend((opaque.len() as i32).to_be_bytes());
    for &id in &opaque {
        let b = states.get(id);
        list.extend((b.len() as u16).to_be_bytes());
        list.extend_from_slice(b);
    }
    let facts_of = |i: usize| (facts[i].points.max(facts[i].merged), facts[i].column);
    let points = (0..rows.len()).map(|i| facts_of(i).0).max().unwrap_or(0);
    let column = (0..rows.len()).map(|i| facts_of(i).1).max().unwrap_or(1).max(1);
    let quadrant = facts.iter().map(|f| f.quadrant).max().unwrap_or(0);
    let params = Params {
        rows: rows.len() as u64,
        states: states.len() as u64,
        state_bytes: states.bytes.len() as u64,
        points: points as u64,
        column: column as u64,
        quadrant: quadrant as u64,
        pool: sim.most,
    };
    let len: u64 = taken.lens.iter().sum();
    let steps = f.push_as("dh/steps", Kind::Table, steps);
    let stored = f.push_as("dh/stored", Kind::Table, stored);
    let list = f.push_as("dh/opaque", Kind::Table, list);
    Some(Node::apply(MODULE, params.bytes(), vec![steps, stored, list], len))
}

/// A span of the contents' places over which the pool holds `bytes` for a row: of a child kept
/// (from its first blob until its parent is made) or of a parent made (until its last blob).
struct Held {
    from: usize,
    to: usize,
    bytes: u64,
    row: usize,
    parent: bool,
}

/// The roles lowered until what the pool holds at once is `budget` at most: where more would be,
/// the span ending last let go (a child kept no more, its quadrant stored; a parent made no more).
/// Letting go lowers roles only, so spans only go: one sweep, those of the rows lowered dropped as
/// they are.
fn settle(rows: &[Row], facts: &[Facts], roles: &mut [Role], parent_of: &[Option<usize>], budget: u64) {
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;
    let mut spans: Vec<Held> = spans(rows, facts, roles, parent_of);
    spans.sort_by_key(|s| s.from);
    let mut of_row: Vec<Vec<usize>> = vec![Vec::new(); rows.len()];
    for (i, s) in spans.iter().enumerate() {
        of_row[s.row].push(i);
    }
    // (whether each span is still held: none yet, held, or gone)
    let mut state = vec![0u8; spans.len()];
    let mut active: BinaryHeap<(usize, usize)> = BinaryHeap::new();
    let mut ending: BinaryHeap<Reverse<(usize, usize)>> = BinaryHeap::new();
    let mut held = 0u64;
    let mut changed = Vec::new();
    for i in 0..spans.len() {
        let from = spans[i].from;
        while let Some(&Reverse((to, j))) = ending.peek() {
            if to > from {
                break;
            }
            ending.pop();
            if state[j] == 1 {
                held -= spans[j].bytes;
            }
            state[j] = 2;
        }
        if state[i] == 2 {
            continue;
        }
        state[i] = 1;
        held += spans[i].bytes;
        active.push((spans[i].to, i));
        ending.push(Reverse((spans[i].to, i)));
        while held > budget {
            let Some((_, j)) = active.pop() else { break };
            if state[j] != 1 {
                continue;
            }
            changed.clear();
            match spans[j].parent {
                true => unmake(roles, parent_of, spans[j].row, &mut changed),
                false => let_go(roles, parent_of, spans[j].row, &mut changed),
            }
            changed.push(spans[j].row);
            // (the spans of the rows lowered that their roles hold no more)
            for &r in &changed {
                for &k in &of_row[r] {
                    let still = match roles[r] {
                        Role::Derived { child, .. } => spans[k].parent || child,
                        Role::Observed => !spans[k].parent,
                        Role::Plain => false,
                    };
                    if !still && state[k] != 2 {
                        if state[k] == 1 {
                            held -= spans[k].bytes;
                        }
                        state[k] = 2;
                    }
                }
            }
        }
    }
}

/// Child `c` kept for its parent no more: the parent takes its quadrant stored (and is made no
/// more where it has no quadrant of a child left). The rows whose roles change are told.
fn let_go(roles: &mut [Role], parent_of: &[Option<usize>], c: usize, changed: &mut Vec<usize>) {
    match &mut roles[c] {
        Role::Derived { child, .. } => *child = false,
        r => *r = Role::Plain,
    }
    changed.push(c);
    let Some(p) = parent_of[c] else { return };
    let Role::Derived { sources, .. } = &mut roles[p] else { return };
    for q in sources.iter_mut() {
        if *q == Some(c) {
            *q = None;
        }
    }
    if sources.iter().all(|q| q.is_none()) {
        unmake(roles, parent_of, p, changed);
    }
}

/// Parent `p` made no more: its children kept for it let go, and its own parent takes its
/// quadrant stored.
fn unmake(roles: &mut [Role], parent_of: &[Option<usize>], p: usize, changed: &mut Vec<usize>) {
    let Role::Derived { sources, child } = roles[p] else { return };
    roles[p] = Role::Plain;
    changed.push(p);
    for c in sources.into_iter().flatten() {
        match &mut roles[c] {
            Role::Derived { child, .. } => *child = false,
            r => *r = Role::Plain,
        }
        changed.push(c);
    }
    if child {
        let_go(roles, parent_of, p, changed);
    }
}

/// The spans of what the pool holds, of the roles.
fn spans(rows: &[Row], facts: &[Facts], roles: &[Role], parent_of: &[Option<usize>]) -> Vec<Held> {
    let mut v = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        let made_at = parent_of[i].map(|p| rows[p].first);
        match roles[i] {
            Role::Observed => {
                if let Some(to) = made_at {
                    let bytes = r.bytes().max(facts[i].quadrant as u64);
                    v.push(Held { from: r.first, to, bytes, row: i, parent: false });
                }
            }
            Role::Derived { child, .. } => {
                v.push(Held { from: r.first, to: r.last, bytes: r.bytes(), row: i, parent: true });
                if child && let Some(to) = made_at {
                    v.push(Held { from: r.first, to, bytes: facts[i].quadrant as u64, row: i, parent: false });
                }
            }
            Role::Plain => {}
        }
    }
    v
}

/// The pool as the module takes and gives its blocks (`crate::lods::build`), of the roles, in the
/// order of the contents: its most is the params' pool. Each kept quadrant's block is of the
/// bytes of its row's quadrant (`facts`).
fn simulate(rows: &[Row], facts: &[Facts], roles: &[Role], taken: &aqr_zstd::formula::Taken, place: &[usize]) -> Pool {
    let mut pool = Pool::new(u64::MAX);
    let mut row_of = HashMap::new();
    for (i, r) in rows.iter().enumerate() {
        for (k, &fr) in r.frames.iter().enumerate() {
            row_of.insert(fr, (i, k));
        }
    }
    let mut blobs = vec![[(0u64, 0u64); 8]; rows.len()];
    let mut got = vec![0u8; rows.len()];
    let mut quad = vec![None::<u64>; rows.len()];
    let mut started = vec![false; rows.len()];
    let take = |pool: &mut Pool, n: u64| pool.take(n).unwrap_or(0);
    for (at, &fr) in taken.order.iter().enumerate() {
        debug_assert_eq!(place[fr], at);
        let Some(&(r, k)) = row_of.get(&fr) else { continue };
        match roles[r] {
            Role::Plain => {}
            Role::Observed => {
                let n = rows[r].lens[k];
                blobs[r][k] = (take(&mut pool, n), n);
                got[r] |= 1 << k;
                if got[r] == 0xFF {
                    quad[r] = Some(take(&mut pool, facts[r].quadrant as u64));
                    for b in blobs[r] {
                        pool.give(b.0, b.1);
                    }
                }
            }
            Role::Derived { sources, child } => {
                if !std::mem::replace(&mut started[r], true) {
                    for c in sources.into_iter().flatten() {
                        if let Some(b) = quad[c].take() {
                            pool.give(b, facts[c].quadrant as u64);
                        }
                    }
                    for (b, &n) in blobs[r].iter_mut().zip(&rows[r].lens) {
                        *b = (take(&mut pool, n), n);
                    }
                    if child {
                        quad[r] = Some(take(&mut pool, facts[r].quadrant as u64));
                    }
                }
                pool.give(blobs[r][k].0, blobs[r][k].1);
            }
        }
    }
    pool
}
