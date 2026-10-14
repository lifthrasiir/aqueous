//! Times deconstruct and reconstruct (wall time, one thread) and measures their working
//! memory: the heap peak beyond the input (files, or streams) and the output (streams, or
//! the largest file, as files are dropped as they come). `-c`: deconstruct only; `-d`:
//! reconstruct five times (for profiling). Options from the environment: BUDGET (ns per
//! byte, or `all`), ORDER (largest, smallest, gain), SKIP, CACHE_MB.
use recons_common::mem::{CUR, Counting, PEAK, peak_reset};
use std::sync::atomic::Ordering::Relaxed;
use std::time::Instant;

#[global_allocator]
static ALLOC: Counting = Counting;

fn read_all(dir: &std::path::Path, prefix: &str, out: &mut Vec<(String, Vec<u8>)>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        let p = format!("{prefix}{}", e.file_name().to_str().unwrap());
        let ft = e.file_type().unwrap();
        if ft.is_dir() {
            read_all(&e.path(), &format!("{p}/"), out);
        } else if ft.is_file() {
            out.push((p, std::fs::read(e.path()).unwrap()));
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let only_c = args.iter().any(|a| a == "-c");
    // -d: reconstruct five times (for profiling it).
    let reps = if args.iter().any(|a| a == "-d") { 5 } else { 1 };
    let cache: usize =
        std::env::var("CACHE_MB").map_or(git_recons::DEFAULT_CACHE, |v| v.parse::<usize>().unwrap() << 20);
    let opt = git_recons::Options {
        cache_bytes: cache,
        ns_per_byte: match std::env::var("BUDGET") {
            Ok(v) if v == "all" => None,
            Ok(v) => Some(v.parse().unwrap()),
            Err(_) => Some(git_recons::budget::TARGET_NS_PER_BYTE),
        },
        order: git_recons::budget::Order::from_env(),
        skip: std::env::var_os("SKIP").is_some(),
    };
    for dir in args.iter().filter(|a| !a.starts_with('-')) {
        let mut files = vec![];
        read_all(std::path::Path::new(dir), "", &mut files);
        let input: usize = files.iter().map(|f| f.1.len()).sum();
        let base = peak_reset();
        let t = Instant::now();
        let mut sink = git_recons::enc::VecSink::default();
        git_recons::enc::encode(files, &opt, &mut sink);
        let tc = t.elapsed().as_secs_f64();
        // The sink's chunks are the output (the files are freed inside, after the peak).
        let wc = PEAK.load(Relaxed) as i64 - base as i64 - sink.bytes() as i64;
        let streams = sink.finish();
        let mb = input as f64 / 1e6;
        let mut line =
            format!("{dir}: {mb:.1} MB  deconstruct {tc:.2}s ({:.1} MB/s, working {:.1} MB)", mb / tc, wc as f64 / 1e6);
        if !only_c {
            let base = peak_reset();
            let mut largest = 0;
            let t = Instant::now();
            for _ in 0..reps {
                let a = git_recons::Archive::open_with(&streams.each_ref().map(|s| &s[..]), cache).unwrap();
                a.for_each(|_, b| {
                    largest = largest.max(b.len());
                    Ok(())
                })
                .unwrap();
            }
            let td = t.elapsed().as_secs_f64() / reps as f64;
            let wd = PEAK.load(Relaxed) as i64 - base as i64 - largest as i64;
            line += &format!("  reconstruct {td:.2}s ({:.1} MB/s, working {:.1} MB)", mb / td, wd as f64 / 1e6);
        }
        let _ = CUR.load(Relaxed);
        println!("{line}");
    }
}
