use obj_recons::block::{NSTREAMS, STREAM_NAMES};
use obj_recons::container::{Options, Stats, compress_with};

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
        "usage: objr c [-l LEVEL] [-B BLOCK] [-x] IN OUT | objr d IN OUT\n       objr bench [-n ITERS] [-l LEVEL] [-B BLOCK] [-x] [-c|-d] [-v] FILE..."
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
            std::fs::write(&args[n - 1], compress_with(&src, &opt, &mut Stats::default())).expect("write");
        }
        Some("d") if args.len() == 3 => {
            let src = std::fs::read(&args[1]).expect("read");
            match obj_recons::decompress(&src) {
                Ok(v) => std::fs::write(&args[2], v).expect("write"),
                Err(e) => {
                    eprintln!("{}: {e}", args[1]);
                    std::process::exit(1);
                }
            }
        }
        Some("bench") => bench(&args[1..]),
        Some("stats") => stats(&args[1..]),
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
