//! A database's zstd blobs through `zstd-frames`: `dbframes FILE [THREADS]` encodes, tells the
//! streams and the params, decodes by the module natively and compares.

use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let f = std::fs::read(&args[0]).unwrap();
    let threads = args.get(1).map_or(16, |t| t.parse().unwrap());
    let t = Instant::now();
    let blobs = aqr_sqlite::blobs::blobs(&f, 8, &|h| h.starts_with(&aqr_zstd::MAGIC)).unwrap();
    let frames: Vec<_> = blobs.into_iter().map(|b| b.pieces).collect();
    let t1 = t.elapsed().as_secs_f64();
    let fm = aqr_zstd::formula::frames_in(&f, &frames, 3, aqr_formula::Kind::Table, threads).unwrap();
    let t2 = t.elapsed().as_secs_f64();
    for s in &fm.streams {
        println!("{}: {} bytes", s.class, s.data.len());
    }
    let aqr_formula::Node::Apply(a) = &fm.root else { panic!() };
    println!("params {:?}; found {t1:.2} s, encoded {:.2} s", aqr_zstd::frames::Params::parse(&a.params), t2 - t1);
    let ins: Vec<&[u8]> = fm.streams[1..].iter().map(|s| &s.data[..]).collect();
    let t = Instant::now();
    let out = aqr_zstd::frames::decode(&a.params, &ins, f.len() as u64, threads).unwrap();
    println!("decoded {:.2} s, the same: {}", t.elapsed().as_secs_f64(), out == f);
}
