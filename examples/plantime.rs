// Times the steps of span analysis over the RLE channels of files (profiling aid).
use psd_recons::packbits;
use psd_recons::pixels::unpackbits;
use psd_recons::psd::Compression;
use std::time::Instant;

fn main() {
    let mut t = [0f64; 4];
    for f in std::env::args().skip(1) {
        let d = std::fs::read(&f).unwrap();
        let p = psd_recons::parse(&d).unwrap();
        let cb = if p.psb { 4 } else { 2 };
        for l in &p.layer_info.layers {
            for c in &l.channels {
                if c.compression(&d) != Some(Compression::Rle) {
                    continue;
                }
                let s = Instant::now();
                let rows = packbits::rows(&d[c.span.start + 2..c.span.end], c.height as usize, cb).unwrap();
                let mut data = Vec::new();
                for r in &rows {
                    unpackbits(r, &mut data);
                }
                t[0] += s.elapsed().as_secs_f64();
                let s = Instant::now();
                let cands = packbits::candidates(&rows);
                t[1] += s.elapsed().as_secs_f64();
                let s = Instant::now();
                let (enc, _) = packbits::identify(&rows);
                t[2] += s.elapsed().as_secs_f64();
                let s = Instant::now();
                let mut buf = Vec::new();
                let w = c.width as usize;
                for (i, _) in rows.iter().enumerate() {
                    buf.clear();
                    packbits::encode(enc, &data[i * w..(i + 1) * w], &mut buf);
                }
                t[3] += s.elapsed().as_secs_f64();
                let _ = cands;
            }
        }
    }
    println!("decode {:.3}s candidates {:.3}s identify (incl. candidates) {:.3}s one encode {:.3}s", t[0], t[1], t[2], t[3]);
}
