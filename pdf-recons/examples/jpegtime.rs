//! Per DCTDecode stream: bytes, 8×8 blocks, progressive, jpeg-recons compression and
//! decompression seconds (best of 2), output bytes. For fitting its cost.
fn main() {
    let now = std::time::Instant::now;
    for f in std::env::args().skip(1) {
        let buf = std::fs::read(&f).unwrap();
        for s in pdf_recons::scan::streams(&buf) {
            if pdf_recons::kind::filter(&s.dict) != pdf_recons::kind::Filter::Dct {
                continue;
            }
            let d = &buf[s.data.clone()];
            let Some((prog, blocks)) = pdf_recons::image::jpeg_frame(d) else { continue };
            let (mut tc, mut td, mut out) = (f64::INFINITY, f64::INFINITY, 0);
            for _ in 0..2 {
                let t = now();
                let p = jpeg_recons::container::compress(d);
                tc = tc.min(t.elapsed().as_secs_f64());
                out = p.len();
                if p.len() < d.len() {
                    let t = now();
                    let _ = jpeg_recons::container::decompress(&p);
                    td = td.min(t.elapsed().as_secs_f64());
                } else {
                    td = 0.0;
                }
            }
            println!("{f}\t{}\t{blocks}\t{}\t{tc:.9}\t{td:.9}\t{out}", d.len(), prog as u8);
        }
    }
}
