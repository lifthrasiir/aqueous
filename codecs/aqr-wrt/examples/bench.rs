//! usage: aqr-wrt-bench FILE...
//!
//! For each text file: its size by zstd (at `LEVEL`, by default 9; with `LONG` set, with long
//! distance matching and a window of 2^27) as it is and as the dictionary and the text coded
//! (each alone), the module's encoding and decoding speeds (of the file's bytes, on `THREADS`
//! threads, by default 1; text of Hangul decoded on as many), and that it decodes back.

use std::time::Instant;

fn zstd(d: &[u8], level: i32, long: bool) -> usize {
    let mut c = zstd::bulk::Compressor::new(level).unwrap();
    if long {
        c.long_distance_matching(true).unwrap();
        c.window_log(27).unwrap();
    }
    c.compress(d).unwrap().len()
}

fn main() {
    let level = std::env::var("LEVEL").map_or(9, |l| l.parse().unwrap());
    let long = std::env::var("LONG").is_ok();
    let threads = std::env::var("THREADS").map_or(1, |l| l.parse().unwrap());
    let (mut total, mut raw, mut packed) = (0usize, 0usize, 0usize);
    let (mut te, mut td) = (0f64, 0f64);
    for path in std::env::args().skip(1) {
        let d = std::fs::read(&path).unwrap();
        let t = Instant::now();
        let Some(f) = aqr_wrt::formula::encode(&d, threads) else {
            println!("{path}: not taken");
            continue;
        };
        let e = t.elapsed().as_secs_f64();
        let aqr_formula::Node::Apply(a) = &f.root else { unreachable!() };
        let inputs: Vec<&[u8]> = f.streams.iter().map(|s| &s.data[..]).collect();
        let dict = &a.resources[0];
        let t = Instant::now();
        let out = match a.module {
            aqr_wrt::formula::HANGUL => {
                aqr_wrt::formula::decode_hangul(&a.params, &[dict], &inputs, d.len() as u64, threads)
            }
            _ => aqr_wrt::formula::decode(&a.params, &[dict], &inputs, d.len() as u64, 1),
        }
        .unwrap();
        let dt = t.elapsed().as_secs_f64();
        assert!(out == d, "{path}: not given back");
        let before = zstd(&d, level, long);
        let sizes = [zstd(dict, level, long), zstd(&f.streams[0].data, level, long)];
        let after: usize = sizes.iter().sum();
        let mb = d.len() as f64 / 1e6;
        println!(
            "{path}: {mb:.1} MB, zstd {:.3} -> {:.3} MB ({:+.1}%), streams {:?} ({:.1} MB coded), encode {:.0} MB/s, decode {:.0} MB/s",
            before as f64 / 1e6,
            after as f64 / 1e6,
            (after as f64 / before as f64 - 1.0) * 100.0,
            ["dict", "text"].iter().zip(&sizes).collect::<Vec<_>>(),
            f.streams[0].data.len() as f64 / 1e6,
            mb / e,
            mb / dt
        );
        (total, raw, packed, te, td) = (total + d.len(), raw + before, packed + after, te + e, td + dt);
    }
    println!(
        "total: {:.1} MB, zstd {:.3} -> {:.3} MB ({:+.1}%), encode {:.0} MB/s, decode {:.0} MB/s (one thread)",
        total as f64 / 1e6,
        raw as f64 / 1e6,
        packed as f64 / 1e6,
        (packed as f64 / raw as f64 - 1.0) * 100.0,
        total as f64 / 1e6 / te,
        total as f64 / 1e6 / td
    );
}
