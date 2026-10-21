// Cost of recording and entropy coding symbols, apart from the model (profiling aid).
use aqr_psd::model::{Coder, Recorder, TABLES};
use std::time::Instant;

fn main() {
    let n = 40_000_000usize;
    let mut seed = 1u64;
    let mut rnd = || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) as u32
    };
    let syms: Vec<(usize, u8)> = (0..n)
        .map(|_| {
            let t = (rnd() % 64) as usize * (TABLES / 64);
            let mut s = 0u8;
            while s < 40 && rnd() % 100 < 60 {
                s += 1;
            }
            (t, s)
        })
        .collect();
    for _ in 0..3 {
        let mut out = Vec::new();
        let t = Instant::now();
        let mut rec = Recorder::new(&mut out);
        for &(tt, s) in &syms {
            std::hint::black_box(rec.code(tt, s));
        }
        rec.finish();
        println!("record + code: {:.2} ns/sym, {} bytes", t.elapsed().as_secs_f64() * 1e9 / n as f64, out.len());
    }
}
