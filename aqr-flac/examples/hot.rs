//! Repeated compression (`c`) or decompression (`d`) of files, for profiling.
use aqr_flac::container;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let iters: usize = args[1].parse().unwrap();
    let data: Vec<Vec<u8>> = args[2..].iter().map(|p| std::fs::read(p).unwrap()).collect();
    let packed: Vec<Vec<u8>> = data.iter().map(|d| container::compress(d)).collect();
    let t = std::time::Instant::now();
    let mut n = 0;
    for _ in 0..iters {
        for (d, c) in data.iter().zip(&packed) {
            if args[0] == "c" {
                n += container::compress(d).len();
            } else {
                n += container::decompress(c).unwrap().len();
            }
        }
    }
    let tot: usize = data.iter().map(|d| d.len()).sum::<usize>() * iters;
    eprintln!("{n} {:.1} MB/s", tot as f64 / t.elapsed().as_secs_f64() / 1e6);
}
