fn main() {
    let t = std::time::Instant::now();
    let f = mp3_recons::codec::builtin_freqs_pub().unwrap();
    println!("load freqs: {:.1} ms", t.elapsed().as_secs_f64() * 1e3);
    let t = std::time::Instant::now();
    let tb = mp3_recons::rans::Tables::new(&f);
    println!("Tables::new: {:.1} ms", t.elapsed().as_secs_f64() * 1e3);
    std::hint::black_box(tb);
    let n: usize = f.iter().map(|v| v.len()).sum();
    println!("{} contexts, {} symbols", f.len(), n);
}
