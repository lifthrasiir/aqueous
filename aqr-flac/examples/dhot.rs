//! Repeated decompression of one file (profiling without compression in the profile).
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let iters: usize = args[0].parse().unwrap();
    let c = aqr_flac::compress(&std::fs::read(&args[1]).unwrap());
    let t = std::time::Instant::now();
    let mut n = 0;
    for _ in 0..iters {
        n += aqr_flac::decompress(&c).unwrap().len();
    }
    eprintln!("{:.1} MB/s", n as f64 / t.elapsed().as_secs_f64() / 1e6);
}
