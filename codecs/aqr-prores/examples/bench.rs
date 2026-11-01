//! Codes the ProRes frames of files in chunks and decodes them again: the frames' bytes, what
//! they were coded to, and the thread seconds of encoding and decoding (each chunk on one
//! thread, the chunks in parallel). Frames are checked byte for byte.
//!
//! ```sh
//! cargo run --release -p aqr-prores --example aqr-prores-bench -- [-c CHUNK_MB] [-n MAX_FRAMES] FILE_OR_DIR...
//! ```

use std::path::{Path, PathBuf};
use std::time::Instant;

use aqr_prores::codec;
use rayon::prelude::*;

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
    let mut paths = Vec::new();
    let (mut chunk_mb, mut max_frames) = (32usize, usize::MAX);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-c" => chunk_mb = args.next().unwrap().parse().unwrap(),
            "-n" => max_frames = args.next().unwrap().parse().unwrap(),
            _ => walk(Path::new(&a), &mut paths),
        }
    }
    let (mut tin, mut tout, mut tenc, mut tdec, mut bad) = (0usize, 0usize, 0f64, 0f64, 0usize);
    for p in paths {
        let Ok(file) = std::fs::File::open(&p) else { continue };
        // SAFETY: the corpus is not written while it is read.
        let Ok(d) = (unsafe { memmap2::Mmap::map(&file) }) else { continue };
        let at = aqr_prores::find(&d);
        if at.is_empty() {
            continue;
        }
        let frames: Vec<&[u8]> = at
            .iter()
            .take(max_frames)
            .map(|&a| &d[a..a + u32::from_be_bytes(d[a..a + 4].try_into().unwrap()) as usize])
            .collect();
        // Chunks of at least chunk_mb.
        let mut chunks: Vec<Vec<&[u8]>> = vec![Vec::new()];
        let mut acc = 0;
        for f in frames {
            if acc >= chunk_mb << 20 {
                chunks.push(Vec::new());
                acc = 0;
            }
            acc += f.len();
            chunks.last_mut().unwrap().push(f);
        }
        let res: Vec<(usize, usize, f64, f64, bool)> = chunks
            .par_iter()
            .map(|ch| {
                let t = Instant::now();
                let e = codec::encode(ch);
                let te = t.elapsed().as_secs_f64();
                let t = Instant::now();
                let back = codec::decode(&e);
                let td = t.elapsed().as_secs_f64();
                let ok = back.is_ok_and(|b| b.len() == ch.len() && b.iter().zip(ch).all(|(x, y)| x == y));
                (ch.iter().map(|f| f.len()).sum(), e.len(), te, td, ok)
            })
            .collect();
        let (i, o): (usize, usize) = (res.iter().map(|r| r.0).sum(), res.iter().map(|r| r.1).sum());
        let (te, td): (f64, f64) = (res.iter().map(|r| r.2).sum(), res.iter().map(|r| r.3).sum());
        let nbad = res.iter().filter(|r| !r.4).count();
        println!(
            "{}: {} chunks, {:.1} MB -> {:.4}, encode {:.1} MB/s, decode {:.1} MB/s per thread{}",
            p.file_name().unwrap().to_string_lossy(),
            res.len(),
            i as f64 / 1e6,
            o as f64 / i as f64,
            i as f64 / 1e6 / te,
            i as f64 / 1e6 / td,
            if nbad > 0 { format!(", {nbad} CHUNKS FAILED") } else { String::new() }
        );
        (tin, tout, tenc, tdec, bad) = (tin + i, tout + o, tenc + te, tdec + td, bad + nbad);
    }
    println!(
        "all: {:.1} MB -> {:.4}, encode {:.1} MB/s, decode {:.1} MB/s per thread, {bad} chunks failed",
        tin as f64 / 1e6,
        tout as f64 / tin as f64,
        tin as f64 / 1e6 / tenc,
        tin as f64 / 1e6 / tdec
    );
}
