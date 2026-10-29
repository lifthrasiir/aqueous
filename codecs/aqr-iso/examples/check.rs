//! What aqr-iso finds in images: their files and the bytes left as the rest, whether the
//! formula gives each image back, and how fast it is made (`-v` lists the files, `-z` adds zstd
//! -3 of the rest).
//!
//! ```sh
//! cargo run --release -p aqr-iso --example aqr-iso-check -- [-v] [-z] IMAGE...
//! ```

use std::time::Instant;

use aqr_formula::Node;

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
        let files = aqr_iso::files(&d);
        let pieces = aqr_iso::pieces(&d, &files);
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
                let f = &files[t.file.unwrap()];
                println!("  {:>12} {:>12}  {}", t.span.start, t.span.len(), String::from_utf8_lossy(&f.name));
            }
        }
        let f = aqr_iso::formula::build(&d, &files, &pieces, &Default::default());
        let Node::Concat(parts) = &f.root else { panic!("no concatenation") };
        let mut out = Vec::with_capacity(d.len());
        for n in parts {
            let &Node::Slice { stream, start, len } = n else { panic!("not a slice") };
            out.extend_from_slice(&f.streams[stream].data[start as usize..][..len as usize]);
        }
        let rebuilt = out == d;
        drop(out);
        println!("  rebuilt: {rebuilt}");
        if zstd {
            let rest = &f.streams[1].data;
            println!("  rest {} -> zstd -3 {}", rest.len(), zstd::bulk::compress(rest, 3).unwrap().len());
        }
    }
}
