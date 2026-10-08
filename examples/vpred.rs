//! Experiment: vertex predictors, as the mean of log2(|residual|+1) summed over x,y,z
//! (a rough bit cost), values quantized at 1e-6.
fn cost(r: [i64; 3]) -> f64 {
    r.iter().map(|&x| ((x.unsigned_abs() + 1) as f64).log2() + 1.0).sum()
}

fn main() {
    for f in std::env::args().skip(1) {
        let src = std::fs::read(&f).unwrap();
        let mut vs: Vec<[i64; 3]> = vec![];
        for l in src.split(|&c| c == b'\n') {
            if l.starts_with(b"v ") {
                let s = std::str::from_utf8(l).unwrap();
                let mut it = s.split_whitespace().skip(1).map(|t| (t.parse::<f64>().unwrap_or(0.0) * 1e6).round() as i64);
                vs.push([it.next().unwrap_or(0), it.next().unwrap_or(0), it.next().unwrap_or(0)]);
            }
        }
        let n = vs.len().max(1) as f64;
        let sub = |a: [i64; 3], b: [i64; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
        let (mut c_prev, mut c_lin, mut c_win, mut c_win2, mut c_par) = (0.0, 0.0, 0.0, 0.0, 0.0);
        const W: usize = 32;
        for i in 2..vs.len() {
            let v = vs[i];
            c_prev += cost(sub(v, vs[i - 1]));
            let lin = [2 * vs[i - 1][0] - vs[i - 2][0], 2 * vs[i - 1][1] - vs[i - 2][1], 2 * vs[i - 1][2] - vs[i - 2][2]];
            c_lin += cost(sub(v, lin));
            let mut best = f64::MAX;
            for k in 1..=W.min(i) {
                best = best.min(cost(sub(v, vs[i - k])));
            }
            c_win += best + 5.0;
            // parallelogram over the window: v[i-1] + v[i-k] - v[i-k-1]
            let mut best2 = best;
            for k in 2..=W.min(i - 1) {
                let p = [vs[i - 1][0] + vs[i - k][0] - vs[i - k - 1][0], vs[i - 1][1] + vs[i - k][1] - vs[i - k - 1][1], vs[i - 1][2] + vs[i - k][2] - vs[i - k - 1][2]];
                best2 = best2.min(cost(sub(v, p)));
            }
            c_win2 += best2 + 6.0;
            c_par += cost(sub(v, vs[i - 1])).min(cost(sub(v, lin)));
        }
        println!(
            "{:>9} verts  prev {:5.1}  linear {:5.1}  min(prev,lin) {:5.1}  window{W}+5b {:5.1}  win+para+6b {:5.1}  bits/vertex  {f}",
            vs.len(), c_prev / n, c_lin / n, c_par / n, c_win / n, c_win2 / n
        );
    }
}
