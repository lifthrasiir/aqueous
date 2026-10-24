use aqr_formula::aqr::{self, Level};
use aqr_formula::{Apply, Error, Formula, Node};
use aqr_sqlite::formula::{self, MODULE, Options};
use aqr_sqlite::mem::{Counting, PEAK, peak_reset};
use aqr_sqlite::transform::{NSTREAMS, STREAM_NAMES};

#[global_allocator]
static ALLOC: Counting = Counting;
use std::sync::atomic::Ordering::Relaxed;

#[cfg(unix)]
#[repr(C)]
struct Timespec {
    sec: i64,
    nsec: i64,
}

#[cfg(unix)]
unsafe extern "C" {
    fn clock_gettime(clk: u32, tp: *mut Timespec) -> i32;
}

/// CPU time of this thread, in seconds (wall time is skewed by other load).
#[cfg(unix)]
fn cpu() -> f64 {
    #[cfg(target_os = "macos")]
    const CLOCK_THREAD_CPUTIME_ID: u32 = 16;
    #[cfg(not(target_os = "macos"))]
    const CLOCK_THREAD_CPUTIME_ID: u32 = 3;
    let mut t = Timespec { sec: 0, nsec: 0 };
    unsafe { clock_gettime(CLOCK_THREAD_CPUTIME_ID, &mut t) };
    t.sec as f64 + t.nsec as f64 * 1e-9
}

#[cfg(not(unix))]
fn cpu() -> f64 {
    wall()
}

/// Seconds: the thread's CPU time with one thread, wall time with more.
fn clock(threads: usize) -> f64 {
    if threads > 1 { wall() } else { cpu() }
}

fn wall() -> f64 {
    use std::sync::OnceLock;
    static T0: OnceLock<std::time::Instant> = OnceLock::new();
    T0.get_or_init(std::time::Instant::now).elapsed().as_secs_f64()
}

/// The database of `len` bytes that `module` rebuilds.
fn apply(a: &Apply, inputs: &[&[u8]], threads: usize) -> Result<Vec<u8>, Error> {
    assert_eq!(a.module, MODULE);
    formula::decode(&a.params, inputs, a.len, threads).map_err(|e| Error::new(e.to_string()))
}

fn eval(f: &Formula, threads: usize) -> Result<Vec<u8>, Error> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| apply(a, inputs, threads))
}

/// The file in archive `c`, if it decodes.
fn unpack(c: &[u8], threads: usize) -> Option<Vec<u8>> {
    let m = |a: &Apply, inputs: &[&[u8]]| apply(a, inputs, threads);
    aqr::decode(c, &aqr::natives(&[MODULE], &m)).ok()
}

fn usage() -> ! {
    eprintln!(
        "usage: aqr-sqlite bench [-n ITERS] [-t THREADS] [-c|-d] [-m] [-v] [-x] FILE...\n       aqr-sqlite fuzz [-n N] [-s SEED] FILE..."
    );
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("bench") => bench(&args[1..]),
        Some("fuzz") => fuzz(&args[1..]),
        _ => usage(),
    }
}

/// The archive of `src` through the formula, with the raw size of each stream; `None` where
/// there is no formula.
fn pack(src: &[u8], opt: &Options) -> Option<(Vec<u8>, Vec<usize>)> {
    let f = formula::encode_with(src, opt)?;
    let raw = f.streams.iter().map(|s| s.data.len()).collect();
    Some((aqr::encode(&f, Some(aqr::checksum(src)), Level::Mid).expect("archive"), raw))
}

fn bench(args: &[String]) {
    let mut iters = 1;
    let mut files = vec![];
    let mut verbose = false;
    let mut opt = Options::default();
    let mut mem = false;
    // Repeat only one side (for profiling): 'c' or 'd'.
    let mut only = ' ';
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-n" => iters = it.next().and_then(|s| s.parse().ok()).unwrap_or_else(|| usage()),
            "-x" => opt.verify = false,
            "-m" => mem = true,
            "-t" => opt.threads = it.next().and_then(|s| s.parse().ok()).unwrap_or_else(|| usage()),
            "-c" => only = 'c',
            "-d" => only = 'd',
            "-v" => verbose = true,
            _ => files.push(a.clone()),
        }
    }
    let (mut tin, mut tout, mut tc, mut td) = (0f64, 0f64, 0f64, 0f64);
    let mut raw = [0usize; NSTREAMS];
    let mut fails = 0;
    let (mut mpeak_c, mut mpeak_d) = (0f64, 0f64);
    // Files without a formula are counted, and left out of the sizes and speeds.
    let mut skipped = 0;
    for f in &files {
        let src = std::fs::read(f).expect("read");
        let mut packed = None;
        let mut bc = f64::MAX;
        for _ in 0..if only == 'd' { 1 } else { iters } {
            let t = clock(opt.threads);
            packed = pack(&src, &opt);
            bc = bc.min(clock(opt.threads) - t);
        }
        let Some((c, sizes)) = packed else {
            skipped += 1;
            if verbose {
                println!("{:>12} {:>10} [no formula] {f}", src.len(), "-");
            }
            continue;
        };
        let mut bd = f64::MAX;
        let mut ok = true;
        for _ in 0..if only == 'c' { 1 } else { iters } {
            let t = clock(opt.threads);
            let d = unpack(&c, opt.threads);
            bd = bd.min(clock(opt.threads) - t);
            ok &= matches!(d, Some(ref d) if *d == src);
        }
        if mem {
            let base = peak_reset();
            let c2 = pack(&src, &opt).map(|p| p.0);
            let pc = PEAK.load(Relaxed) - base - c2.as_ref().map_or(0, |c| c.len());
            drop(c2);
            let base = peak_reset();
            let d2 = unpack(&c, opt.threads);
            let pd = PEAK.load(Relaxed) - base - d2.as_ref().map_or(0, |d| d.len());
            drop(d2);
            let n = src.len() as f64;
            println!(
                "mem {:>11} c {:>11} ({:5.2}x) d {:>11} ({:5.2}x) {f}",
                src.len(),
                pc,
                pc as f64 / n,
                pd,
                pd as f64 / n
            );
            mpeak_c = mpeak_c.max(pc as f64 / n);
            mpeak_d = mpeak_d.max(pd as f64 / n);
        }
        if !ok {
            fails += 1;
            eprintln!("ROUNDTRIP FAILURE: {f}");
        }
        for (r, n) in raw.iter_mut().zip(sizes) {
            *r += n;
        }
        tin += src.len() as f64;
        tout += c.len() as f64;
        tc += bc;
        td += bd;
        if verbose {
            println!(
                "{:>12} {:>10} {:.4} {:7.1} {:7.1} {}",
                src.len(),
                c.len(),
                c.len() as f64 / src.len() as f64,
                src.len() as f64 / bc / 1e6,
                src.len() as f64 / bd / 1e6,
                f
            );
        }
    }
    if mem {
        println!("heap beyond input and output, worst ratio: encode {mpeak_c:.2}x, decode {mpeak_d:.2}x");
    }
    println!("streams (raw):");
    for (name, n) in STREAM_NAMES.iter().zip(raw) {
        if n > 0 {
            println!("  {name:>7} {n:>12} ({:5.1}% of input)", 100.0 * n as f64 / tin);
        }
    }
    println!(
        "{} files ({} without a formula, {} failed): {:.0} -> {:.0} = {:.4}; encode {:.1} MB/s, decode {:.1} MB/s",
        files.len(),
        skipped,
        fails,
        tin,
        tout,
        tout / tin,
        tin / tc / 1e6,
        tin / td / 1e6
    );
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// Mutates a database file the ways corruption and odd writers do.
fn mutate(r: &mut Rng, src: &[u8]) -> Vec<u8> {
    let mut v = src.to_vec();
    let ps = match u16::from_be_bytes([v[16], v[17]]) {
        1 => 65536,
        p if p >= 512 => p as usize,
        _ => 4096,
    };
    let np = (v.len() / ps).max(1);
    for _ in 0..1 + r.below(3) {
        let p = r.below(np);
        let base = p * ps;
        let h = if p == 0 { 100 } else { 0 };
        match r.below(9) {
            0 => {
                for _ in 0..1 + r.below(16) {
                    let i = r.below(v.len());
                    v[i] = r.next() as u8;
                }
            }
            1 if base + h < v.len() => v[base + h] = [0, 2, 5, 10, 13][r.below(5)],
            2 if base + h + 40 < v.len() => {
                let i = base + h + 8 + 2 * r.below(16);
                let x = r.next() as u16 % ps as u16;
                v[i..i + 2].copy_from_slice(&x.to_be_bytes());
            }
            3 => v.truncate(512 + r.below(v.len() - 511)),
            4 => v.extend((0..1 + r.below(2 * ps)).map(|_| r.next() as u8)),
            5 => {
                let q = r.below(np) * ps;
                if q + ps <= v.len() && base + ps <= v.len() {
                    let pg = v[q..q + ps].to_vec();
                    v[base..base + ps].copy_from_slice(&pg);
                }
            }
            6 => {
                let i = r.below(v.len().saturating_sub(4));
                let x = r.below(np + 2) as u32;
                v[i..i + 4].copy_from_slice(&x.to_be_bytes());
            }
            7 => {
                let i = 16 + r.below(84);
                v[i] = r.next() as u8;
            }
            _ => {
                // A varint-ish byte inside a cell area.
                let i = base + ps / 2 + r.below(ps / 2);
                if i < v.len() {
                    v[i] = 0x80 | r.next() as u8;
                }
            }
        }
    }
    v
}

fn fuzz(args: &[String]) {
    let mut n = 100;
    let mut seed = 1u64;
    let mut files = vec![];
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-n" => n = it.next().and_then(|s| s.parse().ok()).unwrap_or_else(|| usage()),
            "-s" => seed = it.next().and_then(|s| s.parse().ok()).unwrap_or_else(|| usage()),
            _ => files.push(a.clone()),
        }
    }
    std::panic::set_hook(Box::new(|_| {}));
    let opt = Options { verify: false, catch: false, ..Default::default() };
    let (mut runs, mut bad) = (0, 0);
    for f in &files {
        let src = std::fs::read(f).expect("read");
        if src.len() < 512 {
            continue;
        }
        for i in 0..n {
            let mut r = Rng(seed.wrapping_mul(0x9E3779B97F4A7C15) ^ (i as u64 + 1) ^ (src.len() as u64) << 20);
            let m = mutate(&mut r, &src);
            runs += 1;
            let res = std::panic::catch_unwind(|| {
                let Some(fm) = formula::encode_with(&m, &opt) else { return true };
                let d = eval(&fm, 1);
                // Corrupted streams: errors, never panics.
                let Node::Apply(ap) = &fm.root else { unreachable!() };
                let ins: Vec<&[u8]> = fm.streams.iter().map(|s| &s.data[..]).collect();
                for k in 0..4 {
                    let mut cc: Vec<Vec<u8>> = ins.iter().map(|s| s.to_vec()).collect();
                    let mut r2 = Rng(i as u64 * 31 + k + 7);
                    let t = r2.below(cc.len());
                    match k {
                        0 => {
                            let n = r2.below(cc[t].len() + 1);
                            cc[t].truncate(n);
                        }
                        _ => {
                            for _ in 0..1 + r2.below(8) {
                                if !cc[t].is_empty() {
                                    let j = r2.below(cc[t].len());
                                    cc[t][j] ^= 1 << r2.below(8);
                                }
                            }
                        }
                    }
                    let cc: Vec<&[u8]> = cc.iter().map(|s| &s[..]).collect();
                    let _ = formula::decode(&ap.params, &cc, ap.len, 1);
                }
                matches!(d, Ok(ref d) if *d == m)
            });
            match res {
                Ok(true) => {}
                Ok(false) => {
                    bad += 1;
                    eprintln!("MISMATCH {f} seed {seed} iter {i}");
                }
                Err(e) => {
                    bad += 1;
                    let msg =
                        e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()));
                    eprintln!("PANIC {f} seed {seed} iter {i}: {msg:?}");
                }
            }
        }
    }
    println!("{runs} runs, {bad} failures");
    if bad > 0 {
        std::process::exit(1);
    }
}
