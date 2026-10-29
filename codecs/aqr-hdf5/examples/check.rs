//! Which HDF5 files the formulas rebuild, what that is worth, and how fast: chunks found and
//! rebuilt, the bytes of the formulas' streams by class, zstd -3 of the files and of the
//! streams, and the encoder's and modules' speeds (`-v`: a line per file).
//!
//! ```sh
//! cargo run --release -p aqr-hdf5 --example aqr-hdf5-check -- [-v] [-t THREADS] [-p PROFILE] FILE_OR_DIR...
//! ```

use std::path::{Path, PathBuf};
use std::time::Instant;

use aqr_hdf5::formula::{self, Options};
use aqr_hdf5::hdf5;

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
    let (mut paths, mut verbose) = (Vec::new(), false);
    let mut opt = Options::default();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match &a[..] {
            "-v" => verbose = true,
            "-t" => opt.threads = args.next().unwrap().parse().unwrap(),
            "-p" => opt.profile = args.next().unwrap().parse().unwrap(),
            _ => walk(Path::new(&a), &mut paths),
        }
    }
    let zstd = |d: &[u8]| zstd::bulk::compress(d, 3).unwrap().len();
    let (mut files, mut bytes, mut zstd_in, mut zstd_out, mut enc_secs, mut dec_secs) = (0, 0, 0, 0, 0.0, 0.0);
    let mut classes: Vec<(String, usize, usize)> = Vec::new();
    for p in &paths {
        let Ok(d) = std::fs::read(p) else { continue };
        let Some(h) = hdf5::superblock(&d).and_then(|_| hdf5::parse(&d)) else { continue };
        files += 1;
        bytes += d.len();
        let zt = zstd(&d);
        zstd_in += zt;
        let start = Instant::now();
        let f = formula::encode_with(&d, &Options { limit: formula::limit(d.len()), ..opt.clone() });
        let enc = start.elapsed().as_secs_f64();
        enc_secs += enc;
        let name = p.display();
        let filtered: usize = h.chunks.iter().map(|c| c.at.len()).sum();
        let Some(f) = f.map(|e| e.formula) else {
            zstd_out += zt;
            if verbose {
                println!(
                    "{name}: {} B, {} datasets, {} chunks ({filtered} B), not taken",
                    d.len(),
                    h.datasets.len(),
                    h.chunks.len()
                );
            }
            continue;
        };
        let start = Instant::now();
        let out = f.eval(&|a: &aqr_formula::Apply, ins: &[&[u8]]| {
            formula::apply(a, ins, opt.threads).map_err(aqr_formula::Error::new)
        });
        dec_secs += start.elapsed().as_secs_f64();
        assert!(out.ok().as_deref() == Some(&d[..]), "{name} not rebuilt");
        let mut z = 0;
        for s in &f.streams {
            let n = zstd(&s.data);
            z += n;
            match classes.iter_mut().find(|c| c.0 == s.class) {
                Some(c) => {
                    c.1 += s.data.len();
                    c.2 += n;
                }
                None => classes.push((s.class.to_string(), s.data.len(), n)),
            }
        }
        zstd_out += z;
        if verbose {
            let (data, rec) = (f.streams[1].data.len(), f.streams[2].data.len());
            println!(
                "{name}: {} B, {} datasets, {} chunks ({filtered} B), {data} B inflated, record {rec} B, zstd {zt} -> {z}, {:.1} MB/s",
                d.len(),
                h.datasets.len(),
                h.chunks.len(),
                d.len() as f64 / enc / 1e6
            );
        }
    }
    println!("{files} files, {bytes} B; zstd -3: {zstd_in} -> {zstd_out} ({:.4})", zstd_out as f64 / zstd_in as f64);
    println!(
        "encode {:.1} MB/s, decode {:.1} MB/s (of the files)",
        bytes as f64 / enc_secs / 1e6,
        bytes as f64 / dec_secs / 1e6
    );
    classes.sort_by_key(|c| std::cmp::Reverse(c.2));
    for (c, n, z) in classes {
        println!("  {c}: {n} B, zstd {z}");
    }
}
