//! usage: [SEGMENT=BYTES] aqr-log-bench FILE...
//!
//! For each table: its streams as zstd -3 compresses them, against the file itself, the
//! module's encoding (in segments of SEGMENT bytes if given, else of `formula::SEGMENT`) and decoding speeds (of the file's bytes), and that it decodes back.

use std::time::Instant;

fn main() {
    let (mut total, mut packed, mut plain) = (0usize, 0usize, 0usize);
    let (mut te, mut td) = (0f64, 0f64);
    let z = |b: &[u8]| zstd::bulk::compress(b, 3).unwrap().len();
    for path in std::env::args().skip(1) {
        let d = std::fs::read(&path).unwrap();
        let t = Instant::now();
        let segment = std::env::var("SEGMENT").ok().and_then(|s| s.parse().ok()).unwrap_or(aqr_log::formula::SEGMENT);
        let Some(f) = aqr_log::formula::encode_in(&d, segment, true, 1) else {
            println!("{path}: not taken");
            continue;
        };
        let e = t.elapsed().as_secs_f64();
        let apps = match &f.root {
            aqr_formula::Node::Concat(ns) => ns.iter().collect(),
            n => vec![n],
        };
        let t = Instant::now();
        let mut out = Vec::new();
        for n in &apps {
            let aqr_formula::Node::Apply(a) = n else { unreachable!() };
            let inputs: Vec<&[u8]> = a
                .inputs
                .iter()
                .map(|n| match n {
                    &aqr_formula::Node::Slice { stream, start, len } => {
                        &f.streams[stream].data[start as usize..][..len as usize]
                    }
                    _ => unreachable!(),
                })
                .collect();
            out.extend(aqr_log::formula::decode(&a.params, &inputs, a.len, 1).unwrap());
        }
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
        for (name, (raw, c)) in aqr_log::formula::STREAM_NAMES.iter().zip(&sizes) {
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
