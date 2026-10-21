//! Parse / write round trip of every frame, with timing.
use aqr_flac::{bits, flac};
use std::time::Instant;

fn main() {
    let (mut tin, mut tp, mut tw, mut bad, mut junk) = (0usize, 0f64, 0f64, 0usize, 0usize);
    for path in std::env::args().skip(1) {
        let d = std::fs::read(&path).unwrap();
        let Some((si, start)) = flac::parse_metadata(&d) else {
            println!("{path}: no metadata");
            continue;
        };
        let pd = bits::padded(&d);
        let mut frames = Vec::new();
        let t = Instant::now();
        let mut p = start;
        while p < d.len() {
            let mut f = flac::Frame::default();
            match flac::parse_frame(&pd, p, d.len(), si.bps, &mut f) {
                Some(e) => {
                    frames.push((p, e, f));
                    p = e;
                }
                None => {
                    junk += 1;
                    p += 1;
                }
            }
        }
        tp += t.elapsed().as_secs_f64();
        let t = Instant::now();
        let mut out = Vec::with_capacity(d.len());
        for (_, _, f) in &frames {
            flac::write_frame(f, &mut out, usize::MAX);
        }
        tw += t.elapsed().as_secs_f64();
        for (s, e, f) in &frames {
            let mut o = Vec::new();
            flac::write_frame(f, &mut o, usize::MAX);
            if o != d[*s..*e] {
                let i = o.iter().zip(&d[*s..*e]).position(|(a, b)| a != b);
                println!("{} vs {} first diff {i:?}", o.len(), e - s);
                bad += 1;
                println!("{path}: frame at {s} differs");
            }
        }
        tin += d.len();
    }
    println!(
        "{tin} bytes, parse {:.1} MB/s, write {:.1} MB/s, {bad} bad, {junk} junk bytes",
        tin as f64 / tp / 1e6,
        tin as f64 / tw / 1e6
    );
}
