//! `aqr-psd bench`: the formula of each file, as an archive, and back.

use aqr_formula::aqr::{self, Level};
use aqr_psd::formula::{self, MODULE};
use std::time::Instant;

fn usage() -> ! {
    eprintln!("usage: aqr-psd bench [-n ITERS] [-t THREADS] [-v] [-c|-d] FILE...\n(-t defaults to 1)");
    std::process::exit(2);
}

/// The value of `-t`.
fn threads_arg(it: &mut std::slice::Iter<String>) -> usize {
    it.next().and_then(|s| s.parse::<usize>().ok()).unwrap_or_else(|| usage()).max(1)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("bench") => bench(&args[1..]),
        _ => usage(),
    }
}

fn bench(args: &[String]) {
    let mut iters = 1;
    let mut files = vec![];
    let mut verbose = false;
    // Repeat only one side (for profiling): 'c' or 'd'.
    let mut only = ' ';
    let mut threads = 1;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-n" => iters = it.next().and_then(|s| s.parse().ok()).unwrap_or_else(|| usage()),
            "-t" => threads = threads_arg(&mut it),
            "-v" => verbose = true,
            "-c" => only = 'c',
            "-d" => only = 'd',
            _ => files.push(a.clone()),
        }
    }
    let (mut tin, mut tout, mut tc, mut td) = (0f64, 0f64, 0f64, 0f64);
    let mut skipped = 0;
    let mut fails = 0;
    let apply = |a: &aqr_formula::Apply, inputs: &[&[u8]]| formula::apply(a, inputs, threads);
    let natives = aqr::natives(&[MODULE, aqr_png::formula::MODULE, aqr_jpeg::formula::MODULE], &apply);
    for f in &files {
        let src = std::fs::read(f).expect("read");
        let mut c = None;
        let mut bc = f64::MAX;
        for _ in 0..if only == 'd' { 1 } else { iters } {
            let t = Instant::now();
            c = formula::encode(&src, threads)
                .map(|fm| aqr::encode(&fm, Some(aqr::checksum(&src)), Level::MID).expect("archive"));
            bc = bc.min(t.elapsed().as_secs_f64());
        }
        let Some(c) = c else {
            skipped += 1;
            if verbose {
                println!("{f}\t{} bytes\tNOT CODED", src.len());
            }
            continue;
        };
        let mut bd = f64::MAX;
        let mut ok = true;
        for _ in 0..if only == 'c' { 1 } else { iters } {
            let t = Instant::now();
            let d = aqr::decode(&c, &natives);
            bd = bd.min(t.elapsed().as_secs_f64());
            if let Err(e) = &d {
                eprintln!("{f}: {e}");
            }
            ok &= d.as_deref().ok() == Some(&src[..]);
        }
        if !ok {
            fails += 1;
            println!("ROUNDTRIP FAILURE {f}");
        }
        tin += src.len() as f64;
        tout += c.len() as f64;
        tc += bc;
        td += bd;
        if verbose {
            println!(
                "{f}\t{} -> {} ({:.4})\tc {:.1} MB/s\td {:.1} MB/s",
                src.len(),
                c.len(),
                c.len() as f64 / src.len() as f64,
                src.len() as f64 / bc / 1e6,
                src.len() as f64 / bd / 1e6
            );
        }
    }
    if std::env::var_os("PSDR_STATS").is_some() {
        let t = aqr_psd::codec::DEC_TIMES.get();
        let k = iters as f64;
        eprintln!(
            "decode per iteration: known rows {:.3}s render {:.3}s model {:.3}s rows out {:.3}s",
            t[0] / k,
            t[1] / k,
            t[2] / k,
            t[3] / k
        );
        let t = aqr_psd::codec::PLAN_TIMES.get();
        eprintln!(
            "plan per iteration: RLE checks {:.3}s tiles {:.3}s ranking {:.3}s hint sampling {:.3}s",
            t[0] / k,
            t[1] / k,
            t[2] / k,
            t[3] / k
        );
        let t = aqr_psd::codec::ENC_TIMES.get();
        eprintln!(
            "encode per iteration: rows in {:.3}s encoder check {:.3}s render {:.3}s model {:.3}s",
            t[0] / k,
            t[1] / k,
            t[2] / k,
            t[3] / k
        );
    }
    println!(
        "{} files, {:.0} -> {:.0} bytes (.aqr), ratio {:.4}, compress {:.1} MB/s, decompress {:.1} MB/s, not coded {skipped}, failures {fails}",
        files.len(),
        tin,
        tout,
        tout / tin,
        tin / tc / 1e6,
        tin / td / 1e6
    );
}
