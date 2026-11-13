//! What aqr-ntfs finds in volumes (through aqr-disk, of all of an image and its partitions): their
//! files and the bytes left as the rest, whether the formula gives each image back, and how long
//! each step takes (`-v` lists the files; `-x DIR` writes each WOF file's compressed bytes to
//! DIR, as `<algorithm>-<n>-<length of the data>.wof`, and each LZNT1 one's as
//! `lznt1-<n>-<length>-<unit>.bin` with the bytes allocated of each unit in `.alloc`).
//!
//! ```sh
//! cargo run --release -p aqr-ntfs --example aqr-ntfs-check -- [-v] [-x DIR] IMAGE...
//! ```

use std::time::Instant;

fn main() {
    let (mut paths, mut verbose, mut out) = (Vec::new(), false, None);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match &a[..] {
            "-v" => verbose = true,
            "-x" => out = args.next(),
            _ => paths.push(a),
        }
    }
    let fs = [aqr_ntfs::FS];
    for p in &paths {
        let d = std::fs::read(p).unwrap();
        let t = Instant::now();
        let files = aqr_disk::files(&d, &fs);
        let read = t.elapsed().as_secs_f64();
        if let Some(dir) = &out {
            for (n, f) in files.iter().enumerate() {
                let bytes: Vec<u8> = f.extents.iter().flat_map(|r| d[r.clone()].to_vec()).collect();
                match &f.compressed {
                    Some(aqr_disk::Compressed::Wof { algorithm, len }) => {
                        std::fs::write(format!("{dir}/{algorithm}-{n}-{len}.wof"), bytes).unwrap();
                        println!("  wof {algorithm} {len} {}", String::from_utf8_lossy(&f.name));
                    }
                    Some(aqr_disk::Compressed::Lznt1 { len, unit, allocated }) => {
                        let at = format!("{dir}/lznt1-{n}-{len}-{unit}");
                        std::fs::write(format!("{at}.bin"), bytes).unwrap();
                        let lines: Vec<String> = allocated.iter().map(u32::to_string).collect();
                        std::fs::write(format!("{at}.alloc"), lines.join("\n") + "\n").unwrap();
                        println!("  lznt1 {len} {} units {}", allocated.len(), String::from_utf8_lossy(&f.name));
                    }
                    Some(aqr_disk::Compressed::Opaque { .. }) | None => {}
                }
            }
        }
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
