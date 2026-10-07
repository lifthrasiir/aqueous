use std::time::Instant;

fn usage() -> ! {
    eprintln!("usage: pngr c IN OUT | pngr d IN OUT | pngr bench [-n ITERS] FILE...");
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("c") if args.len() == 3 => {
            let src = std::fs::read(&args[1]).expect("read");
            std::fs::write(&args[2], png_recons::compress(&src)).expect("write");
        }
        Some("d") if args.len() == 3 => {
            let src = std::fs::read(&args[1]).expect("read");
            match png_recons::decompress(&src) {
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
    let mut ph = png_recons::container::Phases::default();
    let mut stored = 0;
    let mut fails = 0;
    for f in &files {
        let src = std::fs::read(f).expect("read");
        let mut c = vec![];
        let mut bc = f64::MAX;
        let mut p1 = Default::default();
        for _ in 0..if only == 'd' { 1 } else { iters } {
            let t = Instant::now();
            c = png_recons::container::compress_timed(&src, &mut p1);
            bc = bc.min(t.elapsed().as_secs_f64());
        }
        let mut bd = f64::MAX;
        let mut ok = true;
        for _ in 0..if only == 'c' { 1 } else { iters } {
            let t = Instant::now();
            let d = png_recons::decompress(&c);
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
        "compress phases: parse {:.3}s model {:.3}s entropy {:.3}s write {:.3}s",
        ph.parse, ph.model, ph.entropy, ph.write
    );
    let d = png_recons::container::DEC_TIMES.with(|c| c.get());
    let k = iters as f64;
    println!(
        "decompress phases (per iteration): tables {:.3}s tokens {:.3}s headers {:.3}s write {:.3}s, total {:.3}s",
        d[0] / k, d[1] / k, d[2] / k, d[3] / k, td
    );
}
