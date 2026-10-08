use sqlite_recons::container::{Options, Stats, compress_with};
use sqlite_recons::transform::{NSTREAMS, STREAM_NAMES};
use std::time::Instant;

fn usage() -> ! {
    eprintln!(
        "usage: sqlr c IN OUT | sqlr d IN OUT | sqlr bench [-n ITERS] [-l LEVEL] [-v] FILE..."
    );
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("c") if args.len() == 3 => {
            let src = std::fs::read(&args[1]).expect("read");
            std::fs::write(&args[2], sqlite_recons::compress(&src)).expect("write");
        }
        Some("d") if args.len() == 3 => {
            let src = std::fs::read(&args[1]).expect("read");
            match sqlite_recons::decompress(&src) {
                Ok(v) => std::fs::write(&args[2], v).expect("write"),
                Err(e) => {
                    eprintln!("{}: {e}", args[1]);
                    std::process::exit(1);
                }
            }
        }
        Some("bench") => bench(&args[1..]),
        _ => usage(),
    }
}

fn bench(args: &[String]) {
    let mut iters = 1;
    let mut files = vec![];
    let mut verbose = false;
    let mut opt = Options { verify: false, ..Default::default() };
    // Repeat only one side (for profiling): 'c' or 'd'.
    let mut only = ' ';
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-n" => iters = it.next().and_then(|s| s.parse().ok()).unwrap_or_else(|| usage()),
            "-l" => {
                opt.level = it.next().and_then(|s| s.parse().ok()).unwrap_or_else(|| usage());
                opt.level_bytes = opt.level;
            }
            "-V" => opt.verify = true,
            "-b" => opt.level_bytes = it.next().and_then(|s| s.parse().ok()).unwrap_or_else(|| usage()),
            "-L" => opt.ldm = false,
            "-c" => only = 'c',
            "-d" => only = 'd',
            "-v" => verbose = true,
            _ => files.push(a.clone()),
        }
    }
    let (mut tin, mut tout, mut tc, mut td) = (0f64, 0f64, 0f64, 0f64);
    let mut total = Stats::default();
    let mut fails = 0;
    let mut wholes = 0;
    for f in &files {
        let src = std::fs::read(f).expect("read");
        let mut c = vec![];
        let mut bc = f64::MAX;
        let mut st = Stats::default();
        for _ in 0..if only == 'd' { 1 } else { iters } {
            st = Stats::default();
            let t = Instant::now();
            c = compress_with(&src, &opt, &mut st);
            bc = bc.min(t.elapsed().as_secs_f64());
        }
        let mut bd = f64::MAX;
        let mut ok = true;
        for _ in 0..if only == 'c' { 1 } else { iters } {
            let t = Instant::now();
            let d = sqlite_recons::decompress(&c);
            bd = bd.min(t.elapsed().as_secs_f64());
            ok &= matches!(d, Ok(ref d) if *d == src);
        }
        if !ok {
            fails += 1;
            eprintln!("ROUNDTRIP FAILURE: {f}");
        }
        wholes += st.whole as usize;
        for i in 0..NSTREAMS {
            total.raw[i] += st.raw[i];
            total.coded[i] += st.coded[i];
        }
        total.transform_s += st.transform_s;
        total.zstd_s += st.zstd_s;
        tin += src.len() as f64;
        tout += c.len() as f64;
        tc += bc;
        td += bd;
        if verbose {
            println!(
                "{:>12} {:>10} {:.4} {:7.1} {:7.1} {}{}",
                src.len(),
                c.len(),
                c.len() as f64 / src.len() as f64,
                src.len() as f64 / bc / 1e6,
                src.len() as f64 / bd / 1e6,
                if st.whole { "[whole] " } else { "" },
                f
            );
        }
    }
    println!("streams (raw -> coded):");
    for i in 0..NSTREAMS {
        if total.raw[i] > 0 {
            println!(
                "  {:>7} {:>12} -> {:>11} ({:5.1}%)",
                STREAM_NAMES[i],
                total.raw[i],
                total.coded[i],
                100.0 * total.coded[i] as f64 / tout
            );
        }
    }
    println!(
        "{} files ({} whole, {} failed): {:.0} -> {:.0} = {:.4}; compress {:.1} MB/s (transform {:.2}s, zstd {:.2}s), decompress {:.1} MB/s",
        files.len(),
        wholes,
        fails,
        tin,
        tout,
        tout / tin,
        tin / tc / 1e6,
        total.transform_s / iters as f64,
        total.zstd_s / iters as f64,
        tin / td / 1e6
    );
}
