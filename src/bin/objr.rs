use obj_recons::block::{NSTREAMS, STREAM_NAMES};
use obj_recons::container::{Options, Stats, compress_with};
use obj_recons::mem::{Counting, PEAK, peak_reset};
use std::io::Write;
use std::sync::atomic::Ordering::Relaxed;

#[global_allocator]
static ALLOC: Counting = Counting;

#[repr(C)]
struct Timespec {
    sec: i64,
    nsec: i64,
}

unsafe extern "C" {
    fn clock_gettime(clk: u32, tp: *mut Timespec) -> i32;
}

/// CPU time of this thread, in seconds (wall time is skewed by other load).
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
        "usage: objr c [-l LEVEL] [-B BLOCK] [-x] IN OUT | objr d IN OUT\n       objr bench [-n ITERS] [-l LEVEL] [-B BLOCK] [-x] [-c|-d] [-m] [-v] FILE...\n       objr stats FILE... | objr fuzz [-n N] [-s SEED] FILE..."
    );
    std::process::exit(2);
}

/// Parses a shared option; false if `a` is not one.
fn opt_arg<'a>(a: &str, it: &mut impl Iterator<Item = &'a String>, opt: &mut Options) -> bool {
    let mut num = || it.next().and_then(|s| s.parse::<usize>().ok()).unwrap_or_else(|| usage());
    match a {
        "-l" => opt.level = num() as i32,
        "-B" => opt.block = num(),
        "-x" => opt.verify = false,
        _ => return false,
    }
    true
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("c") if args.len() >= 3 => {
            let mut opt = Options::default();
            let n = args.len();
            let mut it = args[1..n - 2].iter();
            while let Some(a) = it.next() {
                if !opt_arg(a, &mut it, &mut opt) {
                    usage();
                }
            }
            let src = std::fs::read(&args[n - 2]).expect("read");
            let mut w = std::io::BufWriter::new(std::fs::File::create(&args[n - 1]).expect("create"));
            obj_recons::container::compress_to(&src, &opt, &mut Stats::default(), &mut w).expect("write");
            w.flush().expect("write");
        }
        Some("d") if args.len() == 3 => {
            let src = std::fs::read(&args[1]).expect("read");
            let mut w = std::io::BufWriter::new(std::fs::File::create(&args[2]).expect("create"));
            match obj_recons::container::decompress_to(&src, &mut w).and_then(|_| w.flush().map_err(|_| obj_recons::Error("write error"))) {
                Ok(()) => {}
                Err(e) => {
                    drop(w);
                    let _ = std::fs::remove_file(&args[2]);
                    eprintln!("{}: {e}", args[1]);
                    std::process::exit(1);
                }
            }
        }
        Some("bench") => bench(&args[1..]),
        Some("stats") => stats(&args[1..]),
        Some("fuzz") => fuzz(&args[1..]),
        _ => usage(),
    }
}

fn bench(args: &[String]) {
    let mut iters = 1;
    let mut files = vec![];
    let mut verbose = false;
    let mut opt = Options::default();
    // Repeat only one side (for profiling): 'c' or 'd'.
    let mut only = ' ';
    let mut mem = false;
    let (mut mpeak_c, mut mpeak_d) = (0usize, 0usize);
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
            "-m" => mem = true,
            _ => files.push(a.clone()),
        }
    }
    let (mut tin, mut tout, mut tc, mut td) = (0f64, 0f64, 0f64, 0f64);
    let mut total = Stats::default();
    let mut fails = 0;
    for f in &files {
        let src = std::fs::read(f).expect("read");
        let mut c = vec![];
        let mut bc = f64::MAX;
        let mut st = Stats::default();
        for _ in 0..if only == 'd' { 1 } else { iters } {
            st = Stats::default();
            let t = cpu();
            c = compress_with(&src, &opt, &mut st);
            bc = bc.min(cpu() - t);
        }
        let mut bd = f64::MAX;
        let mut ok = true;
        for _ in 0..if only == 'c' { 1 } else { iters } {
            let t = cpu();
            let d = obj_recons::decompress(&c);
            bd = bd.min(cpu() - t);
            ok &= matches!(d, Ok(ref d) if *d == src);
        }
        if mem {
            // Heap beyond the input and the output (which the caller owns).
            // The streaming forms, writing to nowhere.
            let base = peak_reset();
            obj_recons::container::compress_to(&src, &opt, &mut Stats::default(), &mut std::io::sink()).unwrap();
            let pc = PEAK.load(Relaxed) - base;
            let base = peak_reset();
            let _ = obj_recons::container::decompress_to(&c, &mut std::io::sink());
            let pd = PEAK.load(Relaxed) - base;
            println!("mem {:>11} c {:>11} d {:>11} {f}", src.len(), pc, pd);
            mpeak_c = mpeak_c.max(pc);
            mpeak_d = mpeak_d.max(pd);
        }
        if !ok {
            fails += 1;
            eprintln!("ROUNDTRIP FAILURE: {f}");
        }
        for i in 0..NSTREAMS {
            total.raw[i] += st.raw[i];
            total.coded[i] += st.coded[i];
        }
        total.whole += st.whole;
        total.whole_coded += st.whole_coded;
        total.transform_s += st.transform_s;
        total.verify_s += st.verify_s;
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
                if st.whole > 0 { format!("[whole {}] ", st.whole) } else { String::new() },
                f
            );
        }
    }
    if mem {
        println!("heap beyond input and output, largest: compress {mpeak_c}, decompress {mpeak_d}");
    }
    println!("streams (raw -> coded, share of output):");
    for i in 0..NSTREAMS {
        if total.raw[i] > 0 {
            println!(
                "  {:>5} {:>12} -> {:>11} ({:5.1}%)",
                STREAM_NAMES[i],
                total.raw[i],
                total.coded[i],
                100.0 * total.coded[i] as f64 / tout
            );
        }
    }
    if total.whole > 0 {
        println!("  whole {:>12} -> {:>11} ({:5.1}%)", total.whole, total.whole_coded, 100.0 * total.whole_coded as f64 / tout);
    }
    println!(
        "{} files ({} failed): {:.0} -> {:.0} = {:.4}; compress {:.1} MB/s (transform {:.2}s, verify {:.2}s, zstd {:.2}s), decompress {:.1} MB/s",
        files.len(),
        fails,
        tin,
        tout,
        tout / tin,
        tin / tc / 1e6,
        total.transform_s,
        total.verify_s,
        total.zstd_s,
        tin / td / 1e6
    );
}

/// Template use and raw lines per file (no zstd).
fn stats(files: &[String]) {
    use obj_recons::varint::Rd;
    for f in files {
        let src = std::fs::read(f).expect("read");
        let mut enc = obj_recons::block::Encoder::default();
        let mut s: [Vec<u8>; NSTREAMS] = Default::default();
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
            for l in s[obj_recons::block::S_TEXT].split(|&c| c == b'\n') {
                let w = l.split(|&c| c == b' ' || c == b'\t').next().unwrap_or(b"").to_vec();
                let w = if w.len() > 12 { w[..12].to_vec() } else { w };
                let e = raw.entry(w).or_insert((0, l.iter().take(80).copied().collect()));
                e.0 += 1;
            }
        }
        println!("== {f}: {} templates, {} raw lines", enc.keys().len(), uses.iter().take(2).sum::<u64>());
        let mut t: Vec<(u64, String)> = enc.keys().iter().enumerate().map(|(i, k)| (uses.get(i + 2).copied().unwrap_or(0), obj_recons::block::describe(k))).collect();
        t.sort_by(|a, b| b.0.cmp(&a.0));
        for (n, d) in t.iter().take(12) {
            println!("  {n:>10} {d}");
        }
        let mut r: Vec<_> = raw.into_iter().collect();
        r.sort_by(|a, b| b.1.0.cmp(&a.1.0));
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
        "-0", "1e5", "nan", "//", "/", "-", ".", "0.", "-.5", "00", "1e-005", "99999999999999999999", "\t", "\r", "f", "v",
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

/// Mutated files must round-trip with verification off (so that the coded path is what is
/// checked), and corrupted containers must fail cleanly.
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
            let opt = Options { verify: false, block: [1 << 10, 64 << 10, 4 << 20][i % 3], level: 1 };
            runs += 1;
            let res = std::panic::catch_unwind(|| {
                let c = compress_with(&m, &opt, &mut Stats::default());
                let d = obj_recons::decompress(&c);
                for k in 0..4 {
                    let mut cc = c.clone();
                    let mut r2 = Rng(i as u64 * 31 + k + 7);
                    match k {
                        0 => cc.truncate(r2.below(cc.len())),
                        _ => {
                            for _ in 0..1 + r2.below(8) {
                                let j = r2.below(cc.len());
                                cc[j] ^= 1 << r2.below(8);
                            }
                        }
                    }
                    if let Ok(d2) = obj_recons::decompress(&cc) {
                        assert!(d2 == m, "corrupted container decoded to something else");
                    }
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
                    let msg = e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()));
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
