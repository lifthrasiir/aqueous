//! Which ZIP archives in files the module rebuilds, what that is worth, and how fast: entries
//! by how their bodies are made, zstd -3 of the archives and of the module's inputs, and the
//! encoder's and module's speeds. Only these totals are printed, never names or contents
//! (`-v` adds a line per archive, by its index).
//!
//! ```sh
//! cargo run --release -p aqr-zip --example aqr-zip-check -- [-v] [-t THREADS] FILE_OR_DIR...
//! ```

use std::path::{Path, PathBuf};
use std::time::Instant;

use aqr_formula::Node;
use aqr_zip::formula;
use aqr_zip::layout::{self, Body};

fn walk(p: &Path, out: &mut Vec<PathBuf>) {
    if p.is_dir() {
        let mut es: Vec<_> = std::fs::read_dir(p).into_iter().flatten().flatten().map(|e| e.path()).collect();
        es.sort();
        es.iter().for_each(|e| walk(e, out));
    } else {
        out.push(p.to_owned());
    }
}

/// Local entries of a layout by how their bodies are made.
fn kinds(layout: &[u8]) -> [usize; 4] {
    let mut n = [0; 4];
    for b in layout::bodies(layout).unwrap() {
        n[b as usize] += 1;
    }
    n
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
    let zstd = |d: &[u8]| zstd::bulk::compress(d, 3).unwrap().len();
    let (mut files, mut rebuilt, mut bytes, mut zstd_in, mut zstd_out) = (0, 0, 0, 0, 0);
    let (mut entries, mut enc_secs, mut dec_secs, mut made, mut failed) = ([0; 4], 0.0, 0.0, 0, 0);
    let mut classes: Vec<(String, usize, usize)> = Vec::new();
    for (k, p) in paths.iter().enumerate() {
        let Ok(d) = std::fs::read(p) else { continue };
        if !d.starts_with(b"PK\x03\x04") {
            continue;
        }
        files += 1;
        bytes += d.len();
        let zt = zstd(&d);
        zstd_in += zt;
        let start = Instant::now();
        let Some(f) = formula::encode(&d, threads) else {
            zstd_out += zt;
            continue;
        };
        enc_secs += start.elapsed().as_secs_f64();
        rebuilt += 1;
        let Node::Apply(a) = &f.root else { unreachable!() };
        let n = kinds(&f.streams[2].data);
        (0..4).for_each(|i| entries[i] += n[i]);
        let zs: Vec<usize> = f.streams.iter().map(|s| zstd(&s.data)).collect();
        zstd_out += zs.iter().sum::<usize>();
        for (s, z) in f.streams.iter().zip(&zs) {
            match classes.iter_mut().find(|c| c.0 == s.class) {
                Some(c) => (c.1, c.2) = (c.1 + s.data.len(), c.2 + z),
                None => classes.push((s.class.to_string(), s.data.len(), *z)),
            }
        }
        let ins: Vec<&[u8]> = f.streams.iter().map(|s| &s.data[..]).collect();
        let start = Instant::now();
        let ok = formula::apply(a, &ins, threads).is_ok_and(|o| o == d);
        dec_secs += start.elapsed().as_secs_f64();
        made += d.len();
        if !ok {
            failed += 1;
        }
        if verbose {
            println!(
                "#{k}: {} bytes, zstd {zt} -> {} ({:+.1}%), stored {} deflated {} kept {} same {}{}",
                d.len(),
                zs.iter().sum::<usize>(),
                100.0 * (zs.iter().sum::<usize>() as f64 / zt as f64 - 1.0),
                n[Body::Stored as usize],
                n[Body::Deflated as usize],
                n[Body::Kept as usize],
                n[Body::Same as usize],
                if ok { "" } else { " FAILED" }
            );
        }
    }
    println!("archives {files}, rebuilt {rebuilt}, round trips failed {failed}");
    println!(
        "entries: stored {}, deflated {}, kept {}, same {}",
        entries[Body::Stored as usize],
        entries[Body::Deflated as usize],
        entries[Body::Kept as usize],
        entries[Body::Same as usize]
    );
    for (c, n, z) in &classes {
        println!("  {c}: {n} bytes, zstd -3 {z}");
    }
    println!(
        "{bytes} bytes; zstd -3 {zstd_in} -> {zstd_out} ({:+.1}%); encoder {:.0} MB/s, module {:.0} MB/s of archives",
        100.0 * (zstd_out as f64 / zstd_in.max(1) as f64 - 1.0),
        made as f64 / enc_secs / 1e6,
        made as f64 / dec_secs / 1e6
    );
}
