//! Entries with a length field: chains of entries, each holding a length v at offset d and the
//! next starting align_up(b + v, align) after it.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use crate::kt::{self, best_run};
pub use crate::module::{KINDS, Rule};
use crate::{MIN_RECORDS, zcost};

const ALIGNS: [usize; 3] = [1, 4, 8];
const MAX_B: usize = 64;

/// Where chains are looked for: spans over the file, the first at its start.
const SPAN: usize = 1 << 20;
const WINDOWS: usize = 8;
const STEPS: usize = 32;
/// The entries' leading bytes that score a chain.
const COLS: usize = 16;
/// Chains of a span are screened on 8 entries and 8 columns, these kept.
const SCREEN: usize = 512;
/// The next fields a chain is followed to from its first: the places most like it.
const NEXT_DELTAS: usize = 2;
/// A chain of fields is scored on FIELD_COLS bytes from FIELD_BACK before its fields (where
/// its entries' fixed parts are, the field being anywhere in them), screened on every 4th.
const FIELD_BACK: i64 = 64;
const FIELD_COLS: usize = 80;
/// The best of them are ranked on this many entries, and FOLLOW_KEEP followed to the end.
const FOLLOW_SAMPLE: usize = 4096;
const FOLLOW_KEEP: usize = 8;
/// The bytes of a chain's first entries its rotation and head are chosen on.
const SAMPLE: usize = 1 << 18;

/// A score, ordered.
#[derive(PartialEq)]
struct Score(f64);

impl Eq for Score {}

impl PartialOrd for Score {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}

impl Ord for Score {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&o.0)
    }
}

/// The value of a field kind at p, which is within data.
#[inline]
fn read(data: &[u8], p: usize, kind: usize) -> u32 {
    let (w, big, mask) = KINDS[kind];
    let v = match (w, big) {
        (1, _) => data[p] as u32,
        (2, false) => u16::from_le_bytes([data[p], data[p + 1]]) as u32,
        (2, true) => u16::from_be_bytes([data[p], data[p + 1]]) as u32,
        (_, false) => u32::from_le_bytes(data[p..p + 4].try_into().unwrap()),
        (_, true) => u32::from_be_bytes(data[p..p + 4].try_into().unwrap()),
    };
    v & mask
}

fn byte(data: &[u8], i: usize) -> u8 {
    *data.get(i).unwrap_or(&0)
}

/// How much more often consecutive entries starting at p agree in their leading ncols bytes,
/// column by column, than with bytes a multiple of 8 into the entry before: runs of zeros,
/// text or periodic data (code) agree either way, and score nothing.
pub fn score(data: &[u8], p: &[usize], ncols: usize) -> f64 {
    score_at(data, p, 0, ncols)
}

/// How many of the bytes around two fields (every 4th from FIELD_BACK before them) are equal.
fn like(span: &[u8], f0: usize, f1: usize) -> usize {
    (0..FIELD_COLS as i64)
        .step_by(4)
        .filter(|&j| {
            let (a, b) = (f0 as i64 - FIELD_BACK + j, f1 as i64 - FIELD_BACK + j);
            a >= 0 && byte(span, a as usize) == byte(span, b as usize)
        })
        .count()
}

/// [`score`] of the bytes from `back` before each of p.
fn score_at(data: &[u8], p: &[usize], back: i64, ncols: usize) -> f64 {
    score_cols(data, p, back, ncols, 1)
}

/// [`score`] of ncols bytes `stride` apart from `back` before each of p.
fn score_cols(data: &[u8], p: &[usize], back: i64, ncols: usize, stride: usize) -> f64 {
    if p.len() < 2 {
        return 0.0;
    }
    let at = |i: i64| if i < 0 { 0 } else { byte(data, i as usize) };
    let (mut same, mut ctrl) = (0usize, 0usize);
    for i in 1..p.len() {
        let l = ((p[i] - p[i - 1]) / 8).max(1);
        let u = 1 + ((i - 1) * 7919) % l;
        let (q, r, c) = (p[i] as i64 - back, p[i - 1] as i64 - back, (p[i - 1] + 8 * u) as i64 - back);
        for j in (0..(ncols * stride) as i64).step_by(stride) {
            let x = at(q + j);
            same += (x == at(r + j)) as usize;
            ctrl += (x == at(c + j)) as usize;
        }
    }
    (same as f64 - ctrl as f64) / ((p.len() - 1) * ncols) as f64
}

/// Entry starts from p0 for as long as the entries hold (at most limit of them), ending with
/// where the last ends.
pub fn follow(data: &[u8], p0: usize, r: &Rule, limit: Option<usize>) -> Vec<usize> {
    let n = data.len();
    let mut out = vec![p0];
    let mut p = p0;
    while p + r.d + r.width() <= n {
        let l = r.length(data, p);
        if l < r.d + r.width() || p + l > n {
            break;
        }
        p += l;
        out.push(p);
        if limit.is_some_and(|m| out.len() > m) {
            break;
        }
    }
    out
}

/// Chains whose entries hold together, with a length field within `fields` bytes of the
/// span at `at`, best first: (score, entry starts, rule).
///
/// A chain follows its length fields: from a field at f, the next is align_up(b + v, align)
/// after it, wherever in the entry the field is (an entry starting d before its field gives
/// the same chain, rotated). So every rule but d is followed from every field position for
/// STEPS entries, scored around the fields (FIELD_BACK bytes before them, FIELD_COLS in all); a
/// chain dies when it leaves the span, and chains of one length are fixed records, left to
/// the column scan. The best start at their fields (`rotate` settles where they start). They
/// are then ranked on their first entries, followed through
/// the whole file, and those that end with the same lengths are one (the one whose first
/// entries score best); the rest are ranked by their score times their entries.
fn find(data: &[u8], at: usize, fields: usize) -> Vec<(f64, Vec<usize>, Rule)> {
    let span = &data[at..(at + SPAN).min(data.len())];
    let n = span.len();
    let mut lanes: Vec<([usize; STEPS + 1], Rule, f64)> = Vec::new();
    // the screen scores of the chains kept so far, the least on top: a chain that scores below
    // the least of SCREEN of them after 8 entries would not be kept at the end either
    let mut screen: BinaryHeap<Reverse<Score>> = BinaryHeap::new();
    for f0 in 0..fields.min(n) {
        for (kind, &(w, _, _)) in KINDS.iter().enumerate() {
            let tab = |p: usize| read(span, p, kind);
            // no entry is shorter than its length
            if f0 + w > n || f0 + tab(f0) as usize > n {
                continue;
            }
            // where the next field may be: f0 + v0 + δ, δ = align_up(b + v0) − v0; the δ at
            // which the next entry's bytes around its field are most like these
            let v0 = tab(f0) as usize;
            let mut deltas = [(0usize, 0usize); NEXT_DELTAS];
            for delta in 0..=MAX_B + 7 {
                let f1 = f0 + v0 + delta;
                if f1 + w > n {
                    break;
                }
                if f1 + tab(f1) as usize > n {
                    continue;
                }
                let sim = like(span, f0, f1);
                if let Some(i) = deltas.iter().position(|&(s, _)| sim > s) {
                    deltas.copy_within(i..NEXT_DELTAS - 1, i + 1);
                    deltas[i] = (sim, delta);
                }
            }
            let rules = deltas.iter().filter(|d| d.0 > 0).flat_map(|&(_, delta)| {
                ALIGNS.into_iter().flat_map(move |align| {
                    (0..=MAX_B)
                        .filter(move |&b| (b + v0).div_ceil(align) * align - v0 == delta)
                        .map(move |b| (b, align))
                })
            });
            for (b, align) in rules {
                {
                    let mut pos = [0usize; STEPS + 1];
                    pos[0] = f0;
                    let (mut f, mut alive, mut varied) = (f0, true, false);
                    let mut first = 0;
                    let mut s8 = 0.0;
                    for step in 0..STEPS {
                        if step == 8 {
                            s8 = score_cols(span, &pos[..9], FIELD_BACK, FIELD_COLS / 4, 4);
                            if screen.len() == SCREEN && s8 < screen.peek().unwrap().0.0 {
                                alive = false;
                                break;
                            }
                        }
                        let l = (b + tab(f) as usize).div_ceil(align) * align;
                        if l < w || f + l + w > n {
                            alive = false;
                            break;
                        }
                        if step == 0 {
                            first = l;
                        } else if l != first {
                            varied = true;
                        }
                        f += l;
                        pos[step + 1] = f;
                    }
                    if alive && varied {
                        lanes.push((pos, Rule { d: 0, kind, b, align }, s8));
                        screen.push(Reverse(Score(s8)));
                        if screen.len() > SCREEN {
                            screen.pop();
                        }
                    }
                }
            }
        }
    }
    if lanes.len() > SCREEN {
        lanes.select_nth_unstable_by(SCREEN, |x, y| y.2.total_cmp(&x.2));
        lanes.truncate(SCREEN);
    }
    // the entries start at their fields until `rotate` moves them
    let mut hyps: Vec<(f64, usize, Rule)> =
        lanes.iter().map(|(pos, r, _)| (score_at(span, pos, FIELD_BACK, FIELD_COLS), at + pos[0], *r)).collect();
    // through the whole file
    hyps.sort_unstable_by(|a, b| {
        let k = |h: &(f64, usize, Rule)| (h.0, h.1, h.2.d, h.2.kind, h.2.b, h.2.align);
        k(b).partial_cmp(&k(a)).unwrap()
    });
    hyps.truncate(64);
    let mut screened: Vec<(f64, usize, Rule)> = hyps
        .iter()
        .map(|&(_, p0, r)| {
            let p = follow(data, p0, &r, Some(FOLLOW_SAMPLE));
            (score(data, &p, COLS) * (p.len() - 1) as f64, p0, r)
        })
        .collect();
    // stable: ties keep the order of hyps
    screened.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut chains: HashMap<Vec<usize>, (f64, f64, Vec<usize>, Rule)> = HashMap::new();
    for &(_, p0, r) in screened.iter().take(FOLLOW_KEEP) {
        let p = follow(data, p0, &r, None);
        let m = p.len().min(4097);
        let mut sample: Vec<usize> =
            (0..m).map(|i| p[((p.len() - 1) as f64 * i as f64 / (m - 1).max(1) as f64) as usize]).collect();
        sample.dedup();
        let per = score(data, &sample, COLS);
        // chains that end alike differ in how they start: keep the one that starts best
        let head = score(data, &p[..p.len().min(65)], COLS);
        let key = tail_key(&p);
        if chains.get(&key).is_none_or(|c| head > c.0) {
            chains.insert(key, (head, per * (p.len() - 1) as f64, p, r));
        }
    }
    let mut ranked: Vec<(f64, Vec<usize>, Rule)> = chains.into_values().map(|c| (c.1, c.2, c.3)).collect();
    ranked.sort_unstable_by(by_score);
    ranked.retain(|c| c.0 > 0.0);
    ranked
}

/// Chains best first; out of a HashMap, so ties go by where they start and their rule.
fn by_score(a: &(f64, Vec<usize>, Rule), b: &(f64, Vec<usize>, Rule)) -> std::cmp::Ordering {
    b.0.total_cmp(&a.0).then(a.1[0].cmp(&b.1[0])).then(a.2.cmp(&b.2)).then(a.1.len().cmp(&b.1.len()))
}

/// The lengths of a chain's last entries: the same under rotation and for copies of the length.
fn tail_key(p: &[usize]) -> Vec<usize> {
    p[p.len().saturating_sub(9)..].windows(2).map(|w| w[1] - w[0]).collect()
}

/// The chain extended before its first entry: while some q < p[0] (within reach) holds an
/// entry ending at p[0], the nearest such q. Chains are found from wherever a window falls,
/// often inside the entries.
fn extend_back(data: &[u8], mut p: Vec<usize>, r: &Rule) -> Vec<usize> {
    let mut before = Vec::new();
    let mut at = p[0];
    while at > 0 {
        let q = (at.saturating_sub(4096)..at).rev().find(|&q| {
            let l = r.length(data, q);
            q + l == at && l >= r.d + r.width()
        });
        let Some(q) = q else { break };
        before.push(q);
        at = q;
    }
    if !before.is_empty() {
        before.reverse();
        before.extend_from_slice(&p);
        p = before;
    }
    p
}

/// The first entries of a chain, up to 2048 of them and SAMPLE bytes, on which its rotation
/// and head are chosen.
fn sample(p: &[usize]) -> &[usize] {
    let end = p.partition_point(|&q| q <= p[0] + SAMPLE).max(3);
    &p[..end.min(p.len()).min(2049)]
}

/// The heads (h bytes of each entry from p, transposed) and the tails, one after another.
fn heads_tails(data: &[u8], p: &[usize], h: usize, out: &mut Vec<u8>) {
    out.clear();
    for j in 0..h {
        out.extend(p[..p.len() - 1].iter().map(|&q| data[q + j]));
    }
    for w in p.windows(2) {
        out.extend_from_slice(&data[w[0] + h..w[1]]);
    }
}

/// The chain moved back by x bytes (its length field at d + x) where it codes best.
///
/// A chain fixes the entries only up to rotation: (p, d) and (p − x, d + x) follow the same
/// lengths. The right one has the variable part (as long as the length field says) at the end
/// of the entry: its tails, one after another, are that part alone (names, payloads), which
/// only an LZ coder sees. So each rotation is coded with zstd, on the first entries, with the
/// heads (b bytes) transposed. Entries that a rotation moves out of the file are left out
/// (a chain found from inside the entries often starts at the file's start, or near it).
fn rotate(data: &[u8], p: Vec<usize>, r: Rule) -> (Vec<usize>, Rule) {
    let lmin = p.windows(2).map(|w| w[1] - w[0]).min().unwrap();
    let h = if r.b >= 8 { r.b.min(lmin) } else { lmin.min(MAX_B) };
    let (d, w) = (r.d as i64, r.width() as i64);
    let moved = |q: &[usize], x: i64| -> Vec<usize> {
        q.iter().map(|&s| s as i64 - x).filter(|&s| 0 <= s && s <= data.len() as i64).map(|s| s as usize).collect()
    };
    let q = sample(&p);
    let mut best = (usize::MAX, 0i64);
    let mut buf = Vec::new();
    for x in -d..=h as i64 - d - w {
        let m = moved(q, x);
        if m.len() < 2 {
            continue;
        }
        heads_tails(data, &m, h, &mut buf);
        best = best.min((zcost::len(&[&buf]), x));
    }
    let x = best.1;
    (moved(&p, x), Rule { d: (d + x) as usize, ..r })
}

/// The run of entries whose leading bytes fit the column models of the first ones.
fn trim(data: &[u8], bg: &[f64; 256], p: Vec<usize>) -> Vec<usize> {
    let lead = |q: usize, j: usize| byte(data, q + j) as usize;
    let mut counts = vec![0u32; COLS * 256];
    let fit = (p.len() - 1).min(256);
    for &q in &p[..fit] {
        for j in 0..COLS {
            counts[j * 256 + lead(q, j)] += 1;
        }
    }
    let lp = kt::column_log2(&counts, fit);
    let scores = p[..p.len() - 1]
        .iter()
        .map(|&q| (0..COLS).map(|j| (lp[j * 256 + lead(q, j)] - bg[lead(q, j)]).max(-4.0)).sum());
    let (s, e, _) = best_run(scores);
    p[s..e + 1].to_vec()
}

/// How many leading bytes of each entry to take as its head: the length field says where the
/// variable part ends, not where it starts. Of a few widths up to the shortest entry (and b),
/// the one that codes best with zstd on the first entries, heads transposed.
pub fn choose_head(data: &[u8], p: &[usize], r: &Rule) -> usize {
    let least = r.d + r.width();
    let most = p.windows(2).map(|w| w[1] - w[0]).min().unwrap();
    let mut widths: Vec<usize> = [least, r.b.min(most), 8, 12, 16, 20, 24, 32, 48, 64]
        .into_iter()
        .filter(|&h| least <= h && h <= most)
        .collect();
    widths.sort_unstable();
    widths.dedup();
    let q = sample(p);
    let mut buf = Vec::new();
    let mut best = (usize::MAX, least);
    for h in widths {
        heads_tails(data, q, h, &mut buf);
        best = best.min((zcost::len(&[&buf]), h));
    }
    best.1
}

/// Chains of entries in the file: (entry starts, the last where the last entry ends; rule).
pub fn chains(data: &[u8]) -> Vec<(Vec<usize>, Rule)> {
    let n = data.len();
    let ats = if n > SPAN / 4 { linspace_end(n, WINDOWS) } else { vec![0] };
    let mut best: HashMap<Vec<usize>, (f64, Vec<usize>, Rule)> = HashMap::new();
    for at in ats {
        let found = if at == 0 { find(data, 0, 320) } else { find(data, at, 160) };
        for c in found.into_iter().take(3) {
            let key = tail_key(&c.1);
            if best.get(&key).is_none_or(|b| c.0 > b.0) {
                best.insert(key, c);
            }
        }
    }
    let mut ranked: Vec<_> = best.into_values().collect();
    ranked.sort_unstable_by(by_score);
    let mut out = Vec::new();
    let bg = kt::order0_log2(data);
    for (_, p, r) in ranked.into_iter().take(6) {
        let p = extend_back(data, p, &r);
        let (mut p, r) = rotate(data, p, r);
        // entries the chain left out as long as they started at their fields
        let more = follow(data, *p.last().unwrap(), &r, None);
        p.extend_from_slice(&more[1..]);
        let p = trim(data, &bg, p);
        if p.len() > MIN_RECORDS {
            out.push((p, r));
        }
    }
    out
}

/// count starts over [0, n): the prototype's `linspace(0, n, count + 1)[:-1]`.
fn linspace_end(n: usize, count: usize) -> Vec<usize> {
    let mut v: Vec<usize> = (0..count).map(|i| (n as f64 * i as f64 / count as f64) as usize).collect();
    v.dedup();
    v
}
