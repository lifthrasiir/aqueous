//! Training for the flate-recons predictor.
//!
//! 1. `samples` runs real encoders (flate-zoo) over a corpus and caches the distinct raw
//!    DEFLATE outputs per input file.
//! 2. `fit` picks a small catalog of predictor configurations (greedy set cover over a grid
//!    by estimated metadata size) and fits the per-context priors (initial probability from
//!    the first occurrences of each context in each stream, and per-group initial confidence
//!    by replaying the recorded decisions). Every fifth input file is held out.
//! 3. `eval` reports the estimated metadata size on the held-out files.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use flate_recons::coder::{Decision, Recorder};
use flate_recons::ctx::{self, Channel, NUM_CTX};
use flate_recons::model::{ContextModel, CostCoder, PROB_ONE};
use flate_recons::{Config, Parsed, Priors};
use flate_zoo::{Encoder, Sweep};
use rayon::prelude::*;

const USAGE: &str = "\
usage:
  flate-recons-train samples [-s SWEEP] [-x BACKEND]... [-o DIR] FILE...
      Compress every FILE with every flate-zoo preset (default sweep `one`), and cache the
      distinct outputs under DIR (default cache/samples). Files already done are skipped.
  flate-recons-train fit [-i DIR] [-o PRIORS] [-k N] [-r ROUNDS] [-g PER_FILE]
      Fit a catalog of N (default 4) predictor configurations, searched on up to PER_FILE
      (default 40) streams per input file, and the context priors on the training split;
      ROUNDS (default 2) alternations; write PRIORS (default model/priors.txt).
  flate-recons-train eval [-i DIR] [-p PRIORS]
      Estimated metadata size on the held-out split under PRIORS (default: built in).
";

type Res<T> = Result<T, Box<dyn std::error::Error>>;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.first().map(String::as_str) {
        Some("samples") => samples(&args[1..]),
        Some("fit") => fit(&args[1..]),
        Some("eval") => eval(&args[1..]),
        _ => {
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("flate-recons-train: {e}");
            ExitCode::FAILURE
        }
    }
}

fn fnv64(s: &[u8]) -> u64 {
    s.iter().fold(0xcbf29ce484222325u64, |h, &b| {
        (h ^ b as u64).wrapping_mul(0x100000001b3)
    })
}

/// Parses `-x value` style options; returns (options, positionals).
type Opts = BTreeMap<String, Vec<String>>;

fn opts(args: &[String], with_value: &[&str]) -> Res<(Opts, Vec<String>)> {
    let mut o = Opts::new();
    let mut pos = vec![];
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if with_value.contains(&a.as_str()) {
            let v = it.next().ok_or_else(|| format!("{a} needs a value"))?;
            o.entry(a.clone()).or_default().push(v.clone());
        } else if a.starts_with('-') {
            return Err(format!("unknown option {a}").into());
        } else {
            pos.push(a.clone());
        }
    }
    Ok((o, pos))
}

fn one<'a>(o: &'a Opts, k: &str, default: &'a str) -> &'a str {
    o.get(k)
        .and_then(|v| v.last())
        .map_or(default, String::as_str)
}

fn samples(args: &[String]) -> Res<()> {
    let (o, files) = opts(args, &["-s", "-x", "-o"])?;
    let sweep: Sweep = one(&o, "-s", "one").parse()?;
    let exclude = o.get("-x").cloned().unwrap_or_default();
    let out = PathBuf::from(one(&o, "-o", "cache/samples"));
    let encoders: Vec<Encoder> = flate_zoo::presets(sweep)
        .into_iter()
        .filter(|e| !exclude.iter().any(|x| x == e.backend().name()))
        .collect();
    eprintln!("{} presets", encoders.len());
    for f in &files {
        let input = std::fs::read(f)?;
        let dir = out.join(format!("{:016x}", fnv64(&input)));
        if dir.join("index.tsv").exists() {
            eprintln!("{f}: cached");
            continue;
        }
        let t = Instant::now();
        let results: Vec<(String, Vec<u8>)> = encoders
            .par_iter()
            .filter_map(|e| {
                let raw = e.compress(&input).ok()?;
                (flate_zoo::inflate(&raw).ok()? == input).then(|| (e.short(), raw))
            })
            .collect();
        let mut classes: BTreeMap<Vec<u8>, Vec<String>> = BTreeMap::new();
        for (spec, raw) in results {
            classes.entry(raw).or_default().push(spec);
        }
        std::fs::create_dir_all(&dir)?;
        let mut index = String::new();
        for (k, (raw, specs)) in classes.iter().enumerate() {
            std::fs::write(dir.join(format!("{k}.raw")), raw)?;
            index += &format!("{k}\t{}\n", specs.join(" "));
        }
        std::fs::write(dir.join("source"), format!("{f}\n"))?;
        std::fs::write(dir.join("index.tsv"), index)?;
        eprintln!(
            "{f}: {} distinct outputs in {:.1?}",
            classes.len(),
            t.elapsed()
        );
    }
    Ok(())
}

struct Sample {
    file: String,
    source: String,
    specs: Vec<String>,
    raw_len: usize,
    parsed: Parsed,
}

impl Sample {
    fn backend(&self) -> &str {
        self.specs[0].split(':').next().unwrap()
    }
}

/// Loads every cached sample, split into (train, held out).
fn load(dir: &Path) -> Res<(Vec<Sample>, Vec<Sample>)> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("index.tsv").exists())
        .collect();
    files.sort();
    let mut jobs = vec![];
    for (i, f) in files.iter().enumerate() {
        let source = std::fs::read_to_string(f.join("source"))?.trim().to_owned();
        for line in std::fs::read_to_string(f.join("index.tsv"))?.lines() {
            let (k, specs) = line.split_once('\t').ok_or("bad index")?;
            jobs.push((
                i % 5 == 4,
                f.clone(),
                source.clone(),
                k.to_owned(),
                specs.to_owned(),
            ));
        }
    }
    let loaded: Vec<(bool, Sample)> = jobs
        .into_par_iter()
        .filter_map(|(held, f, source, k, specs)| {
            let raw = std::fs::read(f.join(format!("{k}.raw"))).ok()?;
            match Parsed::new(&raw) {
                Ok(parsed) => Some((
                    held,
                    Sample {
                        file: f.file_name().unwrap().to_string_lossy().into_owned(),
                        source,
                        specs: specs.split(' ').map(str::to_owned).collect(),
                        raw_len: raw.len(),
                        parsed,
                    },
                )),
                Err(e) => {
                    eprintln!("skipping {specs} on {source}: {e}");
                    None
                }
            }
        })
        .collect();
    let (held, train): (Vec<_>, Vec<_>) = loaded.into_iter().partition(|(h, _)| *h);
    let unzip = |v: Vec<(bool, Sample)>| v.into_iter().map(|(_, s)| s).collect::<Vec<_>>();
    Ok((unzip(train), unzip(held)))
}

fn grid() -> Vec<Config> {
    let depths = [1, 2, 4, 8, 16, 32, 64, 128, 256, 1024, 4096];
    let nices = [8, 16, 32, 64, 128, 258];
    depths
        .iter()
        .flat_map(|d| {
            nices
                .iter()
                .map(move |n| format!("depth={d},nice={n}").parse().unwrap())
        })
        .collect()
}

/// Greedily picks `k` configurations minimizing the summed per-sample minimum cost.
fn pick_catalog(costs: &[Vec<f64>], grid: &[Config], k: usize) -> Vec<Config> {
    let mut chosen: Vec<usize> = vec![];
    let mut cur = vec![f64::INFINITY; costs.len()];
    for _ in 0..k.min(grid.len()) {
        let (best, total) = (0..grid.len())
            .filter(|j| !chosen.contains(j))
            .map(|j| {
                (
                    j,
                    costs
                        .iter()
                        .zip(&cur)
                        .map(|(c, &m)| c[j].min(m))
                        .sum::<f64>(),
                )
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .unwrap();
        let prev: f64 = cur.iter().sum();
        if !chosen.is_empty() && total >= prev - 1.0 {
            break;
        }
        for (c, m) in costs.iter().zip(&mut cur) {
            *m = c[best].min(*m);
        }
        eprintln!(
            "  catalog += {} (total {:.0} bytes)",
            grid[best],
            total / 8.0
        );
        chosen.push(best);
    }
    chosen.into_iter().map(|j| grid[j]).collect()
}

/// Per-group replay cost of recorded decisions with the given priors.
fn replay_cost(decisions: &[Vec<Decision>], priors: &Priors) -> Vec<f64> {
    decisions
        .par_iter()
        .map(|ds| {
            let mut m = ContextModel::new(priors);
            let mut cost = vec![0.0; ctx::GROUPS.len()];
            for d in ds {
                if !d.ctx.is_bypass() {
                    cost[d.ctx.group()] += m.cost(d.ctx, d.bit);
                    m.update(d.ctx, d.bit);
                }
            }
            cost
        })
        .reduce(
            || vec![0.0; ctx::GROUPS.len()],
            |a, b| a.iter().zip(&b).map(|(x, y)| x + y).collect(),
        )
}

fn fit_priors(decisions: &[Vec<Decision>], catalog: Vec<Config>) -> Priors {
    // Initial probabilities from the first FIRST occurrences of each context per stream:
    // with adaptive contexts, that is all the prior influences.
    const FIRST: u32 = 16;
    let mut n0 = vec![0u64; NUM_CTX];
    let mut n1 = vec![0u64; NUM_CTX];
    for ds in decisions {
        let mut seen = vec![0u32; NUM_CTX];
        for d in ds {
            let i = d.ctx.0 as usize;
            if seen[i] < FIRST {
                seen[i] += 1;
                if d.bit { n1[i] += 1 } else { n0[i] += 1 }
            }
        }
    }
    let mut pr = Priors {
        catalog,
        ..Priors::default()
    };
    for i in Channel::COUNT..NUM_CTX {
        let n = (n0[i] + n1[i]) as f64;
        if n > 0.0 {
            let p = (n1[i] as f64 + 0.4) / (n + 0.8);
            pr.p1[i] = (p * PROB_ONE as f64)
                .round()
                .clamp(1.0, PROB_ONE as f64 - 1.0) as u16;
        }
    }
    // Initial confidence per group by replay.
    let candidates = [0u8, 1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 40];
    let mut best = vec![(f64::INFINITY, 0u8); ctx::GROUPS.len()];
    for &v in &candidates {
        let mut trial = pr.clone();
        trial.n.fill(v);
        for (g, c) in replay_cost(decisions, &trial).into_iter().enumerate() {
            if c < best[g].0 {
                best[g] = (c, v);
            }
        }
    }
    for i in Channel::COUNT..NUM_CTX {
        let g = flate_recons::ctx::Ctx(i as u16).group();
        if n0[i] + n1[i] > 0 {
            pr.n[i] = best[g].1;
        }
    }
    pr
}

fn fit(args: &[String]) -> Res<()> {
    let (o, pos) = opts(args, &["-i", "-o", "-k", "-r", "-g"])?;
    if !pos.is_empty() {
        return Err("unexpected arguments".into());
    }
    let dir = PathBuf::from(one(&o, "-i", "cache/samples"));
    let out = PathBuf::from(one(&o, "-o", "model/priors.txt"));
    let k: usize = one(&o, "-k", "4").parse()?;
    let rounds: usize = one(&o, "-r", "2").parse()?;
    let per_file: usize = one(&o, "-g", "40").parse()?;
    let t = Instant::now();
    let (train, held) = load(&dir)?;
    eprintln!(
        "{} training and {} held-out samples ({:.1?})",
        train.len(),
        held.len(),
        t.elapsed()
    );

    let grid = grid();
    // The grid search is the slow part; it runs on an evenly spaced subset of each file's
    // streams.
    let mut by_file: BTreeMap<&str, Vec<&Sample>> = BTreeMap::new();
    for s in &train {
        by_file.entry(&s.file).or_default().push(s);
    }
    let subset: Vec<&Sample> = by_file
        .values()
        .flat_map(|v| {
            let step = v.len().div_ceil(per_file).max(1);
            v.iter().step_by(step).copied()
        })
        .collect();
    let mut priors = Priors::default();
    for round in 1..=rounds {
        let t = Instant::now();
        let costs: Vec<Vec<f64>> = subset
            .par_iter()
            .map(|s| {
                grid.iter()
                    .map(|&c| s.parsed.cost(c, &priors).unwrap())
                    .collect()
            })
            .collect();
        eprintln!(
            "round {round}: {} grid costs in {:.1?}",
            costs.len() * grid.len(),
            t.elapsed()
        );
        let catalog = pick_catalog(&costs, &grid, k);
        let decisions: Vec<Vec<Decision>> = train
            .par_iter()
            .map(|s| {
                let cfg = s.parsed.best_config(&catalog, &priors).unwrap();
                let mut rec = Recorder::default();
                s.parsed.code(&mut rec, cfg).unwrap();
                rec.meta.channels.concat()
            })
            .collect();
        let before: f64 = replay_cost(&decisions, &priors).iter().sum();
        priors = fit_priors(&decisions, catalog);
        let after: f64 = replay_cost(&decisions, &priors).iter().sum();
        eprintln!(
            "round {round}: training metadata {:.0} -> {:.0} bytes (excluding bypass bits)",
            before / 8.0,
            after / 8.0
        );
    }
    let text = priors.to_text();
    std::fs::write(&out, &text)?;
    eprintln!("wrote {} ({:.1?} total)", out.display(), t.elapsed());
    report(&held, &Priors::parse(&text)?);
    Ok(())
}

fn eval(args: &[String]) -> Res<()> {
    let (o, pos) = opts(args, &["-i", "-p"])?;
    if !pos.is_empty() {
        return Err("unexpected arguments".into());
    }
    let dir = PathBuf::from(one(&o, "-i", "cache/samples"));
    let priors = match o.get("-p") {
        Some(p) => Priors::parse(&std::fs::read_to_string(p.last().unwrap())?)?,
        None => Priors::builtin().clone(),
    };
    let (_, held) = load(&dir)?;
    report(&held, &priors);
    Ok(())
}

/// Estimated metadata size of `samples` under `priors`, per channel and per backend.
fn report(samples: &[Sample], priors: &Priors) {
    let rows: Vec<(&Sample, CostCoder, Config)> = samples
        .par_iter()
        .map(|s| {
            let cfg = s.parsed.best_config(&priors.catalog, priors).unwrap();
            let mut c = CostCoder::new(priors);
            s.parsed.code(&mut c, cfg).unwrap();
            (s, c, cfg)
        })
        .collect();
    let mut by_backend: BTreeMap<&str, (f64, usize, usize)> = BTreeMap::new();
    let mut by_channel = [0.0f64; Channel::COUNT];
    let (mut total, mut raw) = (0.0, 0);
    for (s, c, _) in &rows {
        let e = by_backend.entry(s.backend()).or_default();
        e.0 += c.total() / 8.0;
        e.1 += s.raw_len;
        e.2 += 1;
        for (a, b) in by_channel.iter_mut().zip(&c.bits) {
            *a += b / 8.0;
        }
        total += c.total() / 8.0;
        raw += s.raw_len;
    }
    let files: std::collections::BTreeSet<_> =
        rows.iter().map(|(s, ..)| (&s.file, &s.source)).collect();
    println!(
        "held-out: {} streams from {:?}",
        rows.len(),
        files.iter().map(|f| f.1).collect::<Vec<_>>()
    );
    println!(
        "{:<26} {:>7} {:>12} {:>12} {:>8}",
        "backend", "streams", "deflate B", "metadata B", "ratio"
    );
    for (b, (m, r, n)) in &by_backend {
        println!(
            "{b:<26} {n:>7} {r:>12} {m:>12.0} {:>7.3}%",
            m * 100.0 / *r as f64
        );
    }
    println!(
        "{:<26} {:>7} {raw:>12} {total:>12.0} {:>7.3}%",
        "TOTAL",
        rows.len(),
        total * 100.0 / raw as f64
    );
    for ch in Channel::ALL {
        println!(
            "  channel {:<10} {:>12.0} B",
            ch.name(),
            by_channel[ch as usize]
        );
    }
}
