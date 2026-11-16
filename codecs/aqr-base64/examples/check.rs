//! The base64 blocks the encoder finds in files: for each file, the blocks (where, how many
//! bytes, the lines, the alphabet and padding) and the first bytes of each decoded, to tell what
//! they are; then the totals. With `-q`, the totals only.
//!
//! ```sh
//! cargo run --release -p aqr-base64 --example aqr-base64-check -- [-q] FILE...
//! ```

use aqr_base64::formula;

fn main() {
    let mut quiet = false;
    let (mut files, mut blocks, mut bytes) = (0, 0, 0);
    for a in std::env::args().skip(1) {
        if a == "-q" {
            quiet = true;
            continue;
        }
        let d = std::fs::read(&a).unwrap_or_else(|e| panic!("{a}: {e}"));
        let found: Vec<_> = [false, true].into_iter().flat_map(|url| formula::scan(&d, url)).collect();
        if found.is_empty() {
            continue;
        }
        files += 1;
        let n: usize = found.iter().map(|f| f.bytes.len()).sum();
        println!("{a}: {} blocks, {n} bytes of {}", found.len(), d.len());
        for f in found.iter().filter(|_| !quiet) {
            let head: String =
                f.bytes.iter().take(16).map(|&b| if b.is_ascii_graphic() { b as char } else { '.' }).collect();
            println!(
                "  {}..{} {} bytes, lines {}/{}, flags {} {head:?}",
                f.start,
                f.end,
                f.bytes.len(),
                f.first,
                f.line,
                f.flags
            );
        }
        blocks += found.len();
        bytes += n;
    }
    println!("{files} files, {blocks} blocks, {bytes} bytes decoded");
}
