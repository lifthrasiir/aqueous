//! Trains the built-in model: `aqr-mp3-train [-a] [-k K] OUT FILE|DIR...` (every second file, or
//! all with `-a`), writing the model file read by `codec::tables`. Contexts of the same
//! alphabet size are merged greedily, the pair costing the fewest bits on the training set
//! first, until `K` tables are left (500 by default).

use aqr_mp3::codec::{self, Model};
use aqr_mp3::model::{self, Counter};
use aqr_mp3::rans;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

const THREADS: usize = 10;

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let all = if let Some(i) = args.iter().position(|a| a == "-a") {
        args.remove(i);
        true
    } else {
        false
    };
    let k = if let Some(i) = args.iter().position(|a| a == "-k") {
        args.remove(i);
        args.remove(i).parse().expect("-k K")
    } else {
        500
    };
    let out = args.remove(0);
    let mut paths = Vec::new();
    fn walk(p: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        if p.is_dir() {
            let mut es: Vec<_> = std::fs::read_dir(p).unwrap().flatten().map(|e| e.path()).collect();
            es.sort();
            for e in es {
                walk(&e, out);
            }
        } else {
            out.push(p.to_path_buf());
        }
    }
    for a in &args {
        walk(std::path::Path::new(a), &mut paths);
    }
    let paths: Vec<_> = paths.into_iter().step_by(if all { 1 } else { 2 }).collect();
    let counts = count(&paths);
    let used = counts.iter().filter(|v| v.iter().any(|&x| x > 0)).count();
    let (map, n, cost) = merge(&counts, k);
    let mut agg: Vec<Vec<u64>> = vec![Vec::new(); n];
    for (c, &t) in map.iter().enumerate() {
        let a = &mut agg[t as usize];
        a.resize(counts[c].len(), 0);
        for (x, &y) in a.iter_mut().zip(&counts[c]) {
            *x += y;
        }
    }
    let tables = agg
        .iter()
        .map(|a| {
            let mut f = vec![0u16; a.len()];
            rans::normalize(a, &mut f);
            f
        })
        .collect();
    let bytes = Model { map, tables }.to_bytes();
    std::fs::write(&out, &bytes).unwrap();
    eprintln!(
        "{} files, {used} of {} contexts used, {n} tables (+{:.0} bytes on the training set), model {} bytes",
        paths.len(),
        model::N_CTX,
        cost / 8.0,
        bytes.len()
    );
}

/// The symbol counts of every context over the files.
fn count(paths: &[std::path::PathBuf]) -> Vec<Vec<u64>> {
    let next = AtomicUsize::new(0);
    let total = Mutex::new(Counter::new());
    std::thread::scope(|s| {
        for _ in 0..THREADS {
            s.spawn(|| {
                let mut c = Counter::new();
                while let Some(p) = paths.get(next.fetch_add(1, Ordering::Relaxed)) {
                    codec::count(&std::fs::read(p).unwrap(), &mut c);
                }
                let mut t = total.lock().unwrap();
                for (x, y) in t.counts.iter_mut().zip(&c.counts) {
                    for (p, q) in x.iter_mut().zip(y) {
                        *p += q;
                    }
                }
            });
        }
    });
    total.into_inner().unwrap().counts
}

/// −(bits of coding `v` with its own distribution).
fn neg_cost(v: &[u64]) -> f64 {
    let xl = |x: u64| if x == 0 { 0.0 } else { x as f64 * (x as f64).log2() };
    v.iter().map(|&x| xl(x)).sum::<f64>() - xl(v.iter().sum())
}

fn neg_cost_sum(a: &[u64], b: &[u64]) -> f64 {
    let xl = |x: u64| if x == 0 { 0.0 } else { x as f64 * (x as f64).log2() };
    a.iter().zip(b).map(|(&x, &y)| xl(x + y)).sum::<f64>() - xl(a.iter().sum::<u64>() + b.iter().sum::<u64>())
}

/// `f(i)` for `i` in `0..n`, on the threads.
fn par_map(n: usize, f: impl Fn(usize) -> f64 + Sync) -> Vec<f64> {
    let mut out = vec![0.0; n];
    let per = n.div_ceil(THREADS).max(64);
    std::thread::scope(|s| {
        for (k, part) in out.chunks_mut(per).enumerate() {
            let f = &f;
            s.spawn(move || {
                for (i, o) in part.iter_mut().enumerate() {
                    *o = f(k * per + i);
                }
            });
        }
    });
    out
}

/// One alphabet size: its clusters (by their first context), the bits each merge of two
/// costs, and the cheapest merge of each cluster with a later or earlier one.
struct Group {
    ctxs: Vec<usize>,
    m: Vec<Vec<u64>>,
    l: Vec<f64>,
    alive: Vec<bool>,
    /// The cluster each dead one went into.
    parent: Vec<usize>,
    /// `d[i * n + j]`: the cost of merging clusters `i` and `j` (infinite unless both alive).
    d: Vec<f64>,
    best: Vec<(f64, usize)>,
}

impl Group {
    fn n(&self) -> usize {
        self.ctxs.len()
    }

    /// The live cluster that `i` went into.
    fn find(&self, mut i: usize) -> usize {
        while !self.alive[i] {
            i = self.parent[i];
        }
        i
    }

    fn rescan(&mut self, i: usize) {
        let n = self.n();
        let row = &self.d[i * n..(i + 1) * n];
        self.best[i] = row.iter().enumerate().fold((f64::INFINITY, usize::MAX), |b, (j, &x)| if x < b.0 { (x, j) } else { b });
    }

    /// Merges cluster `j` into `i`.
    fn merge(&mut self, i: usize, j: usize) {
        let n = self.n();
        let mj = std::mem::take(&mut self.m[j]);
        for (x, y) in self.m[i].iter_mut().zip(&mj) {
            *x += y;
        }
        self.l[i] = neg_cost(&self.m[i]);
        self.alive[j] = false;
        self.parent[j] = i;
        let row = {
            let (m, l, alive, li) = (&self.m, &self.l, &self.alive, self.l[i]);
            par_map(n, |k| if k != i && alive[k] { li + l[k] - neg_cost_sum(&m[i], &m[k]) } else { f64::INFINITY })
        };
        for (k, &x) in row.iter().enumerate() {
            self.d[i * n + k] = x;
            self.d[k * n + i] = x;
            self.d[j * n + k] = f64::INFINITY;
            self.d[k * n + j] = f64::INFINITY;
        }
        self.best[j] = (f64::INFINITY, usize::MAX);
        self.rescan(i);
        for (k, &x) in row.iter().enumerate() {
            if !self.alive[k] || k == i {
                continue;
            }
            let (b, at) = self.best[k];
            if at == i || at == j {
                self.rescan(k);
            } else if x < b {
                self.best[k] = (x, i);
            }
        }
    }
}

/// Merges the contexts into `k` tables (or as few as their alphabet sizes allow): the table of
/// each context, the number of tables and the bits the merges cost on the counts.
fn merge(counts: &[Vec<u64>], k: usize) -> (Vec<u16>, usize, f64) {
    let mut sizes: Vec<usize> = counts.iter().map(Vec::len).collect();
    sizes.sort();
    sizes.dedup();
    let mut groups: Vec<Group> = sizes
        .iter()
        .map(|&a| {
            let ctxs: Vec<usize> = (0..counts.len()).filter(|&c| counts[c].len() == a).collect();
            let n = ctxs.len();
            let m: Vec<Vec<u64>> = ctxs.iter().map(|&c| counts[c].clone()).collect();
            let l: Vec<f64> = m.iter().map(|v| neg_cost(v)).collect();
            let d = par_map(n * n, |x| {
                let (i, j) = (x / n, x % n);
                if i == j { f64::INFINITY } else { l[i] + l[j] - neg_cost_sum(&m[i], &m[j]) }
            });
            let mut g = Group { ctxs, m, l, alive: vec![true; n], parent: (0..n).collect(), d, best: vec![(0.0, 0); n] };
            for i in 0..n {
                g.rescan(i);
            }
            g
        })
        .collect();
    let mut left = counts.len();
    let mut cost = 0.0;
    while left > k {
        let mut pick = None;
        for (gi, g) in groups.iter().enumerate() {
            for i in 0..g.n() {
                if g.alive[i] && g.best[i].0 < pick.map_or(f64::INFINITY, |(_, _, _, b)| b) {
                    pick = Some((gi, i, g.best[i].1, g.best[i].0));
                }
            }
        }
        let Some((gi, i, j, b)) = pick else { break };
        groups[gi].merge(i.min(j), i.max(j));
        cost += b;
        left -= 1;
    }
    // Tables in the order of their first context.
    let mut roots: Vec<(usize, usize, usize)> = Vec::new();
    for (gi, g) in groups.iter().enumerate() {
        roots.extend((0..g.n()).filter(|&i| g.alive[i]).map(|i| (g.ctxs[i], gi, i)));
    }
    roots.sort();
    let mut table: Vec<Vec<u16>> = groups.iter().map(|g| vec![0; g.n()]).collect();
    for (t, &(_, gi, i)) in roots.iter().enumerate() {
        table[gi][i] = t as u16;
    }
    let mut map = vec![0u16; counts.len()];
    for (gi, g) in groups.iter().enumerate() {
        for (i, &c) in g.ctxs.iter().enumerate() {
            map[c] = table[gi][g.find(i)];
        }
    }
    (map, roots.len(), cost)
}
