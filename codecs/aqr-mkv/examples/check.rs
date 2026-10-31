//! Matroska and WebM files encoded and given back: per file the bytes, the frames and those
//! seen before, the stream, the slices of the formula, and the time.
//!
//! cargo run -r -p aqr-mkv --example aqr-mkv-check -- FILE...

use std::time::Instant;

use aqr_formula::Node;

fn main() {
    let (mut all, mut stored) = (0usize, 0usize);
    for path in std::env::args().skip(1) {
        let d = std::fs::read(&path).unwrap();
        let frames = aqr_mkv::walk(&d).frames;
        let framed: usize = frames.iter().map(|f| f.span.len()).sum();
        if let Ok(dir) = std::env::var("AQR_WEBM_DUMP") {
            // (the frames as lines of start, end, track and flags, for experiments)
            let name = std::path::Path::new(&path).file_name().unwrap().to_string_lossy().into_owned();
            let lines: String = frames
                .iter()
                .map(|f| format!("{}\t{}\t{}\t{}\n", f.span.start, f.span.end, f.track, f.flags))
                .collect();
            std::fs::write(std::path::Path::new(&dir).join(name + ".tsv"), lines).unwrap();
        }
        let t = Instant::now();
        let f = aqr_mkv::formula::encode(&d, 1);
        let e = t.elapsed().as_secs_f64();
        let Some(f) = f else {
            println!("{path}: {} B, {} frames of {framed} B: not taken", d.len(), frames.len());
            (all, stored) = (all + d.len(), stored + d.len());
            continue;
        };
        let none = |_: &aqr_formula::Apply, _: &[&[u8]]| -> aqr_formula::Result<Vec<u8>> { unreachable!() };
        assert!(f.eval(&none).unwrap() == d, "{path}");
        let slices = match &f.root {
            Node::Concat(ns) => ns.len(),
            _ => 1,
        };
        let (rest, keys) = (f.streams[0].data.len(), f.streams[1].data.len());
        let s = rest + keys;
        println!(
            "{path}: {} B, {} frames of {framed} B -> {s} B ({:.2}%; keyframes {keys} B), {slices} slices; {:.0} MB/s",
            d.len(),
            frames.len(),
            100.0 * s as f64 / d.len() as f64,
            d.len() as f64 / e / 1e6
        );
        (all, stored) = (all + d.len(), stored + s);
    }
    println!("all: {all} -> {stored} B ({:.2}%)", 100.0 * stored as f64 / all.max(1) as f64);
}
