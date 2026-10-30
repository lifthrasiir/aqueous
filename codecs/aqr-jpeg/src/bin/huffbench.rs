//! Huffman-decode throughput benchmark and correctness check against a coefdump dump.
//! usage: `aqr-jpeg-huffbench file.jpg [coef.bin] [iters]`

use aqr_jpeg::jpeg::{self, Clean, ZIGZAG};
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let data = std::fs::read(&args[1]).unwrap();
    let iters: usize = args.get(3).map(|s| s.parse().unwrap()).unwrap_or(20);
    let j = jpeg::parse(&data).unwrap();
    let mut clean = Clean::default();
    let mut coefs = Vec::new();
    jpeg::decode_all(&j, &data, jpeg::Order::Natural, &mut coefs, &mut clean).unwrap();
    let slow = jpeg::SLOW_CALLS.with(|c| c.get());

    if let Some(p) = args.get(2).filter(|s| !s.is_empty()) {
        let b = std::fs::read(p).unwrap();
        let rd = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap()) as usize;
        let nc = rd(0);
        assert_eq!(nc, coefs.len());
        let mut off = 4 + nc * (16 + 128);
        for (ci, c) in coefs.iter().enumerate() {
            let (w, h) = (rd(4 + ci * 144), rd(8 + ci * 144));
            assert_eq!((w, h), (j.comps[ci].wib, j.comps[ci].hib), "block dims");
            for y in 0..h {
                for x in 0..w {
                    let blk = &c.blocks[y * c.bw + x];
                    for z in 0..64 {
                        let o = off + ((y * w + x) * 64 + ZIGZAG[z]) * 2;
                        let r = i16::from_le_bytes([b[o], b[o + 1]]);
                        assert_eq!(blk[ZIGZAG[z]], r, "comp {ci} block ({x},{y}) zz {z}");
                    }
                    let m = (0..64).fold(0u64, |m, k| m | ((blk[ZIGZAG[k]] != 0) as u64) << k);
                    assert_eq!(m, c.masks[y * c.bw + x]);
                }
            }
            off += w * h * 128;
        }
    }

    let t0 = Instant::now();
    for s in &j.scans {
        for _ in 0..iters {
            jpeg::destuff(&data[s.start..s.end], &mut clean);
        }
    }
    let td = t0.elapsed().as_secs_f64() / iters as f64;
    // destuff + decode, buffers reused
    let mut reuse = Vec::new();
    let t2 = Instant::now();
    for _ in 0..iters {
        jpeg::decode_all(&j, &data, jpeg::Order::Natural, &mut reuse, &mut clean).unwrap();
        std::hint::black_box(&reuse);
    }
    let tr = t2.elapsed().as_secs_f64() / iters as f64;
    let th = tr - td;
    let n = data.len() as f64;
    let nblk: usize = coefs.iter().map(|c| c.blocks.len()).sum();
    // decoded symbols ~= nonzero coefs + one EOB/DC per block (ZRLs ignored)
    let nsym: u64 = coefs.iter().flat_map(|c| c.masks.iter()).map(|m| (m & !1).count_ones() as u64 + 2).sum();
    println!(
        "{:<24} {:5.2}MB ri {:5} | destuff {:6.0} MB/s  decode {:6.1} MB/s  total {:6.1} MB/s  ({:.1} ns/blk, {:.2} sym/B, {:.2} ns/sym)",
        std::path::Path::new(&args[1]).file_name().unwrap().to_string_lossy(),
        n / 1e6,
        j.scans.iter().map(|s| s.ri).max().unwrap_or(0),
        n / td / 1e6,
        n / th / 1e6,
        n / (td + th) / 1e6,
        th * 1e9 / nblk as f64,
        nsym as f64 / n,
        th * 1e9 / nsym as f64
    );
    println!(
        "    slow-path fraction {:.1}%; reused buffers: decode {:.1} MB/s ({:.2} ns/sym)",
        slow as f64 * 100.0 / nsym as f64,
        n / tr / 1e6,
        tr * 1e9 / nsym as f64
    );
}
