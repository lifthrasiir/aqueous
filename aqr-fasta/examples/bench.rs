//! usage: aqr-fasta-bench FILE...
//!
//! For each FASTA file: its streams (the bases coded, the others as zstd -3 compresses them),
//! the module's encoding and decoding speeds (of the file's bytes), and that it decodes back.

use std::time::Instant;

fn main() {
    let (mut total, mut packed) = (0usize, 0usize);
    let (mut te, mut td) = (0f64, 0f64);
    for path in std::env::args().skip(1) {
        let d = std::fs::read(&path).unwrap();
        if std::env::var("PHASES").is_ok() {
            let t = Instant::now();
            let p = aqr_fasta::fasta::parse(&d).unwrap();
            let tp = t.elapsed().as_secs_f64();
            let t = Instant::now();
            let c = aqr_fasta::bases::encode(&p.bases);
            let tb = t.elapsed().as_secs_f64();
            println!("{path}: parse {tp:.2} s, bases {tb:.2} s ({} B)", c.len());
        }
        let t = Instant::now();
        let Some(f) = aqr_fasta::formula::encode(&d, 1) else {
            println!("{path}: not taken");
            continue;
        };
        let e = t.elapsed().as_secs_f64();
        let aqr_formula::Node::Apply(a) = &f.root else { unreachable!() };
        let inputs: Vec<&[u8]> = f.streams.iter().map(|s| &s.data[..]).collect();
        let t = Instant::now();
        let out = aqr_fasta::formula::decode(&a.params, &inputs, d.len() as u64, 1).unwrap();
        let dt = t.elapsed().as_secs_f64();
        assert!(out == d, "{path}: not given back");
        let sizes: Vec<usize> =
            f.streams
                .iter()
                .map(|s| {
                    if s.class == "fasta/bases" {
                        s.data.len()
                    } else {
                        zstd::bulk::compress(&s.data, 3).unwrap().len()
                    }
                })
                .collect();
        let sum: usize = sizes.iter().sum();
        let mb = d.len() as f64 / 1e6;
        println!(
            "{path}: {mb:.1} MB -> {:.2} MB ({:.4}), streams {:?}, encode {:.0} MB/s, decode {:.0} MB/s",
            sum as f64 / 1e6,
            sum as f64 / d.len() as f64,
            aqr_fasta::formula::STREAM_NAMES.iter().zip(&sizes).collect::<Vec<_>>(),
            mb / e,
            mb / dt
        );
        (total, packed, te, td) = (total + d.len(), packed + sum, te + e, td + dt);
    }
    println!(
        "total: {:.1} MB -> {:.2} MB, encode {:.0} MB/s, decode {:.0} MB/s (one thread)",
        total as f64 / 1e6,
        packed as f64 / 1e6,
        total as f64 / 1e6 / te,
        total as f64 / 1e6 / td
    );
}
