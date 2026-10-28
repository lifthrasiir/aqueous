//! usage: aqr-xsv-bench FILE...
//!
//! For each table: its streams as zstd -3 compresses them, against the file itself, the
//! module's encoding and decoding speeds (of the file's bytes), and that it decodes back.

use std::time::Instant;

fn main() {
    let (mut total, mut packed, mut plain) = (0usize, 0usize, 0usize);
    let (mut te, mut td) = (0f64, 0f64);
    let z = |b: &[u8]| zstd::bulk::compress(b, 3).unwrap().len();
    for path in std::env::args().skip(1) {
        let d = std::fs::read(&path).unwrap();
        let t = Instant::now();
        let Some(f) = aqr_xsv::formula::encode(&d, 1) else {
            println!("{path}: not taken");
            continue;
        };
        let e = t.elapsed().as_secs_f64();
        let aqr_formula::Node::Apply(a) = &f.root else { unreachable!() };
        let inputs: Vec<&[u8]> = f.streams.iter().map(|s| &s.data[..]).collect();
        let t = Instant::now();
        let out = aqr_xsv::formula::decode(&a.params, &inputs, d.len() as u64, 1).unwrap();
        let dt = t.elapsed().as_secs_f64();
        assert!(out == d, "{path}: not given back");
        let sizes: Vec<(usize, usize)> = f.streams.iter().map(|s| (s.data.len(), z(&s.data))).collect();
        let sum: usize = sizes.iter().map(|s| s.1).sum();
        let base = z(&d);
        let mb = d.len() as f64 / 1e6;
        println!(
            "{path}: {mb:.2} MB, zstd -3 {base} -> {sum} B ({:.3}), encode {:.0} MB/s, decode {:.0} MB/s",
            sum as f64 / base as f64,
            mb / e,
            mb / dt
        );
        for (name, (raw, c)) in aqr_xsv::formula::STREAM_NAMES.iter().zip(&sizes) {
            println!("  {name:8} {raw:10} -> {c:9}");
        }
        (total, packed, plain, te, td) = (total + d.len(), packed + sum, plain + base, te + e, td + dt);
    }
    println!(
        "total: {:.2} MB, zstd -3 {plain} -> {packed} B, encode {:.0} MB/s, decode {:.0} MB/s (one thread)",
        total as f64 / 1e6,
        total as f64 / 1e6 / te,
        total as f64 / 1e6 / td
    );
}
