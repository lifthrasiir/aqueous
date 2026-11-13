//! Code lengths under adaptive order-0 models: Krichevsky–Trofimov's estimator with a prior
//! count of [`ALPHA`] per symbol. The adaptive code length charges the model's cost, so more
//! models (more columns) win only where they pay for themselves.

use std::sync::OnceLock;

/// The models' prior count per symbol.
pub const ALPHA: f64 = 1.0 / 16.0;

/// The least score of a byte against the column models: one value the models have not seen
/// does not end a region.
pub const CLIP: f64 = -4.0;

/// Counts the tables reach: the windows are 64 KiB at most, and planes 4096 rows.
const MAX: usize = 1 << 17;

pub struct Tables {
    /// `sym[c]` = log2(c + α): a symbol seen c times before.
    pub sym: Vec<f64>,
    /// `all[n]` = log2(n + 256α): a model that has coded n symbols.
    pub all: Vec<f64>,
    /// Their sums below c (and n): the code length of counts at once.
    sym_sum: Vec<f64>,
    all_sum: Vec<f64>,
}

impl Tables {
    /// Σ log2(i + 256α) for i < n: the models' part of the code length of n symbols.
    pub fn all_sum(&self, n: usize) -> f64 {
        self.all_sum[n]
    }
}

pub fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| {
        let sym: Vec<f64> = (0..MAX).map(|c| (c as f64 + ALPHA).log2()).collect();
        let all: Vec<f64> = (0..MAX).map(|n| (n as f64 + 256.0 * ALPHA).log2()).collect();
        let sums = |v: &[f64]| {
            let mut s = Vec::with_capacity(v.len() + 1);
            s.push(0.0);
            for x in v {
                s.push(s.last().unwrap() + x);
            }
            s
        };
        Tables { sym_sum: sums(&sym), all_sum: sums(&all), sym, all }
    })
}

/// Code length in bits of sequences with these counts (rows of 256, one model each).
pub fn bits(counts: &[u32]) -> f64 {
    let t = tables();
    counts
        .as_chunks::<256>()
        .0
        .iter()
        .map(|row| {
            let n: u32 = row.iter().sum();
            t.all_sum[n as usize] - row.iter().filter(|&&c| c > 0).map(|&c| t.sym_sum[c as usize]).sum::<f64>()
        })
        .sum()
}

/// log2 probabilities of the bytes of data under its order-0 model (with the prior).
pub fn order0_log2(data: &[u8]) -> [f64; 256] {
    // (counted in four quarters, summed after, so that a run of the same byte does not wait on
    // its own count)
    let mut q = [[0u64; 256]; 4];
    let (quads, rest) = data.as_chunks::<4>();
    for &[a, b, c, d] in quads {
        q[0][a as usize] += 1;
        q[1][b as usize] += 1;
        q[2][c as usize] += 1;
        q[3][d as usize] += 1;
    }
    for &b in rest {
        q[0][b as usize] += 1;
    }
    let c: [u64; 256] = std::array::from_fn(|s| q[0][s] + q[1][s] + q[2][s] + q[3][s]);
    let tot = data.len() as f64 + 256.0 * ALPHA;
    std::array::from_fn(|s| ((c[s] as f64 + ALPHA) / tot).log2())
}

/// log2 probabilities of column models: counts (rows of 256, each from `n` symbols).
pub fn column_log2(counts: &[u32], n: usize) -> Vec<f64> {
    let tot = n as f64 + 256.0 * ALPHA;
    counts.iter().map(|&c| ((c as f64 + ALPHA) / tot).log2()).collect()
}

/// The run [s, e) of scores with the largest sum, and the sum: by prefix sums, ending at the
/// first best end, starting at the last minimum of the prefix before it.
pub fn best_run(scores: impl IntoIterator<Item = f64>) -> (usize, usize, f64) {
    let (mut pre, mut low, mut low_at) = (0.0f64, 0.0f64, 0usize);
    let mut best = (0usize, 0usize, 0.0f64);
    for (i, x) in std::iter::once(0.0).chain(scores).enumerate() {
        pre += x;
        if pre <= low {
            (low, low_at) = (pre, i);
        }
        if pre - low > best.2 {
            best = (low_at, i, pre - low);
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequential_and_counts_agree() {
        let data: Vec<u8> = (0..5000u32).map(|i| (i * i % 251 % 7) as u8).collect();
        let t = tables();
        let mut c = [0u32; 256];
        let mut seq = 0.0;
        for (n, &b) in data.iter().enumerate() {
            seq += t.all[n] - t.sym[c[b as usize] as usize];
            c[b as usize] += 1;
        }
        assert!((seq - bits(&c)).abs() < 1e-6 * seq);
    }

    #[test]
    fn runs() {
        assert_eq!(best_run([-1.0, 2.0, -0.5, 3.0, -9.0, 1.0]), (1, 4, 4.5));
        assert_eq!(best_run([-1.0, -2.0]).2, 0.0);
    }
}
