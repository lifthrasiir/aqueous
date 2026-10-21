use std::time::Instant;

fn usage() -> ! {
    eprintln!("usage: pngr c [-t N] IN OUT | pngr d [-t N] IN OUT | pngr bench [-t N] [-n ITERS] FILE...");
    std::process::exit(2);
}

fn cores() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

/// Takes `-t N` out of `args`.
fn threads_opt(args: &mut Vec<String>, default: usize) -> usize {
    match args.iter().position(|a| a == "-t") {
        Some(i) if i + 1 < args.len() => {
            let n = args[i + 1].parse::<usize>().unwrap_or_else(|_| usage());
            args.drain(i..i + 2);
            n.max(1)
        }
        Some(_) => usage(),
        None => default,
    }
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("bench") => return bench(&mut args[1..].to_vec()),
        Some("c" | "d") => {}
        _ => usage(),
    }
    let threads = threads_opt(&mut args, cores());
    match args.first().map(String::as_str) {
        Some("c") if args.len() == 3 => {
            let src = std::fs::read(&args[1]).expect("read");
            std::fs::write(&args[2], aqr_png::compress_threads(&src, threads)).expect("write");
        }
        Some("d") if args.len() == 3 => {
            let src = std::fs::read(&args[1]).expect("read");
            match aqr_png::decompress_threads(&src, threads) {
                Ok(v) => std::fs::write(&args[2], v).expect("write"),
                Err(e) => {
                    eprintln!("{}: {e}", args[1]);
                    std::process::exit(1);
                }
            }
        }
        _ => usage(),
    }
}

fn bench(args: &mut Vec<String>) {
    let threads = threads_opt(args, 1);
    let mut iters = 1;
    let mut files = vec![];
    let mut verbose = false;
    // Repeat only one side (for profiling): 'c' or 'd'.
    let mut only = ' ';
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-n" => iters = it.next().and_then(|s| s.parse().ok()).unwrap_or_else(|| usage()),
            "-v" => verbose = true,
            "-c" => only = 'c',
            "-d" => only = 'd',
            _ => files.push(a.clone()),
        }
    }
    let (mut tin, mut tout, mut tc, mut td) = (0f64, 0f64, 0f64, 0f64);
    let mut ph = aqr_png::container::Phases::default();
    let mut stored = 0;
    let mut fails = 0;
    for f in &files {
        let src = std::fs::read(f).expect("read");
        let mut c = vec![];
        let mut bc = f64::MAX;
        let mut p1 = Default::default();
        for _ in 0..if only == 'd' { 1 } else { iters } {
            let t = Instant::now();
            c = if threads > 1 {
                aqr_png::compress_threads(&src, threads)
            } else {
                aqr_png::container::compress_timed(&src, &mut p1)
            };
            bc = bc.min(t.elapsed().as_secs_f64());
        }
        let mut bd = f64::MAX;
        let mut ok = true;
        for _ in 0..if only == 'c' { 1 } else { iters } {
            let t = Instant::now();
            let d = aqr_png::decompress_threads(&c, threads);
            bd = bd.min(t.elapsed().as_secs_f64());
            ok &= d.as_deref().ok() == Some(&src[..]);
        }
        if !ok {
            fails += 1;
            println!("ROUNDTRIP FAILURE {f}");
        }
        if c.get(5) == Some(&0) {
            stored += 1;
        }
        ph.parse += p1.parse;
        ph.model += p1.model;
        ph.layout += p1.layout;
        ph.entropy += p1.entropy;
        ph.write += p1.write;
        tin += src.len() as f64;
        tout += c.len() as f64;
        tc += bc;
        td += bd;
        if verbose {
            println!(
                "{f}\t{} -> {} ({:.4})\tc {:.1} MB/s\td {:.1} MB/s{}",
                src.len(),
                c.len(),
                c.len() as f64 / src.len() as f64,
                src.len() as f64 / bc / 1e6,
                src.len() as f64 / bd / 1e6,
                if c.get(5) == Some(&0) { "\tSTORED" } else { "" }
            );
        }
    }
    println!(
        "{} files, {:.0} -> {:.0} bytes, ratio {:.4}, compress {:.1} MB/s, decompress {:.1} MB/s, stored {stored}, failures {fails}",
        files.len(),
        tin,
        tout,
        tout / tin,
        tin / tc / 1e6,
        tin / td / 1e6
    );
    println!(
        "compress phases: chunks {:.3}s model {:.3}s layout {:.3}s entropy {:.3}s write {:.3}s",
        ph.parse, ph.model, ph.layout, ph.entropy, ph.write
    );
    let d = aqr_png::container::DEC_TIMES.with(|c| c.get());
    let k = iters as f64;
    println!(
        "decompress phases (per iteration): tables {:.3}s tokens {:.3}s headers {:.3}s write {:.3}s, total {:.3}s",
        d[0] / k,
        d[1] / k,
        d[2] / k,
        d[3] / k,
        td
    );
}
