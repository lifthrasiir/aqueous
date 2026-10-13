//! Experiment: vertex predictors from a window of the last W vertices: delta from v[i-k]
//! and parallelograms v[i-1] + v[i-k] - v[i-k-1]. Cost: sum over components of the bit
//! length of the zigzagged residual (+1), plus the predictor index under an adaptive
//! order-0 model. Strategies: always the previous vertex; the best per vertex (index
//! coded); the best for the previous vertex (nothing coded).
const W: usize = 16;
const NP: usize = 2 * W;

fn bits(r: i64) -> f64 {
    (64 - ((r << 1) ^ (r >> 63)).leading_zeros()) as f64 + 1.0
}

fn main() {
    for f in std::env::args().skip(1) {
        let src = std::fs::read(&f).unwrap();
        let mut vs: Vec<[i64; 3]> = vec![];
        for l in src.split(|&c| c == b'\n') {
            if l.starts_with(b"v ") {
                let s = std::str::from_utf8(l).unwrap();
                let mut it =
                    s.split_whitespace().skip(1).map(|t| (t.parse::<f64>().unwrap_or(0.0) * 1e6).round() as i64);
                vs.push([it.next().unwrap_or(0), it.next().unwrap_or(0), it.next().unwrap_or(0)]);
            }
        }
        let pred = |i: usize, p: usize| -> [i64; 3] {
            let g = |k: usize| vs[i.saturating_sub(k)];
            if p < W {
                g(p + 1)
            } else {
                let k = p - W + 1;
                let (a, b, c) = (g(1), g(k), g(k + 1));
                [a[0] + b[0] - c[0], a[1] + b[1] - c[1], a[2] + b[2] - c[2]]
            }
        };
        let cost = |i: usize, p: usize| -> f64 {
            let q = pred(i, p);
            (0..3).map(|j| bits(vs[i][j] - q[j])).sum()
        };
        let (mut c_prev, mut c_best, mut c_back) = (0.0, 0.0, 0.0);
        let mut cnt = [1f64; NP];
        let mut tot = NP as f64;
        let mut last_best = 0;
        for i in W + 2..vs.len() {
            c_prev += cost(i, 0);
            let (mut bp, mut bc) = (0, f64::MAX);
            for p in 0..NP {
                let c = cost(i, p);
                if c < bc {
                    (bp, bc) = (p, c);
                }
            }
            c_best += bc - (cnt[bp] / tot).log2();
            cnt[bp] += 1.0;
            tot += 1.0;
            c_back += cost(i, last_best);
            last_best = bp;
        }
        let n = vs.len().max(1) as f64;
        println!(
            "{:>9} verts: prev {:5.1}  best+index {:5.1}  previous best {:5.1}  bits/vertex  {f}",
            vs.len(),
            c_prev / n,
            c_best / n,
            c_back / n
        );
    }
}
