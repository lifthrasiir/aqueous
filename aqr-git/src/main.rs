use aqr_git::container::{self, Options as COptions};
use aqr_git::{KIND_NAMES, NSTREAMS, Options, STREAM_NAMES};
use std::path::Path;
use std::time::Instant;

fn usage() -> ! {
    eprintln!(
        "usage: aqr-git c [-l LEVEL] [-B NS | -A] [-O ORDER] [-k] [-t THREADS] DIR OUT | aqr-git d IN DIR | aqr-git x IN PATH\n       aqr-git bench [-l LEVEL] [-B NS | -A] [-O ORDER] [-k] [-t THREADS] [-v] DIR...\n(-B: time budget in ns per input byte by the cost model, default 10; -A: emulate every zlib stream; -O: largest, smallest or gain first; -k: pass over what does not fit; -t: zstd workers, same output for any)"
    );
    std::process::exit(2);
}

fn opts(args: &[String]) -> (Options, COptions, bool, Vec<String>) {
    let (mut o, mut c, mut v, mut rest) = (Options::default(), COptions::default(), false, vec![]);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-l" => c.level = it.next().and_then(|s| s.parse().ok()).unwrap_or_else(|| usage()),
            "-B" => o.ns_per_byte = Some(it.next().and_then(|s| s.parse().ok()).unwrap_or_else(|| usage())),
            "-A" => o.ns_per_byte = None,
            "-k" => o.skip = true,
            "-O" => {
                o.order = match it.next().map(String::as_str) {
                    Some("largest") => aqr_git::budget::Order::Largest,
                    Some("smallest") => aqr_git::budget::Order::Smallest,
                    Some("gain") => aqr_git::budget::Order::Gain,
                    _ => usage(),
                }
            }
            "-t" => c.threads = it.next().and_then(|s| s.parse().ok()).unwrap_or_else(|| usage()),
            "-v" => v = true,
            _ => rest.push(a.clone()),
        }
    }
    (o, c, v, rest)
}

fn die(e: impl std::fmt::Display) -> ! {
    eprintln!("aqr-git: {e}");
    std::process::exit(1);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("c") => {
            let (o, c, _, rest) = opts(&args[1..]);
            let [dir, out] = &rest[..] else { usage() };
            let d = aqr_git::deconstruct(Path::new(dir), &o).unwrap_or_else(|e| die(e));
            let z = container::compress(&d, &c, &mut [0; NSTREAMS]);
            std::fs::write(out, z).unwrap_or_else(|e| die(e));
        }
        Some("d") if args.len() == 3 => {
            let src = std::fs::read(&args[1]).unwrap_or_else(|e| die(e));
            let u = container::unpack(&src).unwrap_or_else(|e| die(e));
            let root = Path::new(&args[2]);
            container::for_each(&u, |p, b| {
                let path = root.join(p);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap_or_else(|e| die(e));
                std::fs::write(&path, b).unwrap_or_else(|e| die(e));
                Ok(())
            })
            .unwrap_or_else(|e| die(e));
            for (p, t) in &u.symlinks {
                let path = root.join(p);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap_or_else(|e| die(e));
                symlink(t, &path).unwrap_or_else(|e| die(e));
            }
        }
        Some("x") if args.len() == 3 => {
            let src = std::fs::read(&args[1]).unwrap_or_else(|e| die(e));
            let u = container::unpack(&src).unwrap_or_else(|e| die(e));
            let b = aqr_git::reconstruct_single(&u.streams, &args[2]).unwrap_or_else(|e| die(e));
            use std::io::Write;
            std::io::stdout().write_all(&b).unwrap_or_else(|e| die(e));
        }
        Some("bench") => bench(&args[1..]),
        _ => usage(),
    }
}

fn bench(args: &[String]) {
    let (o, c, verbose, dirs) = opts(args);
    let (mut tin, mut tout, mut fails) = (0u64, 0u64, 0);
    let mut raw = [0u64; NSTREAMS];
    let mut coded = [0u64; NSTREAMS];
    for dir in &dirs {
        let t = Instant::now();
        let d = aqr_git::deconstruct(Path::new(dir), &o).unwrap_or_else(|e| die(e));
        let tc = t.elapsed().as_secs_f64();
        let input: u64 = d.stats.kind_bytes.iter().sum();
        let t = Instant::now();
        let mut cz = [0u64; NSTREAMS];
        let z = container::compress(&d, &c, &mut cz);
        let tz = t.elapsed().as_secs_f64();
        let t = Instant::now();
        let mut ok = true;
        let mut nf = 0;
        let res = container::unpack(&z).and_then(|u| {
            container::for_each(&u, |p, b| {
                nf += 1;
                match std::fs::read(Path::new(dir).join(p)) {
                    Ok(orig) if orig == b => {}
                    _ => {
                        eprintln!("{dir}: {p} differs");
                        ok = false;
                    }
                }
                Ok(())
            })
        });
        let td = t.elapsed().as_secs_f64();
        if let Err(e) = res {
            eprintln!("{dir}: {e}");
            ok = false;
        }
        if !ok {
            fails += 1;
        }
        tin += input;
        tout += z.len() as u64;
        for k in 0..NSTREAMS {
            raw[k] += d.streams[k].len() as u64;
            coded[k] += cz[k];
        }
        println!(
            "{:<30} {:>12} -> {:>11} ({:6.2}%)  deconstruct {:7.2}s  zstd {:6.2}s  rebuild {:7.2}s  {}",
            dir,
            input,
            z.len(),
            100.0 * z.len() as f64 / input.max(1) as f64,
            tc,
            tz,
            td,
            if ok { "ok" } else { "FAIL" }
        );
        if verbose {
            let s = &d.stats;
            let kinds: Vec<String> = (0..KIND_NAMES.len())
                .filter(|&k| s.kinds[k] > 0)
                .map(|k| format!("{} {}/{}", KIND_NAMES[k], s.kinds[k], s.kind_bytes[k]))
                .collect();
            println!("  files: {}", kinds.join(", "));
            println!(
                "  zlib: emulated {} ({} B of data, {} B compressed), kept {}; entries verbatim {}; deltas verbatim {} untyped {}; model {:.2}s of {:.2}s",
                s.zlib_emulated,
                s.emulated_bytes,
                s.emulated_zbytes,
                s.zlib_verbatim,
                s.entry_verbatim,
                s.delta_verbatim,
                s.delta_untyped,
                s.model_ns * 1e-9,
                s.budget_ns * 1e-9
            );
            let _ = nf;
        }
    }
    if dirs.len() > 1 || verbose {
        println!("{:<30} {:>12} -> {:>11} ({:6.2}%)", "total", tin, tout, 100.0 * tout as f64 / tin.max(1) as f64);
        for k in 0..NSTREAMS {
            println!("  {:<8} {:>12} -> {:>11}", STREAM_NAMES[k], raw[k], coded[k]);
        }
    }
    if fails > 0 {
        std::process::exit(1);
    }
}

/// A symlink at `at` to `target`, as git has it (its bytes on Unix, else `/`-separated).
#[cfg(unix)]
fn symlink(target: &[u8], at: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(target), at)
}

#[cfg(windows)]
fn symlink(target: &[u8], at: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(String::from_utf8_lossy(target).replace('/', "\\"), at)
}
