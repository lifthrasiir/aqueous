//! A stream B of ARM64 code against a stream A by differences (bsdiff's idea, after Colin
//! Percival's "Naive differences of executable code"): copies of A's words at some offset,
//! each coded as B's word minus its prediction, and bytes of B's own between them. The copies
//! are found from exact matches of whole words (hashed at every word of A), then extended over
//! approximate matches both ways and merged where of one offset. A word of A is predicted to
//! become what it last became (`AQR_DELTA_TABLE`: 2^n entries by its hash, 0 for a map). The
//! differences are written dense, and sparse (the gaps between words not 0, and those words);
//! the streams are compressed by zstd and decoded back to B.
//!
//! With `-p`, B is cut into pieces of `AQR_DELTA_PB` MiB (8), each coded against the
//! `AQR_DELTA_K` pieces (1) of `AQR_DELTA_PA` MiB (8) of A that its matches fall in the most.
//!
//! usage: aqr-exe-bench-delta [-p] A B (streams, such as aqr-exe-bench-remap writes)

use std::time::Instant;

/// Bytes of a word: matches are of whole words, at word boundaries.
const WORD: usize = 4;
/// Words hashed for a match.
const SEED: usize = 4;
/// Exact matches shorter than this (in words) are not taken as anchors.
const MIN_MATCH: usize = 6;
/// Copies of one offset at most this far apart are one, the bytes between them differences.
const MERGE_GAP: usize = 64;

struct Index {
    bits: u32,
    ways: usize,
    table: Vec<u32>,
}

fn word(d: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(d[i * WORD..i * WORD + 4].try_into().unwrap())
}

fn hash(d: &[u8], i: usize, bits: u32) -> usize {
    let mut h = 0u64;
    for k in 0..SEED {
        h = (h ^ word(d, i + k) as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
    (h >> (64 - bits)) as usize
}

impl Index {
    fn new(a: &[u8]) -> Index {
        let n = a.len() / WORD;
        let bits = (usize::BITS - n.max(1 << 12).leading_zeros()).min(28);
        let ways = 4;
        let mut table = vec![u32::MAX; (1 << bits) * ways];
        let mut fill = vec![0u8; 1 << bits];
        for i in 0..n.saturating_sub(SEED - 1) {
            let h = hash(a, i, bits);
            let f = &mut fill[h];
            // the most recent ways positions, round robin
            table[h * ways + (*f as usize % ways)] = i as u32;
            *f = f.wrapping_add(1);
        }
        Index { bits, ways, table }
    }
}

/// A copy: B's bytes [b, b + len) against A's at b + off.
#[derive(Clone, Copy, Debug)]
struct Copy {
    b: usize,
    len: usize,
    off: i64,
}

/// Exact matches of whole words, greedily left to right (in words).
fn anchors(a: &[u8], b: &[u8], idx: &Index) -> Vec<Copy> {
    let (na, nb) = (a.len() / WORD, b.len() / WORD);
    let mut out: Vec<Copy> = Vec::new();
    let mut i = 0;
    let mut last: Option<i64> = None;
    while i + SEED <= nb {
        let mut best = (0, 0i64, 0);
        // the last offset first (as LZ's repeat offsets)
        let mut cands: Vec<i64> = last.into_iter().collect();
        let h = hash(b, i, idx.bits);
        for &p in &idx.table[h * idx.ways..(h + 1) * idx.ways] {
            if p != u32::MAX {
                cands.push(p as i64 - i as i64);
            }
        }
        let floor = out.last().map_or(0, |c| (c.b + c.len) / WORD);
        for off in cands {
            let j = i as i64 + off;
            if j < 0 || j as usize >= na {
                continue;
            }
            let j = j as usize;
            let mut f = 0;
            while i + f < nb && j + f < na && word(b, i + f) == word(a, j + f) {
                f += 1;
            }
            if f == 0 {
                continue;
            }
            let mut k = 0;
            while i - k > floor && j > k && word(b, i - k - 1) == word(a, j - k - 1) {
                k += 1;
            }
            if f + k > best.0 {
                best = (f + k, off, k);
            }
        }
        if best.0 >= MIN_MATCH {
            let start = i - best.2;
            out.push(Copy { b: start * WORD, len: best.0 * WORD, off: best.1 * WORD as i64 });
            last = Some(best.1);
            i = start + best.0;
        } else {
            i += 1;
        }
    }
    out
}

/// The copies extended over approximate matches into the gaps between them (bytes): each
/// backwards to the end of the one before (extended), then forwards to the next anchor, as far
/// as matching bytes outnumber the others the most.
fn extend(a: &[u8], b: &[u8], anchors: &[Copy]) -> Vec<Copy> {
    // (in words: the anchors are of whole words, their offsets too)
    let at = |p: usize, off: i64| -> Option<u32> {
        let j = (p * WORD) as i64 + off;
        (j >= 0 && j as usize + WORD <= a.len()).then(|| word(a, j as usize / WORD))
    };
    let nb = b.len() / WORD;
    let mut out: Vec<Copy> = Vec::with_capacity(anchors.len());
    for (k, c) in anchors.iter().enumerate() {
        let (mut s, mut e) = (c.b / WORD, (c.b + c.len) / WORD);
        // backwards into the gap after the previous copy
        let lo = out.last().map_or(0, |p: &Copy| (p.b + p.len) / WORD);
        let (mut score, mut best, mut len) = (0i64, 0i64, 0);
        let mut p = s;
        while p > lo {
            p -= 1;
            score += if at(p, c.off) == Some(word(b, p)) { 1 } else { -1 };
            if score > best {
                (best, len) = (score, s - p);
            }
        }
        s -= len;
        // forwards into the gap before the next anchor
        let hi = anchors.get(k + 1).map_or(nb, |n| n.b / WORD);
        let (mut score, mut best, mut len) = (0i64, 0i64, 0);
        let mut p = e;
        while p < hi {
            score += if at(p, c.off) == Some(word(b, p)) { 1 } else { -1 };
            p += 1;
            if score > best {
                (best, len) = (score, p - e);
            }
        }
        e += len;
        out.push(Copy { b: s * WORD, len: (e - s) * WORD, off: c.off });
    }
    out.retain(|c| c.len > 0);
    // copies of one offset with nothing between them are one (the matches were cut where a word
    // differs)
    let mut merged: Vec<Copy> = Vec::with_capacity(out.len());
    for c in out {
        match merged.last_mut() {
            Some(p) if p.off == c.off && c.b - (p.b + p.len) <= MERGE_GAP => p.len = c.b + c.len - p.b,
            _ => merged.push(c),
        }
    }
    merged
}

struct Coded {
    ctrl: Vec<u8>,
    diff: Vec<u8>,
    extra: Vec<u8>,
}

fn varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn read_varint(d: &[u8], at: &mut usize) -> u64 {
    let (mut v, mut s) = (0u64, 0);
    loop {
        let x = d[*at];
        *at += 1;
        v |= ((x & 0x7f) as u64) << s;
        if x < 0x80 {
            return v;
        }
        s += 7;
    }
}

/// What each word of A was last copied as, as the prediction of the next copy of it (data
/// moved within a page changes every instruction addressing it the same way): a table of
/// 2^AQR_DELTA_TABLE entries by a hash of the word, each the word and what it became (one
/// entry a word, the last one in it kept); 0 for a map of every word.
struct Predict {
    bits: u32,
    table: Vec<(u32, u32)>,
    map: std::collections::HashMap<u32, u32>,
}

impl Default for Predict {
    fn default() -> Predict {
        let bits = std::env::var("AQR_DELTA_TABLE").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
        // (an entry of word 0 standing for 0 itself: as no entry)
        Predict { bits, table: vec![(0, 0); if bits > 0 { 1 << bits } else { 0 }], map: Default::default() }
    }
}

impl Predict {
    fn slot(&self, x: u32) -> usize {
        (x.wrapping_mul(0x9E37_79B1) >> (32 - self.bits)) as usize
    }
    fn get(&self, x: u32) -> u32 {
        if !PREDICT {
            return x;
        }
        if self.bits == 0 {
            return self.map.get(&x).copied().unwrap_or(x);
        }
        let (k, v) = self.table[self.slot(x)];
        if k == x { v } else { x }
    }
    fn set(&mut self, x: u32, y: u32) {
        if !PREDICT {
            return;
        }
        if self.bits == 0 {
            self.map.insert(x, y);
        } else if x != y || self.table[self.slot(x)].0 == x {
            // (a word copied as it is takes no entry, unless it had one)
            let s = self.slot(x);
            self.table[s] = (x, y);
        }
    }
}

const PREDICT: bool = true;

fn code(a: &[u8], b: &[u8], copies: &[Copy]) -> Coded {
    let mut co = Coded { ctrl: Vec::new(), diff: Vec::new(), extra: Vec::new() };
    let (mut pos, mut last_off) = (0usize, 0i64);
    let mut pr = Predict::default();
    for c in copies {
        // extras before the copy
        varint(&mut co.ctrl, (c.b - pos) as u64);
        co.extra.extend_from_slice(&b[pos..c.b]);
        let d = c.off - last_off;
        varint(&mut co.ctrl, ((d << 1) ^ (d >> 63)) as u64);
        varint(&mut co.ctrl, (c.len / WORD) as u64);
        for i in c.b / WORD..(c.b + c.len) / WORD {
            let x = word(a, ((i * WORD) as i64 + c.off) as usize / WORD);
            let y = word(b, i);
            co.diff.extend_from_slice(&y.wrapping_sub(pr.get(x)).to_le_bytes());
            pr.set(x, y);
        }
        (pos, last_off) = (c.b + c.len, c.off);
    }
    varint(&mut co.ctrl, (b.len() - pos) as u64);
    co.extra.extend_from_slice(&b[pos..]);
    co
}

fn decode(a: &[u8], co: &Coded, n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(n);
    let (mut c, mut d, mut e, mut off) = (0, 0, 0, 0i64);
    let mut pr = Predict::default();
    loop {
        let x = read_varint(&co.ctrl, &mut c) as usize;
        out.extend_from_slice(&co.extra[e..e + x]);
        e += x;
        if c == co.ctrl.len() {
            return out;
        }
        let z = read_varint(&co.ctrl, &mut c) as i64;
        off += (z >> 1) ^ -(z & 1);
        let len = read_varint(&co.ctrl, &mut c) as usize;
        for _ in 0..len {
            let x = word(a, ((out.len()) as i64 + off) as usize / WORD);
            let y = word(&co.diff, d / WORD).wrapping_add(pr.get(x));
            pr.set(x, y);
            out.extend_from_slice(&y.to_le_bytes());
            d += WORD;
        }
    }
}

fn stats(copies: &[Copy]) {
    let n = copies.len();
    let (mut same, mut back, mut small) = (0, 0, 0);
    let mut lens = copies.iter().map(|c| c.len).collect::<Vec<_>>();
    lens.sort();
    let mut gaps = Vec::new();
    for w in copies.windows(3) {
        let d = w[2].off - w[1].off;
        if d == 0 {
            same += 1;
        } else if w[2].off == w[0].off {
            back += 1;
        } else if d.abs() <= 64 {
            small += 1;
        }
        gaps.push(w[2].b - (w[1].b + w[1].len));
    }
    gaps.sort();
    let zero = gaps.iter().filter(|&&g| g == 0).count();
    eprintln!(
        "copies {n}: offset same {same}, back to the one before {back}, within 64 {small}; length median {} \
         p10 {}; gaps zero {zero}, median {}",
        lens[n / 2],
        lens[n / 10],
        gaps.get(gaps.len() / 2).copied().unwrap_or(0)
    );
}

/// The words of the copies that differ, by B's top byte (an ARM64 instruction's class), and
/// the costs of their differences.
fn classes(a: &[u8], b: &[u8], copies: &[Copy]) {
    let mut by = vec![(0usize, Vec::new()); 256];
    let mut words = 0;
    for c in copies {
        let (s, e) = (c.b.div_ceil(WORD), (c.b + c.len) / WORD);
        for i in s..e {
            words += 1;
            let j = (i * WORD) as i64 + c.off;
            let (x, y) = (word(b, i), u32::from_le_bytes(a[j as usize..j as usize + 4].try_into().unwrap()));
            if x != y {
                let k = (x >> 24) as usize;
                by[k].0 += 1;
                by[k].1.extend_from_slice(&x.wrapping_sub(y).to_le_bytes());
            }
        }
    }
    // the last word of B each word of A became, as a prediction of the next one
    let mut last: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    let (mut miss, mut diff_hit, mut same_miss) = (0, 0, 0);
    for c in copies {
        for i in c.b.div_ceil(WORD)..(c.b + c.len) / WORD {
            let j = ((i * WORD) as i64 + c.off) as usize;
            let (x, y) = (word(b, i), u32::from_le_bytes(a[j..j + 4].try_into().unwrap()));
            let pred = last.get(&y).copied().unwrap_or(y);
            if pred != x {
                miss += 1;
                same_miss += (x == y) as usize;
            } else if x != y {
                diff_hit += 1;
            }
            last.insert(y, x);
        }
    }
    eprintln!(
        "predicted by A's word's last change: misses {miss} (of words alike {same_miss}), differing hit {diff_hit}"
    );
    let total: usize = by.iter().map(|b| b.0).sum();
    let mut v: Vec<_> = by.into_iter().enumerate().filter(|(_, b)| b.0 > 0).collect();
    v.sort_by_key(|(_, b)| std::cmp::Reverse(b.0));
    eprintln!("words copied {words}, differing {total} ({:.2}%)", 100.0 * total as f64 / words as f64);
    for (k, (n, d)) in v.iter().take(16) {
        eprintln!("  {k:02x}: {n:8} words, differences {:.3} MB (zstd 19)", mb(zstd(d, 19)));
    }
}

fn zstd(d: &[u8], level: i32) -> usize {
    let mut c = zstd::bulk::Compressor::new(level).unwrap();
    c.set_parameter(zstd::zstd_safe::CParameter::WindowLog(27)).unwrap();
    c.set_parameter(zstd::zstd_safe::CParameter::EnableLongDistanceMatching(true)).unwrap();
    c.compress(d).unwrap().len()
}

fn mb(n: usize) -> f64 {
    n as f64 / 1e6
}

/// B against A: the three streams and the time taken.
fn delta(a: &[u8], b: &[u8]) -> (Coded, f64) {
    let t = Instant::now();
    let idx = Index::new(a);
    let anc = anchors(a, b, &idx);
    let copies = extend(a, b, &anc);
    if std::env::var_os("AQR_DELTA_STATS").is_some() {
        stats(&copies);
        classes(a, b, &copies);
    }
    let co = code(a, b, &copies);
    let secs = t.elapsed().as_secs_f64();
    let t = Instant::now();
    let back = decode(a, &co, b.len());
    DECODE.with(|d| d.set(d.get() + t.elapsed().as_secs_f64()));
    assert!(back == b, "decoded back");
    (co, secs)
}

thread_local! {
    static DECODE: std::cell::Cell<f64> = const { std::cell::Cell::new(0.0) };
}

fn report(label: &str, parts: &[Coded], secs: f64, n: usize) {
    let cat = |f: fn(&Coded) -> &Vec<u8>| parts.iter().flat_map(|c| f(c).iter().copied()).collect::<Vec<u8>>();
    let (ctrl, diff, extra) = (cat(|c| &c.ctrl), cat(|c| &c.diff), cat(|c| &c.extra));
    let z = |d: &[u8], l| zstd(d, l);
    let (c19, d7, e7, d19, e19) = (z(&ctrl, 19), z(&diff, 7), z(&extra, 7), z(&diff, 19), z(&extra, 19));
    // the differences sparse: the gaps between words that are not 0, and those words
    let (mut gaps, mut vals, mut run) = (Vec::new(), Vec::new(), 0u64);
    for w in diff.chunks(WORD) {
        if w.iter().all(|&x| x == 0) {
            run += 1;
        } else {
            varint(&mut gaps, run);
            vals.extend_from_slice(w);
            run = 0;
        }
    }
    // values by byte planes
    let planes: Vec<u8> = (0..WORD).flat_map(|k| vals.chunks(WORD).map(move |w| w[k])).collect();
    println!(
        "  sparse: {} words, gaps {:.2} + values {:.2} (planes {:.2}) MB (zstd 19); total ctrl + sparse + extra {:.2} MB",
        vals.len() / WORD,
        mb(z(&gaps, 19)),
        mb(z(&vals, 19)),
        mb(z(&planes, 19)),
        mb(c19 + z(&gaps, 19) + z(&vals, 19) + e19),
    );
    println!(
        "{label}: copied {:.1}%, ctrl {:.2} diff {:.2} extra {:.2} MB raw; zstd 7: {:.2} + {:.2} + {:.2} = {:.2} MB; \
         19: {:.2} MB; {:.0} MB/s, decode {:.0} MB/s",
        100.0 * diff.len() as f64 / n as f64,
        mb(ctrl.len()),
        mb(diff.len()),
        mb(extra.len()),
        mb(c19),
        mb(d7),
        mb(e7),
        mb(c19 + d7 + e7),
        mb(c19 + d19 + e19),
        n as f64 / secs / 1e6,
        n as f64 / DECODE.with(|d| d.get()) / 1e6,
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (pieces, files) = match &args[..] {
        [p, rest @ ..] if p == "-p" => (true, rest),
        rest => (false, rest),
    };
    let [a, b] = files else { panic!("usage: aqr-exe-bench-delta [-p] A B") };
    let (a, b) = (std::fs::read(a).unwrap(), std::fs::read(b).unwrap());
    if !pieces {
        let (co, secs) = delta(&a, &b);
        report("whole", &[co], secs, b.len());
        return;
    }
    // each piece of B against the k pieces of A its matches fall in the most, in A's order (A's
    // index whole, only to pair them); pieces of AQR_DELTA_PB and AQR_DELTA_PA MiB
    let env = |k: &str, d: usize| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
    let (pb, pa, kk) = (env("AQR_DELTA_PB", 8) << 20, env("AQR_DELTA_PA", 8) << 20, env("AQR_DELTA_K", 1));
    let t = Instant::now();
    let idx = Index::new(&a);
    let anc = anchors(&a, &b, &idx);
    let na = a.len().div_ceil(pa);
    let mut parts = Vec::new();
    let mut secs = t.elapsed().as_secs_f64();
    for (k, bp) in b.chunks(pb).enumerate() {
        let (s, e) = (k * pb, k * pb + bp.len());
        let mut votes = vec![0usize; na];
        for c in anc.iter().filter(|c| c.b >= s && c.b < e) {
            let j = (c.b as i64 + c.off) as usize / pa;
            votes[j.min(na - 1)] += c.len;
        }
        let mut order: Vec<usize> = (0..na).collect();
        order.sort_by_key(|&j| std::cmp::Reverse(votes[j]));
        let mut chosen: Vec<usize> = order.into_iter().take(kk).filter(|&j| votes[j] > 0).collect();
        chosen.sort();
        let ap: Vec<u8> = chosen.iter().flat_map(|&j| a[j * pa..((j + 1) * pa).min(a.len())].iter().copied()).collect();
        let (co, t) = delta(&ap, bp);
        secs += t;
        parts.push(co);
    }
    report("pieces", &parts, secs, b.len());
}
