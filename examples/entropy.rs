//! Experiment: cost of the residual streams under adaptive binary models, against zstd.
//! usage: entropy STRIDE_V STRIDE_F FILE...
use obj_recons::block::*;
use obj_recons::varint::Rd;

/// Adaptive bit probability (12-bit), cost in bits.
#[derive(Clone, Copy)]
struct P(u16);
impl P {
    fn code(&mut self, bit: bool) -> f64 {
        let p1 = self.0 as f64 / 4096.0;
        let c = -(if bit { p1 } else { 1.0 - p1 }).log2();
        if bit {
            self.0 += (4096 - self.0) >> 4;
        } else {
            self.0 -= self.0 >> 4;
        }
        c
    }
}

/// Exp-Golomb-like: bucket = bit length of |zz value| coded with a binary tree under ctx,
/// then the top 2 mantissa bits modeled (per bucket), the rest raw.
struct Model {
    tree: Vec<[P; 64]>,
    mant: Vec<[[P; 4]; 64]>,
}
impl Model {
    fn new(nctx: usize) -> Self {
        Model { tree: vec![[P(2048); 64]; nctx], mant: vec![[[P(2048); 4]; 64]; nctx] }
    }
    fn code(&mut self, ctx: usize, v: u64) -> f64 {
        let b = 64 - v.leading_zeros() as usize; // 0..=64
        let b = b.min(63);
        let mut c = 0.0;
        let mut node = 1;
        for i in (0..6).rev() {
            let bit = (b >> i) & 1 == 1;
            c += self.tree[ctx][node].code(bit);
            node = node * 2 + bit as usize;
        }
        if b > 1 {
            // bits below the leading one
            let nb = b - 1;
            let mut node = 1;
            for k in 0..nb.min(2) {
                let bit = (v >> (nb - 1 - k)) & 1 == 1;
                c += self.mant[ctx][b][node].code(bit);
                node = node * 2 + bit as usize;
            }
            c += nb.saturating_sub(2) as f64;
        }
        c
    }
}

fn bucket(v: u64) -> usize {
    (64 - v.leading_zeros() as usize).min(15)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let sv: usize = args[0].parse().unwrap();
    let sf: usize = args[1].parse().unwrap();
    for f in &args[2..] {
        let src = std::fs::read(f).unwrap();
        let mut enc = Encoder::default();
        let mut s: [Vec<u8>; NSTREAMS] = Default::default();
        println!("== {f}");
        let mut tot = [[0f64; 4]; NSTREAMS];
        let mut models: Vec<(Model, Model, [usize; 8])> = (0..NSTREAMS).map(|_| (Model::new(16 * 8), Model::new(8), [0; 8])).collect();
        let mut p = 0;
        while p < src.len() {
            let mut e = (p + (4 << 20)).min(src.len());
            if let Some(i) = memchr::memrchr(b'\n', &src[p..e]) {
                e = p + i + 1;
            }
            enc.encode(&src[p..e], &mut s);
            p = e;
            for (i, st) in s.iter().enumerate() {
                if !(i >= S_VAL || (S_FESC..S_FESC + 3).contains(&i)) || st.is_empty() {
                    continue;
                }
                let stride = if (S_FESC..S_FESC + 3).contains(&i) { sf } else { sv };
                tot[i][0] += st.len() as f64;
                tot[i][1] += zstd::bulk::compress(st, 3).unwrap().len() as f64;
                let (m1, m0, last) = &mut models[i];
                let mut r = Rd::new(st);
                let mut j = 0;
                let (mut c0, mut c1) = (0.0, 0.0);
                while !r.done() {
                    let v = r.uv();
                    let comp = j % stride;
                    c0 += m0.code(comp, v);
                    c1 += m1.code(comp * 16 + last[comp], v);
                    last[comp] = bucket(v);
                    j += 1;
                }
                tot[i][2] += c0 / 8.0;
                tot[i][3] += c1 / 8.0;
            }
        }
        for i in 0..NSTREAMS {
            if tot[i][0] > 0.0 {
                println!("  {:>5} raw {:>11.0} zstd {:>11.0} adapt-o0 {:>11.0} adapt-ctx {:>11.0} ({:.3} of zstd)", STREAM_NAMES[i], tot[i][0], tot[i][1], tot[i][2], tot[i][3], tot[i][3] / tot[i][1]);
            }
        }
    }
}
