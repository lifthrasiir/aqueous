//! Splits resource tables, checks that they are rebuilt, and tells the streams' sizes under
//! zstd.
//!
//! `aqr-arsc-check [-l LEVEL] FILE_OR_DIR...` (zstd level 9 by default): the files' and each
//! stream's bytes, all files' together, as the archive groups them; then the module's inputs
//! (several streams each), the files' inputs one after another.

use aqr_arsc::arsc::{self, s};
use std::path::{Path, PathBuf};

fn walk(p: &Path, out: &mut Vec<PathBuf>) {
    if p.is_dir() {
        let mut e: Vec<_> = std::fs::read_dir(p).unwrap().map(|e| e.unwrap().path()).collect();
        e.sort();
        for p in e {
            walk(&p, out);
        }
    } else if p.extension().is_some_and(|e| e == "arsc") {
        out.push(p.to_path_buf());
    }
}

fn main() {
    let mut level = 9;
    let mut files = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-l" => level = args.next().unwrap().parse().unwrap(),
            _ => walk(Path::new(&a), &mut files),
        }
    }
    let mut whole = Vec::new();
    let mut streams: Vec<Vec<u8>> = vec![Vec::new(); s::N];
    let mut inputs: Vec<Vec<u8>> = vec![Vec::new(); arsc::INPUTS.len()];
    let (mut taken, mut failed) = (0, 0);
    for f in &files {
        let d = std::fs::read(f).unwrap();
        let t = std::time::Instant::now();
        let Some(st) = aqr_arsc::encode::split(&d) else {
            println!("{}: not taken", f.display());
            failed += 1;
            continue;
        };
        let split = t.elapsed();
        let refs: Vec<&[u8]> = st.iter().map(|v| v.as_slice()).collect();
        let t = std::time::Instant::now();
        let back = aqr_arsc::module::decode(&[], &refs, d.len() as u64).unwrap();
        let join = t.elapsed();
        assert!(back == d, "{}: not rebuilt", f.display());
        taken += 1;
        let un = arsc::unpack(&refs).unwrap();
        println!(
            "{}: {} bytes, split {:.1} ms, join {:.1} ms, kept {} bytes",
            f.display(),
            d.len(),
            split.as_secs_f64() * 1e3,
            join.as_secs_f64() * 1e3,
            un[s::RAW].len()
        );
        whole.extend_from_slice(&d);
        for (a, b) in streams.iter_mut().zip(&un) {
            a.extend_from_slice(b);
        }
        for (a, b) in inputs.iter_mut().zip(&st) {
            a.extend_from_slice(b);
        }
    }
    let z = |d: &[u8]| if d.is_empty() { 0 } else { zstd::bulk::compress(d, level).unwrap().len() };
    let zw = z(&whole);
    let mut sum = 0;
    for (name, v) in arsc::STREAM_NAMES.iter().zip(&streams) {
        let n = z(v);
        sum += n;
        println!("  {name:16} {:>11} -> {:>10}", v.len(), n);
    }
    let pct = |n: usize| 100.0 * (n as f64 / zw as f64 - 1.0);
    let mut packed = 0;
    for (name, v) in arsc::INPUT_NAMES.iter().zip(&inputs) {
        let n = z(v);
        packed += n;
        println!("  [{name}] {:>11} -> {:>10}", v.len(), n);
    }
    println!(
        "{taken} taken, {failed} not; {} bytes; zstd -{level}: files {zw}, streams {sum} ({:+.1}%), inputs {packed} ({:+.1}%)",
        whole.len(),
        pct(sum),
        pct(packed)
    );
}
