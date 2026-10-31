//! Which bzip2 files are rebuilt bit for bit, and how fast. Only totals are printed, never
//! names or contents (`AQR_BZ2_NAMES=1` names those not rebuilt).
//!
//! ```sh
//! cargo run --release -p aqr-bz2 --example aqr-bz2-check -- [-t THREADS] FILE_OR_DIR...
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use aqr_bz2::{compress, formula, stream};

fn walk(p: &Path, out: &mut Vec<PathBuf>) {
    if p.is_dir() {
        let mut es: Vec<_> = std::fs::read_dir(p).into_iter().flatten().flatten().map(|e| e.path()).collect();
        es.sort();
        es.iter().for_each(|e| walk(e, out));
    } else {
        out.push(p.to_owned());
    }
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut threads = 1;
    if args.first().is_some_and(|a| a == "-t") {
        threads = args[1].parse().unwrap();
        args.drain(..2);
    }
    let mut paths = Vec::new();
    args.iter().for_each(|a| walk(Path::new(a), &mut paths));
    let names = std::env::var_os("AQR_BZ2_NAMES").is_some();
    let mut outcome: BTreeMap<String, usize> = BTreeMap::new();
    let (mut bytes, mut data, mut done) = (0usize, 0usize, 0usize);
    let (mut t_decode, mut t_encode, mut t_compress) = (0.0, 0.0, 0.0);
    for p in paths {
        let Ok(d) = std::fs::read(&p) else { continue };
        if !formula::starts(&d) {
            continue;
        }
        let t = Instant::now();
        let s = match stream::decode(&d, true, formula::MAX_DATA) {
            Ok(s) => s,
            Err(e) => {
                *outcome.entry(format!("decode: {e}")).or_default() += 1;
                continue;
            }
        };
        t_decode += t.elapsed().as_secs_f64();
        let t = Instant::now();
        let f = formula::encode(&d, threads);
        t_encode += t.elapsed().as_secs_f64();
        let k = match f {
            None => "not rebuilt",
            Some(_) => {
                let t = Instant::now();
                let out = compress::stream(s.level, &compress::blocks(&s.data, s.level, threads));
                t_compress += t.elapsed().as_secs_f64();
                bytes += s.len;
                data += s.data.len();
                done += 1;
                if out[..] == d[..s.len] { "rebuilt" } else { "rebuilt, first stream with ranks noted" }
            }
        };
        if names && k == "not rebuilt" {
            eprintln!("{}: not rebuilt", p.display());
        }
        *outcome.entry(k.into()).or_default() += 1;
    }
    for (k, n) in outcome {
        println!("{n:6} {k}");
    }
    let mb = data as f64 / 1e6;
    println!("{done} streams rebuilt: {bytes} B of streams, {data} B of data");
    println!(
        "decode {t_decode:.2} s ({:.1} MB/s of data), encode {t_encode:.2} s, compress {t_compress:.2} s ({:.1} MB/s), {threads} threads",
        mb / t_decode,
        mb / t_compress
    );
}
