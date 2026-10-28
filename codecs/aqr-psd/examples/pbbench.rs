// PackBits throughput on a file's RLE channel rows: decoding, encoding (profiling aid).
use aqr_psd::packbits;
use aqr_psd::psd::Compression;
use aqr_psd::rows::unpackbits_into;
use std::time::Instant;

fn main() {
    let f = std::env::args().nth(1).unwrap();
    let d = std::fs::read(&f).unwrap();
    let p = aqr_psd::parse(&d).unwrap();
    let cb = if p.psb { 4 } else { 2 };
    let mut rows: Vec<(&[u8], usize, packbits::Encoder)> = Vec::new();
    for l in &p.layer_info.layers {
        for c in &l.channels {
            if c.compression(&d) != Some(Compression::Rle) {
                continue;
            }
            let enc = packbits::rows(&d[c.span.start + 2..c.span.end], c.height as usize, cb).unwrap();
            let (e, _) = packbits::identify(&enc);
            rows.extend(enc.into_iter().map(|r| (r, c.width as usize, e)));
        }
    }
    let raw: usize = rows.iter().map(|r| r.1).sum();
    let mut buf = vec![0u8; rows.iter().map(|r| r.1).max().unwrap_or(0)];
    let mut out = Vec::new();
    for _ in 0..3 {
        let t = Instant::now();
        for &(r, w, _) in &rows {
            unpackbits_into(r, &mut buf[..w]);
        }
        let td = t.elapsed().as_secs_f64();
        let t = Instant::now();
        let mut same = 0;
        for &(r, w, e) in &rows {
            unpackbits_into(r, &mut buf[..w]);
            out.clear();
            packbits::encode(e, &buf[..w], &mut out);
            same += (out == r) as usize;
        }
        let te = t.elapsed().as_secs_f64() - td;
        println!(
            "{} rows, {raw} bytes: decode {:.2} GB/s, encode+compare {:.2} GB/s ({same} same)",
            rows.len(),
            raw as f64 / td / 1e9,
            raw as f64 / te / 1e9
        );
    }
}
