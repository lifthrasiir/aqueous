//! `aqr-inno-check FILE`: the table of the installer, its streams split, rebuilt and timed.

use std::time::Instant;

fn main() {
    let path = std::env::args().nth(1).expect("usage: aqr-inno-check FILE");
    let d = std::fs::read(&path).expect("read");
    println!("{:?}", aqr_inno::table(&d));
    let t = Instant::now();
    let pieces = aqr_inno::split(&d, std::thread::available_parallelism().map_or(1, |n| n.get()));
    println!("{} streams split in {:.2?}", pieces.len(), t.elapsed());
    for p in &pieces {
        let t = Instant::now();
        let back = aqr_lzma::recons::join(&p.data, &p.record, 1);
        let ok = back.as_deref() == Some(&d[p.range.clone()]);
        println!(
            "{:?}: {} -> data {} + record {}, rebuilt {} in {:.2?}",
            p.range,
            p.range.len(),
            p.data.len(),
            p.record.len(),
            ok,
            t.elapsed()
        );
    }
}
