//! The tests of a table as a directory of a target of length `l`: offsets whose rooms its
//! sizes fill (explicit), or sizes that laid end to end end at the target's end (implicit).

use std::collections::HashSet;

use crate::fields::{Field, Profile};
use crate::tables::{MINROWS, Table};

/// Bytes a member's room may exceed its size by (padding).
pub const SLACK: u64 = 4096;
/// Bits of evidence a directory needs: about 2^24 offsets times 2^14 sizes are tried for a
/// table by brute force, fewer by the profile; 48 leave a margin.
pub const EVIDENCE: f64 = 48.0;
/// Edge of a table's end the sums are tried in, in rows before (after: 4 KiB).
const EDGE: usize = 8;
/// Bytes before a table its sizes laid end to end may start at: twice the table's (it may be
/// a fragment), within these.
const BACK: (usize, usize) = (4096, 1 << 16);

#[derive(Clone, Debug)]
pub enum Layout {
    Explicit { off: Field, size: Field, alt: Option<Field>, fit: f64, ctrl: f64 },
    Implicit { size: Field, align: u64, base: u64 },
}

#[derive(Clone, Debug)]
pub struct Cand {
    pub layout: Layout,
    /// The rows that are the directory (an implicit one's may differ from the table found).
    pub table: Table,
    pub ev: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Member {
    pub offset: u64,
    pub size: u64,
    pub row: i64,
}

pub(crate) fn even(n: usize, m: usize) -> Vec<i64> {
    if n <= m { (0..n as i64).collect() } else { (0..m).map(|i| (i * (n - 1) / (m - 1)) as i64).collect() }
}

/// A size fits a member's room where it fills it up to the slack. A size of 0 fits only an
/// empty room (a member shared with the next): else any small room takes it, and a field of
/// zeros "fits" a table's rows of its neighbour.
pub(crate) fn fits(room: u64, s: u64) -> bool {
    room >= s && room - s < SLACK && (s > 0 || room == 0)
}

fn fit_of(rooms: &[u64], sizes: &[u64]) -> f64 {
    rooms.iter().zip(sizes).filter(|(r, s)| fits(**r, **s)).count() as f64 / rooms.len().max(1) as f64
}

/// The slack the members show: the greatest gap of a member fitting its room, rounded up to a
/// power of two.
fn rooms_slack(rooms: &[u64], sizes: &[u64]) -> u64 {
    let gmax = rooms.iter().zip(sizes).filter(|(r, s)| fits(**r, **s)).map(|(r, s)| r - s).max().unwrap_or(0);
    (gmax + 1).next_power_of_two()
}

/// Bits against chance: each member fitting its room to within the slack G seen (a size in
/// [0, room] lands there with chance G / room), less 8 bits for each that does not.
fn room_bits(rooms: &[u64], sizes: &[u64]) -> f64 {
    let gmax = rooms.iter().zip(sizes).filter(|(r, s)| fits(**r, **s)).map(|(r, s)| r - s).max();
    let Some(gmax) = gmax else { return 0.0 };
    let g = (gmax + 1).next_power_of_two() as f64;
    let mut bits = 0.0;
    for (r, s) in rooms.iter().zip(sizes) {
        if fits(*r, *s) {
            bits += ((*r as f64 + 1.0).max(g) / g).log2();
        } else {
            bits -= 8.0;
        }
    }
    bits
}

/// The fits of the members as a binomial against the control's rate (sizes concentrated on a
/// few values fit wrong members too).
fn binomial_bits(n: usize, fit: f64, ctrl: f64) -> f64 {
    let p1 = fit.min(1.0 - 1e-9);
    let p0 = ctrl.max(1.0 / n as f64).min(1.0 - 1e-9);
    if p1 <= p0 {
        return 0.0;
    }
    n as f64 * (p1 * (p1 / p0).log2() + (1.0 - p1) * ((1.0 - p1) / (1.0 - p0)).log2())
}

/// [`rooms`] of offsets whose first `edge` (0 or 1: a row before the table) are no others'
/// bound: such a row's member only below all the others', its room up to the lowest.
fn edge_rooms(offs: &[u64], edge: usize, l: u64) -> Vec<Option<u64>> {
    let mut rm = rooms(&offs[edge..], l);
    if edge > 0 {
        let lo = offs[edge..].iter().copied().filter(|&o| o < l).min();
        let r = lo.filter(|&lo| offs[0] < lo).map(|lo| lo - offs[0]);
        rm.insert(0, r);
    }
    rm
}

/// Each row's room: up to the next distinct offset (None for the last and those out of range).
pub(crate) fn rooms(offs: &[u64], l: u64) -> Vec<Option<u64>> {
    let mut u: Vec<u64> = offs.iter().copied().filter(|&o| o < l).collect();
    u.sort_unstable();
    u.dedup();
    offs.iter()
        .map(|&o| {
            if o >= l {
                return None;
            }
            let i = u.partition_point(|&x| x <= o);
            u.get(i).map(|&nx| nx - o)
        })
        .collect()
}

/// The fields that may be sizes of members filling a target of length `l`: their total,
/// estimated on 256 rows, between 0.3 and 3 times `l` (members fill half the target at least;
/// shared members are counted again), over the rows whose value may be a size or over the
/// rows after the last that may not (sizes laid end to end: a table found may begin with rows
/// of its neighbour). A table with none is no directory.
pub fn sizes(b: &[u8], t: &Table, cands: &[Field], l: u64) -> Vec<Field> {
    gate(b, t, cands, l, true)
}

/// Of `sizes`, those that laid end to end may fill the target: their total over the rows after
/// the last that may not be a size, estimated on 256 rows, between 0.3 and 3 times `l`.
pub fn run_sizes(b: &[u8], t: &Table, sizes: &[Field], l: u64) -> Vec<Field> {
    gate(b, t, sizes, l, false)
}

fn gate(b: &[u8], t: &Table, cands: &[Field], l: u64, any_rows: bool) -> Vec<Field> {
    let samp = even(t.rows(), 256);
    let n = t.rows();
    let near_l = |est: f64| est >= 0.3 * l as f64 && est <= 3.0 * l as f64;
    cands
        .iter()
        .copied()
        .filter(|f| {
            let v: Vec<u64> = samp.iter().map(|&r| f.get(b, t, r)).collect();
            let ok = |x: u64| x > 0 && x <= l;
            let (s, m) = v.iter().filter(|&&x| ok(x)).fold((0.0, 0usize), |(s, m), &x| (s + x as f64, m + 1));
            if any_rows && m * 4 >= v.len() && near_l(s / v.len() as f64 * n as f64) {
                return true;
            }
            // the run of sizes ending near the table's end (a few rows after it may be data)
            let skip = v.iter().rev().take(8).take_while(|&&x| !ok(x)).count();
            let end = v.len() - skip;
            let tail = v[..end].iter().rev().take_while(|&&x| ok(x)).count();
            if tail < 4 {
                return false;
            }
            let (from, to) = (samp[end - tail] as usize, if skip > 0 { samp[end] as usize } else { n });
            let mean = v[end - tail..end].iter().sum::<u64>() as f64 / tail as f64;
            near_l(mean * (to - from) as f64)
        })
        .collect()
}

/// Offset fields of the table into a target of length `l` whose rooms its sizes fill, the
/// sizes among `sizes`.
pub fn explicit(b: &[u8], t: &Table, prof: &Profile, l: u64, sizes: &[Field]) -> Vec<Cand> {
    let n = t.rows();
    if n < MINROWS || sizes.is_empty() {
        return Vec::new();
    }
    let k = t.k as i64;
    let samp = even(n, 512);
    let cands = prof.candidates(l);
    // offsets: in range, distinct (a tenth of the rows: members may be shared, CREATURES has
    // 211 offsets in 662 rows), spread over the
    // target, and the two bits above the field clear (else the field is wider and this is its
    // lower part: aliased offsets tile too, scaled as their sizes)
    let mut offs: Vec<(u64, Field)> = Vec::new();
    let mut seen = HashSet::new();
    for f in cands.iter().filter(|f| f.pos >= 0 && f.pos < k) {
        let sh: Vec<u64> = samp.iter().map(|&r| f.shifted(b, t, r)).collect();
        let v: Vec<u64> = sh.iter().map(|&x| x & crate::fields::mask(f.bits)).collect();
        let clear = f.be || f.shift as u32 + f.bits as u32 >= 62 || {
            sh.iter().filter(|&&x| (x >> f.bits) & 3 == 0).count() * 10 >= sh.len() * 9
        };
        if !clear {
            continue;
        }
        let mut sorted = v.clone();
        sorted.sort_unstable();
        let distinct = 1 + sorted.windows(2).filter(|w| w[0] != w[1]).count();
        if distinct * 10 < v.len() || distinct < MINROWS {
            continue;
        }
        let inr = v.iter().filter(|&&x| x < l).count();
        if inr * 10 < v.len() * 9 || *sorted.last().unwrap() < l / 8 {
            continue;
        }
        if seen.insert(v.clone()) {
            let max = v.iter().copied().filter(|&x| x < l).max().unwrap_or(0);
            offs.push((l.abs_diff(max), *f));
        }
    }
    // first those whose last member (at the greatest offset, over all rows) ends at the end of
    // the target by a size near it, then those whose last member or greatest offset come
    // nearest the end
    // the rows tested: the table's and the one before it, as its start found may be late by
    // less than a record (its first row's fields before it: an index of 16-byte rows after a
    // 16-byte header found from byte 27)
    let before = (t.start >= t.k) as i64;
    let all: Vec<i64> = (-before..n as i64).collect();
    // (the greatest offset of a big table by 16384 rows and its last 256: a sorted table's is
    // at its end; ranked over all of the rows, the offsets of a table of 490 thousand rows of
    // 4 bytes, of floats, took 100 ms)
    let looked: Vec<i64> = if all.len() <= 1 << 14 {
        all.clone()
    } else {
        let mut v: Vec<i64> = even(n, 1 << 14);
        v.extend(n as i64 - 256..n as i64);
        v.sort_unstable();
        v.dedup();
        v
    };
    let mut ranked: Vec<(bool, u64, Field)> = offs
        .iter()
        .map(|&(d, off)| {
            let (mut om, mut rm_) = (0u64, 0i64);
            for &r in &looked {
                let v = off.get(b, t, r);
                if v < l && v >= om {
                    (om, rm_) = (v, r);
                }
            }
            // how far from the end of the target the last member ends, by the best size near
            // (a decoded target may end in padding: CREATURES' last chunk 52 KB past its last
            // member)
            let gap = sizes
                .iter()
                .filter(|f| (f.pos - off.pos).abs() < k)
                .filter_map(|f| {
                    let e = om.saturating_add(f.get(b, t, rm_));
                    (e <= l).then(|| l - e)
                })
                .min()
                .unwrap_or(u64::MAX);
            (gap >= SLACK, gap.min(d), off)
        })
        .collect();
    ranked.sort_by_key(|x| (x.0, x.1));
    let mut out = Vec::new();
    // up to 64 where the table is small (a candidate costs a sort of its rows), fewer where it
    // is big, 8 at least (a table of 490 thousand rows took 600 ms for 32; the directories seen
    // of over 50 thousand rows came within the first 16)
    let take = (2_000_000 / n.max(1)).clamp(8, 64);
    for &(_, _, off) in ranked.iter().take(take) {
        let o = off.values(b, t, &all);
        let rm = edge_rooms(&o, before as usize, l);
        let members: Vec<(i64, u64)> = all.iter().zip(&rm).filter_map(|(&r, x)| x.map(|x| (r, x))).collect();
        if members.len() < MINROWS {
            continue;
        }
        // sizes within a record of the offset: a quick look on 48 members first
        let near: Vec<Field> = sizes.iter().copied().filter(|f| (f.pos - off.pos).abs() < k).collect();
        if near.is_empty() {
            continue;
        }
        let pick = |m: usize| -> (Vec<i64>, Vec<u64>) {
            let idx = even(members.len(), m);
            (idx.iter().map(|&i| members[i as usize].0).collect(), idx.iter().map(|&i| members[i as usize].1).collect())
        };
        let (qr, qroom) = pick(48);
        if !near.iter().any(|f| fit_of(&qroom, &f.values(b, t, &qr)) >= 0.5) {
            continue;
        }
        let (mr, mroom) = pick(512);
        let mut scored: Vec<(f64, Field)> = near.iter().map(|f| (fit_of(&mroom, &f.values(b, t, &mr)), *f)).collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
        let (mut fit, mut size) = scored[0];
        if fit < 0.3 {
            continue;
        }
        // widen as far as the fit holds (the sample may not reach the field's top bits)
        if !size.be {
            while (size.shift as u32 + size.bits as u32) < 64 {
                let g = size.with_bits(size.bits + 1);
                let fg = fit_of(&mroom, &g.values(b, t, &mr));
                if fg < fit {
                    break;
                }
                (fit, size) = (fg, g);
            }
        }
        let s = size.values(b, t, &mr);
        let miss: Vec<bool> = mroom.iter().zip(&s).map(|(r, x)| !fits(*r, *x)).collect();
        // members the first size misses, taken by a second (stored members whose
        // compressed size is 0, say)
        let mut alt = None;
        let mut used = s.clone();
        if miss.iter().any(|&x| x) {
            for &(_, f2) in scored.iter().skip(1).take(8) {
                let s2 = f2.values(b, t, &mr);
                let (mut hit, mut tot) = (0, 0);
                for i in 0..s.len() {
                    if miss[i] {
                        tot += 1;
                        hit += fits(mroom[i], s2[i]) as usize;
                    }
                }
                if hit * 10 > tot * 8 {
                    alt = Some(f2);
                    for i in 0..s.len() {
                        if miss[i] {
                            used[i] = s2[i];
                        }
                    }
                    break;
                }
            }
        }
        // the control on the sample first (sizes of data that looks like a table, texture
        // blocks, fit wrong members as well), before the run of rows is looked for over all
        // the rows: in tables of 100 thousand rows and more, most of the cost
        let g = rooms_slack(&mroom, &used);
        let fit_g = |s: &[u64]| {
            mroom.iter().zip(s).filter(|(r, x)| fits(**r, **x) && **r - **x < g).count() as f64
                / mroom.len().max(1) as f64
        };
        let mut ctrl_s = used.clone();
        ctrl_s.rotate_right(1);
        if binomial_bits(mroom.len(), fit_g(&used), fit_g(&ctrl_s)) < EVIDENCE {
            continue;
        }
        // the rows that are the directory: the longest run of rows that fit (+1 each, -3 for
        // each that does not), as a table found may hold rows of its neighbours (a run of 3976
        // rows of AnimMBIN's is 3251 entries and the start of its chunks' sizes)
        let s_all = size.values(b, t, &all);
        let s2_all = alt.map(|a| a.values(b, t, &all));
        let size_in = |i: usize, room: u64| -> Option<u64> {
            if fits(room, s_all[i]) {
                Some(s_all[i])
            } else {
                s2_all.as_ref().filter(|x| fits(room, x[i])).map(|x| x[i])
            }
        };
        let (mut best, mut bi, mut bj, mut cur, mut ci) = (0i64, 0usize, 0usize, 0i64, 0usize);
        for (i, room) in rm.iter().enumerate() {
            let sc = match *room {
                Some(room) if size_in(i, room).is_some() => 1,
                Some(_) => -3,
                None => 0,
            };
            if cur <= 0 {
                cur = 0;
                ci = i;
            }
            cur += sc;
            if cur > best {
                (best, bi, bj) = (cur, ci, i + 1);
            }
        }
        if bj < all.len() && rm[bj].is_none() {
            bj += 1; // the last member (its room unknown) right after the run
        }
        // the rows just before the run again, by the rooms of the run's own offsets (rows of a
        // header before a table, their small values taken for offsets, split its first
        // member's room: a Wwise bank's first entry, of 1708 bytes, given 8)
        for _ in 0..4 {
            match bi.checked_sub(1).and_then(|p| rooms(&o[p..bj], l)[0].map(|room| (p, room))) {
                Some((p, room)) if size_in(p, room).is_some() => bi = p,
                _ => break,
            }
        }
        if bj - bi < MINROWS {
            continue;
        }
        let at = |i: usize| (t.start as i64 + (i as i64 - before) * k) as usize;
        let tt = Table { start: at(bi), end: at(bj), ..*t };
        // its rooms again, from its own offsets
        let rm2 = edge_rooms(&o[bi..bj], (before as usize).saturating_sub(bi), l);
        let mem2: Vec<(usize, u64)> = (0..bj - bi).filter_map(|i| rm2[i].map(|r| (bi + i, r))).collect();
        if mem2.len() < MINROWS {
            continue;
        }
        let idx = even(mem2.len(), 512);
        let mroom: Vec<u64> = idx.iter().map(|&i| mem2[i as usize].1).collect();
        let used: Vec<u64> = idx
            .iter()
            .map(|&i| {
                let (r, room) = mem2[i as usize];
                size_in(r, room).unwrap_or(s_all[r])
            })
            .collect();
        let fit = fit_of(&mroom, &used);
        // the control (sizes paired with the wrong members) within the slack the members show,
        // not SLACK: with members of a few KB, a wrong size is within 4 KiB of most rooms
        let g = rooms_slack(&mroom, &used);
        let fit_g = |s: &[u64]| {
            mroom.iter().zip(s).filter(|(r, x)| fits(**r, **x) && **r - **x < g).count() as f64
                / mroom.len().max(1) as f64
        };
        let mut ctrl_s = used.clone();
        ctrl_s.rotate_right(1);
        let ctrl = fit_g(&ctrl_s);
        let ev = room_bits(&mroom, &used).min(binomial_bits(mroom.len(), fit_g(&used), ctrl));
        // (and sizes fitting wrong members half the time are no evidence, however many: sizes
        // about as alike as their rooms, of data or of a table of another kind, Outer Wilds'
        // and Slime Rancher's Unity files at 0.60 and 0.41; NMS's decoded entries 0.36)
        if fit >= 0.8 && ev >= EVIDENCE && ctrl < 0.5 {
            out.push(Cand { layout: Layout::Explicit { off, size, alt, fit, ctrl }, table: tt, ev });
        }
    }
    out.sort_by(|a, b| b.ev.partial_cmp(&a.ev).unwrap());
    out
}

/// Size fields that, laid end to end from a base, end exactly at the end of the target.
///
/// The sum needs every row, and the table's ends may be off. Its end is tried from EDGE
/// rows before to 4 KiB after; its start is where the sizes start: after the last row before
/// the end whose value is no size (zero, or over `l`), looking back BACK bytes, and EDGE rows
/// after that. In the table's own file (`same`) the members start right after it: within its
/// last record, or aligned up from there to 16, 512, 2048 or 4096; in another target, within
/// 16 bytes of its start.
pub fn implicit(b: &[u8], t0: &Table, sizes: &[Field], l: u64, same: bool) -> Vec<Cand> {
    let k = t0.k;
    let back = t0.start.min((2 * (t0.end - t0.start)).clamp(BACK.0, BACK.1)) / k;
    let fwd = (4096 / k).min(b.len().saturating_sub(t0.end) / k);
    let t = Table { start: t0.start - back * k, end: t0.end + fwd * k, ..*t0 };
    let n = t.rows();
    if n < MINROWS {
        return Vec::new();
    }
    let lo1 = (n - fwd).saturating_sub(4 * EDGE).max(MINROWS);
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut g = vec![0u64; n];
    let mut lastbad = vec![-1i64; n + 1];
    let mut p1 = vec![0u64; n + 1];
    for &f in sizes {
        // the rows of the table and after it; those before it only where the table has no row
        // that is no size (else the sizes start after that row anyway)
        let mut bad_in = false;
        for (j, v) in g.iter_mut().enumerate().skip(back) {
            *v = f.get(b, &t, j as i64);
            bad_in |= j < lo1 && (*v == 0 || *v > l);
        }
        for (j, v) in g.iter_mut().enumerate().take(back) {
            *v = if bad_in { 0 } else { f.get(b, &t, j as i64) };
        }
        for j in 0..n {
            let bad = g[j] == 0 || g[j] > l;
            lastbad[j + 1] = if bad { j as i64 } else { lastbad[j] };
            p1[j + 1] = p1[j] + g[j].min(l + 1);
        }
        let mut pas: Vec<Option<Vec<u64>>> = vec![None; 13]; // by log2 of the alignment
        // the first row from j on that is no size
        let mut nextbad = vec![n; n + 1];
        for j in (0..n).rev() {
            nextbad[j] = if g[j] == 0 || g[j] > l { j } else { nextbad[j + 1] };
        }
        // the starts: after the last row that is no size before each end tried, and EDGE rows
        // after that
        let mut anchors: Vec<i64> = (lo1..=n).map(|i1| lastbad[i1]).collect();
        anchors.dedup();
        let field_end = |i1: usize| (t.start + (i1 - 1) * k) as i64 + f.end();
        for &anchor in &anchors {
            for d0 in 0..=EDGE {
                let i0 = (anchor + 1) as usize + d0;
                if i0 + MINROWS > n {
                    break;
                }
                // the ends: rows [i0, i1) all sizes, i1 in the edge tried
                let (lo, hi) = (lo1.max(i0 + MINROWS), n.min(nextbad[i0]));
                if lo > hi {
                    continue;
                }
                for al in (0..13).map(|e| 1u64 << e) {
                    let pa: &[u64] = if al == 1 {
                        &p1
                    } else {
                        // the padding a member needs on average at the longest end must be
                        // about what padding to al gives: (al - 1) / 2, give or take
                        // al / sqrt(12 cnt)
                        let cnt = (hi - i0) as f64;
                        let need = l as f64 - (p1[hi] - p1[i0]) as f64 - field_end(hi).max(0) as f64;
                        let tol = 4.0 * al as f64 / (12.0 * cnt).sqrt() + 1.0;
                        if need < -4096.0 || need / cnt > (al - 1) as f64 / 2.0 + tol + 4096.0 / cnt {
                            continue;
                        }
                        let e = al.trailing_zeros() as usize;
                        pas[e].get_or_insert_with(|| {
                            let mut pa = vec![0u64; n + 1];
                            for j in 0..n {
                                pa[j + 1] = pa[j] + g[j].min(l + 1).div_ceil(al) * al;
                            }
                            pa
                        })
                    };
                    // what is left of the target past the members, less where they may start:
                    // decreasing in i1 (sizes > 0, the field's end moves by k)
                    let left = |i1: usize| -> i64 {
                        let tot = (pa[i1] - pa[i0]) as i64;
                        let from = if same { field_end(i1) } else { 0 };
                        l as i64 - tot - from
                    };
                    let w = if same { 4096 + k as i64 } else { 16 };
                    if left(lo) < 0 || left(hi) > w {
                        continue;
                    }
                    // the least i1 with left(i1) <= w, then while left(i1) >= 0
                    let (mut a, mut z) = (lo, hi);
                    while a < z {
                        let m = (a + z) / 2;
                        if left(m) <= w { z = m } else { a = m + 1 }
                    }
                    let mut i1 = a;
                    while i1 <= hi && left(i1) >= 0 {
                        let tot = pa[i1] - pa[i0];
                        let base = l - tot;
                        let fe = field_end(i1);
                        let ok = if same {
                            let fe = fe as u64;
                            (base >= fe && base < fe + k as u64)
                                || [16u64, 512, 2048, 4096].iter().any(|&a| base == fe.div_ceil(a) * a)
                        } else {
                            base < 16
                        };
                        if ok {
                            let tt = Table { start: t.start + i0 * k, end: t.start + i1 * k, ..t };
                            let key: Vec<u64> = g[i0..i1].to_vec();
                            if seen.insert((key, tt.start, tt.end, al)) {
                                // the sum lands on one of about k + 4 bases by chance
                                let ev = (l as f64 / (k + 4) as f64).log2();
                                out.push(Cand { layout: Layout::Implicit { size: f, align: al, base }, table: tt, ev });
                            }
                        }
                        i1 += 1;
                    }
                }
            }
        }
    }
    // the longest runs first (a chance sum is mostly short)
    out.sort_by_key(|c| std::cmp::Reverse(c.table.rows()));
    out
}

/// The members a candidate gives in a target of length `l`.
pub fn members(b: &[u8], c: &Cand, l: u64) -> Vec<Member> {
    let t = &c.table;
    let n = t.rows() as i64;
    match &c.layout {
        Layout::Explicit { off, size, alt, .. } => {
            let rows: Vec<i64> = (0..n).collect();
            let o = off.values(b, t, &rows);
            let s = size.values(b, t, &rows);
            let s2 = alt.map(|a| a.values(b, t, &rows));
            let rm = rooms(&o, l);
            let mut out = Vec::new();
            for i in 0..n as usize {
                if o[i] >= l {
                    continue;
                }
                let room = rm[i].unwrap_or(l - o[i]);
                let size = if fits(room, s[i]) {
                    s[i]
                } else if let Some(x) = s2.as_ref().filter(|x| fits(room, x[i])) {
                    x[i]
                } else if rm[i].is_none() && o[i] + s[i] <= l {
                    s[i] // the last member: its room runs to the end of the target
                } else {
                    continue;
                };
                out.push(Member { offset: o[i], size, row: i as i64 });
            }
            out
        }
        Layout::Implicit { size, align, base } => {
            let mut at = *base;
            (0..n)
                .map(|r| {
                    let s = size.get(b, t, r);
                    let m = Member { offset: at, size: s, row: r };
                    at += s.div_ceil(*align) * align;
                    m
                })
                .collect()
        }
    }
}
