use sqlite_recons::container::{Options, Stats, compress_with};
use sqlite_recons::transform::{NSTREAMS, STREAM_NAMES};
use std::time::Instant;

fn usage() -> ! {
    eprintln!(
        "usage: sqlr c IN OUT | sqlr d IN OUT\n       sqlr bench [-n ITERS] [-l LEVEL] [-b LEVEL] [-V] [-L] [-c|-d] [-v] FILE...\n       sqlr fuzz [-n N] [-s SEED] FILE..."
    );
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("c") if args.len() == 3 => {
            let src = std::fs::read(&args[1]).expect("read");
            std::fs::write(&args[2], sqlite_recons::compress(&src)).expect("write");
        }
        Some("d") if args.len() == 3 => {
            let src = std::fs::read(&args[1]).expect("read");
            match sqlite_recons::decompress(&src) {
                Ok(v) => std::fs::write(&args[2], v).expect("write"),
                Err(e) => {
                    eprintln!("{}: {e}", args[1]);
                    std::process::exit(1);
                }
            }
        }
        Some("bench") => bench(&args[1..]),
        Some("fuzz") => fuzz(&args[1..]),
        _ => usage(),
    }
}

fn bench(args: &[String]) {
    let mut iters = 1;
    let mut files = vec![];
    let mut verbose = false;
    let mut opt = Options { verify: false, ..Default::default() };
    // Repeat only one side (for profiling): 'c' or 'd'.
    let mut only = ' ';
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-n" => iters = it.next().and_then(|s| s.parse().ok()).unwrap_or_else(|| usage()),
            "-l" => {
                opt.level = it.next().and_then(|s| s.parse().ok()).unwrap_or_else(|| usage());
                opt.level_bytes = opt.level;
            }
            "-V" => opt.verify = true,
            "-b" => opt.level_bytes = it.next().and_then(|s| s.parse().ok()).unwrap_or_else(|| usage()),
            "-L" => opt.ldm = false,
            "-c" => only = 'c',
            "-d" => only = 'd',
            "-v" => verbose = true,
            _ => files.push(a.clone()),
        }
    }
    let (mut tin, mut tout, mut tc, mut td) = (0f64, 0f64, 0f64, 0f64);
    let mut total = Stats::default();
    let mut fails = 0;
    let mut wholes = 0;
    for f in &files {
        let src = std::fs::read(f).expect("read");
        let mut c = vec![];
        let mut bc = f64::MAX;
        let mut st = Stats::default();
        for _ in 0..if only == 'd' { 1 } else { iters } {
            st = Stats::default();
            let t = Instant::now();
            c = compress_with(&src, &opt, &mut st);
            bc = bc.min(t.elapsed().as_secs_f64());
        }
        let mut bd = f64::MAX;
        let mut ok = true;
        for _ in 0..if only == 'c' { 1 } else { iters } {
            let t = Instant::now();
            let d = sqlite_recons::decompress(&c);
            bd = bd.min(t.elapsed().as_secs_f64());
            ok &= matches!(d, Ok(ref d) if *d == src);
        }
        if !ok {
            fails += 1;
            eprintln!("ROUNDTRIP FAILURE: {f}");
        }
        wholes += st.whole as usize;
        for i in 0..NSTREAMS {
            total.raw[i] += st.raw[i];
            total.coded[i] += st.coded[i];
        }
        total.transform_s += st.transform_s;
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
                if st.whole { "[whole] " } else { "" },
                f
            );
        }
    }
    println!("streams (raw -> coded):");
    for i in 0..NSTREAMS {
        if total.raw[i] > 0 {
            println!(
                "  {:>7} {:>12} -> {:>11} ({:5.1}%)",
                STREAM_NAMES[i],
                total.raw[i],
                total.coded[i],
                100.0 * total.coded[i] as f64 / tout
            );
        }
    }
    println!(
        "{} files ({} whole, {} failed): {:.0} -> {:.0} = {:.4}; compress {:.1} MB/s (transform {:.2}s, zstd {:.2}s), decompress {:.1} MB/s",
        files.len(),
        wholes,
        fails,
        tin,
        tout,
        tout / tin,
        tin / tc / 1e6,
        total.transform_s,
        total.zstd_s,
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
    let opt = Options { verify: false, catch: false, level: 1, level_bytes: 1, ..Default::default() };
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
                let c = compress_with(&m, &opt, &mut Stats::default());
                let d = sqlite_recons::decompress(&c);
                // Corrupted containers: errors, never panics.
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
                    let _ = sqlite_recons::decompress(&cc);
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
