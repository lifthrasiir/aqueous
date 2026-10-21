//! Cascade predictors on FLAC residuals: ideal adaptive code lengths of the final error.
//! usage: exp2 CONFIG FILE...   CONFIG e.g. "ss16:8,ss16:4" or "nl32:x" (see `Stage`).
#![allow(clippy::needless_range_loop)]
use aqr_flac::{bits, flac};

struct Adaptive {
    n: usize,
    f: Vec<u32>,
    tot: Vec<u32>,
}

impl Adaptive {
    fn new(ctxs: usize, n: usize) -> Self {
        Adaptive { n, f: vec![1; ctxs * n], tot: vec![n as u32; ctxs] }
    }
    fn code(&mut self, c: usize, s: usize) -> f64 {
        let i = c * self.n + s;
        let cost = -((self.f[i] as f64) / (self.tot[c] as f64)).log2();
        self.f[i] += 32;
        self.tot[c] += 32;
        if self.tot[c] > 1 << 16 {
            let mut t = 0;
            for x in &mut self.f[c * self.n..(c + 1) * self.n] {
                *x = (*x).div_ceil(2);
                t += *x;
            }
            self.tot[c] = t;
        }
        cost
    }
}

fn bitlen(v: u32) -> u32 {
    32 - v.leading_zeros()
}

const NTOK: usize = 40;

/// Entropy coder stand-in: scale-context token + raw bits + sign.
struct Ent {
    tok: Adaptive,
    a: [u32; 2],
    fast: [u32; 2],
    mode: u32,
}
impl Ent {
    fn new() -> Self {
        Ent {
            tok: Adaptive::new(64 * 2 * 3 * 4, NTOK),
            a: [16 << 6; 2],
            fast: [16 << 6; 2],
            mode: std::env::var("EMODE").map(|v| v.parse().unwrap()).unwrap_or(0),
        }
    }
    fn code(&mut self, c: usize, e: i32, xc: usize, k: u32) -> f64 {
        let m = e.unsigned_abs();
        let a = (self.a[c] >> 2).max(1);
        let fa = (self.fast[c] >> 2).max(1);
        let rc = match self.mode {
            1 | 3 => {
                if fa * 2 < a {
                    0
                } else if fa > a * 2 {
                    2
                } else {
                    1
                }
            }
            2 => {
                if fa * 3 < a * 2 {
                    0
                } else if fa * 2 > a * 3 {
                    2
                } else {
                    1
                }
            }
            4 => {
                let la = 32 - (a >> 4).max(1).leading_zeros();
                if k + 1 < la {
                    0
                } else if k > la + 1 {
                    2
                } else {
                    1
                }
            }
            5 => (k.min(3)) as usize % 3,
            _ => 0,
        };
        let l = bitlen(a) - 1;
        let frac = if l >= 1 { (a >> (l - 1)) & 1 } else { 0 };
        let ctx = ((l * 2 + frac) as usize).min(63);
        let s = (l as i32 - 4 - 2).max(0) as u32;
        let v = m >> s;
        let mut bits = s as f64 + (m != 0) as u32 as f64;
        let t = if v < 24 {
            v as usize
        } else {
            let l = bitlen(v);
            bits += (l - 2) as f64;
            ((24 + (l - 5) * 2 + ((v >> (l - 2)) & 1)) as usize).min(NTOK - 1)
        };
        let sh = if self.mode == 3 { 6 } else { 4 };
        self.a[c] = self.a[c] - (self.a[c] >> sh) + (m.min(1 << 24) << (sh - 2));
        self.fast[c] = self.fast[c] - (self.fast[c] >> 1) + (m.min(1 << 24) << 5);
        bits + self.tok.code(((ctx * 2 + c) * 3 + xc) * 4 + rc, t)
    }
}

#[derive(Clone)]
enum Stage {
    /// Sign-sign LMS: taps, step, extra cross taps (channel 1 sees channel 0 at t, t-1).
    Ss {
        n: usize,
        mu: i32,
        cross: bool,
        w: Vec<i32>,
        h: Vec<i32>,
    },
    /// Monkey's Audio style: weights move by a magnitude-graded input sign times sign(err).
    /// Sign-sign LMS with i16 weights and clamped i16 history, i32 wrapping accumulator.
    Q {
        n: usize,
        mu: i16,
        w: Vec<i16>,
        h: Vec<i16>,
        d: usize,
    },
    /// Delayed update: the step of sample t is applied after sample t + delay.
    D {
        n: usize,
        mu: i16,
        w: Vec<i16>,
        h: Vec<i16>,
        pend: Vec<(i16, Vec<i16>)>,
        delay: usize,
    },
    /// Float NLMS (upper bound reference): taps, mu in 1/1000.
    Fl {
        n: usize,
        mu: f64,
        cross: bool,
        w: Vec<f64>,
        h: Vec<f64>,
        pw: f64,
    },
    Nl {
        n: usize,
        shift: u32,
        cross: bool,
        w: Vec<i32>,
        h: Vec<i32>,
        d: Vec<i32>,
        avg: i32,
    },
}

fn parse_cfg(s: &str) -> Vec<Stage> {
    s.split(',')
        .filter(|x| !x.is_empty())
        .map(|x| {
            let cross = x.ends_with('x');
            let x = x.trim_end_matches('x');
            let (kind, rest) = x.split_at(2);
            let mut it = rest.split(':');
            let n: usize = it.next().unwrap().parse().unwrap();
            let p: i32 = it.next().map(|v| v.parse().unwrap()).unwrap_or(8);
            let nn = n + if cross { 2 } else { 0 };
            match kind {
                "ss" => Stage::Ss { n, mu: p, cross, w: vec![0; nn], h: vec![0; nn] },
                "qq" => Stage::Q { n, mu: p as i16, w: vec![0; n], h: vec![0; n], d: 0 },
                "q1" => Stage::D { n, mu: p as i16, w: vec![0; n], h: vec![0; n + 4], pend: Vec::new(), delay: 1 },
                "q2" => Stage::D { n, mu: p as i16, w: vec![0; n], h: vec![0; n + 4], pend: Vec::new(), delay: 2 },
                "qd" => Stage::Q { n, mu: p as i16, w: vec![0; n], h: vec![0; n + 1], d: 1 },
                "fl" => Stage::Fl { n, mu: p as f64 / 1000.0, cross, w: vec![0.0; nn], h: vec![0.0; nn], pw: 1.0 },
                "nl" => Stage::Nl { n, shift: p as u32, cross, w: vec![0; nn], h: vec![0; nn], d: vec![0; nn], avg: 0 },
                _ => panic!("stage {kind}"),
            }
        })
        .collect()
}

impl Stage {
    /// Returns the error; `other` is channel 0's stage input at t and t-1 (for cross taps).
    fn run(&mut self, x: i32, other: Option<(i32, i32)>) -> i32 {
        match self {
            Stage::Ss { n, mu, cross, w, h } => {
                let nn = *n + if *cross { 2 } else { 0 };
                if *cross {
                    let (a, b) = other.unwrap_or((0, 0));
                    h[*n] = a;
                    h[*n + 1] = b;
                }
                let mut acc = 0i64;
                for j in 0..nn {
                    acc += w[j] as i64 * h[j] as i64;
                }
                let e = x.wrapping_sub((acc >> 12) as i32);
                let sg = e.signum() * *mu;
                for j in 0..nn {
                    w[j] += sg * h[j].signum();
                }
                h.copy_within(0..*n - 1, 1);
                h[0] = x;
                e
            }
            Stage::Q { n, mu, w, h, d } => {
                let mut acc = 0i32;
                for j in 0..*n {
                    acc = acc.wrapping_add(w[j] as i32 * h[j + *d] as i32);
                }
                let e = x.wrapping_sub(acc >> 12);
                let sg = (e.signum() as i16) * *mu;
                for j in 0..*n {
                    w[j] = w[j].wrapping_add(sg * h[j + *d].signum());
                }
                let l = h.len();
                h.copy_within(0..l - 1, 1);
                h[0] = x.clamp(-32768, 32767) as i16;
                e
            }
            Stage::D { n, mu, w, h, pend, delay } => {
                let mut acc = 0i32;
                for j in 0..*n {
                    acc = acc.wrapping_add(w[j] as i32 * h[j] as i32);
                }
                let e = x.wrapping_sub(acc >> 12);
                pend.push(((e.signum() as i16) * *mu, h[..*n].iter().map(|v| v.signum()).collect()));
                if pend.len() > *delay {
                    let (sg, sh) = pend.remove(0);
                    for j in 0..*n {
                        w[j] = w[j].wrapping_add(sg * sh[j]);
                    }
                }
                let l = h.len();
                h.copy_within(0..l - 1, 1);
                h[0] = x.clamp(-32768, 32767) as i16;
                e
            }
            Stage::Fl { n, mu, cross, w, h, pw } => {
                let nn = *n + if *cross { 2 } else { 0 };
                if *cross {
                    let (a, b) = other.unwrap_or((0, 0));
                    h[*n] = a as f64;
                    h[*n + 1] = b as f64;
                }
                let mut acc = 0.0;
                for j in 0..nn {
                    acc += w[j] * h[j];
                }
                let e = x.wrapping_sub(acc.round() as i32);
                let norm: f64 = h.iter().map(|v| v * v).sum::<f64>() + 1.0;
                let g = *mu * e as f64 / norm;
                for j in 0..nn {
                    w[j] += g * h[j];
                }
                let _ = pw;
                h.copy_within(0..*n - 1, 1);
                h[0] = x as f64;
                e
            }
            Stage::Nl { n, shift, cross, w, h, d, avg } => {
                let nn = *n + if *cross { 2 } else { 0 };
                if *cross {
                    let (a, b) = other.unwrap_or((0, 0));
                    h[*n] = a.clamp(-32768, 32767);
                    h[*n + 1] = b.clamp(-32768, 32767);
                    d[*n] = grade(a, *avg);
                    d[*n + 1] = grade(b, *avg);
                }
                let mut acc = 0i64;
                for j in 0..nn {
                    acc += w[j] as i64 * h[j] as i64;
                }
                let e = x.wrapping_sub((acc >> *shift) as i32);
                let sg = e.signum();
                for j in 0..nn {
                    w[j] += sg * d[j];
                }
                let xs = x.clamp(-32768, 32767);
                let g = grade(x, *avg);
                let ax = x.unsigned_abs().min(1 << 20) as i32;
                *avg += (ax - *avg) / 16;
                h.copy_within(0..*n - 1, 1);
                d.copy_within(0..*n - 1, 1);
                h[0] = xs;
                d[0] = g;
                e
            }
        }
    }
}

fn grade(x: i32, avg: i32) -> i32 {
    let ax = x.unsigned_abs() as i64;
    let a = avg as i64;
    let m = if ax > a * 3 {
        32
    } else if ax > a * 4 / 3 {
        16
    } else if ax > 0 {
        8
    } else {
        0
    };
    if x < 0 { -m } else { m }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cfg = parse_cfg(&args[0]);
    let (mut base, mut cost, mut tot) = (0f64, 0f64, 0usize);
    let xctx = std::env::var("XCTX").is_ok();
    for path in &args[1..] {
        let d = std::fs::read(path).unwrap();
        let Some((si, start)) = flac::parse_metadata(&d) else { continue };
        let pd = bits::padded(&d);
        tot += d.len();
        let mut p = start;
        let mut f = flac::Frame::default();
        let mut st = [cfg.clone(), cfg.clone()];
        let mut ent = Ent::new();
        let mut hist = [[0i64; 2]; 2];
        while p < d.len() {
            let Some(e) = flac::parse_frame(&pd, p, d.len(), si.bps, &mut f) else {
                p += 1;
                continue;
            };
            p = e;
            let nch = f.channels.min(2);
            for ch in 0..f.channels {
                let s = &f.sub[ch];
                if !s.has_residual() {
                    continue;
                }
                let ps = f.block_size >> s.porder;
                base += (s.params.len() * (4 + s.method as usize)) as f64 * 0.0;
                for (t, &r) in f.res[ch].iter().enumerate() {
                    let k = s.params[(t + s.order as usize) / ps];
                    let u = ((r << 1) ^ (r >> 31)) as u32;
                    base += if k & flac::ESCAPE != 0 { (k & 31) as f64 } else { ((u >> k) + 1 + k as u32) as f64 };
                }
            }
            // Sample domain: mid/side of the decoded samples, fixed order-2 residual.
            let sm = std::env::var("SAMPLES").ok().map(|v| v.parse::<u32>().unwrap());
            let mut sres: [Vec<i32>; 2] = [Vec::new(), Vec::new()];
            if let Some(order) = sm {
                let mut ch = [Vec::new(), Vec::new()];
                for c in 0..nch {
                    flac::decode_channel(&f, c, &mut ch[c]);
                }
                if nch == 2 {
                    let (a, b) = (ch[0].clone(), ch[1].clone());
                    for i in 0..f.block_size {
                        let (l, r) = match f.assign {
                            8 => (a[i], a[i] - b[i]),
                            9 => (a[i] + b[i], b[i]),
                            10 => {
                                let m = a[i] << 1 | (b[i] & 1);
                                ((m + b[i]) >> 1, (m - b[i]) >> 1)
                            }
                            _ => (a[i], b[i]),
                        };
                        ch[0][i] = (l + r) >> 1;
                        ch[1][i] = l - r;
                    }
                }
                for c in 0..nch {
                    for i in 0..f.block_size {
                        let x = ch[c][i];
                        let h = &mut hist[c];
                        let p = match order {
                            0 => 0,
                            1 => h[0],
                            _ => 2 * h[0] - h[1],
                        };
                        sres[c].push((x - p) as i32);
                        h[1] = h[0];
                        h[0] = x;
                    }
                }
            }
            // Time order, channels interleaved; stage inputs of channel 0 feed channel 1.
            let mut prev0 = (0i32, 0i32);
            for t in 0..f.block_size {
                let mut cur0 = None;
                let mut m0 = 0u32;
                for c in 0..nch {
                    let s = &f.sub[c];
                    let mut x;
                    if sm.is_some() {
                        x = sres[c][t];
                    } else {
                        if !s.has_residual() || t < s.order as usize {
                            continue;
                        }
                        x = f.res[c][t - s.order as usize];
                    }
                    let mut first = true;
                    for stg in st[c].iter_mut() {
                        let o = if c == 1 && first { cur0.map(|v| (v, prev0.0)) } else { None };
                        x = stg.run(x, o);
                        first = false;
                    }
                    let xc = if xctx && c == 1 { 1 + (m0 > (ent.a[0] >> 6)) as usize } else { 0 };
                    let k = if s.has_residual() {
                        let ps = f.block_size >> s.porder;
                        let kk = s.params[t / ps];
                        if kk & flac::ESCAPE != 0 { 20 } else { kk as u32 }
                    } else {
                        0
                    };
                    cost += ent.code(c, x, xc, k);
                    if c == 0 {
                        cur0 = Some(if sm.is_some() { sres[0][t] } else { f.res[0][t - s.order as usize] });
                        m0 = x.unsigned_abs();
                    }
                }
                if let Some(v) = cur0 {
                    prev0 = (v, prev0.0);
                }
            }
        }
    }
    println!(
        "{:24} residual {:.0} B -> {:.0} B: {:+.2}% of residual, {:+.2}% of input",
        args[0],
        base / 8.0,
        cost / 8.0,
        (cost / base - 1.0) * 100.0,
        (cost - base) / 8.0 / tot as f64 * 100.0
    );
}
