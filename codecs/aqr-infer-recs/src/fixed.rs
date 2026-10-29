//! Tables of fixed-size records: a record size k found in windows of the file, grown into the
//! region the records hold.

use crate::fields::{self, Cost};
use crate::kt::{self, best_run};
use crate::{MIN_RECORDS, linspace};

const WINDOW: usize = 1 << 16;
pub const MAX_K: usize = 1024;
const MAX_WINDOWS: usize = 8;
/// Small tables: windows over the whole file, records up to FINE_K.
const FINE_WINDOW: usize = 1 << 12;
const FINE_K: usize = 64;
const MAX_FINE_WINDOWS: usize = 1024;
/// How far a region grows from a small window.
const FINE_SPAN: usize = 1 << 20;
const MAX_REGIONS: usize = 32;
/// The least score of a byte against the column models: one value the models have not seen
/// does not end a region.
const CLIP: f64 = -4.0;

/// The best k of a window and its code length against k = 1, in bits per byte, and the
/// window's order-0 code length in bits per byte. The code length of the window under k
/// column models (one per position mod k) is summed byte by byte as the adaptive models code
/// them: the symbols' terms by their counts, undone afterwards, and the models' terms (which
/// depend only on how many bytes each column holds) at once.
pub fn scan_window(a: &[u8], max_k: usize, counts: &mut Vec<u16>) -> (usize, f64, f64) {
    let t = kt::tables();
    let n = a.len();
    let top = max_k.min(n / MIN_RECORDS);
    counts.resize(top.max(1) * 256, 0);
    let models = |k: usize| {
        // n % k columns of n / k + 1 bytes, the rest of n / k
        let (q, r) = (n / k, n % k);
        r as f64 * t.all_sum(q + 1) + (k - r) as f64 * t.all_sum(q)
    };
    let mut c0 = [0u32; 256];
    let mut base = models(1);
    for &b in a {
        base -= t.sym[c0[b as usize] as usize];
        c0[b as usize] += 1;
    }
    let mut best = (base, 1);
    for k in 2..=top {
        let mut bits = models(k);
        for chunk in a.chunks(k) {
            for (col, &b) in chunk.iter().enumerate() {
                let c = &mut counts[col * 256 + b as usize];
                bits -= t.sym[*c as usize];
                *c += 1;
            }
        }
        for chunk in a.chunks(k) {
            for (col, &b) in chunk.iter().enumerate() {
                counts[col * 256 + b as usize] = 0;
            }
        }
        if bits < best.0 {
            best = (bits, k);
        }
    }
    (best.1, (base - best.0) / n as f64, base / n as f64)
}

/// The region of records of size k grown from the window [wo, wo + wlen): (start, end, gain
/// in bits). A maximum-sum run of each byte's log-ratio of the column models against the
/// order-0 model; the column models are refitted on the region found and the region grown
/// again, as a window alone misses values that drift along the file (counters, timestamps).
/// With span, the region stays within that many bytes of the window.
pub fn grow_region(data: &[u8], k: usize, wo: usize, wlen: usize, span: Option<usize>) -> (usize, usize, f64) {
    let base = span.map_or(0, |s| wo.saturating_sub(s));
    let data = &data[base..span.map_or(data.len(), |s| (wo + wlen + s).min(data.len()))];
    let wo = wo - base;
    let bg = kt::order0_log2(data);
    let (mut s, mut e, mut gain) = (wo, wo + wlen, 0.0);
    let mut counts = vec![0u32; k * 256];
    let mut llr = vec![0.0f64; k * 256];
    // the column of position i: (i - wo) mod k
    let phase = (k - wo % k) % k;
    for _ in 0..4 {
        counts.fill(0);
        for (i, &b) in data[s..e].iter().enumerate() {
            counts[(s + i + phase) % k * 256 + b as usize] += 1;
        }
        for c in 0..k {
            let row = &counts[c * 256..c * 256 + 256];
            let lp = kt::column_log2(row, row.iter().sum::<u32>() as usize);
            for s in 0..256 {
                llr[c * 256 + s] = (lp[s] - bg[s]).max(CLIP);
            }
        }
        let (ns, ne, g) = best_run(data.iter().enumerate().map(|(i, &b)| llr[(i + phase) % k * 256 + b as usize]));
        gain = g;
        if (ns, ne) == (s, e) {
            break;
        }
        (s, e) = (ns, ne);
    }
    (base + s, base + e, gain)
}

/// The region of records of size k starting at p (mod k) around the window seed, with each
/// record coded by fields: (start, end).
///
/// Order-0 columns cannot tell a sorted table from the bytes after it (offsets that rise
/// look like any other bytes), so the region they find can run on. Here the fields of the
/// window's records (deltas and xors) code each record against the record before, and a
/// record scores the code length of its bytes under the order-0 model of the file less that
/// of its fields' planes under column models, refitted on the region.
pub fn grow_fields(data: &[u8], k: usize, p: usize, seed: (usize, usize), span: usize) -> (usize, usize) {
    let (k_, p_) = (k as i64, p as i64);
    let floor = seed.0.saturating_sub(span) as i64;
    let lo = (p_ - (p_ - floor).div_euclid(k_) * k_) as usize;
    let hi = (seed.1 + span).min(data.len());
    if hi <= lo {
        return seed;
    }
    let nrec = (hi - lo) / k;
    let m = &data[lo..lo + nrec * k];
    let r0 = seed.0.saturating_sub(lo).div_ceil(k);
    let r1 = (seed.1.saturating_sub(lo) / k).min(nrec);
    if r1 < r0 + MIN_RECORDS {
        return seed;
    }
    let (fl, _, _) = fields::split(&m[r0 * k..r1 * k], k, Cost::Kt, false);
    // the coded records, rows of k bytes: each field's planes in turn
    let planes = fields::encode(m, k, &fl);
    let mut t = vec![0u8; nrec * k];
    let mut col = 0;
    let mut at = 0;
    for f in &fl {
        for _ in 0..f.width {
            for i in 0..nrec {
                t[i * k + col] = planes[at + i];
            }
            at += nrec;
            col += 1;
        }
    }
    let bg = kt::order0_log2(data);
    let (mut s, mut e) = (r0, r1);
    let mut counts = vec![0u32; k * 256];
    for _ in 0..3 {
        counts.fill(0);
        for r in t[s * k..e * k].chunks_exact(k) {
            for (c, &b) in r.iter().enumerate() {
                counts[c * 256 + b as usize] += 1;
            }
        }
        let lp = kt::column_log2(&counts, e - s);
        let scores = t.chunks_exact(k).zip(m.chunks_exact(k)).map(|(tr, mr)| {
            tr.iter()
                .zip(mr)
                .enumerate()
                .map(|(c, (&x, &y))| (lp[c * 256 + x as usize] - bg[y as usize]).max(CLIP))
                .sum()
        });
        let (ns, ne, _) = best_run(scores);
        if (ns, ne) == (s, e) {
            break;
        }
        (s, e) = (ns, ne);
    }
    // the first record has no record before it: it is judged by the columns of the bytes
    if s > 0 {
        counts.fill(0);
        for r in m[s * k..e * k].chunks_exact(k) {
            for (c, &b) in r.iter().enumerate() {
                counts[c * 256 + b as usize] += 1;
            }
        }
        let lp = kt::column_log2(&counts, e - s);
        let r = &m[(s - 1) * k..s * k];
        if r.iter().enumerate().map(|(c, &b)| (lp[c * 256 + b as usize] - bg[b as usize]).max(CLIP)).sum::<f64>() > 0.0
        {
            s -= 1;
        }
    }
    (lo + s * k, lo + e * k)
}

/// Where the records of a region really start: the region's edges are found byte by byte, and
/// a record's first bytes (zeros, say) may fit the model of another column. Records start
/// aligned (to the largest power of 2 up to 8 that divides k) near s; of those starts, the one
/// whose records split into fields at the least cost. Starts a field apart often cost the
/// same (8-byte fields of a 24-byte record stay whole in three phases): s itself, where the
/// byte-wise edge fell, is taken if it costs within 0.5% of the least.
fn refine_phase(data: &[u8], s: usize, e: usize, k: usize) -> usize {
    if k > FINE_K {
        return s;
    }
    let a = gcd(k, 8);
    let mut best = (f64::INFINITY, s);
    let mut at_s = f64::INFINITY;
    for p in (s + 1).saturating_sub(k)..s + k {
        if p % a != 0 || p >= e {
            continue;
        }
        let nrec = ((e - p) / k).min(2048);
        if nrec < MIN_RECORDS {
            continue;
        }
        let (_, bits, _) = fields::split(&data[p..p + nrec * k], k, Cost::Kt, false);
        let x = bits / nrec as f64;
        if p == s {
            at_s = x;
        }
        if x < best.0 {
            best = (x, p);
        }
    }
    if at_s <= best.0 * 1.005 { s } else { best.1 }
}

fn gcd(a: usize, b: usize) -> usize {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// Regions of fixed-size records: (start, k, records), sorted, possibly overlapping (zstd
/// settles which to take).
///
/// Big windows (a few, k up to MAX_K) find big tables; small windows over the whole file (k
/// up to FINE_K) find small ones, each grown only within FINE_SPAN, and skipped where a region
/// of the same k already holds them. The same table found from several windows is one.
pub fn find(data: &[u8]) -> Vec<(usize, usize, usize)> {
    struct Found {
        gain: f64,
        s: usize,
        e: usize,
        k: usize,
        wo: usize,
        wlen: usize,
    }
    let n = data.len();
    let mut found: Vec<Found> = Vec::new();
    let mut counts = Vec::new();
    let mut scan = |wo: usize, wlen: usize, max_k: usize, span: Option<usize>, found: &mut Vec<Found>| {
        let (k, gain, h0) = scan_window(&data[wo..wo + wlen], max_k, &mut counts);
        if found.iter().any(|f| f.s <= wo && wo + wlen <= f.e && f.k == k) {
            return;
        }
        if k == 1 || gain < 0.05 || h0 > 7.8 {
            return;
        }
        let (s, e, gain) = grow_region(data, k, wo, wlen, span);
        if (e - s) / k >= MIN_RECORDS {
            found.push(Found { gain, s, e, k, wo, wlen });
        }
    };
    let wlen = WINDOW.min(n);
    if wlen == 0 {
        return Vec::new();
    }
    for wo in linspace(n - wlen, MAX_WINDOWS.min((n / wlen).max(1))) {
        scan(wo, wlen, MAX_K, None, &mut found);
    }
    if n >= 2 * FINE_WINDOW {
        for wo in linspace(n - FINE_WINDOW, MAX_FINE_WINDOWS.min(n / FINE_WINDOW)) {
            scan(wo, FINE_WINDOW, FINE_K, Some(FINE_SPAN), &mut found);
        }
    }
    found.sort_by(|a, b| {
        (b.gain, b.s, b.e, b.k, b.wo, b.wlen).partial_cmp(&(a.gain, a.s, a.e, a.k, a.wo, a.wlen)).unwrap()
    });
    let mut seeds: Vec<&Found> = Vec::new();
    for f in &found {
        let dup = seeds
            .iter()
            .any(|x| f.k == x.k && f.e.min(x.e) as i64 - f.s.max(x.s) as i64 > ((f.e - f.s).max(x.e - x.s) / 2) as i64);
        if !dup {
            seeds.push(f);
        }
    }
    let mut regions = Vec::new();
    for f in seeds.iter().take(MAX_REGIONS) {
        let s = refine_phase(data, f.s, f.e, f.k);
        let mut spans = vec![(s, f.e)];
        if f.k <= FINE_K {
            spans.push(grow_fields(data, f.k, s, (f.wo, f.wo + f.wlen), FINE_SPAN));
        }
        for (s, e) in spans {
            if e > s && (e - s) / f.k >= MIN_RECORDS {
                regions.push((s, f.k, (e - s) / f.k));
            }
        }
    }
    regions.sort();
    regions.dedup();
    regions
}
