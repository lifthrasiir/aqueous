//! Splits DEX files, checks that they are rebuilt, and tells the streams' sizes under zstd.
//!
//! `aqr-dex-check [-l LEVEL] FILE_OR_DIR...` (zstd level 9 by default): the files' and each
//! stream's bytes, all files' together, as the archive groups them; then the module's inputs
//! (several streams each), the files' inputs one after another.

use aqr_dex::dex::{self, s};
use std::path::{Path, PathBuf};

fn walk(p: &Path, out: &mut Vec<PathBuf>) {
    if p.is_dir() {
        let mut e: Vec<_> = std::fs::read_dir(p).unwrap().map(|e| e.unwrap().path()).collect();
        e.sort();
        for p in e {
            walk(&p, out);
        }
    } else if p.extension().is_some_and(|e| e == "dex") || p.extension().is_none() {
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
    let mut inputs: Vec<Vec<u8>> = vec![Vec::new(); dex::INPUTS.len()];
    let (mut taken, mut failed) = (0, 0);
    for f in &files {
        let d = std::fs::read(f).unwrap();
        let t = std::time::Instant::now();
        let Some(st) = aqr_dex::encode::split(&d) else {
            println!("{}: not taken", f.display());
            failed += 1;
            continue;
        };
        let split = t.elapsed();
        let refs: Vec<&[u8]> = st.iter().map(|v| v.as_slice()).collect();
        let t = std::time::Instant::now();
        let back = aqr_dex::module::decode(&[], &refs, d.len() as u64).unwrap();
        let join = t.elapsed();
        assert!(back == d, "{}: not rebuilt", f.display());
        taken += 1;
        let un = dex::unpack(&refs).unwrap();
        let head = un[s::HEAD];
        let count = dex::le32(head, dex::HEADER + 1).unwrap() as usize;
        let items = dex::items(&head[dex::HEADER + 5..][..12 * count]);
        let modes = &head[dex::HEADER + 5 + 12 * count..];
        let raw: Vec<String> = items
            .iter()
            .zip(modes)
            .filter(|(it, m)| dex::takes(it.ty) && **m != dex::mode::TAKEN)
            .map(|(it, _)| format!("{:#06x}", it.ty))
            .collect();
        println!(
            "{}: {} bytes, split {:.1} ms, join {:.1} ms, checks {}{}",
            f.display(),
            d.len(),
            split.as_secs_f64() * 1e3,
            join.as_secs_f64() * 1e3,
            head[dex::HEADER],
            if raw.is_empty() { String::new() } else { format!(", kept {}", raw.join(" ")) }
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
    for (name, v) in dex::STREAM_NAMES.iter().zip(&streams) {
        let n = z(v);
        sum += n;
        println!("  {name:16} {:>11} -> {:>10}", v.len(), n);
    }
    let pct = |n: usize| 100.0 * (n as f64 / zw as f64 - 1.0);
    let mut packed = 0;
    for (name, v) in dex::INPUT_NAMES.iter().zip(&inputs) {
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
