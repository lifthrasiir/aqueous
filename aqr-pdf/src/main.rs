use aqr_formula::aqr::{self, Level};
use aqr_formula::{Apply, Formula};
use aqr_pdf::encode::{self, Options, Stats};
use aqr_pdf::formula::{self, MODES, MODULE};

#[repr(C)]
struct Timespec {
    sec: i64,
    nsec: i64,
}

unsafe extern "C" {
    fn clock_gettime(clk: u32, tp: *mut Timespec) -> i32;
}

/// CPU time of the process, in seconds (wall time is skewed by other load). aqr-jpeg
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

fn usage() -> ! {
    eprintln!(
        "usage: aqr-pdf bench [-t THREADS] [-n ITERS] [-s MB/S] [-l LEVEL] [-V] [-T] [-F] [-P] [-v] FILE...\n       aqr-pdf fuzz [-n ROUNDS] [-r SEED] FILE..."
    );
    std::process::exit(2);
}

/// Options from the flags in `args`; the rest is returned.
fn options(args: &[String]) -> (Options, usize, bool, u64, Vec<String>) {
    let mut o = Options::default();
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
    let (opts, iters, verbose, seed, rest) = options(args.get(1..).unwrap_or(&[]));
    match args.first().map(String::as_str) {
        Some("bench") if !rest.is_empty() => bench(&opts, iters, verbose, &rest),
        Some("fuzz") if !rest.is_empty() => fuzz(iters.max(1), seed, &rest),
        _ => usage(),
    }
}

/// The archive of `file`'s formula.
fn archive(f: &Formula, file: &[u8]) -> Vec<u8> {
    aqr::encode(f, Some(aqr::checksum(file)), Level::Mid).expect("an archive")
}

/// The file in archive `c`, with `threads` threads for this module.
fn unarchive(c: &[u8], threads: usize) -> Result<Vec<u8>, aqr::Failure> {
    let run = |a: &Apply, i: &[&[u8]]| formula::apply(a, i, threads);
    let modules = [MODULE, aqr_flate::formula::MODULE, aqr_png::formula::MODULE, aqr_jpeg::formula::MODULE];
    aqr::decode(c, &aqr::natives(&modules, &run))
}

/// Archives of the files, their sizes and times (files with no formula count as they are,
/// outside the times).
fn bench(opts: &Options, iters: usize, verbose: bool, files: &[String]) {
    if std::env::var_os("RAYON_NUM_THREADS").is_none() {
        // SAFETY: nothing else runs yet.
        unsafe { std::env::set_var("RAYON_NUM_THREADS", "1") };
    }
    let (mut tin, mut tout, mut tc, mut td, mut timed, mut none) = (0usize, 0usize, 0f64, 0f64, 0usize, 0usize);
    let mut tot = Stats::default();
    for f in files {
        let file = read(f);
        let (mut bc, mut bd) = (f64::INFINITY, f64::INFINITY);
        let mut packed = None;
        let mut st = Stats::default();
        for _ in 0..iters {
            let t = clock(opts.threads);
            st = Stats::default();
            let Some(fm) = formula::encode_stats(&file, opts, &mut st) else { break };
            let c = archive(&fm, &file);
            bc = bc.min(clock(opts.threads) - t);
            let t = clock(opts.threads);
            let back = unarchive(&c, opts.threads);
            bd = bd.min(clock(opts.threads) - t);
            if back.as_deref() != Ok(&file[..]) {
                println!("ROUND TRIP FAILED: {f}");
                std::process::exit(1);
            }
            packed = Some(c);
        }
        let out = packed.as_ref().map_or(file.len(), Vec::len);
        tin += file.len();
        tout += out;
        if packed.is_some() {
            tc += bc;
            td += bd;
            timed += file.len();
        } else {
            none += 1;
        }
        for (a, b) in tot.modes.iter_mut().zip(&st.modes) {
            a.0 += b.0;
            a.1 += b.1;
            a.2 += b.2;
        }
        tot.predicted += st.predicted;
        tot.failed += st.failed;
        for (a, b) in tot.times.iter_mut().zip(&st.times) {
            *a += b;
        }
        if verbose {
            println!(
                "{:>11} {:.4} {:>7.1} {:>7.1} MB/s (est. {:>7.1})  {}",
                file.len(),
                out as f64 / file.len() as f64,
                file.len() as f64 / bc / 1e6,
                file.len() as f64 / bd / 1e6,
                file.len() as f64 / st.predicted * 1e3,
                f
            );
        }
    }
    println!(
        "{} files ({none} without a formula), {} -> {} ({:.4}), encode {:.1} MB/s, decode {:.1} MB/s (of the others)",
        files.len(),
        tin,
        tout,
        tout as f64 / tin as f64,
        timed as f64 / tc / 1e6,
        timed as f64 / td / 1e6
    );
    for (n, m) in MODES.iter().zip(&tot.modes) {
        println!("  {n:<5} {:>7} items {:>12} -> {:>12}", m.0, m.1, m.2);
    }
    println!("  failed {}", tot.failed);
    println!("  estimated time {:.3} s (measured {:.3} s encoding, {:.3} s decoding)", tot.predicted * 1e-9, tc, td);
    let t = tot.times;
    println!(
        "  encode phases (s, last run): plan {:.3}, parse {:.3}, aqr-flate {:.3}, aqr-jpeg and aqr-png {:.3}",
        t[0], t[1], t[2], t[3]
    );
}

/// Mutated files must round-trip under every set of modes (with the panic net off), and
/// corrupted archives must fail cleanly. Failing inputs are saved as `fuzz_failure_*`.
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
                let Some(fm) = encode::try_formula(&f, o, &mut Stats::default()) else { return true };
                let eval = |fm: &Formula, t: usize| fm.eval(&|a: &Apply, i: &[&[u8]]| formula::apply(a, i, t)).ok();
                // Threads: the same formula, and the same file back.
                let threads = [2, 3, 4][round % 3];
                let ot = Options { threads, ..o.clone() };
                let same = encode::try_formula(&f, &ot, &mut Stats::default()).as_ref() == Some(&fm);
                let back = eval(&fm, 1);
                let back_t = eval(&fm, threads);
                // A corrupted archive must not panic either, and fails the same way with
                // threads, or decodes to the file (its checksum).
                let mut m = archive(&fm, &f);
                let k = (m.len() * 7919 + 13) % m.len();
                m[k] ^= 0x10;
                let corrupt = unarchive(&m, 1);
                let corrupt_ok = corrupt == unarchive(&m, threads) && corrupt.map_or(true, |d| d == f);
                same && corrupt_ok && back.as_deref() == Some(&f[..]) && back_t == back
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
