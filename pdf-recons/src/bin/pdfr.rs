use pdf_recons::container::{self, Options, Stats};

#[repr(C)]
struct Timespec {
    sec: i64,
    nsec: i64,
}

unsafe extern "C" {
    fn clock_gettime(clk: u32, tp: *mut Timespec) -> i32;
}

/// CPU time of the process, in seconds (wall time is skewed by other load). jpeg-recons
/// works on rayon's pool, so this counts every thread; `bench` runs with one by default.
fn cpu() -> f64 {
    #[cfg(target_os = "macos")]
    const CLOCK_PROCESS_CPUTIME_ID: u32 = 12;
    #[cfg(not(target_os = "macos"))]
    const CLOCK_PROCESS_CPUTIME_ID: u32 = 2;
    let mut t = Timespec { sec: 0, nsec: 0 };
    unsafe { clock_gettime(CLOCK_PROCESS_CPUTIME_ID, &mut t) };
    t.sec as f64 + t.nsec as f64 * 1e-9
}

fn wall() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64()
}

/// Seconds: the process's CPU time with one thread, wall time with more.
fn clock(threads: usize) -> f64 {
    if threads > 1 { wall() } else { cpu() }
}

fn cores() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

fn usage() -> ! {
    eprintln!(
        "usage: pdfr c [-t THREADS] [-s MB/S] [-l LEVEL] [-V] [-T] [-F] [-P] IN OUT | pdfr d [-t THREADS] IN OUT\n       pdfr bench [-t THREADS] [-n ITERS] [-s MB/S] [-l LEVEL] [-V] [-T] [-F] [-P] [-v] FILE...\n       pdfr fuzz [-n ROUNDS] [-r SEED] FILE..."
    );
    std::process::exit(2);
}

/// Options from the flags in `args`; the rest is returned. Threads are 0 if not given.
fn options(args: &[String]) -> (Options, usize, bool, u64, Vec<String>) {
    let mut o = Options { threads: 0, ..Options::default() };
    let (mut iters, mut verbose, mut rest) = (1, false, vec![]);
    let mut seed = 1u64;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().unwrap_or_else(|| usage()).clone();
        match a.as_str() {
            "-s" => o.speed = val().parse().unwrap_or_else(|_| usage()),
            "-l" => o.level = val().parse().unwrap_or_else(|_| usage()),
            "-t" => o.threads = val().parse::<usize>().unwrap_or_else(|_| usage()).max(1),
            "-n" => iters = val().parse().unwrap_or_else(|_| usage()),
            "-V" => o.verify = true,
            "-T" => o.tok = false,
            "-F" => o.full = false,
            "-P" => o.png = false,
            "-v" => verbose = true,
            "-r" => seed = val().parse().unwrap_or_else(|_| usage()),
            _ => rest.push(a.clone()),
        }
    }
    (o, iters, verbose, seed, rest)
}

fn read(p: &str) -> Vec<u8> {
    std::fs::read(p).unwrap_or_else(|e| {
        eprintln!("{p}: {e}");
        std::process::exit(1)
    })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (mut opts, iters, verbose, seed, rest) = options(args.get(1..).unwrap_or(&[]));
    let cmd = args.first().map(String::as_str);
    if opts.threads == 0 {
        opts.threads = if matches!(cmd, Some("c" | "d")) { cores() } else { 1 };
    }
    if opts.threads == 1 && matches!(cmd, Some("c" | "d")) {
        // With one thread the library runs as before, jpeg-recons on rayon's global pool:
        // one thread there too.
        let _ = rayon::ThreadPoolBuilder::new().num_threads(1).build_global();
    }
    match cmd {
        Some("c") if rest.len() == 2 => {
            let out = container::compress_with(&read(&rest[0]), &opts, &mut Stats::default());
            std::fs::write(&rest[1], out).unwrap();
        }
        Some("d") if rest.len() == 2 => match container::decompress_threads(&read(&rest[0]), opts.threads) {
            Ok(d) => std::fs::write(&rest[1], d).unwrap(),
            Err(e) => {
                eprintln!("{}: {e}", rest[0]);
                std::process::exit(1)
            }
        },
        Some("bench") if !rest.is_empty() => bench(&opts, iters, verbose, &rest),
        Some("fuzz") if !rest.is_empty() => fuzz(iters.max(1), seed, &rest),
        _ => usage(),
    }
}

fn bench(opts: &Options, iters: usize, verbose: bool, files: &[String]) {
    if std::env::var_os("RAYON_NUM_THREADS").is_none() {
        // SAFETY: nothing else runs yet.
        unsafe { std::env::set_var("RAYON_NUM_THREADS", "1") };
    }
    let (mut tin, mut tout, mut tc, mut td) = (0usize, 0usize, 0f64, 0f64);
    let mut tot = Stats::default();
    let mut dtot = container::DStats::default();
    // Per item: file, mode, class, raw, inflated, compression and decompression seconds.
    let mut calib = std::env::var_os("PDFR_CALIB").map(|p| std::io::BufWriter::new(std::fs::File::create(p).unwrap()));
    for f in files {
        let file = read(f);
        let (mut bc, mut bd) = (f64::INFINITY, f64::INFINITY);
        let mut packed = vec![];
        let mut st = Stats::default();
        let mut ds = container::DStats::default();
        for _ in 0..iters {
            let t = clock(opts.threads);
            st = Stats::default();
            packed = container::compress_with(&file, opts, &mut st);
            bc = bc.min(clock(opts.threads) - t);
            let t = clock(opts.threads);
            ds = container::DStats::default();
            let back = container::decompress_with_threads(&packed, &mut ds, opts.threads);
            bd = bd.min(clock(opts.threads) - t);
            if back.as_deref() != Ok(&file[..]) {
                println!("ROUND TRIP FAILED: {f}");
                std::process::exit(1);
            }
        }
        tin += file.len();
        tout += packed.len();
        tc += bc;
        td += bd;
        for (a, b) in tot.modes.iter_mut().zip(&st.modes) {
            a.0 += b.0;
            a.1 += b.1;
            a.2 += b.2;
        }
        tot.skeleton.0 += st.skeleton.0;
        tot.skeleton.1 += st.skeleton.1;
        tot.pools += st.pools;
        tot.meta += st.meta;
        tot.tok += st.tok;
        tot.tok_finish += st.tok_finish;
        tot.predicted += st.predicted;
        dtot.setup += ds.setup;
        for m in 0..container::NMODES {
            for c in 0..7 {
                tot.class_out[m][c] += st.class_out[m][c];
            }
        }
        for m in 0..container::NMODES {
            for c in 0..7 {
                tot.cost[m][c].add(&st.cost[m][c]);
                dtot.cost[m][c].add(&ds.cost[m][c]);
            }
        }
        if let Some(w) = &mut calib {
            use std::io::Write;
            for (a, b) in st.items.iter().zip(&ds.items) {
                writeln!(w, "{f}\t{}\t{}\t{}\t{}\t{:.9}\t{:.9}", a.0, a.1, a.2, a.3, a.4, b.4).unwrap();
            }
        }
        tot.failed += st.failed;
        for (a, b) in tot.times.iter_mut().zip(&st.times) {
            *a += b;
        }
        if verbose {
            println!(
                "{:>11} {:.4} {:>7.1} {:>7.1} MB/s (est. {:>7.1})  skel {:>9} -> {:>8}  {}",
                file.len(),
                packed.len() as f64 / file.len() as f64,
                file.len() as f64 / bc / 1e6,
                file.len() as f64 / bd / 1e6,
                file.len() as f64 / st.predicted * 1e3,
                st.skeleton.0,
                st.skeleton.1,
                f
            );
        }
    }
    println!(
        "{} files, {} -> {} ({:.4}), compress {:.1} MB/s, decompress {:.1} MB/s",
        files.len(),
        tin,
        tout,
        tout as f64 / tin as f64,
        tin as f64 / tc / 1e6,
        tin as f64 / td / 1e6
    );
    for (n, m) in container::MODES.iter().zip(&tot.modes) {
        println!("  {n:<5} {:>7} items {:>12} -> {:>12}", m.0, m.1, m.2);
    }
    println!(
        "  skeleton {} -> {}, pools {}, meta {}, tokens {}, failed {}",
        tot.skeleton.0, tot.skeleton.1, tot.pools, tot.meta, tot.tok, tot.failed
    );
    println!("  mode/class: items, raw, inflated; ns per raw byte compressing, decompressing; output / raw");
    let cname = |m: usize, c: usize| {
        if c < 6 {
            pdf_recons::kind::CLASSES[c].name()
        } else if m == container::TOK as usize {
            "pixels"
        } else {
            "-"
        }
    };
    for m in 0..container::NMODES {
        for c in 0..7 {
            let (a, b) = (tot.cost[m][c], dtot.cost[m][c]);
            if a.n > 0 {
                println!(
                    "    {:<5} {:<8} {:>7} {:>12} {:>13} {:>6.2} {:>6.2} {:>6.3}",
                    container::MODES[m],
                    cname(m, c),
                    a.n,
                    a.raw,
                    a.plain,
                    a.secs * 1e9 / a.raw as f64,
                    b.secs * 1e9 / b.raw.max(1) as f64,
                    if m == 2 || m >= 4 { tot.class_out[m][c] / a.raw as f64 } else { f64::NAN }
                );
            }
        }
    }
    println!(
        "  estimated time {:.3} s (measured {:.3} s compressing, {:.3} s decompressing)",
        tot.predicted * 1e-9,
        tc,
        td
    );
    println!("  token coder finish {:.3} s, decompression setup {:.3} s", tot.tok_finish, dtot.setup);
    let t = tot.times;
    println!(
        "  compress phases (s, last run): plan {:.3}, parse {:.3}, flate-recons {:.3}, jpeg-recons {:.3}, sections {:.3}",
        t[0], t[1], t[2], t[3], t[4]
    );
}

/// Mutated files must round-trip under every set of modes (with the panic net off), and
/// corrupted containers must fail cleanly. Failing inputs are saved as `fuzz_failure_*`.
fn fuzz(rounds: usize, seed: u64, files: &[String]) {
    let mut x = seed.wrapping_mul(0x9e3779b97f4a7c15) | 1;
    let mut rnd = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let base = Options::default();
    let modes = [
        Options { speed: f64::INFINITY, ..base.clone() },
        Options { speed: f64::INFINITY, full: false, png: false, ..base.clone() },
        Options { speed: f64::INFINITY, tok: false, png: false, ..base.clone() },
        base,
    ];
    let inputs: Vec<Vec<u8>> = files.iter().map(|f| read(f)).collect();
    let mut failures = 0;
    for round in 0..rounds {
        let mut f = inputs[rnd() as usize % inputs.len()].clone();
        if f.is_empty() {
            continue;
        }
        for _ in 0..1 + rnd() % 8 {
            let n = f.len().max(1);
            let at = rnd() as usize % n;
            match rnd() % 6 {
                0 => f[at.min(n - 1)] ^= 1 << (rnd() % 8),
                1 => f.truncate(at),
                2 => {
                    let k = (rnd() % 64) as usize;
                    f.drain(at..(at + k).min(f.len()));
                }
                3 => {
                    let junk: Vec<u8> = (0..rnd() % 64).map(|_| rnd() as u8).collect();
                    f.splice(at..at, junk);
                }
                4 => {
                    // Copy a piece of the file elsewhere (repeated streams, odd structure).
                    let k = (rnd() % 4096) as usize;
                    let piece = f[at..(at + k).min(f.len())].to_vec();
                    let to = rnd() as usize % f.len().max(1);
                    f.splice(to..to, piece);
                }
                _ => {
                    if let Some(i) = memchr::memmem::find(&f[at..], b"stream") {
                        let p = at + i + 7 + (rnd() % 64) as usize;
                        if p < f.len() {
                            f[p] = rnd() as u8;
                        }
                    }
                }
            }
            if f.is_empty() {
                break;
            }
        }
        for o in &modes {
            let ok = std::panic::catch_unwind(|| {
                let c = container::compress_unchecked(&f, o, &mut Stats::default());
                let back = container::decompress(&c);
                // Threads: the same bytes, and the same file back.
                let threads = [2, 3, 4][round % 3];
                let ot = Options { threads, ..o.clone() };
                let same = container::compress_unchecked(&f, &ot, &mut Stats::default()) == c;
                let back_t = container::decompress_threads(&c, threads);
                // A corrupted container must not panic either, and fails the same way with
                // threads.
                let mut m = c.clone();
                let mut corrupt_same = true;
                if !m.is_empty() {
                    let k = (m.len() * 7919 + 13) % m.len();
                    m[k] ^= 0x10;
                    corrupt_same = container::decompress(&m) == container::decompress_threads(&m, threads);
                }
                same && corrupt_same && back.as_deref() == Ok(&f[..]) && back_t == back
            });
            if !matches!(ok, Ok(true)) {
                failures += 1;
                let path = format!("fuzz_failure_{round}.pdf");
                std::fs::write(&path, &f).unwrap();
                println!("round {round}: {} ({:?}), saved {path}", if ok.is_err() { "panic" } else { "mismatch" }, o);
                break;
            }
        }
        if (round + 1) % 100 == 0 {
            println!("{} rounds, {failures} failures", round + 1);
        }
    }
    println!("{rounds} rounds, {failures} failures");
    if failures > 0 {
        std::process::exit(1);
    }
}
