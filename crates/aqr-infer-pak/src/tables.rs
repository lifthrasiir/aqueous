//! Tables of fixed-size records: strides by byte autocorrelation, regions where some stride
//! repeats, each region's stride by the code length of its columns under adaptive models, and
//! its ends by a maximum-sum run of the bytes' log-ratio under the column models.

use std::cell::RefCell;
use std::sync::OnceLock;

use crate::par_map;

pub const KMIN: usize = 4;
pub const KMAX: usize = 128;
/// Fewer rows than this are no evidence (docs/pak-inference-design.md, small directories).
pub const MINROWS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Table {
    pub start: usize,
    pub end: usize,
    pub k: usize,
    /// Bits the column models save over the run.
    pub score: f64,
    /// The stride the run was labelled with (not one related to it).
    pub primary: bool,
}

impl Table {
    pub fn rows(&self) -> usize {
        (self.end - self.start) / self.k
    }

    pub fn same(&self, o: &Table) -> bool {
        self.start == o.start && self.end == o.end && self.k == o.k
    }
}

/// log2 of Γ(c + 1/2) / Γ(1/2) by c, for the KT code length.
fn lgh() -> &'static [f64] {
    static T: OnceLock<Vec<f64>> = OnceLock::new();
    T.get_or_init(|| {
        let mut v = Vec::with_capacity(4096);
        let mut s = 0.0;
        for c in 0..4096 {
            v.push(s);
            s += (c as f64 + 0.5).log2();
        }
        v
    })
}

thread_local! {
    /// [`colcost`]'s counts, all zero between calls.
    static COUNTS: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
}

/// Bits a byte of `z` costs under k adaptive (KT) order-0 column models. `counts` (k × 256,
/// all zero) is left all zero: only the counts used are touched.
fn colcost(z: &[u8], k: usize, counts: &mut Vec<u32>) -> f64 {
    let n = z.len() / k;
    if counts.len() < k * 256 {
        counts.resize(k * 256, 0);
    }
    for row in z[..n * k].chunks_exact(k) {
        for (c, &x) in row.iter().enumerate() {
            counts[c * 256 + x as usize] += 1;
        }
    }
    let t = lgh();
    // log2 Γ(n + 128) / Γ(128) - Σ log2 Γ(c + 1/2) / Γ(1/2), each count taken (and cleared)
    // at its first byte
    let mut bits = k as f64 * all_sum(n);
    for row in z[..n * k].chunks_exact(k) {
        for (c, &x) in row.iter().enumerate() {
            let e = &mut counts[c * 256 + x as usize];
            if *e > 0 {
                bits -= t[*e as usize];
                *e = 0;
            }
        }
    }
    bits / (n * k) as f64
}

/// Σ log2(i + 128) for i < n.
fn all_sum(n: usize) -> f64 {
    static T: OnceLock<Vec<f64>> = OnceLock::new();
    let t = T.get_or_init(|| {
        let mut v = Vec::with_capacity(4097);
        let mut s = 0.0;
        v.push(0.0);
        for i in 0..4096 {
            s += (i as f64 + 128.0).log2();
            v.push(s);
        }
        v
    });
    t[n]
}

/// log2(c + 1/2) by c.
fn log2_half() -> &'static [f64] {
    static T: OnceLock<Vec<f64>> = OnceLock::new();
    T.get_or_init(|| (0..1 << 16).map(|c| (c as f64 + 0.5).log2()).collect())
}

/// Share of bytes equal to the byte k before, for k = KMIN..=KMAX.
fn autocorrelation(z: &[u8]) -> Vec<f64> {
    (KMIN..=KMAX.min(z.len() / 4))
        .map(|k| {
            let n = z.len() - k;
            let eq = z[..n].iter().zip(&z[k..]).filter(|(a, b)| a == b).count();
            eq as f64 / n as f64
        })
        .collect()
}

/// The strides worth looking at in `w`: by tiles of 64 KiB (a small table drowns in a bigger
/// window's), those repeating over the tile's median and not explained by a divisor, and
/// their divisors and multiples (a table of a divisor's stride may sit next to a bigger one).
fn strides(w: &[u8]) -> Vec<usize> {
    let mut cand = Vec::new();
    // tiles of 4 KiB over the first 64 KiB and of 16 KiB to 256 KiB (a table among data whose
    // bytes repeat at any stride stands out only in a tile near its own size: 480 bytes of a
    // table in 16 KiB of text did not), then the first 16 KiB of each 64 KiB
    let (h1, h2) = (w.len().min(1 << 16), w.len().min(1 << 18));
    let tiles = w[..h1]
        .chunks(1 << 12)
        .chain(w[h1..h2].chunks(1 << 14))
        .chain(w[h2..].chunks(1 << 16).map(|z| &z[..z.len().min(1 << 14)]));
    for z in tiles {
        if z.len() < 16 * KMIN {
            continue;
        }
        let eq = autocorrelation(z);
        let mut sorted = eq.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let base = sorted[sorted.len() / 2].max(1.0 / 256.0);
        let mut order: Vec<usize> = (0..eq.len()).collect();
        order.sort_by(|&a, &b| eq[b].partial_cmp(&eq[a]).unwrap());
        let mut got: Vec<usize> = Vec::new();
        for i in order {
            let k = i + KMIN;
            // over the median by a share and a margin both: a table of many zero bytes in its
            // tile repeats at every stride (a median of 0.06 by 0.078 at its own), data with
            // repeated bytes too (0.08 by 0.119), random data hardly (0.004 by 0.0055)
            if eq[i] < (base * 1.15).max(base + 0.01) || got.len() >= 3 {
                break;
            }
            if got.iter().any(|&c| k.is_multiple_of(c) && eq[c - KMIN] >= 0.85 * eq[i]) {
                continue;
            }
            got.push(k);
        }
        cand.extend(got);
    }
    let mut all = cand.clone();
    for &k in &cand {
        for d in [k / 2, k / 4] {
            if d >= KMIN && k % d == 0 {
                all.push(d);
            }
        }
        for m in [2 * k, 4 * k] {
            if m <= KMAX {
                all.push(m);
            }
        }
    }
    all.sort_unstable();
    all.dedup();
    all
}

/// The table of stride k in w[a..z], its ends moved to the maximum-sum run of the bytes'
/// log-ratio under the column models (fitted on w[ta..tz]) against order 0 (`bg`), searched
/// 2 KiB around.
fn refine(
    w: &[u8],
    a: usize,
    z: usize,
    k: usize,
    bg: &[f64; 256],
    ta: usize,
    tz: usize,
) -> Option<(usize, usize, f64)> {
    let rows = (tz - ta) / k;
    if rows < 2 {
        return None;
    }
    let mut cols = vec![0u32; k * 256];
    for row in w[ta..ta + rows * k].chunks_exact(k) {
        for (c, &x) in row.iter().enumerate() {
            cols[c * 256 + x as usize] += 1;
        }
    }
    // log2((count + 1/2) / (rows + 128)), by a table of log2(c + 1/2)
    let lc = log2_half();
    let lt = (rows as f64 + 128.0).log2();
    let lo = a.saturating_sub(2048);
    let hi = (z + 2048).min(w.len());
    // maximum-sum run (Kadane's)
    let (mut best, mut bi, mut bj) = (0.0, 0, 0);
    let (mut cur, mut ci) = (0.0, lo);
    for p in lo..hi {
        let c = (p + k * (1 + ta / k) - ta) % k;
        let cnt = cols[c * 256 + w[p] as usize] as usize;
        let s = if cnt < lc.len() { lc[cnt] } else { (cnt as f64 + 0.5).log2() } - lt - bg[w[p] as usize];
        if cur <= 0.0 {
            cur = 0.0;
            ci = p;
        }
        cur += s;
        if cur > best {
            best = cur;
            bi = ci;
            bj = p + 1;
        }
    }
    if bj <= bi {
        return None;
    }
    let end = bi + ((bj - bi + k / 2) / k) * k;
    Some((bi, end.min(w.len()), best))
}

/// Tables in b[lo..hi], the strides' densities, the windows' labels and the runs' ends
/// computed on `threads` (each in order: the tables are the same on any number).
pub fn find(b: &[u8], lo: usize, hi: usize, threads: usize) -> Vec<Table> {
    let w = &b[lo..hi];
    let n = w.len();
    if n < 64 {
        return Vec::new();
    }
    let t0 = std::time::Instant::now();
    let cand = strides(w);
    if cand.is_empty() {
        return Vec::new();
    }
    let t_strides = t0.elapsed();
    // the density of bytes equal to the byte k before, by blocks of 16 bytes, smoothed over
    // a few records; regions where some stride's is 0.12 or more
    const B: usize = 16;
    let nb = n / B;
    let dens: Vec<Vec<f32>> = par_map(cand.len(), threads, |ci| {
        let k = cand[ci];
        let mut f = vec![0f32; nb];
        for (i, v) in f.iter_mut().enumerate() {
            let s = i * B;
            let e = (s + B).min(n - k);
            if s < e {
                *v = w[s..e].iter().zip(&w[s + k..e + k]).filter(|(a, b)| a == b).count() as f32 / B as f32;
            }
        }
        let r = (4 * k / B).max(1);
        let mut c = vec![0f32; nb + 1];
        for i in 0..nb {
            c[i + 1] = c[i] + f[i];
        }
        let mut dk = vec![0f32; nb];
        for i in 0..nb {
            let a = i.saturating_sub(r / 2);
            let z = (i + r - r / 2).min(nb);
            dk[i] = (c[z] - c[a]) / (z - a).max(1) as f32;
        }
        dk
    });
    let mut top = vec![0f32; nb];
    for dk in &dens {
        for (t, &d) in top.iter_mut().zip(dk) {
            if d > *t {
                *t = d;
            }
        }
    }
    let mut regions: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < nb {
        if top[i] < 0.12 {
            i += 1;
            continue;
        }
        let s = i;
        while i < nb && top[i] >= 0.12 {
            i += 1;
        }
        match regions.last_mut() {
            Some(last) if s - last.1 <= 4 => last.1 = i,
            _ => regions.push((s, i)),
        }
    }
    let mut bg = [0.5f64; 256];
    for &x in w {
        bg[x as usize] += 1.0;
    }
    let tot: f64 = bg.iter().sum();
    for v in bg.iter_mut() {
        *v = (*v / tot).log2();
    }
    let t_regions = t0.elapsed();
    let mut t_label = std::time::Duration::ZERO;
    // the runs to refine: (a, z, stride, primary, ta, tz)
    let mut runs: Vec<(usize, usize, usize, bool, usize, usize)> = Vec::new();
    for (s, e) in regions {
        let (rs, re) = (s * B, (e * B + KMAX).min(n));
        if re - rs < 256 {
            continue; // fewer bytes than 8 records of 32 (smaller ones are noise, mostly)
        }
        // each window labelled with the stride of least column code length
        let tl = std::time::Instant::now();
        // the strides dense in the region (most of a file's strides are another region's)
        let peak = |i: usize| dens[i][s..e].iter().copied().fold(0f32, f32::max);
        let best = (0..cand.len()).map(peak).fold(0f32, f32::max);
        let mut dense: Vec<(f32, usize)> =
            (0..cand.len()).map(|i| (peak(i), cand[i])).filter(|&(p, _)| p >= 0.8 * best).collect();
        dense.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap()); // the densest first
        let cand: Vec<usize> = dense.into_iter().map(|x| x.1).collect();
        // (windows of 2 KiB: KT's labels on so few rows are unsure, a 32-byte table of 12.7 KB
        // labelled 56; windows sized to the region lost a table of CREATURES' instead)
        let wn = 2048;
        let wins: Vec<usize> = (rs..re).step_by(wn).collect();
        let lab: Vec<Option<usize>> = par_map(wins.len(), threads, |i| {
            let a = wins[i];
            let z = &w[a..(a + wn).min(re)];
            COUNTS.with_borrow_mut(|counts| {
                cand.iter()
                    .filter(|&&k| z.len() >= MINROWS * k)
                    .map(|&k| (colcost(z, k, counts), k))
                    .min_by(|x, y| x.0.partial_cmp(&y.0).unwrap())
                    .map(|(_, k)| k)
            })
        });
        t_label += tl.elapsed();
        let mut i = 0;
        while i < wins.len() {
            let k = lab[i];
            let mut j = i;
            while j < lab.len() && lab[j] == k {
                j += 1;
            }
            if let Some(k) = k {
                let (a, z) = (wins[i], (wins[j - 1] + wn).min(re));
                // the run at its stride and at the strides related to it, the models fitted on
                // the run and on its first and last windows (a run may be two tables met)
                // (related: by the label, and the other strides dense in the region, as KT on a
                // window may label a 32-byte table 56: both miss what the other finds)
                let mut related: Vec<usize> = [k / 4, k / 2, 2 * k, 4 * k]
                    .into_iter()
                    .filter(|&x| (KMIN..=KMAX).contains(&x) && (x % k == 0 || k % x == 0))
                    .chain(cand.iter().copied().filter(|&x| x != k).take(4))
                    .collect();
                related.sort_unstable();
                related.dedup();
                for kk in std::iter::once(k).chain(related) {
                    let mut seeds = vec![(a, z)];
                    if z - a > 2 * wn {
                        seeds.push((a, a + wn));
                        seeds.push((z - wn, z));
                    }
                    for (ta, tz) in seeds {
                        runs.push((a, z, kk, kk == k, ta, tz));
                    }
                }
            }
            i = j;
        }
    }
    let mut tables: Vec<Table> =
        par_map(runs.len(), threads, |i| {
            let (a, z, kk, primary, ta, tz) = runs[i];
            refine(w, a, z, kk, &bg, ta, tz)
                .filter(|&(st, en, _)| (en - st) / kk >= MINROWS)
                .map(|(st, en, sc)| Table { start: lo + st, end: lo + en, k: kk, score: sc, primary })
        })
        .into_iter()
        .flatten()
        .collect();
    if std::env::var_os("AQR_PAK_VERBOSE").is_some() {
        eprintln!(
            "  find {lo:#x}..{hi:#x}: {} strides {:?} ({:.1} ms), regions ({:.1} ms), labels {:.1} ms, all {:.1} ms, {} tables",
            cand.len(),
            &cand[..cand.len().min(12)],
            t_strides.as_secs_f64() * 1e3,
            (t_regions - t_strides).as_secs_f64() * 1e3,
            t_label.as_secs_f64() * 1e3,
            t0.elapsed().as_secs_f64() * 1e3,
            tables.len()
        );
    }
    // fragments of one table (same stride and phase, meeting): one table, and the fragments
    // too (a table may meet data of its stride: 300 records of 16 bytes and the members after
    // them, padded to 16, are one run at 16)
    tables.sort_by_key(|t| (t.k, t.start % t.k, t.start));
    let mut out: Vec<Table> = Vec::new();
    let mut merged: Option<(Table, usize)> = None;
    for t in tables {
        if let Some((p, count)) = merged.as_mut()
            && p.k == t.k
            && (t.start - p.start).is_multiple_of(t.k)
            && t.start <= p.end + t.k
        {
            p.end = p.end.max(t.end);
            p.score += t.score;
            p.primary |= t.primary;
            *count += 1;
            out.push(t);
            continue;
        }
        if let Some((p, count)) = merged.take()
            && count > 1
        {
            out.push(p);
        }
        merged = Some((t, 1));
        out.push(t);
    }
    if let Some((p, count)) = merged
        && count > 1
    {
        out.push(p);
    }
    out
}

/// The tables of a file worth testing, near its head or tail first (where directories are;
/// data has tables of its own: blocks of textures), then a run's own stride before those
/// related to it, then by score; at most `keep`, at most `per` for a region.
pub fn rank(tables: Vec<Table>, len: usize, keep: usize, per: usize) -> Vec<Table> {
    rank_near(tables, len, keep, per, false)
}

/// [`rank`], and with `near_only` only the tables near the head or tail (for a big file: those
/// in the middle are the data's own, code and texture blocks).
pub fn rank_near(tables: Vec<Table>, len: usize, keep: usize, per: usize, near_only: bool) -> Vec<Table> {
    let edge = |t: &Table| t.start < 1 << 16 || t.end + (1 << 16) > len;
    let edges: Vec<Table> = tables.iter().copied().filter(edge).collect();
    let near = |t: &Table| edge(t) || edges.iter().any(|u| t.start <= u.end + 4096 && t.end + 4096 >= u.start);
    let mut v: Vec<(bool, bool, f64, Table)> = tables.iter().map(|t| (!near(t), !t.primary, -t.score, *t)).collect();
    v.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)).then(a.2.partial_cmp(&b.2).unwrap()));
    let mut out: Vec<Table> = Vec::new();
    let mut per_region = std::collections::HashMap::new();
    for (far, _, _, t) in v {
        if far && near_only {
            break; // sorted: the rest are far too
        }
        let key = (t.start >> 8, t.end >> 8); // (finer than 4 KiB: a table and its run into data of its stride end apart)
        let c = per_region.entry(key).or_insert(0);
        if *c >= per || out.iter().any(|u| u.same(&t)) {
            continue;
        }
        *c += 1;
        out.push(t);
        if out.len() >= keep {
            break;
        }
    }
    out
}

/// The tables of a file: its head (grown while a table runs to the window's end, up to
/// 64 MiB) and tail, or the whole of a small file.
pub fn of_file(b: &[u8], keep: usize, per: usize, threads: usize) -> Vec<Table> {
    const HEAD: usize = 1 << 20;
    const TAIL: usize = 256 << 10;
    let n = b.len();
    if n <= 2 * HEAD {
        return rank(find(b, 0, n, threads), n, keep, per);
    }
    let mut hi = HEAD;
    // the head grows while tables from the head, one meeting the next, run to its end
    // (MetadataEtc's entries run 1.6 MB, in fragments of different strides, its chunks' sizes
    // after them)
    let mut ts = loop {
        let mut ts = find(b, 0, hi, threads);
        ts.sort_by_key(|t| t.start);
        // (links of 16 KiB at least: data has small tables everywhere, 9 thousand in a MiB of
        // LZO, close enough to chain)
        let mut reach = 1usize << 16;
        for t in ts.iter().filter(|t| t.end - t.start >= 1 << 14) {
            if t.start <= reach + 4096 {
                reach = reach.max(t.end);
            }
        }
        if reach + 4096 < hi || hi >= n || hi >= 16 << 20 {
            break ts;
        }
        hi = (hi * 2).min(n);
    };
    if hi < n - TAIL {
        ts.extend(find(b, n - TAIL, n, threads));
    }
    rank_near(ts, n, keep, per, true)
}
