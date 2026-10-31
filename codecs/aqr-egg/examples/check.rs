//! Which EGG archives the formula rebuilds, what that is worth, and how fast: the bytes of the
//! archives, of their blocks by method and those rebuilt, of the DEFLATE and LZMA records and of
//! what is kept; the encoder's and the formula's speeds. Only totals are printed, never names or
//! contents (`-v` adds a line per archive, by its index).
//!
//! ```sh
//! cargo run --release -p aqr-egg --example aqr-egg-check -- [-v] [-t THREADS] [-P PROFILE] FILE_OR_DIR...
//! ```

use std::path::{Path, PathBuf};
use std::time::Instant;

use aqr_egg::{egg, formula};
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

const SLOTS: [&str; 6] = ["stored", "DEFLATE", "bzip2", "AZO", "LZMA", "other/encrypted"];

fn main() {
    let (mut paths, mut verbose, mut threads, mut profile) = (Vec::new(), false, 1, aqr_flate::DEFAULT_PROFILE);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match &a[..] {
            "-v" => verbose = true,
            "-t" => threads = args.next().unwrap().parse().unwrap(),
            "-P" => profile = args.next().unwrap().parse().unwrap(),
            _ => walk(Path::new(&a), &mut paths),
        }
    }
    let (mut files, mut rebuilt, mut failed, mut bytes, mut short) = (0, 0, 0, 0, 0);
    // bytes of blocks by method (stored, DEFLATE, bzip2, AZO, LZMA, other or encrypted), all and
    // rebuilt
    let (mut by_method, mut taken) = ([0usize; 6], [0usize; 6]);
    let (mut flate_records, mut lzma_records, mut azo_records, mut kept, mut data) = (0, 0, 0, 0, 0);
    let (mut enc_secs, mut dec_secs) = (0.0, 0.0);
    for (i, p) in paths.iter().enumerate() {
        let Ok(d) = std::fs::read(p) else { continue };
        let Some(a) = egg::parse(&d) else { continue };
        files += 1;
        bytes += d.len();
        // (records ending before the file does: something not read, or other data after)
        short += (a.end < d.len()) as usize;
        let blocks: Vec<(usize, usize)> = a
            .entries
            .iter()
            .flat_map(|e| {
                e.blocks.iter().map(|b| (if e.encrypted || b.method > 4 { 5 } else { b.method as usize }, b.body.len()))
            })
            .collect();
        blocks.iter().for_each(|&(s, n)| by_method[s] += n);
        let opt = formula::Options { threads, profile, limit: formula::limit(d.len()), ..Default::default() };
        let t = Instant::now();
        let e = formula::encode_with(&d, &opt, &mut |_, _, _| true);
        enc_secs += t.elapsed().as_secs_f64();
        let Some(e) = e else {
            if verbose {
                let mut all = [0usize; 6];
                blocks.iter().for_each(|&(s, n)| all[s] += n);
                println!("{i}: {} B (records to {}), not rebuilt, of {all:?}", d.len(), a.end);
            }
            continue;
        };
        let f = e.formula;
        let t = Instant::now();
        let out =
            f.eval(&|a: &Apply, inputs: &[&[u8]]| formula::apply(a, inputs, threads).map_err(aqr_formula::Error::new));
        dec_secs += t.elapsed().as_secs_f64();
        if out.as_deref().ok() != Some(&d[..]) {
            failed += 1;
            println!("{i}: FAILED");
            continue;
        }
        rebuilt += 1;
        let k = f.streams[0].data.len();
        let class = |c: &str| -> usize { f.streams.iter().filter(|s| s.class == c).map(|s| s.data.len()).sum() };
        let (fr, lr, ar) = (class("flate/record"), class("lzma/record"), class("azo/record"));
        (flate_records, lzma_records, azo_records, kept, data) =
            (flate_records + fr, lzma_records + lr, azo_records + ar, kept + k, data + e.inflated as usize);
        let mut all = [0usize; 6];
        blocks.iter().for_each(|&(s, n)| all[s] += n);
        let mut got = [0usize; 6];
        blocks.iter().zip(&e.rebuilt).filter(|r| *r.1).for_each(|(&(s, n), _)| got[s] += n);
        (0..6).for_each(|m| taken[m] += got[m]);
        if verbose {
            println!(
                "{i}: {} B (records to {}), {} files, records {fr}+{lr}+{ar} B, kept {k} B, rebuilt {got:?} of {all:?}",
                d.len(),
                a.end,
                a.entries.len()
            );
        }
    }
    println!("{files} archives ({bytes} B, {short} ending before their files), {rebuilt} rebuilt, {failed} failed");
    for (m, name) in SLOTS.iter().enumerate() {
        println!("  {name}: {} B of blocks, {} B rebuilt", by_method[m], taken[m]);
    }
    let pct = |a: usize, b: usize| 100.0 * a as f64 / b.max(1) as f64;
    println!(
        "records: DEFLATE {flate_records} B ({:.2}% of the DEFLATE rebuilt, profile {profile}), LZMA {lzma_records} B \
         ({:.2}%), AZO {azo_records} B ({:.2}%); kept {kept} B, data {data} B",
        pct(flate_records, taken[1]),
        pct(lzma_records, taken[4]),
        pct(azo_records, taken[3])
    );
    let mb = data as f64 / 1e6;
    println!(
        "encode {enc_secs:.2} s ({:.1} MB/s of data), formula {dec_secs:.2} s ({:.1} MB/s), {threads} threads",
        mb / enc_secs,
        mb / dec_secs
    );
}
