//! Which ar, tar and cpio archives in files the module rebuilds, what that is worth, and how
//! fast: members by format, zstd -3 of the archives and of the module's inputs (the contents
//! and the layout), and the encoder's and module's speeds. Only these totals are printed
//! (`-v` adds a line per archive, with its path).
//!
//! ```sh
//! cargo run --release -p aqr-bundle --example aqr-bundle-check -- [-v] FILE_OR_DIR...
//! ```

use std::path::{Path, PathBuf};
use std::time::Instant;

use aqr_bundle::formula::{self, Format};
use aqr_bundle::layout;
use aqr_formula::Node;

fn walk(p: &Path, out: &mut Vec<PathBuf>) {
    if p.is_dir() {
        let mut es: Vec<_> = std::fs::read_dir(p).into_iter().flatten().flatten().map(|e| e.path()).collect();
        es.sort();
        es.iter().for_each(|e| walk(e, out));
    } else if p.is_file() {
        out.push(p.to_owned());
    }
}

fn main() {
    let (mut paths, mut verbose) = (Vec::new(), false);
    for a in std::env::args().skip(1) {
        match &a[..] {
            "-v" => verbose = true,
            _ => walk(Path::new(&a), &mut paths),
        }
    }
    let zstd = |d: &[u8]| zstd::bulk::compress(d, 3).unwrap().len();
    // by format: archives, rebuilt, bytes, members, zstd of the archives, of the inputs
    let mut by: [(usize, usize, usize, usize, usize, usize); 3] = Default::default();
    let (mut enc_secs, mut dec_secs, mut failed) = (0.0, 0.0, 0);
    for p in &paths {
        let Ok(d) = std::fs::read(p) else { continue };
        let Some(format) = formula::detect(&d) else { continue };
        let row = &mut by[format as usize];
        row.0 += 1;
        let start = Instant::now();
        let f = formula::encode(&d, 1);
        enc_secs += start.elapsed().as_secs_f64();
        let Some(f) = f else {
            if verbose {
                println!("{format:?} not taken: {}", p.display());
            }
            continue;
        };
        let Node::Apply(a) = &f.root else { unreachable!() };
        let eval = |n: &Node| -> Vec<u8> {
            let mut out = Vec::new();
            let mut take = |n: &Node| {
                if let &Node::Slice { stream, start, len } = n {
                    out.extend_from_slice(&f.streams[stream].data[start as usize..][..len as usize]);
                }
            };
            match n {
                Node::Concat(ns) => ns.iter().for_each(&mut take),
                n => take(n),
            }
            out
        };
        let (data, lay) = (eval(&a.inputs[0]), eval(&a.inputs[1]));
        let start = Instant::now();
        let out = formula::decode(&[], &[&data, &lay], d.len() as u64, 1);
        dec_secs += start.elapsed().as_secs_f64();
        if out.as_deref() != Ok(&d[..]) {
            failed += 1;
            println!("{format:?} NOT REBUILT: {}", p.display());
            continue;
        }
        let mut members = 0;
        layout::build_with(&lay, &data, d.len() as u64, &mut |i| members += (i.tag >= layout::AR) as usize).unwrap();
        let (zin, zout) = (zstd(&d), zstd(&data) + zstd(&lay));
        *row = (row.0, row.1 + 1, row.2 + d.len(), row.3 + members, row.4 + zin, row.5 + zout);
        if verbose {
            println!(
                "{format:?} {} B, {members} members, layout {} B, zstd {zin} -> {zout}: {}",
                d.len(),
                lay.len(),
                p.display()
            );
        }
    }
    for (format, r) in [Format::Ar, Format::Tar, Format::Cpio].iter().zip(by) {
        if r.0 > 0 {
            println!(
                "{format:?}: {} archives, {} rebuilt ({} MB, {} members); zstd -3 {} -> {} ({:+.2}%)",
                r.0,
                r.1,
                r.2 as f64 / 1e6,
                r.3,
                r.4,
                r.5,
                (r.5 as f64 / r.4.max(1) as f64 - 1.0) * 100.0
            );
        }
    }
    let bytes: usize = by.iter().map(|r| r.2).sum();
    println!(
        "encode {:.0} MB/s, decode {:.0} MB/s; {failed} not rebuilt",
        bytes as f64 / 1e6 / enc_secs.max(1e-9),
        bytes as f64 / 1e6 / dec_secs.max(1e-9)
    );
    if failed > 0 {
        std::process::exit(1);
    }
}
