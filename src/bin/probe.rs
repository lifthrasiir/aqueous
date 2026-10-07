//! Times the stages the codec builds on (research tool).
use png_recons::png;
use std::time::Instant;

fn main() {
    let (mut tp, mut tw, mut png_bytes, mut raw_bytes, mut toks) = (0f64, 0f64, 0f64, 0f64, 0usize);
    for p in std::env::args().skip(1) {
        let file = std::fs::read(&p).unwrap();
        let Some((chunks, _)) = png::chunks(&file) else { continue };
        let idat: Vec<u8> = chunks.iter().filter(|c| &c.kind == b"IDAT").flat_map(|c| c.data.iter().copied()).collect();
        let t0 = Instant::now();
        let f = flate_recons::fast::inflate::parse(&idat[2..]).unwrap();
        let t1 = Instant::now();
        let mut stream = f.stream.clone();
        for b in &mut stream.blocks {
            if let flate_recons::deflate::Body::Fixed { tokens } | flate_recons::deflate::Body::Dynamic { tokens, .. } = &mut b.body {
                tokens.clear();
            }
        }
        let t2 = Instant::now();
        let w = flate_recons::fast::write::write(&stream, &f.starts, &f.tokens, &f.data).unwrap();
        let t3 = Instant::now();
        assert_eq!(&w[..], &idat[2..]);
        tp += (t1 - t0).as_secs_f64();
        tw += (t3 - t2).as_secs_f64();
        png_bytes += file.len() as f64;
        raw_bytes += f.data.len() as f64;
        toks += f.tokens.len();
    }
    println!("png {png_bytes:.0} raw {raw_bytes:.0} tokens {toks}: inflate {:.1} MB/s, write {:.1} MB/s (of png)", png_bytes / tp / 1e6, png_bytes / tw / 1e6);
}
