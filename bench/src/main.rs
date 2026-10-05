//! Compares reconstruction data sizes and speeds of flate-recons, preflate (C++) and
//! preflate-rs over the samples cached by `flate-recons-train samples`.
//!
//! Every tool's output is verified by a full round trip; a stream counts as failed for a tool
//! if the tool errors out, decompresses to something else, or does not reproduce the stream.
//! Times are per-thread CPU times, so running in parallel does not skew them much.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rayon::prelude::*;

const USAGE: &str = "\
usage: flate-recons-bench [-i DIR] [-s held|train|all] [-t TOOL,...] [-o TSV] [--phases]
  DIR   samples cached by flate-recons-train (default ../cache/samples)
  -s    which split to run (default held: every fifth input file, as in training)
  -t    tools (default recons,preflate,preflate-rs)
  -o    per-stream results (default ../cache/bench.tsv)
  --phases  instead, time the phases of flate-recons (CPU time summed over streams)
";

mod ffi {
    unsafe extern "C" {
        pub fn pf_decode(
            raw: *const u8,
            raw_len: usize,
            plain: *mut *mut u8,
            plain_len: *mut usize,
            diff: *mut *mut u8,
            diff_len: *mut usize,
        ) -> i32;
        pub fn pf_reencode(
            diff: *const u8,
            diff_len: usize,
            plain: *const u8,
            plain_len: usize,
            raw: *mut *mut u8,
            raw_len: *mut usize,
        ) -> i32;
        pub fn pf_free(p: *mut u8);
    }

    /// Takes ownership of a buffer returned by the wrapper.
    pub unsafe fn take(p: *mut u8, n: usize) -> Vec<u8> {
        let v = unsafe { std::slice::from_raw_parts(p, n) }.to_vec();
        unsafe { pf_free(p) };
        v
    }
}

fn thread_cpu() -> Duration {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
}

fn timed<T>(f: impl FnOnce() -> T) -> (T, Duration) {
    let t = thread_cpu();
    let r = f();
    (r, thread_cpu() - t)
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Tool {
    Recons,
    Preflate,
    PreflateRs,
}

impl Tool {
    fn name(self) -> &'static str {
        match self {
            Tool::Recons => "recons",
            Tool::Preflate => "preflate",
            Tool::PreflateRs => "preflate-rs",
        }
    }

    /// Splits `raw` into (plain text, reconstruction data).
    fn split(self, raw: &[u8]) -> Result<(Vec<u8>, Vec<u8>), String> {
        match self {
            Tool::Recons => {
                let (data, meta) = flate_recons::deconstruct(raw).map_err(|e| e.to_string())?;
                Ok((
                    data,
                    flate_recons::model::pack(&meta, flate_recons::Priors::builtin()),
                ))
            }
            Tool::Preflate => unsafe {
                let (mut p, mut pn, mut d, mut dn) =
                    (std::ptr::null_mut(), 0, std::ptr::null_mut(), 0);
                if ffi::pf_decode(raw.as_ptr(), raw.len(), &mut p, &mut pn, &mut d, &mut dn) == 0 {
                    return Err("preflate_decode failed".into());
                }
                Ok((ffi::take(p, pn), ffi::take(d, dn)))
            },
            Tool::PreflateRs => {
                let config = preflate_rs::PreflateConfig {
                    verify_compression: false,
                    ..Default::default()
                };
                let (r, plain) = preflate_rs::preflate_whole_deflate_stream(raw, &config)
                    .map_err(|e| format!("{e:?}"))?;
                if r.compressed_size != raw.len() {
                    return Err(format!(
                        "only {} of {} bytes processed",
                        r.compressed_size,
                        raw.len()
                    ));
                }
                Ok((plain.text().to_vec(), r.corrections))
            }
        }
    }

    fn join(self, plain: &[u8], diff: &[u8]) -> Result<Vec<u8>, String> {
        match self {
            Tool::Recons => {
                flate_recons::model::unpack(plain, diff, flate_recons::Priors::builtin())
                    .map_err(|e| e.to_string())
            }
            Tool::Preflate => unsafe {
                let (mut r, mut rn) = (std::ptr::null_mut(), 0);
                if ffi::pf_reencode(
                    diff.as_ptr(),
                    diff.len(),
                    plain.as_ptr(),
                    plain.len(),
                    &mut r,
                    &mut rn,
                ) == 0
                {
                    return Err("preflate_reencode failed".into());
                }
                Ok(ffi::take(r, rn))
            },
            Tool::PreflateRs => preflate_rs::recreate_whole_deflate_stream(plain, diff)
                .map_err(|e| format!("{e:?}")),
        }
    }
}

struct Sample {
    file: String,
    specs: String,
    raw: Vec<u8>,
}

struct Outcome {
    /// Reconstruction data size, or the failure.
    size: Result<usize, String>,
    split: Duration,
    join: Duration,
}

fn run(tool: Tool, raw: &[u8], expect: &[u8]) -> Outcome {
    let (r, split) = timed(|| tool.split(raw));
    let (plain, diff) = match r {
        Ok(x) => x,
        Err(e) => {
            return Outcome {
                size: Err(e),
                split,
                join: Duration::ZERO,
            };
        }
    };
    if plain != expect {
        return Outcome {
            size: Err("wrong plain text".into()),
            split,
            join: Duration::ZERO,
        };
    }
    let (r, join) = timed(|| tool.join(&plain, &diff));
    let size = match r {
        Ok(back) if back == raw => Ok(diff.len()),
        Ok(_) => Err("reconstruction differs".into()),
        Err(e) => Err(e),
    };
    Outcome { size, split, join }
}

fn load(dir: &Path, split: &str) -> Result<Vec<Sample>, Box<dyn std::error::Error>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("index.tsv").exists())
        .collect();
    files.sort();
    let mut out = vec![];
    for (i, f) in files.iter().enumerate() {
        // Same split as flate-recons-train.
        let held = i % 5 == 4;
        if (split == "held" && !held) || (split == "train" && held) {
            continue;
        }
        let source = std::fs::read_to_string(f.join("source"))?.trim().to_owned();
        for line in std::fs::read_to_string(f.join("index.tsv"))?.lines() {
            let (k, specs) = line.split_once('\t').ok_or("bad index")?;
            out.push(Sample {
                file: source.clone(),
                specs: specs.to_owned(),
                raw: std::fs::read(f.join(format!("{k}.raw")))?,
            });
        }
    }
    Ok(out)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut dir = PathBuf::from("../cache/samples");
    let mut split = "held".to_owned();
    let mut tools = vec![Tool::Recons, Tool::Preflate, Tool::PreflateRs];
    let mut tsv = PathBuf::from("../cache/bench.tsv");
    let mut phases_only = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut val = || {
            args.next()
                .ok_or_else(|| format!("{a} needs a value\n{USAGE}"))
        };
        match a.as_str() {
            "-i" => dir = val()?.into(),
            "-s" => split = val()?,
            "-o" => tsv = val()?.into(),
            "--phases" => phases_only = true,
            "-t" => {
                tools = val()?
                    .split(',')
                    .map(|t| match t {
                        "recons" => Ok(Tool::Recons),
                        "preflate" => Ok(Tool::Preflate),
                        "preflate-rs" => Ok(Tool::PreflateRs),
                        _ => Err(format!("unknown tool {t}")),
                    })
                    .collect::<Result<_, _>>()?
            }
            _ => return Err(USAGE.into()),
        }
    }
    let samples = load(&dir, &split)?;
    eprintln!("{} streams", samples.len());
    if phases_only {
        phases(&samples);
        return Ok(());
    }

    let results: Vec<(usize, Vec<Outcome>)> = samples
        .par_iter()
        .enumerate()
        .map(|(i, s)| {
            let expect = flate_recons::deflate::parse(&s.raw)
                .map(|(_, d)| d)
                .unwrap_or_default();
            (i, tools.iter().map(|&t| run(t, &s.raw, &expect)).collect())
        })
        .collect();

    let mut rows = String::from("file\tspecs\traw\ttool\tsize\tsplit_ms\tjoin_ms\terror\n");
    for (i, outs) in &results {
        let s = &samples[*i];
        for (t, o) in tools.iter().zip(outs) {
            rows += &format!(
                "{}\t{}\t{}\t{}\t{}\t{:.3}\t{:.3}\t{}\n",
                s.file,
                s.specs,
                s.raw.len(),
                t.name(),
                o.size.as_ref().map_or(-1, |&n| n as i64),
                o.split.as_secs_f64() * 1e3,
                o.join.as_secs_f64() * 1e3,
                o.size.as_ref().err().map_or("", String::as_str)
            );
        }
    }
    std::fs::write(&tsv, rows)?;

    // Per backend: sizes over the streams every tool handled, failures separately.
    struct Agg {
        streams: usize,
        raw: usize,
        size: Vec<usize>,
        fail: Vec<usize>,
        split: Vec<Duration>,
        join: Vec<Duration>,
    }
    let nt = tools.len();
    let new = || Agg {
        streams: 0,
        raw: 0,
        size: vec![0; nt],
        fail: vec![0; nt],
        split: vec![Duration::ZERO; nt],
        join: vec![Duration::ZERO; nt],
    };
    let mut by: BTreeMap<String, Agg> = BTreeMap::new();
    let mut total = new();
    let mut errors: BTreeMap<(Tool, String), usize> = BTreeMap::new();
    for (i, outs) in &results {
        let s = &samples[*i];
        let backend = s.specs.split([':', ' ']).next().unwrap().to_owned();
        let all_ok = outs.iter().all(|o| o.size.is_ok());
        for agg in [by.entry(backend).or_insert_with(new), &mut total] {
            for (k, o) in outs.iter().enumerate() {
                agg.split[k] += o.split;
                agg.join[k] += o.join;
                if o.size.is_err() {
                    agg.fail[k] += 1;
                }
            }
            if all_ok {
                agg.streams += 1;
                agg.raw += s.raw.len();
                for (k, o) in outs.iter().enumerate() {
                    agg.size[k] += *o.size.as_ref().unwrap();
                }
            }
        }
        for (t, o) in tools.iter().zip(outs) {
            if let Err(e) = &o.size {
                let e = e.chars().take(60).collect::<String>();
                *errors.entry((*t, e)).or_default() += 1;
            }
        }
    }

    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    print!("{:<24} {:>7} {:>10}", "backend", "streams", "deflate B");
    for n in &names {
        print!(" {:>17}", format!("{n} %"));
    }
    for n in &names {
        print!(" {:>12}", format!("{n} fail"));
    }
    println!();
    let line = |name: &str, a: &Agg| {
        print!("{name:<24} {:>7} {:>10}", a.streams, a.raw);
        for &s in &a.size {
            print!(" {:>8} {:>7.3}%", s, s as f64 * 100.0 / a.raw.max(1) as f64);
        }
        for &f in &a.fail {
            print!(" {f:>12}");
        }
        println!();
    };
    for (b, a) in &by {
        line(b, a);
    }
    line("TOTAL", &total);
    println!("\nsizes count only streams every tool round-tripped; CPU time over all streams:");
    for (k, n) in names.iter().enumerate() {
        println!(
            "  {n:<12} split {:>8.1} s   join {:>8.1} s",
            total.split[k].as_secs_f64(),
            total.join[k].as_secs_f64()
        );
    }
    if !errors.is_empty() {
        println!("\nfailures:");
        for ((t, e), n) in &errors {
            println!("  {:<12} {n:>5}  {e}", t.name());
        }
    }
    Ok(())
}

/// CPU time of each phase of flate-recons, summed over all streams.
fn phases(samples: &[Sample]) {
    use flate_recons::{Priors, coder::Recorder, matchfind::Matcher, model};
    const NAMES: [&str; 7] = [
        "parse+check",
        "chains",
        "config search",
        "record",
        "pack",
        "unpack",
        "total",
    ];
    let priors = Priors::builtin();
    let configs = &priors.catalog;
    let t: Vec<[Duration; 7]> = samples
        .par_iter()
        .map(|s| {
            let mut t = [Duration::ZERO; 7];
            let (parsed, t0) = timed(|| flate_recons::Parsed::new(&s.raw).unwrap());
            t[0] = t0;
            t[1] = timed(|| Matcher::new(&parsed.data)).1;
            let (cfg, t2) = timed(|| parsed.best_config(configs, priors).unwrap());
            t[2] = t2;
            let (rec, t3) = timed(|| {
                let mut rec = Recorder::default();
                parsed.code(&mut rec, cfg).unwrap();
                rec
            });
            t[3] = t3;
            let (packed, t4) = timed(|| model::pack(&rec.meta, priors));
            t[4] = t4;
            t[5] = timed(|| model::unpack(&parsed.data, &packed, priors).unwrap()).1;
            t[6] = t.iter().sum();
            t
        })
        .collect();
    let plain: usize = samples
        .iter()
        .map(|s| flate_recons::deflate::parse(&s.raw).map_or(0, |(_, d)| d.len()))
        .sum();
    println!(
        "{} streams, {:.1} MB plain text",
        samples.len(),
        plain as f64 / 1e6
    );
    for (k, name) in NAMES.iter().enumerate() {
        let d: Duration = t.iter().map(|x| x[k]).sum();
        println!(
            "  {name:<14} {:>8.2} s  {:>7.1} MB/s",
            d.as_secs_f64(),
            plain as f64 / 1e6 / d.as_secs_f64()
        );
    }
}
