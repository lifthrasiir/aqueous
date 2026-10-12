use flac_recons::codec::STREAM_NAMES;
use flac_recons::container;
use flac_recons::pcm::Format;
use std::time::Instant;

fn usage() -> ! {
    eprintln!(
        "usage:
  flr c [-r FORMAT] IN OUT     compress (FLAC, WAV, AIFF; other files are stored)
  flr d IN OUT                 decompress
  flr bench [-r FORMAT] [-n ITERS] [-v] FILE|DIR...
  flr info FILE|DIR...
  flr fuzz [-r FORMAT] [-n N] [-s SEED] FILE|DIR...

  -r FORMAT: take the input as headerless PCM, e.g. s16le:2, s24be:6, u8:1
             (sign, bits, endianness, ':' channels)"
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

/// Options shared by the commands: (raw format, iterations / count, verbose, seed, rest).
struct Opts {
    raw: Option<Format>,
    n: Option<usize>,
    verbose: bool,
    seed: u64,
    rest: Vec<String>,
}

fn opts(args: &[String]) -> Opts {
    let mut o = Opts { raw: None, n: None, verbose: false, seed: 1, rest: Vec::new() };
    let mut i = 0;
    while i < args.len() {
        let next = |i: &mut usize| {
            *i += 1;
            args.get(*i).cloned().unwrap_or_else(|| usage())
        };
        match args[i].as_str() {
            "-r" => o.raw = Some(Format::parse(&next(&mut i)).unwrap_or_else(|| usage())),
            "-n" => o.n = Some(next(&mut i).parse().unwrap_or_else(|_| usage())),
            "-s" => o.seed = next(&mut i).parse().unwrap_or_else(|_| usage()),
            "-v" => o.verbose = true,
            a => o.rest.push(a.to_string()),
        }
        i += 1;
    }
    o
}

fn bench(args: &[String]) {
    let o = opts(args);
    let iters = o.n.unwrap_or(1);
    let (mut tin, mut tout, mut tc, mut td) = (0usize, 0usize, 0f64, 0f64);
    let mut fails = 0;
    let mut sizes_total: Vec<(usize, usize)> = vec![(0, 0); STREAM_NAMES.len()];
    for path in files(&o.rest) {
        let d = read(&path);
        let mut best_c = f64::MAX;
        let mut best_d = f64::MAX;
        let mut c = Vec::new();
        let mut sizes = Vec::new();
        for _ in 0..iters {
            let t = Instant::now();
            let (cc, ss) = container::compress_sizes(&d, o.raw);
            best_c = best_c.min(t.elapsed().as_secs_f64());
            c = cc;
            sizes = ss;
        }
        // The coded path, even when the container stores the file.
        let mut ok = container::roundtrip(&d, o.raw);
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
        if o.verbose {
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
    let o = opts(args);
    let rounds = o.n.unwrap_or(100);
    let mut seed = o.seed;
    let mut rng = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let (mut fails, mut total) = (0, 0);
    for path in files(&o.rest) {
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
            let r = std::panic::catch_unwind(|| container::compress_sizes(&m, o.raw).0);
            let ok = match r {
                Ok(c) => matches!(container::decompress(&c), Ok(ref back) if *back == m),
                Err(_) => false,
            } && std::panic::catch_unwind(|| container::roundtrip(&m, o.raw)).unwrap_or(false);
            if !ok {
                fails += 1;
                let name = format!("fuzz_failure_{round}");
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
        match flac_recons::codec::encode(&d) {
            Some(c) => {
                let sz: Vec<String> = c.streams.iter().map(|s| s.len().to_string()).collect();
                println!(
                    "{path}: FLAC, {} bytes, streams {}, {} rANS words, {} + {} raw bytes",
                    d.len(),
                    sz.join(" "),
                    c.words.len(),
                    c.raw[0].len(),
                    c.raw[1].len()
                );
            }
            None => match flac_recons::pcm::detect(&d) {
                Some((f, start, len)) => println!("{path}: PCM {f:?}, data at {start}, {len} bytes"),
                None => println!("{path}: not recognised (stored)"),
            },
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        usage();
    }
    match args[0].as_str() {
        "c" => {
            let o = opts(&args[1..]);
            let [i, out] = &o.rest[..] else { usage() };
            let d = read(i);
            std::fs::write(out, container::compress_with(&d, o.raw)).unwrap();
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
