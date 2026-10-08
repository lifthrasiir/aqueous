use std::time::Instant;

fn usage() -> ! {
    eprintln!("usage: psdr c IN OUT | psdr d IN OUT | psdr bench [-n ITERS] [-v] [-c|-d] FILE...");
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("c") if args.len() == 3 => {
            let src = std::fs::read(&args[1]).expect("read");
            std::fs::write(&args[2], psd_recons::compress(&src)).expect("write");
        }
        Some("d") if args.len() == 3 => {
            let src = std::fs::read(&args[1]).expect("read");
            match psd_recons::decompress(&src) {
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
    let mut stored = 0;
    let mut fails = 0;
    for f in &files {
        let src = std::fs::read(f).expect("read");
        let mut c = vec![];
        let mut bc = f64::MAX;
        for _ in 0..if only == 'd' { 1 } else { iters } {
            let t = Instant::now();
            c = psd_recons::compress(&src);
            bc = bc.min(t.elapsed().as_secs_f64());
        }
        let mut bd = f64::MAX;
        let mut ok = true;
        for _ in 0..if only == 'c' { 1 } else { iters } {
            let t = Instant::now();
            let d = psd_recons::decompress(&c);
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
        let is_stored = c.get(5) == Some(&0);
        stored += is_stored as usize;
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
                if is_stored { "\tSTORED" } else { "" }
            );
        }
    }
    if std::env::var_os("PSDR_STATS").is_some() {
        let t = psd_recons::container::DEC_TIMES.with(|c| c.get());
        let k = iters as f64;
        eprintln!("decompress per iteration: known rows {:.3}s render {:.3}s model {:.3}s rows out {:.3}s", t[0] / k, t[1] / k, t[2] / k, t[3] / k);
        let t = psd_recons::container::PLAN_TIMES.with(|c| c.get());
        eprintln!("plan per iteration: RLE checks {:.3}s tiles {:.3}s ranking {:.3}s hint sampling {:.3}s", t[0] / k, t[1] / k, t[2] / k, t[3] / k);
        let t = psd_recons::container::ENC_TIMES.with(|c| c.get());
        eprintln!("compress per iteration: rows in {:.3}s encoder check {:.3}s render {:.3}s model {:.3}s", t[0] / k, t[1] / k, t[2] / k, t[3] / k);
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
}
