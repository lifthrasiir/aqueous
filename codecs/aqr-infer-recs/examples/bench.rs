//! usage: aqr-infer-recs-bench [-j THREADS] [-v] [-t] FILE...
//!
//! For each file: zstd -3 of it, and of it with the regions inferred in their forms (each
//! checked to restore), and the CPU time of the inference (of its thread: the files run on
//! THREADS threads, all the cores by default). As `scripts/rec_bench.py`, for comparison.
//! `-v` prints the candidates, `-t` the CPU time of each step of the inference.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

fn thread_cpu() -> f64 {
    let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut t) };
    t.tv_sec as f64 + t.tv_nsec as f64 * 1e-9
}

struct Row {
    name: String,
    n: usize,
    z0: usize,
    got: usize,
    cpu: f64,
    lines: Vec<String>,
    steps: [f64; 5],
}

fn bench(path: &str) -> Row {
    let data = std::fs::read(path).unwrap();
    let z0 = aqr_infer_recs::zstd_size(&data);
    let mut steps = [0.0; 5];
    let t = thread_cpu();
    let mut lap = |i: usize, from: &mut f64| {
        let now = thread_cpu();
        steps[i] = now - *from;
        *from = now;
    };
    let mut at = t;
    let chains = aqr_infer_recs::chain::chains(&data);
    lap(0, &mut at);
    let tables = aqr_infer_recs::fixed::find(&data);
    lap(1, &mut at);
    let mut cands: Vec<aqr_infer_recs::Region> =
        chains.into_iter().map(|(p, r)| aqr_infer_recs::Region::entries(&data, p, r, None)).collect();
    lap(2, &mut at);
    cands.extend(tables.into_iter().map(|(s, k, n)| aqr_infer_recs::Region::fixed(&data, s, k, n)));
    lap(3, &mut at);
    let (choices, lines) = aqr_infer_recs::choose(&data, cands);
    lap(4, &mut at);
    let cpu = thread_cpu() - t;
    for c in &choices {
        let r = &c.region;
        let back = r.restore(c.form, &r.render(&data, c.form));
        assert!(back.as_deref() == Some(&data[r.start..r.end]), "{path}: {} does not restore", r.label());
    }
    let got = aqr_infer_recs::zstd_size(&aqr_infer_recs::apply(&data, &choices));
    let name = std::path::Path::new(path).file_name().unwrap().to_string_lossy().into_owned();
    Row { name, n: data.len(), z0, got, cpu, lines, steps }
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let mut verbose = false;
    let mut timed = false;
    while let Some(a) = args.first().cloned() {
        match a.as_str() {
            "-j" => {
                threads = args[1].parse().unwrap();
                args.drain(..2);
            }
            "-v" => {
                verbose = true;
                args.remove(0);
            }
            "-t" => {
                timed = true;
                args.remove(0);
            }
            _ => break,
        }
    }
    let next = AtomicUsize::new(0);
    let rows: Mutex<Vec<Option<Row>>> = Mutex::new((0..args.len()).map(|_| None).collect());
    let printed = Mutex::new(0usize);
    let tot = Mutex::new((0usize, 0usize, 0usize, 0f64));
    println!("| file | MB | zstd -3 | inferred | CPU s | MB/s |");
    println!("|---|---|---|---|---|---|");
    std::thread::scope(|s| {
        for _ in 0..threads.min(args.len()) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= args.len() {
                        break;
                    }
                    let row = bench(&args[i]);
                    rows.lock().unwrap()[i] = Some(row);
                    // in order, as the rows come
                    let mut p = printed.lock().unwrap();
                    let mut rows = rows.lock().unwrap();
                    while *p < rows.len() {
                        let Some(r) = rows[*p].take() else { break };
                        println!(
                            "| {} | {:.2} | {} | {} ({:.3}) | {:.2} | {:.2} |",
                            r.name,
                            r.n as f64 / 1e6,
                            r.z0,
                            r.got,
                            r.got as f64 / r.z0 as f64,
                            r.cpu,
                            r.n as f64 / 1e6 / r.cpu
                        );
                        if timed {
                            let s = r.steps;
                            println!(
                                "  chains {:.2}, tables {:.2}, entries' fields {:.2}, records' fields {:.2}, choice {:.2}",
                                s[0], s[1], s[2], s[3], s[4]
                            );
                        }
                        if verbose {
                            for l in &r.lines {
                                println!("{l}");
                            }
                        }
                        let mut t = tot.lock().unwrap();
                        *t = (t.0 + r.n, t.1 + r.z0, t.2 + r.got, t.3 + r.cpu);
                        *p += 1;
                    }
                }
            });
        }
    });
    let (n, z0, got, cpu) = *tot.lock().unwrap();
    println!(
        "| all | {:.2} | {z0} | {got} ({:.3}) | {cpu:.2} | {:.2} |",
        n as f64 / 1e6,
        got as f64 / z0 as f64,
        n as f64 / 1e6 / cpu
    );
}
