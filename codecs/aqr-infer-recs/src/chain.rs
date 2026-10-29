//! Entries with a length field: chains of entries, each holding a length v at offset d and the
//! next starting align_up(b + v, align) after it.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use crate::kt::{self, best_run};
use crate::{MIN_RECORDS, zcost};

/// The field kinds a length is read as: (width, big-endian, mask).
pub const KINDS: [(usize, bool, u32); 9] = [
    (1, false, 0xFF),
    (2, false, 0xFFFF),
    (2, false, 0x7FFF),
    (2, false, 0x0FFF),
    (2, true, 0xFFFF),
    (2, true, 0x7FFF),
    (2, true, 0x0FFF),
    (4, false, 0xFFFF_FFFF),
    (4, true, 0xFFFF_FFFF),
];
const ALIGNS: [usize; 3] = [1, 4, 8];
const MAX_B: usize = 64;

/// Where chains are looked for: spans over the file, the first at its start.
const SPAN: usize = 1 << 20;
const WINDOWS: usize = 16;
const STEPS: usize = 32;
/// The entries' leading bytes that score a chain.
const COLS: usize = 16;
/// Chains of a span are screened on 8 entries and 8 columns, these kept.
const SCREEN: usize = 512;
/// The best of them are ranked on this many entries, and FOLLOW_KEEP followed to the end.
const FOLLOW_SAMPLE: usize = 4096;
const FOLLOW_KEEP: usize = 8;

/// A hypothesis: entries from p0, the length at d of kind KINDS[kind], plus b, aligned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rule {
    pub d: usize,
    pub kind: usize,
    pub b: usize,
    pub align: usize,
}

impl Rule {
    pub fn width(&self) -> usize {
        KINDS[self.kind].0
    }

    /// The length field at p (zero past the end of data).
    pub fn value(&self, data: &[u8], p: usize) -> u64 {
        let (w, big, mask) = KINDS[self.kind];
        let at = |i: usize| *data.get(p + self.d + i).unwrap_or(&0) as u64;
        let v = if big { (0..w).fold(0, |v, i| v << 8 | at(i)) } else { (0..w).rev().fold(0, |v, i| v << 8 | at(i)) };
        v & mask as u64
    }

    /// The length of the entry at p.
    pub fn length(&self, data: &[u8], p: usize) -> usize {
        let a = self.align as u64;
        ((self.b as u64 + self.value(data, p)).div_ceil(a) * a) as usize
    }

    pub fn describe(&self) -> String {
        let (w, big, mask) = KINDS[self.kind];
        format!(
            "length at {} as {}{}&{mask:#x}, +{} align {}",
            self.d,
            if big { "be" } else { "le" },
            8 * w,
            self.b,
            self.align
        )
    }
}

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

fn byte(data: &[u8], i: usize) -> u8 {
    *data.get(i).unwrap_or(&0)
}

/// How much more often consecutive entries starting at p agree in their leading ncols bytes,
/// column by column, than with bytes a multiple of 8 into the entry before: runs of zeros,
/// text or periodic data (code) agree either way, and score nothing.
pub fn score(data: &[u8], p: &[usize], ncols: usize) -> f64 {
    if p.len() < 2 {
        return 0.0;
    }
    let (mut same, mut ctrl) = (0usize, 0usize);
    for i in 1..p.len() {
        let l = ((p[i] - p[i - 1]) / 8).max(1);
        let u = 1 + ((i - 1) * 7919) % l;
        let c = p[i - 1] + 8 * u;
        for j in 0..ncols {
            let x = byte(data, p[i] + j);
            same += (x == byte(data, p[i - 1] + j)) as usize;
            ctrl += (x == byte(data, c + j)) as usize;
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

/// Chains whose entries hold together, starting within the span at `at`, best first:
/// (score, entry starts, rule).
///
/// Every rule is followed from every start for STEPS entries; a chain dies when an entry is
/// shorter than its length field or leaves the span, and chains of one length are fixed
/// records, left to the column scan. The chains of each d are screened, scored, and the best
/// 16 kept. The best of all are ranked on their first entries, followed through the whole
/// file, and those that end with the same lengths are one (the one whose first entries score
/// best); the rest are ranked by their score times their entries.
fn find(
    data: &[u8],
    at: usize,
    max_d: usize,
    starts: impl Iterator<Item = usize> + Clone,
) -> Vec<(f64, Vec<usize>, Rule)> {
    let span = &data[at..(at + SPAN).min(data.len())];
    let n = span.len();
    // each kind's value at every position of the span (zero past its end)
    let tables: Vec<Vec<u32>> = (0..KINDS.len())
        .map(|kind| {
            let r = Rule { d: 0, kind, b: 0, align: 1 };
            (0..n).map(|p| r.value(span, p) as u32).collect()
        })
        .collect();
    let mut found: HashMap<Vec<usize>, (f64, usize, Rule)> = HashMap::new();
    let mut lanes: Vec<([usize; STEPS + 1], usize, Rule, f64)> = Vec::new();
    // the screen scores of the chains kept so far, the least on top: a chain that scores below
    // the least of SCREEN of them after 8 entries would not be kept at the end either
    let mut screen: BinaryHeap<Reverse<Score>> = BinaryHeap::new();
    for d in 0..max_d {
        lanes.clear();
        screen.clear();
        for p0 in starts.clone().filter(|&p| p < n) {
            for (kind, tab) in tables.iter().enumerate() {
                // no entry is shorter than its length
                if p0 + d + KINDS[kind].0 > n || p0 + tab[p0 + d] as usize > n {
                    continue;
                }
                for b in 0..=MAX_B {
                    for align in ALIGNS {
                        let r = Rule { d, kind, b, align };
                        let w = r.width();
                        let mut pos = [0usize; STEPS + 1];
                        pos[0] = p0;
                        let (mut p, mut alive, mut varied) = (p0, true, false);
                        let mut first = 0;
                        let mut s8 = 0.0;
                        for step in 0..STEPS {
                            if step == 8 {
                                s8 = score(span, &pos[..9], 8);
                                if screen.len() == SCREEN && s8 < screen.peek().unwrap().0.0 {
                                    alive = false;
                                    break;
                                }
                            }
                            if p + d + w > n {
                                alive = false;
                                break;
                            }
                            let l = (b + tab[p + d] as usize).div_ceil(align) * align;
                            if l < d + w || p + l > n {
                                alive = false;
                                break;
                            }
                            if step == 0 {
                                first = l;
                            } else if l != first {
                                varied = true;
                            }
                            p += l;
                            pos[step + 1] = p;
                        }
                        if alive && varied {
                            lanes.push((pos, p0, r, s8));
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
            let mut s: Vec<(f64, usize)> = lanes.iter().enumerate().map(|(i, l)| (-l.3, i)).collect();
            s.select_nth_unstable_by(SCREEN, |a, b| a.partial_cmp(b).unwrap());
            let keep: Vec<usize> = s[..SCREEN].iter().map(|x| x.1).collect();
            lanes = keep.into_iter().map(|i| lanes[i]).collect();
        }
        let mut scored: Vec<(f64, usize)> =
            lanes.iter().enumerate().map(|(i, l)| (score(span, &l.0, COLS), i)).collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap().then(a.1.cmp(&b.1)));
        for &(sc, i) in scored.iter().take(16) {
            let (pos, p0, r, _) = &lanes[i];
            let key: Vec<usize> = pos.iter().map(|p| p + d).collect();
            let e = found.entry(key).or_insert((-1.0, 0, *r));
            if sc > e.0 {
                *e = (sc, at + p0, *r);
            }
        }
    }
    // through the whole file
    let mut hyps: Vec<(f64, usize, Rule)> = found.into_values().collect();
    hyps.sort_by(|a, b| {
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
    screened.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
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
    ranked.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    ranked.retain(|c| c.0 > 0.0);
    ranked
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
    let h = if r.b >= 8 { r.b.min(lmin) } else { lmin };
    let (d, w) = (r.d as i64, r.width() as i64);
    let moved = |q: &[usize], x: i64| -> Vec<usize> {
        q.iter().map(|&s| s as i64 - x).filter(|&s| 0 <= s && s <= data.len() as i64).map(|s| s as usize).collect()
    };
    let q = &p[..p.len().min(2050)];
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
fn trim(data: &[u8], p: Vec<usize>) -> Vec<usize> {
    let lead = |q: usize, j: usize| byte(data, q + j) as usize;
    let mut counts = vec![0u32; COLS * 256];
    let fit = (p.len() - 1).min(256);
    for &q in &p[..fit] {
        for j in 0..COLS {
            counts[j * 256 + lead(q, j)] += 1;
        }
    }
    let lp = kt::column_log2(&counts, fit);
    let bg = kt::order0_log2(data);
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
    widths.sort();
    widths.dedup();
    let q = &p[..p.len().min(2049)];
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
        let found =
            if at == 0 { find(data, 0, 64, (0..=256).step_by(4)) } else { find(data, at, 32, (0..=128).step_by(4)) };
        for c in found.into_iter().take(3) {
            let key = tail_key(&c.1);
            if best.get(&key).is_none_or(|b| c.0 > b.0) {
                best.insert(key, c);
            }
        }
    }
    let mut ranked: Vec<_> = best.into_values().collect();
    ranked.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    let mut out = Vec::new();
    for (_, p, r) in ranked.into_iter().take(6) {
        let p = extend_back(data, p, &r);
        let (p, r) = rotate(data, p, r);
        let p = trim(data, p);
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
