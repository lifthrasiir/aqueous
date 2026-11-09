//! What aqr-ntfs finds in volumes (through aqr-disk, of all of an image and its partitions): their
//! files and the bytes left as the rest, whether the formula gives each image back, and how long
//! each step takes (`-v` lists the files).
//!
//! ```sh
//! cargo run --release -p aqr-ntfs --example aqr-ntfs-check -- [-v] IMAGE...
//! ```

use std::time::Instant;

fn main() {
    let (mut paths, mut verbose) = (Vec::new(), false);
    for a in std::env::args().skip(1) {
        match &a[..] {
            "-v" => verbose = true,
            _ => paths.push(a),
        }
    }
    let fs = [aqr_ntfs::FS];
    for p in &paths {
        let d = std::fs::read(p).unwrap();
        let t = Instant::now();
        let files = aqr_disk::files(&d, &fs);
        let read = t.elapsed().as_secs_f64();
        let t = Instant::now();
        let pieces = aqr_disk::pieces(&d, &files);
        let laid = t.elapsed().as_secs_f64();
        let t = Instant::now();
        let f = aqr_disk::formula::build(&d, &files, &pieces, &Default::default());
        let built = t.elapsed().as_secs_f64();
        let taken = pieces.iter().filter(|p| p.file.is_some()).count();
        let rest: usize = pieces.iter().filter(|p| p.file.is_none()).map(|p| p.span.len()).sum();
        println!(
            "{p}: {} bytes, {} files, {taken} pieces taken ({} bytes), {} split, rest {rest} bytes; \
             read {read:.3} s, laid {laid:.3} s, formula {built:.3} s",
            d.len(),
            files.len(),
            d.len() - rest,
            f.shared.len(),
        );
        if verbose {
            for f in &files {
                println!("  {:>3} {}", f.extents.len(), String::from_utf8_lossy(&f.name));
            }
        }
        let none = |_: &aqr_formula::Apply, _: &[&[u8]]| -> aqr_formula::Result<Vec<u8>> { unreachable!("no module") };
        println!("  rebuilt: {}", f.eval(&none).unwrap() == d);
    }
}
