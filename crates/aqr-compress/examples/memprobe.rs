//! Peak heap of archiving each file (recompression, the archive and its verification) and of
//! extracting it, as aqr-show does them, and the process's peak resident set.
//!
//! usage: aqr-compress-memprobe [-t THREADS] [-e] [-s DIR] FILE... (`-e` as in aqr-show; `-s`
//! a spill there, `Options::spill`); `LOG=MB` logs the allocations of at least that many MB,
//! with where they were made.

use aqr_common::mem::{self, Counting};
use aqr_compress::{Nest, Options};

#[global_allocator]
static A: Counting = Counting;

fn main() {
    let log = std::env::var("LOG").ok().and_then(|s| s.parse::<f64>().ok()).map(|mb| (mb * 1e6) as usize);
    let mut opt = Options::default();
    let mut files = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match &*arg {
            "-t" => opt.threads = args.next().and_then(|s| s.parse().ok()).expect("-t THREADS"),
            "-e" => opt.file = Nest::Exact,
            "-s" => opt.spill = args.next().map(Into::into),
            _ => files.push(arg),
        }
    }
    let mb = |x: usize| x as f64 / 1e6;
    for path in &files {
        let d = std::fs::read(path).unwrap();
        println!("{path}: {:.2} MB", mb(d.len()));
        mem::log_large(log);
        let ((c, _f), enc) = mem::peak(|| aqr_compress::compress_formula(&d, &opt));
        if log.is_some() {
            println!("  -- extracting");
        }
        let (out, dec) = mem::peak(|| aqr_decompress::decompress(&c, opt.threads).expect("extracting"));
        mem::log_large(None);
        assert!(out == d, "{path}: not given back");
        let x = |n: usize| n as f64 / d.len() as f64;
        println!(
            "  archiving {:.1} MB ({:.1}x), extracting {:.1} MB ({:.1}x), peak RSS {:.1} MB",
            mb(enc),
            x(enc),
            mb(dec),
            x(dec),
            mem::max_rss() as f64 / 1e6
        );
    }
}
