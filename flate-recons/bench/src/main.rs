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
usage: flate-recons-bench [-i DIR] [-s held|train|all] [-t TOOL,...] [-P PROFILE] [-o TSV] [--no-zlib]
       flate-recons-bench -f RAW [-t TOOL,...] [-P PROFILE] [-j THREADS] [--no-zlib]
  DIR   samples cached by flate-recons-train samples (default ../cache/samples)
  -s    which split to run (default held: every fifth input file, as in training)
  -t    tools (default recons,preflate,preflate-rs)
  -P    flate-recons' profile (default 10)
  -o    per-stream results (default ../cache/bench.tsv)
  --no-zlib turn flate-recons' zlib emulation off (adaptive profiles)
  -f    instead, run the tools on one (long) raw DEFLATE stream and report wall and process
        CPU times; run one tool per process for its peak RSS
  -j    threads for flate-recons in -f mode (default: available parallelism)
  PREFLATE_RS_CHUNK  preflate-rs' plain text limit per chunk in -f mode (default 128 MiB)
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

/// CPU time of the whole process (all threads).
fn process_cpu() -> Duration {
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
    let tv = |t: libc::timeval| Duration::new(t.tv_sec as u64, t.tv_usec as u32 * 1000);
    tv(ru.ru_utime) + tv(ru.ru_stime)
}

/// Peak resident set size of the process, in bytes.
fn peak_rss() -> u64 {
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
    // Bytes on macOS, KiB elsewhere.
    if cfg!(target_os = "macos") {
        ru.ru_maxrss as u64
    } else {
        ru.ru_maxrss as u64 * 1024
    }
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
    fn split(self, raw: &[u8], profile: u16, zlib: bool) -> Result<(Vec<u8>, Vec<u8>), String> {
        match self {
            Tool::Recons => recons_split(raw, profile, zlib, 1),
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
            // One thread: times are the calling thread's CPU time.
            Tool::Recons => {
                flate_recons::reconstruct_with_threads(plain, diff, 1).map_err(|e| e.to_string())
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

fn run(tool: Tool, raw: &[u8], expect: &[u8], profile: u16, zlib: bool) -> Outcome {
    let (r, split) = timed(|| tool.split(raw, profile, zlib));
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
    let mut profile = flate_recons::ADAPTIVE;
    let mut use_zlib = true;
    let mut file = None;
    let mut threads = flate_recons::default_threads();
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
            "-f" => file = Some(PathBuf::from(val()?)),
            "-j" => threads = val()?.parse()?,
            "-P" => profile = val()?.parse()?,
            "--no-zlib" => use_zlib = false,
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
    if let Some(f) = file {
        return run_file(&f, &tools, profile, use_zlib, threads);
    }
    let samples = load(&dir, &split)?;
    eprintln!("{} streams", samples.len());

    let results: Vec<(usize, Vec<Outcome>)> = samples
        .par_iter()
        .enumerate()
        .map(|(i, s)| {
            let expect = flate_recons::deflate::parse(&s.raw)
                .map(|(_, d)| d)
                .unwrap_or_default();
            (
                i,
                tools
                    .iter()
                    .map(|&t| run(t, &s.raw, &expect, profile, use_zlib))
                    .collect(),
            )
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

fn recons_split(
    raw: &[u8],
    profile: u16,
    zlib: bool,
    threads: usize,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    let opts = flate_recons::Options {
        threads,
        zlib,
        ..flate_recons::Options::for_profile(profile).map_err(|e| e.to_string())?
    };
    flate_recons::deconstruct_with(raw, &opts).map_err(|e| e.to_string())
}

/// preflate-rs' whole-stream API stops at `plain_text_limit` (128 MiB by default, or
/// `$PREFLATE_RS_CHUNK`), so long streams go through its stream processor chunk by chunk, as
/// its container does. The chunks'
/// reconstruction data are concatenated, each after its own and its plain text's length.
fn preflate_rs_split_chunked(raw: &[u8]) -> Result<(Vec<u8>, Vec<u8>), String> {
    let config = preflate_rs::PreflateConfig {
        verify_compression: false,
        plain_text_limit: std::env::var("PREFLATE_RS_CHUNK")
            .map_or(128 << 20, |v| v.parse().unwrap()),
        ..Default::default()
    };
    let mut state = preflate_rs::PreflateStreamProcessor::new(&config);
    let (mut plain, mut diff, mut pos) = (vec![], vec![], 0);
    while !state.is_done() {
        let r = state
            .decompress(&raw[pos..])
            .map_err(|e| format!("{e:?}"))?;
        pos += r.compressed_size;
        let text = state.plain_text().text();
        diff.extend((r.corrections.len() as u32).to_le_bytes());
        diff.extend((text.len() as u32).to_le_bytes());
        diff.extend(&r.corrections);
        plain.extend_from_slice(text);
        state.shrink_to_dictionary();
    }
    if pos != raw.len() {
        return Err(format!("only {pos} of {} bytes processed", raw.len()));
    }
    Ok((plain, diff))
}

fn preflate_rs_join_chunked(plain: &[u8], mut diff: &[u8]) -> Result<Vec<u8>, String> {
    let mut state = preflate_rs::RecreateStreamProcessor::new();
    let (mut out, mut pos) = (vec![], 0);
    while !diff.is_empty() {
        let n = |d: &[u8]| u32::from_le_bytes(d[..4].try_into().unwrap()) as usize;
        let (cn, pn) = (n(diff), n(&diff[4..]));
        let chunk = &mut std::io::Cursor::new(&plain[pos..pos + pn]);
        let (o, _) = state
            .recompress(chunk, &diff[8..8 + cn])
            .map_err(|e| format!("{e:?}"))?;
        out.extend(o);
        pos += pn;
        diff = &diff[8 + cn..];
    }
    Ok(out)
}

/// Runs `tools` on one raw DEFLATE stream: size, and wall and process CPU times of each step.
fn run_file(
    path: &Path,
    tools: &[Tool],
    profile: u16,
    zlib: bool,
    threads: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let raw = std::fs::read(path)?;
    let measure = |f: &mut dyn FnMut() -> Result<Vec<u8>, String>| {
        let (w, c) = (std::time::Instant::now(), process_cpu());
        let r = f();
        (r, w.elapsed(), process_cpu() - c)
    };
    println!(
        "{}: {} B raw\n{:<12} {:>12} {:>8} {:>18} {:>18} {:>10}",
        path.display(),
        raw.len(),
        "tool",
        "size B",
        "%",
        "split wall/CPU s",
        "join wall/CPU s",
        "peak RSS"
    );
    for &tool in tools {
        let mut plain = vec![];
        let (diff, sw, sc) = measure(&mut || {
            let (p, d) = match tool {
                Tool::PreflateRs => preflate_rs_split_chunked(&raw)?,
                Tool::Recons => recons_split(&raw, profile, zlib, threads)?,
                _ => tool.split(&raw, profile, zlib)?,
            };
            plain = p;
            Ok(d)
        });
        let diff = match diff {
            Ok(d) => d,
            Err(e) => {
                println!(
                    "{:<12} split failed after {:.1} s: {e}",
                    tool.name(),
                    sw.as_secs_f64()
                );
                continue;
            }
        };
        let (back, jw, jc) = measure(&mut || match tool {
            Tool::Recons => flate_recons::reconstruct_with_threads(&plain, &diff, threads)
                .map_err(|e| e.to_string()),
            Tool::PreflateRs => preflate_rs_join_chunked(&plain, &diff),
            _ => tool.join(&plain, &diff),
        });
        let status = match back {
            Ok(b) if b == raw => String::new(),
            Ok(_) => "  RECONSTRUCTION DIFFERS".into(),
            Err(e) => format!("  join failed: {e}"),
        };
        println!(
            "{:<12} {:>12} {:>7.4}% {:>8.1} / {:>7.1} {:>8.1} / {:>7.1} {:>7.0} MB{status}",
            tool.name(),
            diff.len(),
            diff.len() as f64 * 100.0 / raw.len() as f64,
            sw.as_secs_f64(),
            sc.as_secs_f64(),
            jw.as_secs_f64(),
            jc.as_secs_f64(),
            peak_rss() as f64 / 1e6
        );
    }
    Ok(())
}
