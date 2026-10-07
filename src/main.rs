use mp3_recons::codec::STREAM_NAMES;
use mp3_recons::container;
use std::time::Instant;

fn usage() -> ! {
    eprintln!(
        "usage:\n  mp3r c IN OUT\n  mp3r d IN OUT\n  mp3r bench [-n ITERS] [-v] FILE...\n  mp3r info FILE...\n  mp3r fuzz [-n N] [-s SEED] FILE..."
    );
    std::process::exit(2)
}

fn read(path: &str) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| {
        eprintln!("{path}: {e}");
        std::process::exit(1)
    })
}

/// Expands directories into the files below them.
fn files(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(p: &std::path::Path, out: &mut Vec<String>) {
        if p.is_dir() {
            let mut es: Vec<_> = std::fs::read_dir(p).unwrap().flatten().map(|e| e.path()).collect();
            es.sort();
            for e in es {
                walk(&e, out);
            }
        } else {
            out.push(p.to_string_lossy().into_owned());
        }
    }
    for a in args {
        walk(std::path::Path::new(a), &mut out);
    }
    out
}

fn bench(args: &[String]) {
    let mut iters = 1;
    let mut verbose = false;
    let mut paths = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-n" => {
                i += 1;
                iters = args[i].parse().unwrap();
            }
            "-v" => verbose = true,
            a => paths.push(a.to_string()),
        }
        i += 1;
    }
    let (mut tin, mut tout, mut tc, mut td) = (0usize, 0usize, 0f64, 0f64);
    let mut fails = 0;
    let mut sizes_total: Vec<(usize, usize)> = vec![(0, 0); STREAM_NAMES.len()];
    for path in files(&paths) {
        let d = read(&path);
        let mut best_c = f64::MAX;
        let mut best_d = f64::MAX;
        let mut c = Vec::new();
        let mut sizes = Vec::new();
        for _ in 0..iters {
            let t = Instant::now();
            let (cc, ss) = container::compress_sizes(&d);
            best_c = best_c.min(t.elapsed().as_secs_f64());
            c = cc;
            sizes = ss;
        }
        // The coded path, even when the container stores the file.
        let mut ok = matches!(mp3_recons::codec::decode(&mp3_recons::codec::encode(&d)), Ok(ref r) if *r == d);
        if !ok {
            eprintln!("{path}: coded path does not round-trip");
        }
        for _ in 0..iters {
            let t = Instant::now();
            let r = container::decompress(&c);
            best_d = best_d.min(t.elapsed().as_secs_f64());
            match r {
                Ok(r) if r == d => {}
                Ok(_) => ok = false,
                Err(e) => {
                    eprintln!("{path}: {}", e.0);
                    ok = false
                }
            }
        }
        if !ok {
            fails += 1;
            println!("FAIL {path}");
        }
        for (t, s) in sizes_total.iter_mut().zip(&sizes) {
            t.0 += s.0;
            t.1 += s.1;
        }
        tin += d.len();
        tout += c.len();
        tc += best_c;
        td += best_d;
        if verbose {
            println!(
                "{path}: {} -> {} ({:.4}) c {:.1} MB/s d {:.1} MB/s",
                d.len(),
                c.len(),
                c.len() as f64 / d.len() as f64,
                d.len() as f64 / best_c / 1e6,
                d.len() as f64 / best_d / 1e6
            );
        }
    }
    println!(
        "total: {tin} -> {tout} ({:.4}), compress {:.1} MB/s, decompress {:.1} MB/s, {fails} failures",
        tout as f64 / tin.max(1) as f64,
        tin as f64 / tc / 1e6,
        tin as f64 / td / 1e6
    );
    println!("streams (raw -> stored, % of input):");
    for (name, (r, z)) in STREAM_NAMES.iter().zip(&sizes_total) {
        println!("  {name:10} {r:12} -> {z:12}  {:6.2}%", *z as f64 * 100.0 / tin.max(1) as f64);
    }
}

fn fuzz(args: &[String]) {
    let mut rounds = 100;
    let mut seed = 1u64;
    let mut paths = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-n" => {
                i += 1;
                rounds = args[i].parse().unwrap();
            }
            "-s" => {
                i += 1;
                seed = args[i].parse().unwrap();
            }
            a => paths.push(a.to_string()),
        }
        i += 1;
    }
    let mut rng = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let (mut fails, mut total) = (0, 0);
    for path in files(&paths) {
        let d = read(&path);
        if d.is_empty() {
            continue;
        }
        for round in 0..rounds {
            let mut m = d.clone();
            match rng() % 4 {
                0 => {
                    // Flip a few random bytes.
                    for _ in 0..1 + rng() % 8 {
                        let p = (rng() % m.len() as u64) as usize;
                        m[p] ^= 1 << (rng() % 8);
                    }
                }
                1 => {
                    let p = (rng() % m.len() as u64) as usize;
                    m.truncate(p);
                }
                2 => {
                    let p = (rng() % m.len() as u64) as usize;
                    let n = (rng() % 600) as usize;
                    m.drain(p..(p + n).min(m.len()));
                }
                _ => {
                    let p = (rng() % m.len() as u64) as usize;
                    let ins: Vec<u8> = (0..rng() % 64).map(|_| rng() as u8).collect();
                    m.splice(p..p, ins);
                }
            }
            total += 1;
            let r = std::panic::catch_unwind(|| container::compress_sizes(&m).0);
            let ok = match r {
                Ok(c) => matches!(container::decompress(&c), Ok(ref back) if *back == m),
                Err(_) => false,
            } && std::panic::catch_unwind(|| matches!(mp3_recons::codec::decode(&mp3_recons::codec::encode(&m)), Ok(ref r) if *r == m))
                .unwrap_or(false);
            if !ok {
                fails += 1;
                let name = format!("fuzz_failure_{round}.mp3");
                std::fs::write(&name, &m).unwrap();
                println!("FAIL {path} round {round} -> {name}");
            }
        }
    }
    println!("{total} mutated inputs, {fails} failures");
}

fn info(args: &[String]) {
    for path in files(args) {
        let d = read(&path);
        let c = mp3_recons::codec::encode(&d);
        let frames = c.streams[2].len() / 4;
        println!(
            "{path}: {} bytes, {frames} frames, junk {} B, tail {} bits, raw granules {} bits, gaps {} bits, {} rANS words, {} raw bits",
            d.len(),
            c.streams[1].len(),
            c.bit_lens[0],
            c.bit_lens[1],
            c.bit_lens[2],
            c.words.len(),
            c.raw_bits
        );
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        usage();
    }
    match args[0].as_str() {
        "c" if args.len() == 3 => {
            let d = read(&args[1]);
            std::fs::write(&args[2], container::compress(&d)).unwrap();
        }
        "d" if args.len() == 3 => {
            let c = read(&args[1]);
            match container::decompress(&c) {
                Ok(d) => std::fs::write(&args[2], d).unwrap(),
                Err(e) => {
                    eprintln!("{}", e.0);
                    std::process::exit(1)
                }
            }
        }
        "bench" => bench(&args[1..]),
        "info" => info(&args[1..]),
        "fuzz" => fuzz(&args[1..]),
        _ => usage(),
    }
}
