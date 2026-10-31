//! Which ALZ archives the formula rebuilds, what that is worth, and how fast: the bytes of the
//! archives, of their bodies by method and those rebuilt, of the DEFLATE records and of what is
//! kept; the encoder's and the formula's speeds. Only totals are printed, never names or
//! contents (`-v` adds a line per archive, by its index).
//!
//! ```sh
//! cargo run --release -p aqr-alz --example aqr-alz-check -- [-v] [-t THREADS] [-P PROFILE] FILE_OR_DIR...
//! ```

use std::path::{Path, PathBuf};
use std::time::Instant;

use aqr_alz::{alz, formula};
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
    let (mut files, mut rebuilt, mut failed, mut bytes) = (0, 0, 0, 0);
    // bytes of bodies by method (stored, bzip2, DEFLATE, other or encrypted), all and rebuilt
    let (mut by_method, mut taken) = ([0usize; 4], [0usize; 4]);
    let (mut records, mut kept, mut data) = (0, 0, 0);
    let (mut enc_secs, mut dec_secs) = (0.0, 0.0);
    for (i, p) in paths.iter().enumerate() {
        let Ok(d) = std::fs::read(p) else { continue };
        let Some(a) = alz::parse(&d) else { continue };
        files += 1;
        bytes += d.len();
        let slot = |e: &alz::Entry| match e.method {
            _ if e.encrypted => 3,
            Some(m @ 0..=2) => m as usize,
            _ => 3,
        };
        a.entries.iter().for_each(|e| by_method[slot(e)] += e.body.len());
        let opt = formula::Options { threads, profile, limit: formula::limit(d.len()), ..Default::default() };
        let t = Instant::now();
        let e = formula::encode_with(&d, &opt, &mut |_, _, _| true);
        enc_secs += t.elapsed().as_secs_f64();
        let Some(e) = e else {
            if verbose {
                println!("{i}: {} B, not rebuilt", d.len());
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
        let rec: usize = f.streams.iter().filter(|s| s.class == "flate/record").map(|s| s.data.len()).sum();
        (records, kept, data) = (records + rec, kept + k, data + e.inflated as usize);
        let mut got = [0usize; 4];
        a.entries.iter().zip(&e.rebuilt).filter(|r| *r.1).for_each(|(e, _)| got[slot(e)] += e.body.len());
        (0..4).for_each(|m| taken[m] += got[m]);
        if verbose {
            println!("{i}: {} B, {} entries, records {rec} B, kept {k} B, rebuilt {got:?}", d.len(), a.entries.len());
        }
    }
    println!("{files} archives ({bytes} B), {rebuilt} rebuilt, {failed} failed");
    for (m, name) in ["stored", "bzip2", "DEFLATE", "other/encrypted"].iter().enumerate() {
        println!("  {name}: {} B of bodies, {} B rebuilt", by_method[m], taken[m]);
    }
    let pct = |a: usize, b: usize| 100.0 * a as f64 / b.max(1) as f64;
    println!(
        "records {records} B ({:.2}% of the DEFLATE rebuilt, profile {profile}), kept {kept} B, data {data} B",
        pct(records, taken[2])
    );
    let mb = data as f64 / 1e6;
    println!(
        "encode {enc_secs:.2} s ({:.1} MB/s of data), formula {dec_secs:.2} s ({:.1} MB/s), {threads} threads",
        mb / enc_secs,
        mb / dec_secs
    );
}
