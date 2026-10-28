//! rANS decoding throughput alone (research tool).
use aqr_png::rans::{self, BitW, Decoder, Tables};
use std::time::Instant;

fn main() {
    let n = 30_000_000usize;
    let nt = 96;
    let mut seed = 1u64;
    let mut rnd = || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) as u32
    };
    // Laplacian-ish residual symbols (zigzagged), contexts depending on the previous symbol.
    let mut syms = Vec::with_capacity(n);
    let mut counts = vec![vec![0u32; 256]; nt];
    let mut prev = 0u32;
    for _ in 0..n {
        let t = ((prev * 7) % nt as u32) as usize;
        let mut s = 0;
        while s < 255 && rnd() % 100 < 70 {
            s += 1;
        }
        counts[t][s as usize] += 1;
        syms.push((t as u8, s as u8));
        prev = s;
    }
    let mut w = BitW::default();
    let freqs: Vec<Vec<u32>> = counts
        .iter()
        .map(|c| if c.iter().any(|&x| x > 0) { rans::write_table(&mut w, c) } else { vec![rans::PROB_SCALE] })
        .collect();
    let t0 = Instant::now();
    let bytes = rans::encode(&syms, &freqs);
    println!("encode: {:.2} ns/sym", t0.elapsed().as_secs_f64() * 1e9 / n as f64);
    let tables = Tables::new(&freqs);
    for dep in [false, true] {
        let mut best = f64::MAX;
        for _ in 0..3 {
            let t = Instant::now();
            let mut d = Decoder::new(&bytes);
            let mut prev = 0u32;
            let mut acc = 0u32;
            for (i, &(tt, _)) in syms.iter().enumerate() {
                let t = if dep { ((prev * 7) % nt as u32) as usize } else { tt as usize };
                let s = d.get(&tables, t);
                acc = acc.wrapping_add(s);
                prev = s;
                let _ = i;
            }
            std::hint::black_box(acc);
            best = best.min(t.elapsed().as_secs_f64());
        }
        println!("decode (context from previous symbol: {dep}): {:.2} ns/sym", best * 1e9 / n as f64);
    }
}
