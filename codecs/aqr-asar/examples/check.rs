//! Which asar archives in files the module rebuilds, what that is worth, and how fast: zstd -3
//! of the headers as they are and of the module's (the header with holes and the index), the
//! files and the holes, and the encoder's and module's speeds. `-v` adds a line per archive.
//!
//! ```sh
//! cargo run --release -p aqr-asar --example aqr-asar-check -- [-v] FILE_OR_DIR...
//! ```

use std::path::{Path, PathBuf};
use std::time::Instant;

use aqr_asar::formula;
use aqr_formula::{Formula, Node};

fn walk(p: &Path, out: &mut Vec<PathBuf>) {
    if p.is_dir() {
        let mut es: Vec<_> = std::fs::read_dir(p).into_iter().flatten().flatten().map(|e| e.path()).collect();
        es.sort();
        es.iter().for_each(|e| walk(e, out));
    } else if p.is_file() {
        out.push(p.to_owned());
    }
}

fn eval(n: &Node, f: &Formula) -> Vec<u8> {
    match n {
        &Node::Slice { stream, start, len } => f.streams[stream].data[start as usize..][..len as usize].to_vec(),
        Node::Concat(ns) => ns.iter().flat_map(|n| eval(n, f)).collect(),
        _ => panic!("{n:?}"),
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
    let (mut found, mut rebuilt, mut failed, mut bytes, mut files) = (0, 0, 0, 0, 0);
    let (mut zhead, mut zours, mut head_bytes) = (0, 0, 0);
    let (mut enc_secs, mut dec_secs) = (0.0, 0.0);
    for p in &paths {
        let Ok(d) = std::fs::read(p) else { continue };
        let Some((json, start)) = aqr_asar::header(&d) else { continue };
        found += 1;
        let t = Instant::now();
        let f = formula::encode(&d, 1);
        enc_secs += t.elapsed().as_secs_f64();
        let Some(f) = f else {
            println!("not taken: {}", p.display());
            continue;
        };
        let Node::Apply(a) = &f.root else { unreachable!() };
        let ins: Vec<Vec<u8>> = a.inputs.iter().map(|n| eval(n, &f)).collect();
        let ins: Vec<&[u8]> = ins.iter().map(|i| &i[..]).collect();
        let t = Instant::now();
        let out = formula::decode(&[], &ins, d.len() as u64, 1);
        dec_secs += t.elapsed().as_secs_f64();
        if out.as_deref() != Ok(&d[..]) {
            failed += 1;
            println!("NOT REBUILT: {}", p.display());
            continue;
        }
        rebuilt += 1;
        bytes += d.len();
        let n = formula::files(&d).len();
        files += n;
        let (zh, zo) = (zstd(&d[..start]), zstd(ins[1]) + zstd(ins[2]));
        let holes = ins[1].iter().filter(|&&b| b == 0).count();
        (zhead, zours, head_bytes) = (zhead + zh, zours + zo, head_bytes + start);
        if verbose {
            println!(
                "{}: {} bytes, {n} files, header {} -> {} + {} raw, {holes} holes, zstd {zh} -> {zo}",
                p.display(),
                d.len(),
                json.len(),
                ins[1].len(),
                ins[2].len(),
            );
        }
    }
    println!("{found} archives, {rebuilt} rebuilt, {failed} failed; {bytes} bytes, {files} files");
    println!("headers {head_bytes} bytes: zstd -3 {zhead} -> {zours} (header with holes, index)");
    let mb = bytes as f64 / 1e6;
    println!("encode {:.1} MB/s, decode {:.1} MB/s", mb / enc_secs, mb / dec_secs);
}
