//! Estimates the gain of per-file frequency tables over the built-in ones: for each
//! context, the static cost against the empirical entropy plus a table cost.
use mp3_recons::model::Counter;

fn main() {
    let t = mp3_recons::codec::tables();
    let _ = t;
    let freqs = mp3_recons::codec::builtin_freqs_pub().unwrap();
    let ks = [8usize, 16, 32, 64, 128, 100000];
    let (mut tin, mut stat, mut best) = (0usize, 0f64, vec![0f64; ks.len()]);
    for a in std::env::args().skip(1) {
        for e in std::fs::read_dir(&a).unwrap().flatten() {
            let d = std::fs::read(e.path()).unwrap();
            tin += d.len();
            let mut r = Counter::new();
            mp3_recons::codec::count(&d, &mut r);
            let counts = &r.counts;
            let mut order: Vec<usize> = (0..counts.len()).collect();
            order.sort_by_key(|&c| std::cmp::Reverse(counts[c].iter().sum::<u64>()));
            let mut rank = vec![0usize; counts.len()];
            for (i, &c) in order.iter().enumerate() {
                rank[c] = i;
            }
            for (c, cnt) in counts.iter().enumerate() {
                let tot: u64 = cnt.iter().sum();
                if tot == 0 {
                    continue;
                }
                let s: f64 = cnt.iter().zip(&freqs[c]).map(|(&n, &f)| n as f64 * -(f as f64 / 4096.0).log2()).sum();
                let nz = cnt.iter().filter(|&&n| n > 0).count();
                let own: f64 = cnt.iter().filter(|&&n| n > 0).map(|&n| n as f64 * (tot as f64 / n as f64).log2()).sum::<f64>()
                    + nz as f64 * 8.0 + 16.0;
                stat += s;
                for (i, &k) in ks.iter().enumerate() {
                    best[i] += if rank[c] < k { s.min(own) } else { s };
                }
            }
        }
    }
    for (i, &k) in ks.iter().enumerate() {
        println!("top {k:6} contexts: gain {:.3}% of input", (stat - best[i]) / 8.0 / tin as f64 * 100.0);
    }
}
