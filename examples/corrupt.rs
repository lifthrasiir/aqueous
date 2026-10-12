//! Times decompression of corrupted containers (byte flips every 97 bytes).
fn main() {
    for p in std::env::args().skip(1) {
        let d = std::fs::read(&p).unwrap();
        let c = flac_recons::compress(&d);
        for i in (5..c.len()).step_by(97) {
            let mut m = c.clone();
            m[i] ^= 0x5a;
            let t = std::time::Instant::now();
            let r = flac_recons::decompress(&m);
            let e = t.elapsed().as_secs_f64();
            if e > 0.05 {
                println!("{p} byte {i}: {:.2}s {:?}", e, r.err().map(|e| e.0));
            }
        }
    }
}
