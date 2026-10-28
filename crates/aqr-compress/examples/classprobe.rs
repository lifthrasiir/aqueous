//! How much the streams of a class that the formula of a file holds would archive smaller
//! together than apart (research tool): as the archive encoder compresses each stream on its
//! own, while those of a class are alike (the same recompressor's, of files of a container).
//!
//! usage: aqr-compress-classprobe [-t THREADS] FILE...
//!
//! For each file, its formula's streams of each class (but those of `Kind::Compressed`, stored
//! as they are), at zstd -3 each, and one after another at zstd -3 with long-distance matching
//! over a window of all of them; the classes of at least two streams that gain, and the totals.

use std::collections::BTreeMap;

use aqr_compress::{Options, recompress};
use aqr_formula::Kind;

fn zstd(d: &[u8], long: bool) -> usize {
    let mut c = zstd::bulk::Compressor::new(3).unwrap();
    if long {
        let window = (64 - (d.len().max(2) as u64 - 1).leading_zeros()).clamp(10, 30);
        c.set_parameter(zstd::zstd_safe::CParameter::WindowLog(window)).unwrap();
        c.set_parameter(zstd::zstd_safe::CParameter::EnableLongDistanceMatching(true)).unwrap();
    }
    c.compress(d).unwrap().len()
}

fn main() {
    let mut opt = Options::default();
    let mut files = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match &*arg {
            "-t" => opt.threads = args.next().and_then(|s| s.parse().ok()).expect("-t THREADS"),
            _ => files.push(arg),
        }
    }
    let (mut all_apart, mut all_together, mut all_streams) = (0usize, 0usize, 0usize);
    for path in files {
        let Ok(d) = std::fs::read(&path) else { continue };
        let f = recompress(&d, &opt);
        let mut classes: BTreeMap<&str, Vec<&[u8]>> = BTreeMap::new();
        for s in f.streams.iter().filter(|s| s.kind != Kind::Compressed && !s.data.is_empty()) {
            classes.entry(&s.class).or_default().push(&s.data);
        }
        let (mut apart, mut together, mut lines) = (0, 0, Vec::new());
        for (class, ds) in &classes {
            let a: usize = ds.iter().map(|s| zstd(s, false)).sum();
            let t = if ds.len() > 1 { zstd(&ds.concat(), true) } else { a };
            if ds.len() > 1 && t < a * 99 / 100 {
                lines.push(format!(
                    "  {class}: {} streams, {} B, {a} -> {t} ({:+.1}%)",
                    ds.len(),
                    ds.iter().map(|s| s.len()).sum::<usize>(),
                    100.0 * (t as f64 / a as f64 - 1.0)
                ));
            }
            apart += a;
            together += t.min(a);
        }
        println!(
            "{path}: {} B, streams {} B compressed apart, {} together ({:+.1}%)",
            d.len(),
            apart,
            together,
            100.0 * (together as f64 / apart.max(1) as f64 - 1.0)
        );
        lines.iter().for_each(|l| println!("{l}"));
        (all_apart, all_together, all_streams) =
            (all_apart + apart, all_together + together, all_streams + f.streams.len());
    }
    println!(
        "all: {all_streams} streams, {all_apart} B apart, {all_together} together ({:+.1}%)",
        100.0 * (all_together as f64 / all_apart.max(1) as f64 - 1.0)
    );
}
