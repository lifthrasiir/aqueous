//! Evaluation and rule derivation for the rule-based representation (`flate_recons::fast`),
//! over the samples that `flate-recons-train samples` cached from flate-zoo's encoders.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use flate_recons::fast::{self, Options, Report, Rule};
use rayon::prelude::*;

const USAGE: &str = "\
usage:
  flate-recons-fast rules [-i DIR] [-o RULES] [-k N] [-g PER_CLASS] [-p PREFIX] [-x EXTRA]...
                          [-F FITTED] [-l LAMBDA]
      Derive a catalog of up to N (default 32) rules from the training split: the best rule of
      each preset class (coordinate descent on the first PREFIX bytes, default 65536, of up to
      PER_CLASS streams, default 8; cached in FITTED, default cache/fast-fitted-s3-lLAMBDA.tsv),
      then a greedy set cover of them and of the rules in EXTRA, then the rule for every
      fingerprint; write RULES (default model/rules.txt). Costs count LAMBDA (default 0)
      bits per kilobyte for every nanosecond per byte a rule takes.
  flate-recons-fast models [-i DIR] [-r RULES] [-o MODELS]
      Fit the default models of the correction fields on the training split (the mean of
      the streams' symbol distributions); write MODELS (default model/fast-models.txt).
  flate-recons-fast speed [-i DIR] [-r RULES] [-b BACKEND]...
      Each rule of RULES alone on the held-out split: metadata size and single-threaded
      reconstruction speed.
  flate-recons-fast eval [-i DIR] [-s held|train|all] [-r RULES] [-p PREFIX] [-b BACKEND]...
      Metadata size per backend and per section, with the catalog RULES (default: built in)
      compared on the first PREFIX bytes of each stream; CPU times of splitting and joining
      on one thread.
";

type Res<T> = Result<T, Box<dyn std::error::Error>>;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.first().map(String::as_str) {
        Some("eval") => eval(&args[1..]),
        Some("rules") => rules(&args[1..]),
        Some("models") => models(&args[1..]),
        Some("speed") => speed(&args[1..]),
        _ => {
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("flate-recons-fast: {e}");
            ExitCode::FAILURE
        }
    }
}

type Opts = BTreeMap<String, Vec<String>>;

fn opts(args: &[String], with_value: &[&str]) -> Res<Opts> {
    let mut o = Opts::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if with_value.contains(&a.as_str()) {
            let v = it.next().ok_or_else(|| format!("{a} needs a value"))?;
            o.entry(a.clone()).or_default().push(v.clone());
        } else {
            return Err(format!("unknown argument {a}").into());
        }
    }
    Ok(o)
}

fn one<'a>(o: &'a Opts, k: &str, default: &'a str) -> &'a str {
    o.get(k)
        .and_then(|v| v.last())
        .map_or(default, String::as_str)
}

pub struct Sample {
    pub source: String,
    pub specs: String,
    pub raw: Vec<u8>,
}

impl Sample {
    fn backend(&self) -> &str {
        self.specs.split([':', ' ']).next().unwrap()
    }
}

/// The cached samples of a split (every fifth input file is held out, as in training).
fn load(dir: &Path, split: &str) -> Res<Vec<Sample>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("index.tsv").exists())
        .collect();
    files.sort();
    let mut out = vec![];
    for (i, f) in files.iter().enumerate() {
        let held = i % 5 == 4;
        if (split == "held" && !held) || (split == "train" && held) {
            continue;
        }
        let source = std::fs::read_to_string(f.join("source"))?.trim().to_owned();
        for line in std::fs::read_to_string(f.join("index.tsv"))?.lines() {
            let (k, specs) = line.split_once('\t').ok_or("bad index")?;
            out.push(Sample {
                source: source.clone(),
                specs: specs.to_owned(),
                raw: std::fs::read(f.join(format!("{k}.raw")))?,
            });
        }
    }
    Ok(out)
}

fn read_rules(path: &str) -> Res<Vec<Rule>> {
    Ok(fast::parse_catalog(&std::fs::read_to_string(path)?)?.0)
}

/// Options using the catalog (rules and fingerprint map) in `path`, or the built-in one.
fn catalog_opts(path: Option<&String>) -> Res<Options> {
    let mut o = Options {
        threads: 1,
        ..Default::default()
    };
    if let Some(p) = path {
        (o.rules, o.map) = fast::parse_catalog(&std::fs::read_to_string(p)?)?;
    }
    Ok(o)
}

fn eval(args: &[String]) -> Res<()> {
    let o = opts(args, &["-i", "-s", "-r", "-p", "-b"])?;
    let dir = PathBuf::from(one(&o, "-i", "cache/samples"));
    let mut samples = load(&dir, one(&o, "-s", "held"))?;
    if let Some(b) = o.get("-b") {
        samples.retain(|s| b.iter().any(|b| b == s.backend()));
    }
    let t = Instant::now();
    let mut opts = catalog_opts(o.get("-r").and_then(|r| r.last()))?;
    if let Some(p) = o.get("-p") {
        opts.prefix = p.last().unwrap().parse()?;
    }
    let reports: Vec<(usize, Report, usize, f64, f64)> = samples
        .par_iter()
        .map(|s| {
            let t0 = thread_cpu();
            let (data, meta, r) = fast::deconstruct_report(&s.raw, &opts).unwrap();
            let t1 = thread_cpu();
            let back = fast::reconstruct_with_threads(&data, &meta, 1).unwrap();
            let t2 = thread_cpu();
            assert!(back == s.raw, "{} on {}: reconstruction differs", s.specs, s.source);
            (s.raw.len(), r, meta.len(), t1 - t0, t2 - t1)
        })
        .collect();
    eprintln!(
        "{} streams in {:.1?}; CPU split {:.2} s, join {:.2} s",
        samples.len(),
        t.elapsed(),
        reports.iter().map(|r| r.3).sum::<f64>(),
        reports.iter().map(|r| r.4).sum::<f64>()
    );
    let phase = |k: usize| reports.iter().map(|r| r.1.times[k]).sum::<f64>();
    eprintln!(
        "split phases: parse {:.2} s, rule {:.2} s, tokens {:.2} s, headers {:.2} s, rest {:.2} s",
        phase(0),
        phase(1),
        phase(2),
        phase(3),
        phase(4)
    );

    #[derive(Default)]
    struct Agg {
        n: usize,
        raw: usize,
        meta: usize,
        main: usize,
        headers: usize,
        corr: usize,
        tokens: usize,
        misses: usize,
        fields: [f64; fast::tokens::FIELDS],
        coded: [f64; fast::tokens::FIELDS],
        tables: usize,
        rules: BTreeMap<String, usize>,
    }
    let mut by: BTreeMap<String, Agg> = BTreeMap::new();
    let mut total = Agg::default();
    for (s, (raw, r, meta, ..)) in samples.iter().zip(&reports) {
        for a in [by.entry(s.backend().to_owned()).or_default(), &mut total] {
            a.n += 1;
            a.raw += raw;
            a.meta += meta;
            a.main += r.main;
            a.tables += r.tables;
            a.headers += r.headers;
            a.corr += r.corrections;
            a.tokens += r.tokens;
            a.misses += r.misses;
            for (x, y) in a.fields.iter_mut().zip(&r.fields) {
                *x += y;
            }
            for (x, y) in a.coded.iter_mut().zip(&r.coded) {
                *x += y;
            }
            *a.rules.entry(r.rule.to_string()).or_default() += 1;
        }
    }
    println!(
        "{:<24} {:>4} {:>9} {:>8} {:>7} | {:>6} {:>6} {:>7} | {:>6} | {:>6} {:>6} {:>6} {:>6} {:>6} | rules",
        "backend", "n", "deflate", "meta", "%", "main", "hdrs", "corr", "miss%", "gap", "kind", "fresh", "delta", "rank"
    );
    let line = |name: &str, a: &Agg| {
        let mut rules: Vec<_> = a.rules.iter().collect();
        rules.sort_by_key(|r| std::cmp::Reverse(*r.1));
        let rules: Vec<String> = rules.iter().take(2).map(|(k, v)| format!("{k}:{v}")).collect();
        println!(
            "{name:<24} {:>4} {:>9} {:>8} {:>6.3}% | {:>6} {:>6} {:>7} | {:>6.2} | {:>6.0} {:>6.0} {:>6.0} {:>6.0} {:>6.0} | {}",
            a.n,
            a.raw,
            a.meta,
            a.meta as f64 * 100.0 / a.raw as f64,
            a.main,
            a.headers,
            a.corr,
            a.misses as f64 * 100.0 / a.tokens.max(1) as f64,
            a.fields[0],
            a.fields[1],
            a.fields[2],
            a.fields[3],
            a.fields[4],
            rules.join(" ")
        );
    };
    for (b, a) in &by {
        line(b, a);
    }
    line("TOTAL", &total);
    println!(
        "coded with their models: {:.0?}; tables {} B",
        total.coded, total.tables
    );
    Ok(())
}

/// A rule with one parameter set.
type Setter = Box<dyn Fn(Rule) -> Rule + Sync>;

/// The values tried for each parameter of a rule, as setters.
fn dimensions() -> Vec<Vec<Setter>> {
    use fast::tokens::Kind;
    fn dim<T: Copy + Sync + 'static>(vals: &[T], set: fn(&mut Rule, T)) -> Vec<Setter> {
        vals.iter()
            .map(|&v| {
                Box::new(move |mut r: Rule| {
                    set(&mut r, v);
                    r
                }) as Box<dyn Fn(Rule) -> Rule + Sync>
            })
            .collect()
    }
    vec![
        dim(&[3u8, 4], |r, v| r.key = v),
        dim(&[1u8, 2, 4, 8], |r, v| r.ways = v),
        dim(&[0u8, 5, 6, 8], |r, v| r.long = v),
        dim(&[false, true], |r, v| r.rank = v),
        dim(&[0u16, 4, 5, 6, 16, 32, 259], |r, v| r.inner = v),
        dim(&[0u16, 4, 5, 6, 8, 16, 32, 64, 128, 258, 259], |r, v| r.lazy = v),
        dim(&[0u8, 1], |r, v| r.margin = v),
        dim(&[3u8, 4, 5], |r, v| r.min_len = v),
        dim(&[(0u8, 4096u16), (3, 4096), (3, 8192), (4, 4096)], |r, v| {
            (r.far_len, r.far_dist) = v
        }),
        dim(
            &[256u16, 1024, 2048, 4096, 8192, 16384, 32506, 32510, 32767, 32768],
            |r, v| r.window = v,
        ),
        dim(&[Kind::Search, Kind::Literals, Kind::Runs], |r, v| r.kind = v),
    ]
}

/// Starting points of the search.
fn seeds() -> Vec<Rule> {
    [
        "key=3,ways=1",
        "key=3,ways=8,lazy=16,far",
        "key=3,ways=1,starts",
        "key=4,ways=4,min=4",
        "literals",
    ]
    .iter()
    .map(|s| s.parse().unwrap())
    .collect()
}

/// The rule minimizing the summed cost over `streams`, by coordinate descent from each seed.
/// A fingerprint keeps its second best rule if it costs at most this much more.
const SECOND: f64 = 1.3;

/// A rule's cost: its bits, plus `lambda` bits per kilobyte of data for every nanosecond per
/// byte it takes.
fn penalized(r: &Rule, bits: f64, bytes: usize, lambda: f64) -> f64 {
    bits + lambda * r.ns_per_byte() * bytes as f64 / 1000.0
}

fn fit(streams: &mut [fast::Prepared], lambda: f64) -> (Rule, f64) {
    let dims = dimensions();
    let mut memo: std::collections::HashMap<Rule, f64> = Default::default();
    let mut cost = |r: Rule, streams: &mut [fast::Prepared]| -> f64 {
        if r.check().is_err() {
            return f64::INFINITY;
        }
        *memo.entry(r).or_insert_with(|| {
            streams
                .iter_mut()
                .map(|p| penalized(&r, p.cost(r).unwrap(), p.bytes(), lambda))
                .sum()
        })
    };
    let mut best = (Rule::default(), f64::INFINITY);
    for seed in seeds() {
        let mut cur = (seed, cost(seed, streams));
        loop {
            let before = cur.1;
            for d in &dims {
                for set in d {
                    let r = set(cur.0);
                    let c = cost(r, streams);
                    if c < cur.1 {
                        cur = (r, c);
                    }
                }
            }
            if cur.1 >= before {
                break;
            }
        }
        if cur.1 < best.1 {
            best = cur;
        }
    }
    best
}

fn rules(args: &[String]) -> Res<()> {
    let o = opts(args, &["-i", "-o", "-k", "-g", "-p", "-x", "-F", "-l"])?;
    let dir = PathBuf::from(one(&o, "-i", "cache/samples"));
    let out = one(&o, "-o", "model/rules.txt");
    let lambda: f64 = one(&o, "-l", "0").parse()?;
    // Fits depend on the speed model (`Rule::ns_per_byte`): its version is in the name.
    let default_fitted = format!("cache/fast-fitted-s3-l{lambda}.tsv");
    let fitted_path = one(&o, "-F", &default_fitted);
    let k: usize = one(&o, "-k", "32").parse()?;
    let per_class: usize = one(&o, "-g", "8").parse()?;
    let prefix: usize = one(&o, "-p", "65536").parse()?;
    let samples = load(&dir, "train")?;
    // Streams by preset class (the first spec of each distinct output).
    let mut by_class: BTreeMap<String, Vec<&Sample>> = BTreeMap::new();
    for s in &samples {
        let spec = s.specs.split(' ').next().unwrap().to_owned();
        by_class.entry(spec).or_default().push(s);
    }
    let classes: Vec<(String, Vec<&Sample>)> = by_class
        .into_iter()
        .map(|(c, v)| {
            let step = v.len().div_ceil(per_class).max(1);
            (c, v.into_iter().step_by(step).collect())
        })
        .collect();
    let t = Instant::now();
    // The fits are cached (they take minutes): class, rule, cost.
    let cached: BTreeMap<String, (Rule, f64)> = std::fs::read_to_string(fitted_path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let (c, r, x) = (f.next()?, f.next()?, f.next()?);
            Some((c.to_owned(), (r.parse().ok()?, x.parse().ok()?)))
        })
        .collect();
    let fitted: Vec<(String, Rule, f64)> = classes
        .par_iter()
        .map(|(c, v)| {
            if let Some(&(r, cost)) = cached.get(c) {
                return (c.clone(), r, cost);
            }
            let mut ps: Vec<fast::Prepared> = v
                .iter()
                .map(|s| fast::Prepared::new(&s.raw, prefix).unwrap())
                .collect();
            let (r, cost) = fit(&mut ps, lambda);
            (c.clone(), r, cost)
        })
        .collect();
    std::fs::write(
        fitted_path,
        fitted
            .iter()
            .map(|(c, r, x)| format!("{c}\t{r}\t{x}\n"))
            .collect::<String>(),
    )?;
    eprintln!("fitted {} classes in {:.1?}", fitted.len(), t.elapsed());
    let mut cands: Vec<Rule> = fitted.iter().map(|f| f.1).collect();
    // Extra candidates, e.g. a catalog from a grid.
    for x in o.get("-x").into_iter().flatten() {
        cands.extend(read_rules(x)?);
    }
    cands.sort();
    cands.dedup();
    // Every candidate on every stream used for fitting.
    let subset: Vec<&Sample> = classes.iter().flat_map(|(_, v)| v.iter().copied()).collect();
    let t = Instant::now();
    let costs: Vec<Vec<f64>> = subset
        .par_iter()
        .map(|s| {
            let mut p = fast::Prepared::new(&s.raw, prefix).unwrap();
            cands
                .iter()
                .map(|&r| penalized(&r, p.cost(r).unwrap(), p.bytes(), lambda))
                .collect()
        })
        .collect();
    eprintln!(
        "{} distinct rules on {} streams in {:.1?}",
        cands.len(),
        subset.len(),
        t.elapsed()
    );
    let ideal: f64 = costs
        .iter()
        .map(|c| c.iter().copied().fold(f64::INFINITY, f64::min))
        .sum();
    let mut chosen: Vec<usize> = vec![];
    let mut cur = vec![f64::INFINITY; costs.len()];
    let mut text = format!(
        "# flate-recons fast rules, derived from flate-zoo's encoders: the best rule of each of\n\
         # {} preset classes (coordinate descent on up to {per_class} training streams each, first\n\
         # {prefix} bytes), then a greedy set cover of the {} distinct ones on those {} streams\n\
         # (all of them: {:.0} bytes).\n",
        classes.len(),
        cands.len(),
        subset.len(),
        ideal / 8.0
    );
    for _ in 0..k {
        let Some((best, total)) = (0..cands.len())
            .filter(|j| !chosen.contains(j))
            .map(|j| {
                let t: f64 = costs.iter().zip(&cur).map(|(c, &m)| c[j].min(m)).sum();
                (j, t)
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
        else {
            break;
        };
        let prev: f64 = cur.iter().sum();
        if !chosen.is_empty() && prev - total < 8.0 * 64.0 {
            break;
        }
        for (c, m) in costs.iter().zip(&mut cur) {
            *m = c[best].min(*m);
        }
        chosen.push(best);
        let users: Vec<&str> = fitted
            .iter()
            .filter(|f| f.1 == cands[best])
            .map(|f| f.0.as_str())
            .take(4)
            .collect();
        eprintln!("  + {} -> {:.0} bytes ({})", cands[best], total / 8.0, users.join(" "));
        text += &format!("{}\n", cands[best]);
    }
    // Which of the chosen rules is best for every fingerprint, over every training stream.
    let catalog: Vec<Rule> = chosen.iter().map(|&j| cands[j]).collect();
    let t = Instant::now();
    let analyzed: Vec<((u64, u64), Vec<f64>)> = samples
        .par_iter()
        .map(|s| {
            let (fp, c, bytes) = fast::analyze(&s.raw, &catalog, prefix).unwrap();
            let c = c
                .iter()
                .zip(&catalog)
                .map(|(&b, r)| penalized(r, b, bytes, lambda))
                .collect();
            (fp, c)
        })
        .collect();
    let mut sums: BTreeMap<u64, Vec<f64>> = BTreeMap::new();
    for ((fine, coarse), c) in &analyzed {
        for k in [fine, coarse] {
            let e = sums.entry(*k).or_insert_with(|| vec![0.0; catalog.len()]);
            for (a, b) in e.iter_mut().zip(c) {
                *a += b;
            }
        }
    }
    let ideal: f64 = analyzed
        .iter()
        .map(|(_, c)| c.iter().copied().fold(f64::INFINITY, f64::min))
        .sum();
    let mapped: f64 = analyzed
        .iter()
        .map(|((fine, _), c)| {
            let s = &sums[fine];
            let i = (0..s.len()).min_by(|&a, &b| s[a].total_cmp(&s[b])).unwrap();
            c[i]
        })
        .sum();
    eprintln!(
        "fingerprints of {} streams in {:.1?}: {} keys; {:.0} bytes by fingerprint, {:.0} at best",
        analyzed.len(),
        t.elapsed(),
        sums.len(),
        mapped / 8.0,
        ideal / 8.0
    );
    text += &format!(
        "# the rule for each fingerprint (fine and coarse) of the {} training streams\n",
        analyzed.len()
    );
    // The best rule, and the second best too where it is nearly as good (to be tried
    // against each other on a short prefix).
    for (k, s) in &sums {
        let mut order: Vec<usize> = (0..s.len()).collect();
        order.sort_by(|&a, &b| s[a].total_cmp(&s[b]));
        let (a, b) = (order[0], order.get(1).copied());
        match b {
            Some(b) if s[b] <= s[a] * SECOND => text += &format!("map {k:016x} {a} {b}\n"),
            _ => text += &format!("map {k:016x} {a}\n"),
        }
    }
    std::fs::write(out, text)?;
    eprintln!("wrote {out}");
    Ok(())
}

fn models(args: &[String]) -> Res<()> {
    let o = opts(args, &["-i", "-r", "-o"])?;
    let dir = PathBuf::from(one(&o, "-i", "cache/samples"));
    let out = one(&o, "-o", "model/fast-models.txt");
    let samples = load(&dir, "train")?;
    let opts = catalog_opts(o.get("-r").and_then(|r| r.last()))?;
    let counts: Vec<Vec<Vec<u32>>> = samples
        .par_iter()
        .map(|s| fast::deconstruct_report(&s.raw, &opts).unwrap().2.counts)
        .collect();
    let mut text = format!(
        "# flate-recons fast default models: mean symbol distributions of {} training streams\n\
         # (per line: gap and kind of either class, fresh length, length delta, distance index, raw, rank)\n",
        samples.len()
    );
    for f in 0..fast::tokens::FIELDS {
        let mut p = vec![0.0; fast::code::ALPHABET];
        let mut streams = 0;
        for c in &counts {
            let n: u32 = c[f].iter().sum();
            if n > 0 {
                streams += 1;
                for (x, &k) in p.iter_mut().zip(&c[f]) {
                    *x += k as f64 / n as f64;
                }
            }
        }
        for x in &mut p {
            *x /= streams.max(1) as f64;
        }
        text += &fast::code::Model::from_distribution(&p).to_text();
        text += "\n";
    }
    std::fs::write(out, text)?;
    eprintln!("wrote {out}");
    Ok(())
}

fn speed(args: &[String]) -> Res<()> {
    let o = opts(args, &["-i", "-r", "-b"])?;
    let dir = PathBuf::from(one(&o, "-i", "cache/samples"));
    let rules = match o.get("-r") {
        Some(r) => read_rules(r.last().unwrap())?,
        None => fast::catalog(),
    };
    let mut samples = load(&dir, "held")?;
    if let Some(b) = o.get("-b") {
        samples.retain(|s| b.iter().any(|b| b == s.backend()));
    }
    let plain: usize = samples
        .iter()
        .map(|s| flate_recons::deflate::parse(&s.raw).unwrap().1.len())
        .sum();
    println!("{} streams, {:.1} MB of data", samples.len(), plain as f64 / 1e6);
    for rule in rules {
        let opts = Options {
            rules: vec![rule],
            threads: 1,
            ..Default::default()
        };
        // CPU time of reconstruction, summed over streams split across threads.
        let r: Vec<(usize, f64)> = samples
            .par_iter()
            .map(|s| {
                let (data, meta) = fast::deconstruct_with(&s.raw, &opts).unwrap();
                let t = thread_cpu();
                let back = fast::reconstruct_with_threads(&data, &meta, 1).unwrap();
                let dt = thread_cpu() - t;
                assert!(back == s.raw);
                (meta.len(), dt)
            })
            .collect();
        let size: usize = r.iter().map(|x| x.0).sum();
        let time: f64 = r.iter().map(|x| x.1).sum();
        println!(
            "{:<52} {:>9} B  join {:>6.2} s  {:>6.1} MB/s",
            rule.to_string(),
            size,
            time,
            plain as f64 / 1e6 / time
        );
    }
    Ok(())
}

/// CPU time of the calling thread, in seconds.
fn thread_cpu() -> f64 {
    let mut ts = Timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { clock_gettime(CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    ts.tv_sec as f64 + ts.tv_nsec as f64 * 1e-9
}

#[repr(C)]
struct Timespec {
    tv_sec: i64,
    tv_nsec: i64,
}

#[cfg(target_os = "macos")]
const CLOCK_THREAD_CPUTIME_ID: i32 = 16;
#[cfg(not(target_os = "macos"))]
const CLOCK_THREAD_CPUTIME_ID: i32 = 3;

unsafe extern "C" {
    fn clock_gettime(clk: i32, ts: *mut Timespec) -> i32;
}
