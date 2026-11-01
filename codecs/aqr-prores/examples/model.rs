//! Estimates what models of the coefficients would cost: frames sampled evenly from each file,
//! their symbols counted by context, and the static entropy of those counts (tables not
//! counted) plus the raw bits, against the components' coded bytes.
//!
//! ```sh
//! cargo run --release -p aqr-prores --example aqr-prores-model -- [-n FRAMES] FILE_OR_DIR...
//! ```
//!
//! Variants by environment: `PLACE=1` an alternative block placement, `DC=0|1|2` the DC
//! predictor (left, median, average), `NBR=0` no neighbour context for AC.

use std::path::{Path, PathBuf};

use aqr_prores::frame::Frame;
use aqr_prores::plane::{Plane, planes};
use rayon::prelude::*;

fn walk(p: &Path, out: &mut Vec<PathBuf>) {
    if p.is_dir() {
        let mut es: Vec<_> = std::fs::read_dir(p).into_iter().flatten().flatten().map(|e| e.path()).collect();
        es.sort();
        es.iter().for_each(|e| walk(e, out));
    } else {
        out.push(p.to_owned());
    }
}

fn env(k: &str, d: usize) -> usize {
    std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d)
}

const CAT: usize = 17;

fn cat(v: i32) -> usize {
    (32 - v.unsigned_abs().leading_zeros()) as usize
}

fn rem_class(r: usize) -> usize {
    match r {
        0..=4 => r.saturating_sub(1),
        5..=6 => 4,
        7..=8 => 5,
        9..=12 => 6,
        13..=16 => 7,
        17..=24 => 8,
        25..=32 => 9,
        _ => 10,
    }
}

const NREM: usize = 11;
const NNBR: usize = 20;

/// Counts of the symbols of one kind, by context: `alpha` symbols per context.
struct Tab {
    alpha: usize,
    c: Vec<u32>,
}

impl Tab {
    fn new(ctxs: usize, alpha: usize) -> Self {
        Tab { alpha, c: vec![0; ctxs * alpha] }
    }

    #[inline]
    fn add(&mut self, ctx: usize, sym: usize) {
        self.c[ctx * self.alpha + sym] += 1;
    }

    /// Static entropy, and a rough cost of the tables: 6 bits per symbol seen per context.
    fn bits(&self) -> (f64, f64) {
        let (mut e, mut t) = (0.0, 0.0);
        for h in self.c.chunks_exact(self.alpha) {
            let n: u32 = h.iter().sum();
            if n == 0 {
                continue;
            }
            let n = n as f64;
            for &c in h.iter().filter(|&&c| c > 0) {
                e -= c as f64 * (c as f64 / n).log2();
                t += 6.0;
            }
        }
        (e, t)
    }
}

struct Hist {
    /// nz, AC cats, DC cats, modelled mantissa bits, signs.
    t: [Tab; 5],
    raw: u64,
}

const NAMES: [&str; 5] = ["nz", "ac", "dc", "mant", "sign"];

impl Hist {
    fn new() -> Self {
        Hist {
            t: [
                Tab::new(2 * 16 * 4, 64),
                Tab::new(2 * 63 * NREM * NNBR * 8, CAT),
                Tab::new(2 * 16, CAT),
                Tab::new(2 * CAT * 4 * 8 * 4 * 3, 2),
                Tab::new(2 * 64 * 9, 2),
            ],
            raw: 0,
        }
    }

    fn add(&mut self, o: &Hist) {
        for (a, b) in self.t.iter_mut().zip(&o.t) {
            a.c.iter_mut().zip(&b.c).for_each(|(x, y)| *x += y);
        }
        self.raw += o.raw;
    }
}

/// ProRes's progressive scan: scan position -> raster position in the 8x8 block.
const SCAN: [usize; 64] = [
    0, 1, 8, 9, 2, 3, 10, 11, 16, 17, 24, 25, 18, 19, 26, 27, 4, 5, 12, 20, 13, 6, 7, 14, 21, 28, 29, 22, 15, 23, 30,
    31, 32, 33, 40, 48, 41, 34, 35, 42, 49, 56, 57, 50, 43, 36, 37, 44, 51, 58, 59, 52, 45, 38, 39, 46, 53, 60, 61, 54,
    47, 55, 62, 63,
];

/// For each scan position, the scan positions of the raster neighbours above and to the left
/// (64 when there is none or it comes later in the scan).
const FNBR: [[usize; 2]; 64] = {
    let mut inv = [0usize; 64];
    let mut k = 0;
    while k < 64 {
        inv[SCAN[k]] = k;
        k += 1;
    }
    let mut t = [[64usize; 2]; 64];
    let mut k = 0;
    while k < 64 {
        let r = SCAN[k];
        let (y, x) = (r / 8, r % 8);
        if y > 0 && inv[r - 8] < k {
            t[k][0] = inv[r - 8];
        }
        if x > 0 && inv[r - 1] < k {
            t[k][1] = inv[r - 1];
        }
        k += 1;
    }
    t
};

/// Half-octave buckets: 0, 1, 2, 3, 4-5, 6-7, 8-11, 12-15, ...
fn half_oct(e: u32) -> usize {
    if e < 4 {
        return e as usize;
    }
    let n = 31 - e.leading_zeros();
    (2 * n + ((e >> (n - 1)) & 1)) as usize
}

fn nz_bucket(n: usize) -> usize {
    match n {
        0..=7 => n,
        8..=11 => 8,
        12..=15 => 9,
        16..=23 => 10,
        24..=31 => 11,
        32..=39 => 12,
        40..=47 => 13,
        48..=55 => 14,
        _ => 15,
    }
}

#[derive(Clone, Copy)]
struct Opts {
    dc: usize,
    nbr: bool,
    prev: bool,
    xb: usize,
    sign: bool,
    nzq: bool,
    fnb: usize,
    tp: usize,
    tpdc: usize,
    tm: bool,
}

fn model(p: &Plane, prev_f: Option<&Plane>, cc: usize, h: &mut Hist, o: Opts) {
    let prev_f = prev_f.filter(|q| q.bw == p.bw && q.bh == p.bh);
    let (bw, bh) = (p.bw, p.bh);
    let mut nzs = vec![0usize; bw * bh];
    let mut res = vec![0usize; bw * bh];
    for by in 0..bh {
        for bx in 0..bw {
            let i = by * bw + bx;
            let b = &p.blocks[i];
            let a = (by > 0).then(|| &p.blocks[i - bw]);
            let l = (bx > 0).then(|| &p.blocks[i - 1]);
            // DC
            let dl = l.map(|l| l[0] as i32);
            let da = a.map(|a| a[0] as i32);
            let pred = match (dl, da) {
                (Some(l), Some(a)) => match o.dc {
                    0 => l,
                    1 => {
                        let al = p.blocks[i - bw - 1][0] as i32;
                        let g = l + a - al;
                        l.max(a).min(g).max(l.min(a))
                    }
                    _ => (l + a) >> 1,
                },
                (Some(l), None) => l,
                (None, Some(a)) => a,
                (None, None) => 0,
            };
            let pred = match prev_f {
                Some(q) if o.tpdc == 2 => {
                    let pq = |j: usize| p.blocks[j][0] as i32 - q.blocks[j][0] as i32;
                    let dpred = match (bx > 0, by > 0) {
                        (true, true) => {
                            let (l, a, al) = (pq(i - 1), pq(i - bw), pq(i - bw - 1));
                            l.max(a).min(l + a - al).max(l.min(a))
                        }
                        (true, false) => pq(i - 1),
                        (false, true) => pq(i - bw),
                        _ => 0,
                    };
                    q.blocks[i][0] as i32 + dpred
                }
                Some(q) if o.tpdc == 1 => q.blocks[i][0] as i32,
                _ => pred,
            };
            let r = b[0] as i32 - pred;
            let c = cat(r);
            let rl = if bx > 0 { res[i - 1] } else { 0 };
            let ra = if by > 0 { res[i - bw] } else { rl };
            let rl = if bx > 0 { rl } else { ra };
            h.t[2].add(cc * 16 + (rl + ra).div_ceil(2).min(15), c);
            h.raw += c as u64;
            res[i] = c;
            // nonzero count
            let n = b[1..].iter().filter(|&&v| v != 0).count();
            let nl = if bx > 0 { Some(nzs[i - 1]) } else { None };
            let na = if by > 0 { Some(nzs[i - bw]) } else { None };
            let avg = match (nl, na) {
                (Some(x), Some(y)) => (x + y).div_ceil(2),
                (Some(x), None) | (None, Some(x)) => x,
                _ => 0,
            };
            let qb = if o.nzq { (p.q[i] as usize).min(15) / 4 } else { 0 };
            h.t[0].add((cc * 16 + nz_bucket(avg)) * 4 + qb, n);
            nzs[i] = n;
            let mut rem = n;
            let mut prev = 0;
            for k in 1..64 {
                if rem == 0 {
                    break;
                }
                let v = b[k] as i32;
                let m = match (l, a) {
                    _ if !o.nbr => 0,
                    (Some(l), Some(a)) => (l[k] as i32).abs() + (a[k] as i32).abs(),
                    (Some(x), None) | (None, Some(x)) => 2 * (x[k] as i32).abs(),
                    _ => 0,
                };
                let mut e_sp = 0u32;
                let mut bb = [0i32; 65];
                bb[..64].iter_mut().zip(b.iter()).for_each(|(x, y)| *x = (*y as i32).abs());
                let f = bb[FNBR[k][0]] + bb[FNBR[k][1]];
                let (nb, pv) = match o.fnb {
                    1 => (cat(m).min(NNBR - 1), cat(f).min(3)),
                    2 => (cat(m + f).min(NNBR - 1), if o.prev { prev.min(3) } else { 0 }),
                    3 => (cat(m + 2 * f).min(NNBR - 1), cat(f).min(3)),
                    4 | 5 => {
                        let get = |blk: Option<&[i16; 64]>| blk.map(|x| (x[k] as i32).abs());
                        let al = (bx > 0 && by > 0).then(|| &p.blocks[i - bw - 1]);
                        let ar = (bx + 1 < bw && by > 0).then(|| &p.blocks[i - bw + 1]);
                        let (sa, sl) = (get(a), get(l));
                        let near = match (sa, sl) {
                            (Some(x), Some(y)) => 2 * (x + y),
                            (Some(x), None) | (None, Some(x)) => 4 * x,
                            _ => 0,
                        };
                        let far = match (get(al), get(ar)) {
                            (Some(x), Some(y)) => x + y,
                            (Some(x), None) | (None, Some(x)) => 2 * x,
                            _ => near / 2,
                        };
                        let e = (near + far) * if o.fnb == 5 { 1 } else { 2 } + 3 * f;
                        e_sp = e as u32 / 4;
                        (half_oct(e as u32).min(NNBR - 1), cat(f).min(3))
                    }
                    _ => (cat(m).min(NNBR - 1), if o.prev { prev.min(3) } else { 0 }),
                };
                let (nb, pv) = match prev_f {
                    Some(q) if o.tp == 1 => (nb, half_oct((q.blocks[i][k] as i32).unsigned_abs()).min(3)),
                    Some(q) if o.tp == 2 => (half_oct(2 * (q.blocks[i][k] as i32).unsigned_abs()).min(NNBR - 1), pv),
                    Some(q) if o.tp == 3 => (nb, half_oct((q.blocks[i][k] as i32).unsigned_abs()).min(7)),
                    Some(q) if o.tp == 4 => {
                        let t = (q.blocks[i][k] as i32).unsigned_abs();
                        (half_oct(2 * t + e_sp).min(NNBR - 1), half_oct(t).min(7))
                    }
                    _ => (nb, pv),
                };
                let c = cat(v);
                h.t[1].add((((cc * 63 + k - 1) * NREM + rem_class(rem)) * NNBR + nb) * 8 + pv, c);
                prev = c;
                if c > 0 {
                    rem -= 1;
                    // mantissa: the bits below the leading one, the first `xb` modelled
                    let mag = v.unsigned_abs();
                    let tq = match prev_f {
                        Some(q) if o.tm => q.blocks[i][k] as i32,
                        _ => 0,
                    };
                    let tc = cat(tq);
                    let band = [0, 1, 2, 3, 3, 4, 4, 4, 5, 5, 5, 5, 6, 6, 6, 6][(k - 1) / 4].min(7);
                    let mut ctx_bits = 1usize;
                    for j in 0..c - 1 {
                        let bit = (mag >> (c - 2 - j)) & 1;
                        if j < o.xb {
                            let pre = ctx_bits.min(3);
                            let tm = if tc == c { 1 + ((tq.unsigned_abs() >> (c - 2 - j)) & 1) as usize } else { 0 };
                            h.t[3].add(((((cc * CAT + c) * 4 + j.min(3)) * 8 + band) * 4 + pre) * 3 + tm, bit as usize);
                            ctx_bits = ctx_bits * 2 + bit as usize;
                        } else {
                            h.raw += 1;
                        }
                    }
                    if o.sign {
                        let ts = if tq == 0 {
                            0
                        } else if tq < 0 {
                            1
                        } else {
                            2
                        };
                        let tcm = if tq == 0 {
                            0
                        } else if tc == c {
                            1
                        } else {
                            2
                        };
                        h.t[4].add((cc * 64 + k) * 9 + ts * 3 + tcm, (v < 0) as usize);
                    } else {
                        h.raw += 1;
                    }
                }
            }
        }
    }
}

fn main() {
    let mut paths = Vec::new();
    let mut nframes = 12;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "-n" {
            nframes = args.next().unwrap().parse().unwrap();
        } else {
            walk(Path::new(&a), &mut paths);
        }
    }
    let o = Opts {
        dc: env("DC", 1),
        nbr: env("NBR", 1) != 0,
        prev: env("PREV", 0) != 0,
        xb: env("XB", 0),
        sign: env("SIGN", 0) != 0,
        nzq: env("NZQ", 0) != 0,
        fnb: env("FN", 0),
        tp: env("TP", 0),
        tpdc: env("TPDC", 0),
        tm: env("TM", 0) != 0,
    };
    let mut tot = [0f64; 3];
    for p in paths {
        let Ok(file) = std::fs::File::open(&p) else { continue };
        // SAFETY: the corpus is not written while it is read.
        let Ok(d) = (unsafe { memmap2::Mmap::map(&file) }) else { continue };
        let frames = aqr_prores::find(&d);
        if frames.is_empty() {
            continue;
        }
        let pick: Vec<usize> =
            (0..nframes.min(frames.len())).map(|k| frames[k * frames.len() / nframes.min(frames.len())]).collect();
        let parts: Vec<(Hist, usize, usize)> = pick
            .par_iter()
            .filter_map(|&at| {
                let f = Frame::parse(&d[at..]).ok()?;
                let pf = (o.tp > 0 || o.tpdc > 0 || o.tm)
                    .then(|| frames.iter().position(|&x| x == at).filter(|&j| j > 0))
                    .flatten()
                    .and_then(|j| Frame::parse(&d[frames[j - 1]..]).ok());
                let mut h = Hist::new();
                let mut coded = 0;
                for pic in &f.pictures {
                    let ws = pic.widths(&f.header);
                    let pp = pf.as_ref().and_then(|g| {
                        let gp = g.pictures.get(f.pictures.iter().position(|x| std::ptr::eq(x, pic))?)?;
                        Some(planes(gp, &g.header, &gp.widths(&g.header)))
                    });
                    for (k, pl) in planes(pic, &f.header, &ws).iter().enumerate() {
                        let q = pp.as_ref().map(|x| &x[k]).filter(|q| q.bw == pl.bw && q.bh == pl.bh);
                        if env("TD", 0) != 0
                            && let Some(q) = q
                        {
                            let d = Plane {
                                bw: pl.bw,
                                bh: pl.bh,
                                blocks: pl
                                    .blocks
                                    .iter()
                                    .zip(&q.blocks)
                                    .map(|(a, b)| std::array::from_fn(|j| a[j].wrapping_sub(b[j])))
                                    .collect(),
                                q: pl.q.clone(),
                            };
                            model(&d, Some(q), (k > 0) as usize, &mut h, o);
                            continue;
                        }
                        model(pl, q, (k > 0) as usize, &mut h, o);
                    }
                    coded += pic.slices.iter().map(|s| s.sizes.iter().sum::<usize>()).sum::<usize>();
                }
                let size = u32::from_be_bytes(d[at..at + 4].try_into().unwrap()) as usize;
                Some((h, coded, size))
            })
            .collect();
        let mut h = Hist::new();
        let (mut coded, mut size) = (0, 0);
        for (x, c, s) in &parts {
            h.add(x);
            coded += c;
            size += s;
        }
        // Tables are per file: their cost spread over all its frames.
        let scale = frames.len() as f64 / parts.len() as f64;
        let mut parts_b = Vec::new();
        let (mut e, mut t) = (h.raw as f64, 0.0);
        for (tab, name) in h.t.iter().zip(NAMES) {
            let (x, y) = tab.bits();
            e += x;
            t += y / scale;
            parts_b.push(format!("{name} {:.3}", x / 8.0 / coded as f64));
        }
        let est = (e + t) / 8.0;
        println!(
            "{}: {} frames, coded {:.1} MB -> {:.4} ({}, raw {:.3}, tables {:.4})",
            p.file_name().unwrap().to_string_lossy(),
            parts.len(),
            coded as f64 / 1e6,
            est / coded as f64,
            parts_b.join(" "),
            h.raw as f64 / 8.0 / coded as f64,
            t / 8.0 / coded as f64,
        );
        tot[0] += coded as f64;
        tot[1] += est;
        tot[2] += size as f64;
    }
    println!(
        "all: coded {:.1} MB -> {:.4}; frames {:.1} MB -> {:.4}",
        tot[0] / 1e6,
        tot[1] / tot[0],
        tot[2] / 1e6,
        (tot[2] - tot[0] + tot[1]) / tot[2]
    );
}
