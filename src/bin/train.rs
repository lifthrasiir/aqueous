//! Trains the built-in frequency tables from FLAC, WAV and AIFF files:
//! `train OUT FILE|DIR...` (every second file, or all
//! with `-a`), writing the model file read by `codec::tables`.

use flac_recons::{codec, model, pcm, rans};

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let all = if let Some(i) = args.iter().position(|a| a == "-a") {
        args.remove(i);
        true
    } else {
        false
    };
    let out = args.remove(0);
    let mut paths = Vec::new();
    fn walk(p: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        if p.is_dir() {
            let mut es: Vec<_> = std::fs::read_dir(p).unwrap().flatten().map(|e| e.path()).collect();
            es.sort();
            for e in es {
                walk(&e, out);
            }
        } else {
            out.push(p.to_path_buf());
        }
    }
    for a in &args {
        walk(std::path::Path::new(a), &mut paths);
    }
    let mut counts = vec![vec![0u64; model::N_TOK]; model::N_CTX];
    let mut n = 0;
    for p in paths.iter().step_by(if all { 1 } else { 2 }) {
        let d = std::fs::read(p).unwrap();
        codec::count(&d, &mut counts);
        pcm::count(&d, &mut counts);
        n += 1;
    }
    let freqs: Vec<Vec<u16>> = counts
        .iter()
        .map(|c| {
            let mut f = vec![0u16; c.len()];
            rans::normalize(c, &mut f);
            f
        })
        .collect();
    let bytes = codec::freqs_to_bytes(&freqs);
    std::fs::write(&out, &bytes).unwrap();
    let used = counts.iter().filter(|v| v.iter().any(|&x| x > 0)).count();
    eprintln!("{n} files, {used} of {} contexts used, model {} bytes", model::N_CTX, bytes.len());
}
