//! Compares aqr-png (the archive of its formula) with other lossless recompressors of PNG
//! files (preflate-rs' container, precomp, paq8px) and with generic compressors (zstd, xz), on sets of PNG files.
//! Every output is checked by a round trip. Times are CPU times on one thread: the thread's
//! own for the libraries (best of two runs), the children's for the tools run as processes.
//!
//! ```text
//! cargo run --release -- [-t TOOL,...] [-n FILES] [-o OUT.tsv] DIR...
//! ```
//!
//! Each DIR is a set (searched recursively for *.png). `-n` takes the first FILES files of
//! each set (in name order). Results are cached per tool and file in `../cache/bench.tsv`.

use aqr_formula::aqr;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const TOOLS: &[&str] = &["aqr-png", "preflate-rs", "precomp", "paq8px", "zstd", "xz"];
const DEFAULT: &[&str] = &["aqr-png", "preflate-rs", "precomp", "zstd", "xz"];

fn third_party(p: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../third_party").join(p)
}

fn thread_cpu() -> f64 {
    let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: a valid out-pointer.
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut t) };
    t.tv_sec as f64 + t.tv_nsec as f64 * 1e-9
}

fn children_cpu() -> f64 {
    // SAFETY: a valid out-pointer.
    let mut r: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_CHILDREN, &mut r) };
    let tv = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 * 1e-6;
    tv(r.ru_utime) + tv(r.ru_stime)
}

/// One tool on one file: output size, compression and decompression CPU seconds, and
/// whether the round trip gave the file back.
#[derive(Clone, Copy)]
struct Res {
    out: usize,
    c: f64,
    d: f64,
    ok: bool,
}

fn run(cmd: &mut Command) -> bool {
    cmd.stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
}

/// Runs an external tool: `comp(input, packed)` and `decomp(packed, output)`.
fn external(
    file: &Path,
    ext: &str,
    comp: impl Fn(&Path, &Path) -> Command,
    decomp: impl Fn(&Path, &Path) -> Command,
) -> Res {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../cache/tmp");
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("in.png");
    let packed = dir.join(format!("in.{ext}"));
    let output = dir.join("out.png");
    for p in [&packed, &output] {
        let _ = std::fs::remove_file(p);
    }
    std::fs::copy(file, &input).unwrap();
    let t0 = children_cpu();
    let ok1 = run(&mut comp(&input, &packed));
    let t1 = children_cpu();
    let ok2 = ok1 && run(&mut decomp(&packed, &output));
    let t2 = children_cpu();
    let out = std::fs::metadata(&packed).map(|m| m.len() as usize).unwrap_or(0);
    let ok = ok2 && std::fs::read(&output).ok() == std::fs::read(file).ok();
    Res { out, c: t1 - t0, d: t2 - t1, ok }
}

/// The libraries' times are the best of two runs (the first pays for warming up).
fn measure(tool: &str, file: &Path) -> Res {
    let a = measure_once(tool, file);
    if matches!(tool, "aqr-png" | "preflate-rs") {
        let b = measure_once(tool, file);
        return Res { c: a.c.min(b.c), d: a.d.min(b.d), ok: a.ok && b.ok, ..a };
    }
    a
}

fn measure_once(tool: &str, file: &Path) -> Res {
    let src = std::fs::read(file).unwrap();
    match tool {
        "aqr-png" => {
            // The archive of the file's formula; a file with no formula is kept as it is.
            let apply = |a: &aqr_formula::Apply, inputs: &[&[u8]]| {
                aqr_png::formula::decode(&a.params, inputs, a.len, 1)
                    .map_err(|e| aqr_formula::Error::new(e.to_string()))
            };
            let natives = aqr::natives(&[aqr_png::formula::MODULE], &apply);
            let t0 = thread_cpu();
            let Some(f) = aqr_png::formula::encode(&src, 1) else {
                return Res { out: src.len(), c: thread_cpu() - t0, d: 0.0, ok: true };
            };
            let c = aqr::encode(&f, Some(aqr::checksum(&src)), aqr::Level::MID).expect("archive");
            let t1 = thread_cpu();
            let d = aqr::decode(&c, &natives);
            let t2 = thread_cpu();
            Res { out: c.len(), c: t1 - t0, d: t2 - t1, ok: d.ok().as_deref() == Some(&src[..]) }
        }
        "preflate-rs" => {
            let cfg = preflate_container::PreflateContainerConfig::default();
            let t0 = thread_cpu();
            let mut c = vec![];
            let r = preflate_container::preflate_whole_into_container(&cfg, &mut &src[..], &mut c);
            let t1 = thread_cpu();
            let mut d = vec![];
            let r2 = r.is_ok() && preflate_container::recreate_whole_from_container(&mut &c[..], &mut d).is_ok();
            let t2 = thread_cpu();
            Res { out: c.len(), c: t1 - t0, d: t2 - t1, ok: r2 && d == src }
        }
        "precomp" => {
            let bin = third_party("precomp-cpp/build/precomp");
            external(
                file,
                "pcf",
                |i, o| {
                    let mut c = Command::new(&bin);
                    c.arg("-cl").arg("-lt1").arg(format!("-o{}", o.display())).arg(i);
                    c
                },
                |i, o| {
                    let mut c = Command::new(&bin);
                    c.arg("-r").arg(format!("-o{}", o.display())).arg(i);
                    c
                },
            )
        }
        "paq8px" => {
            let bin = third_party("paq8px/build-cm/paq8px");
            external(
                file,
                "paq8px",
                |i, o| {
                    let mut c = Command::new(&bin);
                    c.arg("-8").arg(i).arg(o);
                    c
                },
                |i, o| {
                    let mut c = Command::new(&bin);
                    c.arg("-d").arg(i).arg(o);
                    c
                },
            )
        }
        "zstd" => external(
            file,
            "zst",
            |i, o| {
                let mut c = Command::new("zstd");
                c.args(["-19", "--single-thread", "-q", "-f", "-o"]).arg(o).arg(i);
                c
            },
            |i, o| {
                let mut c = Command::new("zstd");
                c.args(["-d", "-q", "-f", "-o"]).arg(o).arg(i);
                c
            },
        ),
        "xz" => external(
            file,
            "xz",
            |i, o| {
                let mut c = Command::new("sh");
                c.arg("-c").arg(format!("xz -9 -T1 -c '{}' > '{}'", i.display(), o.display()));
                c
            },
            |i, o| {
                let mut c = Command::new("sh");
                c.arg("-c").arg(format!("xz -d -T1 -c '{}' > '{}'", i.display(), o.display()));
                c
            },
        ),
        _ => panic!("unknown tool {tool}"),
    }
}

fn pngs(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut es: Vec<_> = std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).collect();
    es.sort();
    for p in es {
        if p.is_dir() {
            pngs(&p, out);
        } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("png")) {
            out.push(p);
        }
    }
}

fn main() {
    let mut tools: Vec<String> = DEFAULT.iter().map(|s| s.to_string()).collect();
    let mut limit = usize::MAX;
    let mut sets = vec![];
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-t" => tools = args.next().expect("-t TOOLS").split(',').map(str::to_owned).collect(),
            "-n" => limit = args.next().and_then(|n| n.parse().ok()).expect("-n FILES"),
            _ => sets.push(PathBuf::from(a)),
        }
    }
    for t in &tools {
        assert!(TOOLS.contains(&t.as_str()), "unknown tool {t} (tools: {TOOLS:?})");
    }

    // Cache: tool, path, size, out, c, d, ok.
    let cache_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../cache/bench.tsv");
    std::fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
    let mut cache: HashMap<(String, String, usize), Res> = HashMap::new();
    if let Ok(text) = std::fs::read_to_string(&cache_path) {
        for l in text.lines() {
            let f: Vec<&str> = l.split('\t').collect();
            if f.len() == 7 {
                let key = (f[0].to_owned(), f[1].to_owned(), f[2].parse().unwrap_or(0));
                cache.insert(
                    key,
                    Res {
                        out: f[3].parse().unwrap_or(0),
                        c: f[4].parse().unwrap_or(0.0),
                        d: f[5].parse().unwrap_or(0.0),
                        ok: f[6] == "1",
                    },
                );
            }
        }
    }
    let mut log = std::fs::OpenOptions::new().create(true).append(true).open(&cache_path).unwrap();

    println!("| set | files | PNG bytes | tool | output | ratio | compress | decompress | failures |");
    println!("|---|---|---|---|---|---|---|---|---|");
    let mut total: HashMap<String, (usize, usize, usize, f64, f64, usize)> = HashMap::new();
    for set in &sets {
        let mut files = vec![];
        pngs(set, &mut files);
        files.truncate(limit);
        let size: usize = files.iter().map(|f| std::fs::metadata(f).unwrap().len() as usize).sum();
        for tool in &tools {
            let (mut out, mut c, mut d, mut fails) = (0usize, 0f64, 0f64, 0usize);
            for f in &files {
                let n = std::fs::metadata(f).unwrap().len() as usize;
                let key = (tool.clone(), f.display().to_string(), n);
                let r = match cache.get(&key) {
                    Some(r) => *r,
                    None => {
                        let r = measure(tool, f);
                        writeln!(
                            log,
                            "{}\t{}\t{}\t{}\t{:.6}\t{:.6}\t{}",
                            key.0, key.1, key.2, r.out, r.c, r.d, r.ok as u8
                        )
                        .unwrap();
                        cache.insert(key, r);
                        r
                    }
                };
                if r.ok {
                    out += r.out;
                } else {
                    // A failure keeps the file as it is.
                    out += n;
                    fails += 1;
                }
                c += r.c;
                d += r.d;
            }
            println!(
                "| {} | {} | {} | {tool} | {out} | {:.4} | {:.1} MB/s | {:.1} MB/s | {fails} |",
                set.display(),
                files.len(),
                size,
                out as f64 / size as f64,
                size as f64 / c / 1e6,
                size as f64 / d / 1e6
            );
            let e = total.entry(tool.clone()).or_default();
            *e = (e.0 + files.len(), e.1 + size, e.2 + out, e.3 + c, e.4 + d, e.5 + fails);
        }
    }
    for tool in &tools {
        if let Some(&(n, size, out, c, d, fails)) = total.get(tool) {
            println!(
                "| total | {n} | {size} | {tool} | {out} | {:.4} | {:.1} MB/s | {:.1} MB/s | {fails} |",
                out as f64 / size as f64,
                size as f64 / c / 1e6,
                size as f64 / d / 1e6
            );
        }
    }
}
