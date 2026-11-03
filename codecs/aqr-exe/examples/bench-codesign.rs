//! The code signatures of Mach-O files (`codesign`): of each file, its signatures, their code
//! slots derived, the signatures compressed (zstd -19) as they are and with those slots zeroed,
//! and the speed of decoding the file back (hashing its pages).
//!
//! usage: aqr-exe-bench-codesign FILE...

use std::time::Instant;

use aqr_exe::codesign;
use aqr_exe::format::macho;

fn zstd(d: &[u8]) -> usize {
    zstd::bulk::compress(d, 19).unwrap().len()
}

fn main() {
    let (mut before, mut after) = (0, 0);
    for path in std::env::args().skip(1) {
        let d = std::fs::read(&path).unwrap();
        let Some(images) = macho::images(&d) else { continue };
        let sigs: Vec<(usize, usize, usize)> =
            images.iter().filter_map(|i| i.signature.map(|(at, n)| (i.range.start, at, n))).collect();
        let Some(s) = codesign::encode(&d, &sigs) else {
            println!("{path}: {} signatures, none derived", sigs.len());
            continue;
        };
        let mut body = Vec::with_capacity(d.len());
        let mut at = 0;
        for r in &s.ranges {
            body.extend_from_slice(&d[at..r.start]);
            at = r.end;
        }
        body.extend_from_slice(&d[at..]);
        let t = Instant::now();
        let back = codesign::decode(&s.params, &body, &s.sigs, d.len());
        let secs = t.elapsed().as_secs_f64();
        assert!(back.as_deref() == Some(&d[..]), "{path}: decoded back");
        let orig: Vec<u8> = s.ranges.iter().flat_map(|r| d[r.clone()].iter().copied()).collect();
        let (a, b) = (zstd(&orig), zstd(&s.sigs));
        (before, after) = (before + a, after + b);
        println!(
            "{path}: {} of {} signatures, {} bytes: {a} -> {b} compressed; decoded at {:.0} MB/s",
            s.ranges.len(),
            sigs.len(),
            orig.len(),
            d.len() as f64 / secs / 1e6
        );
    }
    println!("all: {:.3} -> {:.3} MB", before as f64 / 1e6, after as f64 / 1e6);
}
