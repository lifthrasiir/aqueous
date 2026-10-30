//! Which 7z archives in files the formula rebuilds, what that is worth, and how fast: the bytes
//! of the archives, of their packed streams rebuilt, of the data and records of those, and of
//! what is kept; the encoder's and modules' speeds. Only totals are printed, never names or
//! contents (`-v` adds a line per archive, by its index).
//!
//! ```sh
//! cargo run --release -p aqr-7z --example aqr-7z-check -- [-v] [-t THREADS] FILE_OR_DIR...
//! ```

use std::path::{Path, PathBuf};
use std::time::Instant;

use aqr_7z::formula;
use aqr_formula::Apply;

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
    let (mut paths, mut verbose, mut threads) = (Vec::new(), false, 1);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match &a[..] {
            "-v" => verbose = true,
            "-t" => threads = args.next().unwrap().parse().unwrap(),
            _ => walk(Path::new(&a), &mut paths),
        }
    }
    let (mut files, mut rebuilt, mut bytes, mut packed, mut data, mut records, mut kept) = (0, 0, 0, 0, 0, 0, 0);
    let (mut enc_secs, mut dec_secs, mut failed) = (0.0, 0.0, 0);
    for (k, p) in paths.iter().enumerate() {
        let Ok(d) = std::fs::read(p) else { continue };
        if !formula::starts(&d) {
            continue;
        }
        files += 1;
        bytes += d.len();
        let start = Instant::now();
        let Some(f) = formula::encode(&d, threads) else {
            if verbose {
                println!("{k}: {} bytes, not taken", d.len());
            }
            kept += d.len();
            continue;
        };
        enc_secs += start.elapsed().as_secs_f64();
        let start = Instant::now();
        let out = f.eval(&|a: &Apply, inputs: &[&[u8]]| {
            let decode = if a.module == aqr_lzma::formula::LZMA2 {
                aqr_lzma::formula::decode2
            } else {
                aqr_lzma::formula::decode
            };
            decode(&a.params, inputs, a.len, threads).map_err(|e| aqr_formula::Error::new(e.to_string()))
        });
        dec_secs += start.elapsed().as_secs_f64();
        if out.ok().as_deref() != Some(&d[..]) {
            println!("{k}: rebuilt otherwise");
            failed += 1;
            continue;
        }
        rebuilt += 1;
        let (mut p, mut r) = (0, 0);
        for a in f.applies() {
            p += a.len as usize;
            r += a.inputs[1].len() as usize;
        }
        let container = f.streams[0].data.len();
        let dt: usize =
            f.streams.iter().filter(|s| s.class == "raw" || s.class.starts_with("7z/")).map(|s| s.data.len()).sum();
        if verbose {
            println!(
                "{k}: {} bytes, {p} packed rebuilt from {dt} of data and {r} of records, {container} kept",
                d.len()
            );
        }
        packed += p;
        data += dt;
        records += r;
        kept += container;
    }
    let mb = |x: usize| x as f64 / 1e6;
    println!(
        "{files} archives, {rebuilt} rebuilt ({failed} otherwise): {:.3} MB; packed streams rebuilt {:.3} MB from data {:.3} MB and records {:.3} MB ({:.1}%); kept {:.3} MB",
        mb(bytes),
        mb(packed),
        mb(data),
        mb(records),
        100.0 * records as f64 / packed.max(1) as f64,
        mb(kept)
    );
    println!(
        "  encode {enc_secs:.2} s ({:.1} MB/s of data), rebuild {dec_secs:.2} s ({:.1} MB/s)",
        mb(data) / enc_secs,
        mb(data) / dec_secs
    );
}
