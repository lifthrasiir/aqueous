//! Compresses files into archives, as aqr-compress does (verification included), and
//! decompresses them back.
//!
//! usage: aqr-show [-t THREADS] [-d DEPTH] [-l LEVEL] [-e] [-r] [-m] [-f] FILE...
//!
//! For each file: the detected format, the modules applied, the stream bytes by class, and the
//! archive (of level `-l`, by default mid), the bytes of its resource blocks among them, with
//! the speeds of compressing it (recompression, the archive and its verification) and of
//! decompressing it; `-e` takes each file as one file of the format it starts like rather than
//! scanning it, `-r` decides whether formulas pay without their resources (as in an archive of
//! many files sharing them; the archive still holds them), `-m` likewise without their modules,
//! and `-f` prints the formula itself.

use std::time::Instant;

use aqr_compress::{Nest, Options, classes, compress_formula_as, detect, kind_by_name};
use aqr_decompress::decompress;
use aqr_formula::aqr;

#[global_allocator]
static ALLOC: aqr_common::mem::Mapped = aqr_common::mem::Mapped;

fn main() {
    let mut opt = Options::default();
    let mut tree = false;
    let mut files = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match &*arg {
            "-t" => opt.threads = args.next().and_then(|s| s.parse().ok()).expect("-t THREADS"),
            "-d" => opt.depth = args.next().and_then(|s| s.parse().ok()).expect("-d DEPTH"),
            "-l" => opt.level = args.next().expect("-l LEVEL").parse().expect("-l 0-9|low|mid|high|zstdN"),
            "-e" => opt.file = Nest::Exact,
            "-r" => opt.shared_resources = true,
            "-m" => opt.shared_modules = true,
            "-f" => tree = true,
            _ => files.push(arg),
        }
    }

    // (the threads of the pool, as many as -t)
    aqr_common::pool::init(opt.threads);
    if !aqr_common::pool::run(|| show(&files, &opt, tree)) {
        std::process::exit(1);
    }
}

/// Shows each of `files` as the module docs tell; returns whether all went well.
fn show(files: &[String], opt: &Options, tree: bool) -> bool {
    // the resources compressed once for all files
    let enc = aqr::Encoder::with_threads(opt.level, opt.threads);
    let mut failed = false;
    for path in files {
        let d = match std::fs::read(path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("{path}: {e}");
                failed = true;
                continue;
            }
        };
        let mb = d.len() as f64 / 1e6;
        let format = detect(&d).map_or("-", |f| f.name());
        let start = Instant::now();
        let (c, f) = compress_formula_as(&d, kind_by_name(path.as_bytes()), opt);
        let pack = start.elapsed().as_secs_f64();
        let start = Instant::now();
        let unpacked = decompress(&c, opt.threads);
        let unpack = start.elapsed().as_secs_f64();
        let res = enc.resources_len(&f).expect("slices within their streams");
        let archived = match &unpacked {
            Ok(out) if *out == d => format!(
                "archive {} B, resources {res} B, compress {:.1} MB/s, decompress {:.1} MB/s",
                c.len(),
                mb / pack,
                mb / unpack
            ),
            Ok(_) => format!("archive {} B, decompressed MISMATCH", c.len()),
            Err(e) => format!("archive {} B, decompressing failed: {e}", c.len()),
        };
        failed |= !matches!(&unpacked, Ok(out) if *out == d);
        let modules: Vec<String> = f.applies().iter().map(|a| a.module.to_string()).collect();
        println!("{path}: {format}, {mb:.2} MB, {} streams, {archived}", f.streams.len());
        println!("  modules: {}", if modules.is_empty() { "-".into() } else { modules.join(" ") });
        let classes = classes(&f);
        let shown: Vec<String> = classes.iter().take(8).map(|(c, n)| format!("{c} {n}")).collect();
        let more = if classes.len() > 8 { format!(" (+{} classes)", classes.len() - 8) } else { String::new() };
        println!("  streams: {}{more}", shown.join(", "));
        if tree {
            print!("{f}");
        }
    }
    !failed
}
