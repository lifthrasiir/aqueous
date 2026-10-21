//! Container overhead: codec alone vs container, both directions.
use std::time::Instant;
fn main() {
    let mut files = Vec::new();
    for a in std::env::args().skip(1) {
        for e in std::fs::read_dir(&a).unwrap().flatten() {
            files.push(std::fs::read(e.path()).unwrap());
        }
    }
    let tin: usize = files.iter().map(|d| d.len()).sum();
    let _ = aqr_mp3::codec::tables();
    let mb = |t: Instant| tin as f64 / t.elapsed().as_secs_f64() / 1e6;
    for _ in 0..2 {
        let t = Instant::now();
        let coded: Vec<_> = files.iter().map(|d| aqr_mp3::codec::encode(d)).collect();
        println!("codec::encode     {:.1} MB/s", mb(t));
        let t = Instant::now();
        let packed: Vec<_> = files.iter().map(|d| aqr_mp3::compress(d)).collect();
        println!("container compress {:.1} MB/s", mb(t));
        let t = Instant::now();
        for c in &coded {
            std::hint::black_box(aqr_mp3::codec::decode(c).unwrap());
        }
        println!("codec::decode     {:.1} MB/s", mb(t));
        let t = Instant::now();
        for c in &packed {
            std::hint::black_box(aqr_mp3::decompress(c).unwrap());
        }
        println!("container decompress {:.1} MB/s", mb(t));
        let t = Instant::now();
        for d in &files {
            std::hint::black_box(xxhash_rust::xxh3::xxh3_64(d));
        }
        println!("xxh3 {:.1} MB/s", mb(t));
    }
}
