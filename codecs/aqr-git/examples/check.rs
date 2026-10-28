//! Which git packs in files the module rebuilds, what that is worth, and how fast: entries
//! rebuilt and kept, the bytes of the formulas' streams by class, zstd -3 of the packs and of
//! the streams, and the encoder's and modules' speeds. Only totals are printed, never names or
//! contents (`-v` adds a line per pack, by its index).
//!
//! ```sh
//! cargo run --release -p aqr-git --example aqr-git-check -- [-v] [-t THREADS] [-p PROFILE] [-z] FILE_OR_DIR...
//! ```
//!
//! `-p`: aqr-flate's profile of the records (by default its default); `-z`: zlib emulation
//! (adaptive profiles); `-d DIR`: each formula's streams written to `DIR/<index>.<n>.<class>`.

use std::path::{Path, PathBuf};
use std::time::Instant;

use aqr_git::formula::{self, Options};

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
    let (mut paths, mut verbose, mut dump) = (Vec::new(), false, None::<PathBuf>);
    let mut opt = Options { zlib: false, ..Options::default() };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match &a[..] {
            "-v" => verbose = true,
            "-t" => opt.threads = args.next().unwrap().parse().unwrap(),
            "-p" => opt.profile = args.next().unwrap().parse().unwrap(),
            "-z" => opt.zlib = true,
            "-d" => dump = Some(args.next().unwrap().into()),
            _ => walk(Path::new(&a), &mut paths),
        }
    }
    let zstd = |d: &[u8]| zstd::bulk::compress(d, 3).unwrap().len();
    let (mut packs, mut bytes, mut zstd_in, mut zstd_out, mut enc_secs, mut dec_secs) = (0, 0, 0, 0, 0.0, 0.0);
    let mut classes: Vec<(String, usize, usize)> = Vec::new();
    for (k, p) in paths.iter().enumerate() {
        let Ok(d) = std::fs::read(p) else { continue };
        if !aqr_git::pack::starts(&d) {
            continue;
        }
        packs += 1;
        bytes += d.len();
        let zt = zstd(&d);
        zstd_in += zt;
        let start = Instant::now();
        let f = formula::encode_with(&d, &Options { limit: formula::limit(d.len()), ..opt.clone() }).map(|e| e.formula);
        let enc = start.elapsed().as_secs_f64();
        enc_secs += enc;
        let Some(f) = f else {
            zstd_out += zt;
            if verbose {
                println!("#{k}: {} B, not taken", d.len());
            }
            continue;
        };
        let start = Instant::now();
        let out = f.eval(&|a: &aqr_formula::Apply, ins: &[&[u8]]| {
            formula::apply(a, ins, opt.threads).map_err(|e| aqr_formula::Error::new(e.to_string()))
        });
        dec_secs += start.elapsed().as_secs_f64();
        assert!(out.ok().as_deref() == Some(&d[..]), "#{k} not rebuilt");
        let mut z = 0;
        for (n, s) in f.streams.iter().enumerate() {
            if let Some(dir) = &dump {
                std::fs::write(dir.join(format!("{k}.{n}.{}", s.class.replace('/', "-"))), &s.data).unwrap();
            }
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
            println!("#{k}: {} B, zstd {zt} -> {z}, {:.1} MB/s", d.len(), d.len() as f64 / enc / 1e6);
        }
    }
    println!("{packs} packs, {bytes} B; zstd -3: {zstd_in} -> {zstd_out} ({:.4})", zstd_out as f64 / zstd_in as f64);
    println!(
        "encode {:.1} MB/s, decode {:.1} MB/s (of the packs)",
        bytes as f64 / enc_secs / 1e6,
        bytes as f64 / dec_secs / 1e6
    );
    classes.sort_by_key(|c| std::cmp::Reverse(c.2));
    for (c, n, z) in classes {
        println!("  {c}: {n} B, zstd {z}");
    }
}
