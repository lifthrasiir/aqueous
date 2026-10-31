//! Which CAB files the formula rebuilds, what that is worth, and how fast: the bytes of the
//! files, of their folders by method and those rebuilt, of the data and records of those, and of
//! what is kept; the encoder's and modules' speeds. Only totals are printed, never names or
//! contents (`-v` adds a line per file, by its index).
//!
//! ```sh
//! cargo run --release -p aqr-mszip --example aqr-mszip-check -- [-v] [-t THREADS] [-P PROFILE] FILE_OR_DIR...
//! ```

use std::path::{Path, PathBuf};
use std::time::Instant;

use aqr_formula::Apply;
use aqr_mszip::{cab, formula};

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
    let (mut files, mut rebuilt, mut bytes, mut data, mut records, mut kept, mut failed) = (0, 0, 0, 0, 0, 0, 0);
    // bytes of folders' spans by method (none, MSZIP, Quantum, LZX, other), all and rebuilt
    let (mut by_method, mut taken) = ([0usize; 5], [0usize; 5]);
    let (mut enc_secs, mut dec_secs) = (0.0, 0.0);
    let mut deflate = 0;
    for (k, p) in paths.iter().enumerate() {
        let Ok(d) = std::fs::read(p) else { continue };
        let Some(c) = cab::parse(&d) else { continue };
        files += 1;
        bytes += d.len();
        let spans: Vec<(usize, std::ops::Range<usize>)> =
            c.folders.iter().filter_map(|f| Some(((f.method() as usize).min(4), f.span()?))).collect();
        spans.iter().for_each(|(m, s)| by_method[*m] += s.len());
        let start = Instant::now();
        let opt = formula::Options { threads, profile, ..Default::default() };
        let Some(e) = formula::encode_with(&d, &opt) else {
            if verbose {
                println!("{k}: {} bytes, not taken", d.len());
            }
            kept += d.len();
            continue;
        };
        let f = e.formula;
        enc_secs += start.elapsed().as_secs_f64();
        let start = Instant::now();
        let out = f.eval(&|a: &Apply, inputs: &[&[u8]]| {
            formula::apply(a, inputs, threads).map_err(|e| aqr_formula::Error::new(e.to_string()))
        });
        dec_secs += start.elapsed().as_secs_f64();
        if out.ok().as_deref() != Some(&d[..]) {
            println!("{k}: rebuilt otherwise");
            failed += 1;
            continue;
        }
        rebuilt += 1;
        let container = f.streams[0].data.len();
        // the spans not kept in the container are the folders rebuilt
        let mut r = 0;
        for a in f.applies().into_iter().filter(|a| a.module == aqr_flate::formula::MODULE) {
            r += a.inputs[1].len() as usize;
            deflate += a.len as usize;
        }
        let dt: usize = f
            .streams
            .iter()
            .filter(|s| s.class == "raw" || s.class.starts_with("cab/") && s.class != "cab/data-headers")
            .map(|s| s.data.len())
            .sum();
        let rest = d.len() - container;
        // the pieces of the root that are not the container's: the folders rebuilt
        let mut starts = Vec::new();
        let mut at = 0;
        let children = match &f.root {
            aqr_formula::Node::Concat(v) => v.clone(),
            n => vec![n.clone()],
        };
        for n in &children {
            if !matches!(n, aqr_formula::Node::Slice { stream: 0, .. }) {
                starts.push(at);
            }
            at += n.len() as usize;
        }
        for (m, s) in &spans {
            if starts.contains(&s.start) {
                taken[*m] += s.len();
            }
        }
        if verbose {
            println!("{k}: {} bytes, {rest} rebuilt from {dt} of data and {r} of records, {container} kept", d.len());
        }
        data += dt;
        records += r;
        kept += container;
    }
    let mb = |x: usize| x as f64 / 1e6;
    println!(
        "{files} files, {rebuilt} rebuilt ({failed} otherwise): {:.3} MB; data {:.3} MB, records {:.3} MB ({:.2}% of {:.3} MB joined DEFLATE); kept {:.3} MB",
        mb(bytes),
        mb(data),
        mb(records),
        100.0 * records as f64 / deflate.max(1) as f64,
        mb(deflate),
        mb(kept)
    );
    let names = ["none", "MSZIP", "Quantum", "LZX", "other"];
    for m in 0..5 {
        if by_method[m] > 0 {
            println!("  {}: {:.3} MB of folders, {:.3} MB rebuilt", names[m], mb(by_method[m]), mb(taken[m]));
        }
    }
    println!(
        "  encode {enc_secs:.2} s ({:.1} MB/s of data), rebuild {dec_secs:.2} s ({:.1} MB/s)",
        mb(data) / enc_secs,
        mb(data) / dec_secs
    );
}
