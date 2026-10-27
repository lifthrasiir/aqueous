use aqr_flac::formula;
use aqr_formula::{Apply, Formula, aqr};
use aqr_raw::audio::pcm::Format;
use aqr_raw::container::{self, Layout};
use std::collections::BTreeMap;
use std::time::Instant;

fn usage() -> ! {
    eprintln!(
        "usage:
  aqr-flac bench [-r FORMAT] [-t THREADS] [-n ITERS] [-v] FILE|DIR...   (sizes as .aqr archives)
  aqr-flac info FILE|DIR...
  aqr-flac fuzz [-r FORMAT] [-t THREADS] [-n N] [-s SEED] FILE|DIR...

  FLAC files are coded by aqr-flac, WAV and AIFF (and headerless PCM) by aqr-raw.
  -r FORMAT: take the input as headerless PCM, e.g. s16le:2, s24be:6, u8:1
             (sign, bits, endianness, ':' channels)
  -t THREADS: bench defaults to 1; fuzz also checks that 2-4 threads (or THREADS) give
             the same output as one"
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

/// Options shared by the commands: (raw format, threads, iterations / count, verbose,
/// seed, rest).
struct Opts {
    raw: Option<Format>,
    threads: Option<usize>,
    n: Option<usize>,
    verbose: bool,
    seed: u64,
    rest: Vec<String>,
}

fn opts(args: &[String]) -> Opts {
    let mut o = Opts { raw: None, threads: None, n: None, verbose: false, seed: 1, rest: Vec::new() };
    let mut i = 0;
    while i < args.len() {
        let next = |i: &mut usize| {
            *i += 1;
            args.get(*i).cloned().unwrap_or_else(|| usage())
        };
        match args[i].as_str() {
            "-r" => o.raw = Some(Format::parse(&next(&mut i)).unwrap_or_else(|| usage())),
            "-t" => o.threads = Some(next(&mut i).parse::<usize>().unwrap_or_else(|_| usage()).max(1)),
            "-n" => o.n = Some(next(&mut i).parse().unwrap_or_else(|_| usage())),
            "-s" => o.seed = next(&mut i).parse().unwrap_or_else(|_| usage()),
            "-v" => o.verbose = true,
            a => o.rest.push(a.to_string()),
        }
        i += 1;
    }
    o
}

/// The modules formulas of [`encode`] apply.
const MODULES: [aqr_formula::Module; 2] = [formula::MODULE, aqr_raw::formula::pcm::MODULE];

/// The modules' decoders on `threads` threads, as the modules of [`Formula::eval`] and archives.
fn modules(threads: usize) -> impl Fn(&Apply, &[&[u8]]) -> aqr_formula::Result<Vec<u8>> {
    move |a, inputs| {
        let res: Vec<&[u8]> = a.resources.iter().map(|r| &r[..]).collect();
        let r = if a.module == formula::MODULE {
            formula::decode(&a.params, &res, inputs, a.len, threads).map_err(|e| e.0)
        } else {
            assert_eq!(a.module, aqr_raw::formula::pcm::MODULE);
            aqr_raw::formula::pcm::decode(&a.params, &res, inputs, a.len, threads).map_err(|e| e.0)
        };
        r.map_err(aqr_formula::Error::new)
    }
}

/// The formula of `d`: FLAC by aqr-flac, anything else as audio by aqr-raw (headerless PCM of
/// `raw`, or a WAV or AIFF file). None where there is none (a panic of the coder is none).
fn encode(d: &[u8], raw: Option<Format>, threads: usize) -> Option<Formula> {
    std::panic::catch_unwind(|| match raw {
        Some(fmt) => aqr_raw::formula::encode_samples(d, Layout::Pcm(fmt), threads),
        None => formula::encode(d, threads).or_else(|| {
            let found = container::detect(d).filter(|c| matches!(c.layout, Layout::Pcm(_)))?;
            aqr_raw::formula::encode_found(d, &found, threads)
        }),
    })
    .ok()?
}

fn bench(args: &[String]) {
    let o = opts(args);
    let threads = o.threads.unwrap_or(1);
    let iters = o.n.unwrap_or(1);
    let (mut tin, mut tout, mut tc, mut td) = (0usize, 0usize, 0f64, 0f64);
    let (mut fails, mut skipped) = (0, 0);
    let mut sizes_total = BTreeMap::<String, usize>::new();
    for path in files(&o.rest) {
        let d = read(&path);
        let mut best_c = f64::MAX;
        let mut best_d = f64::MAX;
        let mut coded = None;
        for _ in 0..iters {
            let t = Instant::now();
            let f = encode(&d, o.raw, threads);
            let c = f.as_ref().map(|f| aqr::encode(f, Some(aqr::checksum(&d)), aqr::Level::MID).unwrap());
            best_c = best_c.min(t.elapsed().as_secs_f64());
            coded = f.zip(c);
        }
        let Some((f, c)) = coded else {
            skipped += 1;
            if o.verbose {
                println!("{path}: not coded");
            }
            continue;
        };
        let m = modules(threads);
        let mut ok = f.eval(&m).is_ok_and(|r| r == d);
        if !ok {
            eprintln!("{path}: formula does not evaluate to the input");
        }
        for _ in 0..iters {
            let t = Instant::now();
            let r = aqr::decode(&c, &aqr::natives(&MODULES, &m));
            best_d = best_d.min(t.elapsed().as_secs_f64());
            match r {
                Ok(r) if r == d => {}
                Ok(_) => ok = false,
                Err(e) => {
                    eprintln!("{path}: {e}");
                    ok = false
                }
            }
        }
        if !ok {
            fails += 1;
            println!("FAIL {path}");
        }
        for s in &f.streams {
            *sizes_total.entry(s.class.to_string()).or_default() += s.data.len();
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
        "total (.aqr): {tin} -> {tout} ({:.4}), encode {:.1} MB/s, decode {:.1} MB/s, {fails} failures, {skipped} not coded",
        tout as f64 / tin.max(1) as f64,
        tin as f64 / tc / 1e6,
        tin as f64 / td / 1e6
    );
    println!("streams (bytes before the archive's compression, % of input):");
    for (name, z) in &sizes_total {
        println!("  {name:16} {z:12}  {:6.2}%", *z as f64 * 100.0 / tin.max(1) as f64);
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
            let threads = o.threads.unwrap_or([2, 3, 4][round % 3]);
            // The formula (if any) evaluates to the input, on one thread and on several, and
            // is the same whatever the number of threads of the encoder.
            let ok = std::panic::catch_unwind(|| {
                let Some(f) = encode(&m, o.raw, 1) else { return true };
                f.eval(&modules(1)).is_ok_and(|r| r == m)
                    && f.eval(&modules(threads)).is_ok_and(|r| r == m)
                    && encode(&m, o.raw, threads) == Some(f)
            })
            .unwrap_or(false);
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
        match aqr_flac::codec::encode(&d) {
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
            None => match container::detect(&d) {
                Some(c) => println!("{path}: {}, {:?}, samples at {}, {} bytes", c.name, c.layout, c.start, c.len),
                None => println!("{path}: not recognised (not coded)"),
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
        "bench" => bench(&args[1..]),
        "info" => info(&args[1..]),
        "fuzz" => fuzz(&args[1..]),
        _ => usage(),
    }
}
