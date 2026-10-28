//! Repeats only encoding (`enc`) or only decoding (`dec`) of the given files, for profiling.
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args[0].clone();
    let iters: usize = args[1].parse().unwrap();
    let mut files = Vec::new();
    for a in &args[2..] {
        for e in std::fs::read_dir(a)
            .map(|r| r.flatten().map(|e| e.path()).collect::<Vec<_>>())
            .unwrap_or_else(|_| vec![a.into()])
        {
            files.push(std::fs::read(e).unwrap());
        }
    }
    let coded: Vec<_> = files.iter().map(|d| aqr_mp3::codec::encode(d)).collect();
    let tin: usize = files.iter().map(|d| d.len()).sum();
    let t = Instant::now();
    for _ in 0..iters {
        if mode == "enc" {
            for d in &files {
                std::hint::black_box(aqr_mp3::codec::encode(d));
            }
        } else {
            for c in &coded {
                std::hint::black_box(aqr_mp3::codec::decode(c, aqr_mp3::codec::tables()).unwrap());
            }
        }
    }
    println!("{mode}: {:.1} MB/s", (tin * iters) as f64 / t.elapsed().as_secs_f64() / 1e6);
}
