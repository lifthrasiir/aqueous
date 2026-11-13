//! The ADPCM waves of XACT wave banks (`.xwb`, version 42 on, not compact) coded: the record's
//! bytes against the blocks', zstd -19 of the blocks, each wave given back, and the speeds.
//!
//! ```sh
//! cargo run --release -p aqr-msadpcm --example aqr-msadpcm-check -- FILE.xwb...
//! ```

use aqr_msadpcm::codec::{self, Layout};

fn u32le(d: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(d[at..at + 4].try_into().unwrap())
}

fn main() {
    let (mut blocks, mut record, mut zstd, mut secs, mut back) = (0usize, 0usize, 0usize, 0.0, 0.0);
    for p in std::env::args().skip(1) {
        let d = std::fs::read(&p).unwrap();
        let seg = |k: usize| (u32le(&d, 12 + 8 * k) as usize, u32le(&d, 16 + 8 * k) as usize);
        let (bank, meta, wave) = (seg(0).0, seg(1).0, seg(4).0);
        let (n, size) = (u32le(&d, bank + 4) as usize, u32le(&d, bank + 72) as usize);
        let (mut b, mut r) = (0, 0);
        for i in 0..n {
            let e = meta + i * size;
            let (format, offset, len) = (u32le(&d, e + 4), u32le(&d, e + 8) as usize, u32le(&d, e + 12) as usize);
            let (channels, align) = ((format >> 2 & 7) as usize, (format >> 23 & 0xff) as usize);
            if format & 3 != 2 {
                continue;
            }
            let Some(l) = Layout::new(channels, (align + 22) * channels) else { continue };
            let w = &d[wave + offset..wave + offset + len];
            let t = std::time::Instant::now();
            let (used, rec) = codec::encode(l, w);
            secs += t.elapsed().as_secs_f64();
            let t = std::time::Instant::now();
            assert!(codec::decode(l, &rec, used).unwrap() == w[..used], "{p}: wave {i} not given back");
            back += t.elapsed().as_secs_f64();
            zstd += zstd::bulk::compress(&w[..used], 19).unwrap().len();
            (b, r) = (b + used, r + rec.len());
        }
        println!("{p}: {:.2} MB of blocks, record {:.3}", b as f64 / 1e6, r as f64 / b.max(1) as f64);
        (blocks, record) = (blocks + b, record + r);
    }
    println!(
        "all: {:.2} MB, record {:.2} MB ({:.3}), zstd -19 {:.3}; encode {:.1} MB/s, decode {:.1} MB/s",
        blocks as f64 / 1e6,
        record as f64 / 1e6,
        record as f64 / blocks.max(1) as f64,
        zstd as f64 / blocks.max(1) as f64,
        blocks as f64 / 1e6 / secs,
        blocks as f64 / 1e6 / back
    );
}
