//! Token statistics of PNG files' IDAT streams (research tool).
use png_recons::png;

fn main() {
    let (mut png_bytes, mut lits, mut mats, mut mbytes) = (0usize, 0usize, 0usize, 0usize);
    for p in std::env::args().skip(1) {
        let file = std::fs::read(&p).unwrap();
        let Some((chunks, _)) = png::chunks(&file) else { continue };
        let idat: Vec<u8> = chunks.iter().filter(|c| &c.kind == b"IDAT").flat_map(|c| c.data.iter().copied()).collect();
        let f = flate_recons::fast::inflate::parse(&idat[2..]).unwrap();
        png_bytes += file.len();
        for t in &f.tokens {
            match t {
                flate_recons::deflate::Token::Lit(_) => lits += 1,
                flate_recons::deflate::Token::Match { len, .. } => {
                    mats += 1;
                    mbytes += *len as usize
                }
            }
        }
    }
    println!(
        "png {png_bytes}: literals {lits} ({:.2}/png byte), matches {mats} ({:.2}/png byte, {:.1} bytes each)",
        lits as f64 / png_bytes as f64,
        mats as f64 / png_bytes as f64,
        mbytes as f64 / mats as f64
    );
}
