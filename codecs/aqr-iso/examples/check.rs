//! What aqr-iso finds in images (through aqr-disk, of all of an image and its partitions): their files and the bytes left as the rest, whether the
//! formula gives each image back, and how fast it is made (`-v` lists the files, `-z` adds zstd
//! -3 of the rest).
//!
//! ```sh
//! cargo run --release -p aqr-iso --example aqr-iso-check -- [-v] [-z] IMAGE...
//! ```

use std::time::Instant;

fn main() {
    let (mut paths, mut verbose, mut zstd) = (Vec::new(), false, false);
    for a in std::env::args().skip(1) {
        match &a[..] {
            "-v" => verbose = true,
            "-z" => zstd = true,
            _ => paths.push(a),
        }
    }
    for p in &paths {
        let d = std::fs::read(p).unwrap();
        let start = Instant::now();
        let files = aqr_disk::files(&d, &[aqr_iso::FS]);
        let pieces = aqr_disk::pieces(&d, &files);
        let secs = start.elapsed().as_secs_f64();
        let by_rank = |r: u8| files.iter().filter(|f| f.rank == r).count();
        let taken: Vec<_> = pieces.iter().filter(|p| p.file.is_some()).collect();
        let rest: usize = pieces.iter().filter(|p| p.file.is_none()).map(|p| p.span.len()).sum();
        println!(
            "{p}: {} bytes, files UDF {} Joliet {} ISO 9660 {}; {} extents taken ({} bytes), rest {rest} bytes; {secs:.3} s",
            d.len(),
            by_rank(0),
            by_rank(1),
            by_rank(2),
            taken.len(),
            d.len() - rest,
        );
        if verbose {
            for t in &taken {
                let f = &files[t.file.unwrap().0];
                println!("  {:>12} {:>12}  {}", t.span.start, t.span.len(), String::from_utf8_lossy(&f.name));
            }
        }
        let f = aqr_disk::formula::build(&d, &files, &pieces, &Default::default());
        let none = |_: &aqr_formula::Apply, _: &[&[u8]]| -> aqr_formula::Result<Vec<u8>> { unreachable!("no module") };
        let out = f.eval(&none).unwrap();
        let rebuilt = out == d;
        drop(out);
        println!("  rebuilt: {rebuilt}");
        if zstd {
            let rest = &f.streams[1].data;
            println!("  rest {} -> zstd -3 {}", rest.len(), zstd::bulk::compress(rest, 3).unwrap().len());
        }
    }
}
