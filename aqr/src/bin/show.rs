//! Recompresses files into formulas and evaluates them back.
//!
//! usage: aqr-show [-t THREADS] [-d DEPTH] [-l mid|ZSTD_LEVEL] [-e] [-f] FILE...
//!
//! For each file: the detected format, the modules applied, the stream bytes by class, the
//! times of recompression and evaluation, and the archive (of level `-l`, by default mid) with
//! its round trip; `-e` takes each file as one file of the format it starts like rather than scanning
//! it, and `-f` prints the formula itself.

use std::time::Instant;

use aqr_compress::{Nest, Options, classes, detect, recompress};
use aqr_decompress::{Native, decompress};
use aqr_formula::aqr;

fn main() {
    let mut opt = Options::default();
    let mut tree = false;
    let mut files = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match &*arg {
            "-t" => opt.threads = args.next().and_then(|s| s.parse().ok()).expect("-t THREADS"),
            "-d" => opt.depth = args.next().and_then(|s| s.parse().ok()).expect("-d DEPTH"),
            "-l" => {
                opt.level = match args.next().expect("-l mid|ZSTD_LEVEL") {
                    l if l == "mid" => aqr::Level::Mid,
                    l => aqr::Level::Zstd(l.parse().expect("-l mid|ZSTD_LEVEL")),
                }
            }
            "-e" => opt.file = Nest::Exact,
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
        let start = Instant::now();
        let packed = aqr::Encoder::with_threads(opt.level, opt.threads).encode(&f, Some(aqr::checksum(&d)));
        let pack = start.elapsed().as_secs_f64();
        let start = Instant::now();
        let unpacked = packed.as_ref().ok().map(|c| decompress(c, opt.threads));
        let unpack = start.elapsed().as_secs_f64();
        let archived = match (&packed, &unpacked) {
            (Ok(c), Some(Ok(out))) if *out == d => {
                format!("archive {} B, pack {:.1} MB/s, unpack {:.1} MB/s", c.len(), mb / pack, mb / unpack)
            }
            (Ok(c), Some(Ok(_))) => format!("archive {} B, unpacked MISMATCH", c.len()),
            (Ok(c), Some(Err(e))) => format!("archive {} B, unpacking failed: {e}", c.len()),
            (Err(e), _) => format!("archive failed: {e}"),
            (Ok(_), None) => unreachable!(),
        };
        failed |= !matches!(&unpacked, Some(Ok(out)) if *out == d);
        let modules: Vec<String> = f.applies().iter().map(|a| a.module.to_string()).collect();
        println!(
            "{path}: {format}, {mb:.2} MB, {} streams, enc {:.1} MB/s, eval {:.1} MB/s{}, {archived}",
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
