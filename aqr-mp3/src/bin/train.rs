//! Trains the built-in frequency tables: `mp3r-train OUT FILE|DIR...` (every second file, or all
//! with `-a`), writing the model file read by `codec::tables`.

use aqr_mp3::codec;
use aqr_mp3::model::{self, Counter};
use aqr_mp3::rans;

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
    let mut c = Counter::new();
    let mut n = 0;
    for p in paths.iter().step_by(if all { 1 } else { 2 }) {
        let d = std::fs::read(p).unwrap();
        codec::count(&d, &mut c);
        n += 1;
    }
    // Each context's counts are smoothed towards its family's distribution.
    let strength: f64 = std::env::var("PRIOR").map(|v| v.parse().unwrap()).unwrap_or(0.0);
    let mut fam: std::collections::HashMap<usize, Vec<f64>> = Default::default();
    for ctx in 0..model::N_CTX {
        let e = fam.entry(model::family(ctx)).or_insert_with(|| vec![0.0; c.counts[ctx].len()]);
        for (a, &n) in e.iter_mut().zip(&c.counts[ctx]) {
            *a += n as f64;
        }
    }
    let freqs: Vec<Vec<u16>> = (0..model::N_CTX)
        .map(|ctx| {
            let parent = &fam[&model::family(ctx)];
            let ptot: f64 = parent.iter().sum::<f64>().max(1.0);
            let k = strength * c.counts[ctx].len() as f64;
            let smoothed: Vec<u64> =
                c.counts[ctx].iter().zip(parent).map(|(&n, &p)| ((n as f64 + k * p / ptot) * 1024.0) as u64).collect();
            let mut f = vec![0u16; c.counts[ctx].len()];
            rans::normalize(&smoothed, &mut f);
            f
        })
        .collect();
    let bytes = codec::freqs_to_bytes(&freqs);
    std::fs::write(&out, &bytes).unwrap();
    let used = c.counts.iter().filter(|v| v.iter().any(|&x| x > 0)).count();
    eprintln!("{n} files, {used} of {} contexts used, model {} bytes", model::N_CTX, bytes.len());
}
