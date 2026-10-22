//! Compression ratio and speed of the formula module on PNG files (research tool): each
//! file's formula as an archive, and the archive evaluated back.
use aqr_formula::Apply;
use aqr_formula::aqr::{self, Level};
use aqr_png::formula::{self, MODULE};
use std::time::Instant;

fn usage() -> ! {
    eprintln!("usage: pngr bench [-t N] [-n ITERS] [-v] [-c | -d] FILE...");
    std::process::exit(2);
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("bench") => args.remove(0),
        _ => usage(),
    };
    let mut threads = 1;
    let mut iters = 1;
    let mut files = vec![];
    let mut verbose = false;
    // Repeat only one side (for profiling): 'c' or 'd'.
    let mut only = ' ';
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-t" => threads = it.next().and_then(|s| s.parse::<usize>().ok()).unwrap_or_else(|| usage()).max(1),
            "-n" => iters = it.next().and_then(|s| s.parse().ok()).unwrap_or_else(|| usage()),
            "-v" => verbose = true,
            "-c" => only = 'c',
            "-d" => only = 'd',
            _ => files.push(a.clone()),
        }
    }
    let apply = |a: &Apply, inputs: &[&[u8]]| {
        formula::decode(&a.params, inputs, a.len, threads).map_err(|e| aqr_formula::Error::new(e.to_string()))
    };
    let (mut tin, mut tout, mut tc, mut td) = (0f64, 0f64, 0f64, 0f64);
    let mut skipped = 0;
    let mut fails = 0;
    for f in &files {
        let src = std::fs::read(f).expect("read");
        // The formula is the module's own work; the archive is its size.
        let mut c = None;
        let mut bc = f64::MAX;
        for _ in 0..if only == 'd' { 1 } else { iters } {
            let t = Instant::now();
            c = formula::encode(&src, threads)
                .map(|g| aqr::encode(&g, Some(aqr::checksum(&src)), Level::Mid).expect("archive"));
            bc = bc.min(t.elapsed().as_secs_f64());
        }
        let Some(c) = c else {
            skipped += 1;
            if verbose {
                println!("{f}\t{}\tNO FORMULA", src.len());
            }
            continue;
        };
        let natives = aqr::natives(&[MODULE], &apply);
        let mut bd = f64::MAX;
        let mut ok = true;
        for _ in 0..if only == 'c' { 1 } else { iters } {
            let t = Instant::now();
            let d = aqr::decode(&c, &natives);
            bd = bd.min(t.elapsed().as_secs_f64());
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
                src.len() as f64 / bd / 1e6,
            );
        }
    }
    println!(
        "{} files, {:.0} -> {:.0} bytes, ratio {:.4}, compress {:.1} MB/s, decompress {:.1} MB/s, no formula {skipped}, failures {fails}",
        files.len(),
        tin,
        tout,
        tout / tin,
        tin / tc / 1e6,
        tin / td / 1e6
    );
}
