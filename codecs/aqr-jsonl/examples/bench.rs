//! usage: aqr-jsonl-bench FILE...
//!
//! For each file: its streams as zstd -3 compresses them (those of a class one after another, as
//! the archive has them), against the file itself, the module's encoding and decoding speeds (of
//! the file's bytes), and that it decodes back (a segment at a time).

use std::time::Instant;

use aqr_formula::Node;

fn main() {
    let (mut total, mut packed, mut plain) = (0usize, 0usize, 0usize);
    let (mut te, mut td) = (0f64, 0f64);
    let z = |b: &[u8]| zstd::bulk::compress(b, 3).unwrap().len();
    for path in std::env::args().skip(1) {
        let d = std::fs::read(&path).unwrap();
        let t = Instant::now();
        let Some(f) = aqr_jsonl::formula::encode(&d, 1) else {
            // (why: no style, or too much kept as it is)
            let n = d.len().min(64 << 10);
            match aqr_jsonl::parse::style(&d[..n], n == d.len()) {
                None => println!("{path}: not taken (no style in its first 64 KiB)"),
                Some(st) => match aqr_jsonl::formula::encode_as(&d, st) {
                    Some((_, raw)) => println!("{path}: not taken ({raw} B of lines kept as they are)"),
                    None => println!("{path}: not taken (too many paths)"),
                },
            }
            continue;
        };
        let e = t.elapsed().as_secs_f64();
        let segments = match &f.root {
            Node::Concat(ns) => ns.iter().collect(),
            n => vec![n],
        };
        let t = Instant::now();
        let mut at = 0;
        for n in &segments {
            let Node::Apply(a) = n else { unreachable!() };
            let inputs: Vec<&[u8]> = a
                .inputs
                .iter()
                .map(|n| match n {
                    &Node::Slice { stream, start, len } => &f.streams[stream].data[start as usize..][..len as usize],
                    _ => unreachable!(),
                })
                .collect();
            let out = aqr_jsonl::formula::decode(&a.params, &inputs, a.len, 1).unwrap();
            assert!(d[at..].starts_with(&out), "{path}: not given back");
            at += out.len();
        }
        let dt = t.elapsed().as_secs_f64();
        assert!(at == d.len(), "{path}: not given back");
        let classes = aqr_jsonl::formula::STREAM_NAMES.map(|name| {
            let class = format!("jsonl/{name}");
            f.streams.iter().filter(|s| s.class == class).flat_map(|s| s.data.iter().copied()).collect::<Vec<u8>>()
        });
        let sizes: Vec<(usize, usize)> = classes.iter().map(|s| (s.len(), z(s))).collect();
        let sum: usize = sizes.iter().map(|s| s.1).sum();
        let base = z(&d);
        let mb = d.len() as f64 / 1e6;
        println!(
            "{path}: {mb:.2} MB, {} segments, zstd -3 {base} -> {sum} B ({:.3}), encode {:.0} MB/s, decode {:.0} MB/s",
            segments.len(),
            sum as f64 / base as f64,
            mb / e,
            mb / dt
        );
        for (name, (raw, c)) in aqr_jsonl::formula::STREAM_NAMES.iter().zip(&sizes) {
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
