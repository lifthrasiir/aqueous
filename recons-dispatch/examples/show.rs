//! Recompresses files into formulas and evaluates them back.
//!
//! usage: show [-t THREADS] [-d DEPTH] [-f] FILE...
//!
//! For each file: the detected format, the modules applied, the stream bytes by class, and
//! the times of recompression and evaluation; `-f` prints the formula itself.

use std::time::Instant;

use recons_dispatch::{Native, Options, classes, detect, native, recompress};
use recons_formula::container;

fn main() {
    let mut opt = Options::default();
    let mut tree = false;
    let mut files = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match &*arg {
            "-t" => opt.threads = args.next().and_then(|s| s.parse().ok()).expect("-t THREADS"),
            "-d" => opt.depth = args.next().and_then(|s| s.parse().ok()).expect("-d DEPTH"),
            "-f" => tree = true,
            _ => files.push(arg),
        }
    }

    let mut failed = false;
    for path in &files {
        let d = match std::fs::read(path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("{path}: {e}");
                failed = true;
                continue;
            }
        };
        let mb = d.len() as f64 / 1e6;
        let start = Instant::now();
        let f = recompress(&d, &opt);
        let enc = start.elapsed().as_secs_f64();
        let start = Instant::now();
        let ok = f.eval(&Native { threads: opt.threads }).is_ok_and(|out| out == d);
        let dec = start.elapsed().as_secs_f64();
        failed |= !ok;

        let format = detect(&d).map_or("-", |f| f.name());
        let packed = container::encode(&f, &native::IDS).map_or_else(|e| e.to_string(), |c| format!("{} B", c.len()));
        let modules: Vec<String> = f.applies().iter().map(|a| a.module.to_string()).collect();
        println!(
            "{path}: {format}, {mb:.2} MB, {} streams, container {packed}, enc {:.1} MB/s, eval {:.1} MB/s{}",
            f.streams.len(),
            mb / enc,
            mb / dec,
            if ok { "" } else { ", MISMATCH" }
        );
        println!("  modules: {}", if modules.is_empty() { "-".into() } else { modules.join(" ") });
        let classes = classes(&f);
        let shown: Vec<String> = classes.iter().take(8).map(|(c, n)| format!("{c} {n}")).collect();
        let more = if classes.len() > 8 { format!(" (+{} classes)", classes.len() - 8) } else { String::new() };
        println!("  streams: {}{more}", shown.join(", "));
        if tree {
            print!("{f}");
        }
    }
    if failed {
        std::process::exit(1);
    }
}
