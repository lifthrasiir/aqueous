//! The base64 and hexadecimal blocks and the quoted-printable bodies the encoders find in files:
//! for each file, the blocks (where, how many bytes, the lines, the alphabet and padding) and the
//! first bytes of each decoded, to tell what they are, and the bodies (where, the bytes, the line
//! length, the flags and the exceptions of the layout against the tokens); then the totals. With
//! `-q`, the totals only.
//!
//! ```sh
//! cargo run --release -p aqr-b2t --example aqr-b2t-check -- [-q] FILE...
//! ```

use aqr_b2t::formula::{self, Alpha};
use aqr_b2t::quopri;

fn main() {
    let mut quiet = false;
    let mut verbose = false;
    let (mut files, mut blocks, mut bytes) = (0, 0, 0);
    for a in std::env::args().skip(1) {
        if a == "-v" {
            verbose = true;
            continue;
        }
        if a == "-q" {
            quiet = true;
            continue;
        }
        let d = std::fs::read(&a).unwrap_or_else(|e| panic!("{a}: {e}"));
        let found: Vec<_> = Alpha::ALL.into_iter().flat_map(|a| formula::scan(&d, a)).collect();
        for r in quopri::formula::runs(&d) {
            let what = match quopri::formula::analyze(&d[r.clone()]) {
                Some(b) => format!(
                    "{} bytes, line {}, flags {}, {} exceptions of {} tokens",
                    b.data.len(),
                    b.line,
                    b.flags,
                    b.exceptions.len(),
                    b.tokens
                ),
                None => "not taken".to_string(),
            };
            println!("{a}: quoted-printable {}..{}: {what}", r.start, r.end);
            if let (true, Some(b)) = (verbose, quopri::formula::analyze(&d[r.clone()])) {
                for &(i, kind) in b.exceptions.iter().take(12) {
                    // (the data's byte of about that token: tokens are bytes but for a CRLF)
                    let at = (i as usize).min(b.data.len());
                    let from = at.saturating_sub(24);
                    println!(
                        "  token {i} kind {kind}: {:?}",
                        String::from_utf8_lossy(&b.data[from..(at + 8).min(b.data.len())])
                    );
                }
            }
        }
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
