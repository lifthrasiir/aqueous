use aqr_formula::aqr::{self, Level};
use aqr_formula::{Apply, Formula};
use aqr_obj::block::NSTREAMS;
use aqr_obj::codec::Options;
use aqr_obj::formula::{self, MODULE};

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

/// Wall time where there is no thread CPU time to ask for.
#[cfg(not(unix))]
fn cpu() -> f64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_secs_f64()
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

fn usage() -> ! {
    eprintln!(
        "usage: objr bench [-n ITERS] [-B BLOCK] [-x] [-t THREADS] [-c|-d] [-v] FILE...\n       objr stats FILE... | objr fuzz [-n N] [-s SEED] FILE..."
    );
    std::process::exit(2);
}

/// Parses a shared option; false if `a` is not one.
fn opt_arg<'a>(a: &str, it: &mut impl Iterator<Item = &'a String>, opt: &mut Options) -> bool {
    let mut num = || it.next().and_then(|s| s.parse::<usize>().ok()).unwrap_or_else(|| usage());
    match a {
        "-B" => opt.block = num(),
        "-x" => opt.verify = false,
        "-t" => opt.threads = num().max(1),
        _ => return false,
    }
    true
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

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("bench") => bench(&args[1..]),
        Some("stats") => stats(&args[1..]),
        Some("fuzz") => fuzz(&args[1..]),
        _ => usage(),
    }
}

/// The module, with `threads` threads.
fn apply(threads: usize) -> impl Fn(&Apply, &[&[u8]]) -> aqr_formula::Result<Vec<u8>> {
    move |a, inputs| {
        assert_eq!(a.module, MODULE);
        formula::decode(&a.params, inputs, a.len, threads).map_err(|e| aqr_formula::Error::new(e.to_string()))
    }
}

/// Sizes and speeds of archives of the files (the formula, then the archive); files the
/// formula does not code are counted, and left out of the totals.
fn bench(args: &[String]) {
    let mut iters = 1;
    let mut files = vec![];
    let mut verbose = false;
    let mut opt = Options::default();
    // Repeat only one side (for profiling): 'c' or 'd'.
    let mut only = ' ';
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if opt_arg(a, &mut it, &mut opt) {
            continue;
        }
        match a.as_str() {
            "-n" => iters = it.next().and_then(|s| s.parse().ok()).unwrap_or_else(|| usage()),
            "-c" => only = 'c',
            "-d" => only = 'd',
            "-v" => verbose = true,
            _ => files.push(a.clone()),
        }
    }
    let (mut tin, mut tout, mut tc, mut td) = (0f64, 0f64, 0f64, 0f64);
    let (mut fails, mut skipped) = (0, 0);
    let modules = apply(opt.threads);
    for f in &files {
        let src = std::fs::read(f).expect("read");
        let mut c = vec![];
        let mut bc = f64::MAX;
        let mut coded = true;
        for _ in 0..if only == 'd' { 1 } else { iters } {
            let t = clock(opt.threads);
            let Some(fm) = formula::encode_with(&src, &opt) else {
                coded = false;
                break;
            };
            c = aqr::encode(&fm, Some(aqr::checksum(&src)), Level::Mid).expect("archive");
            bc = bc.min(clock(opt.threads) - t);
        }
        if !coded {
            skipped += 1;
            continue;
        }
        let mut bd = f64::MAX;
        let mut ok = true;
        for _ in 0..if only == 'c' { 1 } else { iters } {
            let t = clock(opt.threads);
            let d = aqr::decode(&c, &aqr::natives(&[MODULE], &modules));
            bd = bd.min(clock(opt.threads) - t);
            ok &= matches!(d, Ok(ref d) if *d == src);
        }
        if !ok {
            fails += 1;
            eprintln!("ROUNDTRIP FAILURE: {f}");
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
    println!(
        "{} files ({} failed, {} not coded): {:.0} -> {:.0} = {:.4}; compress {:.1} MB/s, decompress {:.1} MB/s",
        files.len(),
        fails,
        skipped,
        tin,
        tout,
        tout / tin,
        tin / tc / 1e6,
        tin / td / 1e6
    );
}

/// Template use and raw lines per file (no zstd).
fn stats(files: &[String]) {
    use aqr_obj::varint::Rd;
    for f in files {
        let src = std::fs::read(f).expect("read");
        let mut enc = aqr_obj::block::Encoder::default();
        let mut s: [Vec<u8>; NSTREAMS] = std::array::from_fn(|_| Vec::new());
        let mut uses: Vec<u64> = vec![];
        let mut raw: std::collections::HashMap<Vec<u8>, (u64, Vec<u8>)> = Default::default();
        for b in src.chunks(4 << 20) {
            enc.encode(b, &mut s);
            let mut r = Rd::new(&s[0]);
            while !r.done() {
                let op = r.uv() as usize;
                if uses.len() <= op {
                    uses.resize(op + 1, 0);
                }
                uses[op] += 1;
            }
            for l in s[aqr_obj::block::S_TEXT].split(|&c| c == b'\n') {
                let w = l.split(|&c| c == b' ' || c == b'\t').next().unwrap_or(b"").to_vec();
                let w = if w.len() > 12 { w[..12].to_vec() } else { w };
                let e = raw.entry(w).or_insert((0, l.iter().take(80).copied().collect()));
                e.0 += 1;
            }
        }
        println!("== {f}: {} templates, {} raw lines", enc.keys().len(), uses.iter().take(2).sum::<u64>());
        let mut t: Vec<(u64, String)> = enc
            .keys()
            .iter()
            .enumerate()
            .map(|(i, k)| (uses.get(i + 2).copied().unwrap_or(0), aqr_obj::block::describe(k)))
            .collect();
        t.sort_by_key(|x| std::cmp::Reverse(x.0));
        for (n, d) in t.iter().take(12) {
            println!("  {n:>10} {d}");
        }
        let mut r: Vec<_> = raw.into_iter().collect();
        r.sort_by_key(|x| std::cmp::Reverse(x.1.0));
        for (w, (n, ex)) in r.iter().take(8) {
            println!("  raw {n:>8} {:<12} e.g. {}", String::from_utf8_lossy(w), String::from_utf8_lossy(ex).trim_end());
        }
    }
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

/// Mutates an OBJ file the ways odd writers and corruption do.
fn mutate(r: &mut Rng, src: &[u8]) -> Vec<u8> {
    const TOKENS: [&str; 16] = [
        "-0",
        "1e5",
        "nan",
        "//",
        "/",
        "-",
        ".",
        "0.",
        "-.5",
        "00",
        "1e-005",
        "99999999999999999999",
        "\t",
        "\r",
        "f",
        "v",
    ];
    let mut v = src.to_vec();
    for _ in 0..1 + r.below(4) {
        if v.is_empty() {
            v.extend_from_slice(b"v 1 2 3\n");
        }
        let i = r.below(v.len());
        match r.below(8) {
            0 => v[i] = r.next() as u8,
            1 => v[i] = b" 0123456789.-/e\n"[r.below(16)],
            2 => {
                let t = TOKENS[r.below(TOKENS.len())].as_bytes();
                v.splice(i..i, t.iter().copied());
            }
            3 => {
                let n = r.below(20).min(v.len() - i);
                v.drain(i..i + n);
            }
            4 => v.truncate(i),
            5 => {
                // A line copied elsewhere.
                let a = v[..i].iter().rposition(|&c| c == b'\n').map_or(0, |p| p + 1);
                let b = v[i..].iter().position(|&c| c == b'\n').map_or(v.len(), |p| i + p + 1);
                let l = v[a..b].to_vec();
                let j = r.below(v.len());
                v.splice(j..j, l);
            }
            6 => {
                // Line ends turned to CRLF or back.
                if let Some(p) = v[i..].iter().position(|&c| c == b'\n') {
                    if i + p > 0 && v[i + p - 1] == b'\r' {
                        v.remove(i + p - 1);
                    } else {
                        v.insert(i + p, b'\r');
                    }
                }
            }
            _ => {
                // A digit changed, so that values jump.
                if let Some(p) = v[i..].iter().position(u8::is_ascii_digit) {
                    v[i + p] = b'0' + r.below(10) as u8;
                }
            }
        }
    }
    v
}

/// Mutated files must round-trip through the formula with verification off (so that the
/// coded path is what is checked).
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
    let (mut runs, mut bad) = (0, 0);
    for f in &files {
        let src = std::fs::read(f).expect("read");
        for i in 0..n {
            let mut r = Rng(seed.wrapping_mul(0x9E3779B97F4A7C15) ^ (i as u64 + 1) ^ (src.len() as u64) << 20);
            let m = mutate(&mut r, &src);
            let opt = Options {
                verify: false,
                block: [1 << 10, 64 << 10, 4 << 20][i % 3],
                force_whole: if i % 2 == 1 { r.next() } else { 0 },
                threads: 1,
            };
            runs += 1;
            let res = std::panic::catch_unwind(|| {
                // Nothing to check where the formula does not code the file.
                let Some(fm) = formula::encode_with(&m, &opt) else { return true };
                // Threads: the same formula, and the same file back.
                let threads = [2, 3, 4][i % 3];
                let opt_t = Options { threads, verify: i % 4 == 0, ..opt };
                let same = opt_t.verify || formula::encode_with(&m, &opt_t).as_ref() == Some(&fm);
                let d = Formula::eval(&fm, &apply(threads));
                same && matches!(d, Ok(ref d) if *d == m)
            });
            match res {
                Ok(true) => {}
                Ok(false) => {
                    bad += 1;
                    eprintln!("MISMATCH {f} seed {seed} iter {i} (block {})", opt.block);
                    if let Ok(dir) = std::env::var("FUZZ_SAVE") {
                        let name = format!("{dir}/{}-{seed}-{i}", f.replace('/', "_"));
                        std::fs::write(&name, &m).expect("write");
                        std::fs::write(name + ".opt", format!("{} {:x}", opt.block, opt.force_whole)).expect("write");
                    }
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
