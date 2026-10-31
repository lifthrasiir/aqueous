//! What the symbols after the BWT of bzip2 files would cost under adaptive models, against
//! bzip2's own Huffman coding of them: an experiment for storing streams without undoing the
//! BWT. Costs are ideal code lengths (an arithmetic coder comes within a few bytes).
//!
//! ```sh
//! cargo run --release -p aqr-bz2 --example aqr-bz2-bwt-model -- FILE...
//! ```

use aqr_bz2::stream;

/// An adaptive probability of a 1, its rate falling to 1/`LIMIT`.
#[derive(Clone, Copy)]
struct Counter {
    p: f32,
    n: u16,
}

fn limit() -> u16 {
    static L: std::sync::OnceLock<u16> = std::sync::OnceLock::new();
    *L.get_or_init(|| std::env::var("LIMIT").ok().and_then(|v| v.parse().ok()).unwrap_or(30))
}

impl Default for Counter {
    fn default() -> Self {
        Counter { p: 0.5, n: 0 }
    }
}

impl Counter {
    fn update(&mut self, bit: bool) {
        self.n = (self.n + 1).min(limit());
        self.p += ((bit as u8 as f32) - self.p) / (self.n as f32 + 0.5);
        self.p = self.p.clamp(1e-4, 1.0 - 1e-4);
    }
}

fn cost(p: f32, bit: bool) -> f64 {
    -(if bit { p } else { 1.0 - p } as f64).log2()
}

fn stretch(p: f32) -> f32 {
    (p / (1.0 - p)).ln()
}

fn squash(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// A logistic mix of `N` predictions, by a set of weights per context.
struct Mixer<const N: usize> {
    w: Vec<[f32; N]>,
}

impl<const N: usize> Mixer<N> {
    fn new(sets: usize) -> Self {
        Mixer { w: vec![[0.3; N]; sets] }
    }

    fn mix(&self, set: usize, x: &[f32; N]) -> f32 {
        squash(self.w[set].iter().zip(x).map(|(w, x)| w * x).sum::<f32>()).clamp(1e-4, 1.0 - 1e-4)
    }

    fn update(&mut self, set: usize, x: &[f32; N], p: f32, bit: bool, rate: f32) {
        let e = (bit as u8 as f32) - p;
        self.w[set].iter_mut().zip(x).for_each(|(w, x)| *w += rate * e * x);
    }
}

/// Order 0, 1 and 2 of the BWT's output itself, mixed, then refined by an SSE stage of order 1:
/// in the manner of bcm and bsc's CM.
struct Direct {
    o0: Vec<Counter>,
    o1: Vec<Counter>,
    o2: Vec<Counter>,
    mixer: Mixer<7>,
    f1: Vec<f32>,
    f2: Vec<f32>,
    sse: Vec<Counter>,
    run: Vec<Counter>,
    c1: usize,
    c2: usize,
    len: usize,
}

impl Direct {
    fn new() -> Self {
        Direct {
            o0: vec![Counter::default(); 256],
            o1: vec![Counter::default(); 1 << 16],
            o2: vec![Counter::default(); 1 << 24],
            mixer: Mixer::new(8 * 256 * 8),
            f1: vec![0.5; 1 << 16],
            f2: vec![0.5; 1 << 24],
            sse: vec![Counter::default(); 256 * 256 * 33],
            run: vec![Counter::default(); 256 * 256 * 8],
            c1: 0,
            c2: 0,
            len: 0,
        }
    }

    fn byte(&mut self, c: u8) -> f64 {
        let mut bits = 0.0;
        let mut node = 1usize;
        for i in (0..8).rev() {
            let bit = c >> i & 1 != 0;
            let (a, b, d) = (node, self.c1 << 8 | node, (self.c2 << 16 | self.c1 << 8) | node);
            let e = (bucket(self.len) * 256 + self.c1) * 256 + node;
            let x = [
                stretch(self.o0[a].p),
                stretch(self.o1[b].p),
                stretch(self.o2[d].p),
                stretch(self.run[e].p),
                stretch(self.f1[b]),
                stretch(self.f2[d]),
                0.3,
            ];
            let set = (bucket(self.len) * 256 + self.c1) * 8 + (7 - i);
            let p = self.mixer.mix(set, &x);
            // (SSE by the order-1 context and the node, interpolated over 33 buckets)
            let s = ((stretch(p) + 8.0).clamp(0.0, 15.999) * 2.0) as usize;
            let k = (self.c1 << 8 | node) * 33 + s;
            let q = (p + 3.0 * self.sse[k].p) / 4.0;
            bits += cost(q, bit);
            self.mixer.update(set, &x, p, bit, 0.02);
            self.o0[a].update(bit);
            self.o1[b].update(bit);
            self.o2[d].update(bit);
            self.run[e].update(bit);
            let t = bit as u8 as f32;
            self.f1[b] = (self.f1[b] + (t - self.f1[b]) / 4.0).clamp(1e-3, 1.0 - 1e-3);
            self.f2[d] = (self.f2[d] + (t - self.f2[d]) / 4.0).clamp(1e-3, 1.0 - 1e-3);
            self.sse[k].update(bit);
            node = node << 1 | bit as usize;
        }
        self.len = if c as usize == self.c1 { self.len + 1 } else { 0 };
        self.c2 = self.c1;
        self.c1 = c as usize;
        bits
    }
}

/// MTF ranks with their zero runs, coded bit by bit: zero or not by the previous ranks and
/// the run of zeros, then the rank's magnitude and mantissa (as QLFC-like coders do).
struct Ranks {
    front: Vec<u8>,
    zero: Vec<Counter>,
    unary: Vec<Counter>,
    mantissa: Vec<Counter>,
    r1: usize,
    r2: usize,
    run: usize,
}

fn bucket(r: usize) -> usize {
    match r {
        0..=2 => r,
        3..=4 => 3,
        5..=8 => 4,
        9..=16 => 5,
        17..=32 => 6,
        _ => 7,
    }
}

impl Ranks {
    fn new() -> Self {
        Ranks {
            front: (0..=255).collect(),
            zero: vec![Counter::default(); 8 * 8 * 8 * 256],
            unary: vec![Counter::default(); 8 * 8 * 8],
            mantissa: vec![Counter::default(); 8 * 8 * 256],
            r1: 0,
            r2: 0,
            run: 0,
        }
    }

    fn byte(&mut self, c: u8) -> f64 {
        // (by the front symbol before this one moves: what a decoder knows)
        let f0 = self.front[0] as usize;
        let r = self.front.iter().position(|&x| x == c).unwrap();
        self.front.copy_within(0..r, 1);
        self.front[0] = c;
        let ctx = (bucket(self.r1) * 8 + bucket(self.r2)) * 8 + bucket(self.run);
        let mut bits = 0.0;
        let z = &mut self.zero[ctx * 256 + f0];
        bits += cost(z.p, r == 0);
        z.update(r == 0);
        if r > 0 {
            let k = 63 - (r as u64).leading_zeros() as usize;
            for i in 0..8 {
                let u = &mut self.unary[(bucket(self.r1) * 8 + i) * 8 + bucket(self.run)];
                let more = i < k;
                bits += cost(u.p, more);
                u.update(more);
                if !more {
                    break;
                }
            }
            let mut prefix = 1;
            for i in (0..k).rev() {
                let bit = r >> i & 1 != 0;
                let m = &mut self.mantissa[(k * 8 + bucket(self.r1)) * 256 + (prefix & 255)];
                bits += cost(m.p, bit);
                m.update(bit);
                prefix = prefix << 1 | bit as usize;
            }
        }
        self.run = if r == 0 { self.run + 1 } else { 0 };
        self.r2 = self.r1;
        self.r1 = r;
        bits
    }
}

fn main() {
    let (mut tot_bz, mut tot_direct, mut tot_ranks) = (0usize, 0.0, 0.0);
    for p in std::env::args().skip(1) {
        let d = std::fs::read(&p).unwrap();
        let (mut direct, mut ranks) = (Direct::new(), Ranks::new());
        let (mut bd, mut br, mut blocks) = (0.0, 0.0, 0);
        let mut at = 0;
        // (every stream of the file, each block's origPtr 24 bits)
        while at < d.len() {
            let Ok(s) = stream::decode_each(&d[at..], false, usize::MAX, &mut |last, _| {
                blocks += 1;
                for &c in last {
                    bd += direct.byte(c);
                    br += ranks.byte(c);
                }
            }) else {
                break;
            };
            at += s.len;
        }
        let (bd, br) = ((bd + 24.0 * blocks as f64) / 8.0, (br + 24.0 * blocks as f64) / 8.0);
        println!(
            "{p}: bzip2 {at} B, ranks {br:.0} B ({:+.1}%), direct {bd:.0} B ({:+.1}%)",
            100.0 * (br / at as f64 - 1.0),
            100.0 * (bd / at as f64 - 1.0)
        );
        (tot_bz, tot_direct, tot_ranks) = (tot_bz + at, tot_direct + bd, tot_ranks + br);
    }
    println!(
        "all: bzip2 {tot_bz} B, ranks {:+.1}%, direct {:+.1}%",
        100.0 * (tot_ranks / tot_bz as f64 - 1.0),
        100.0 * (tot_direct / tot_bz as f64 - 1.0)
    );
}
