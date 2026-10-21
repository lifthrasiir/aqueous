//! Per DCTDecode stream: bytes, 8×8 blocks, progressive, aqr-jpeg compression and
//! decompression seconds (best of 2), output bytes. For fitting its cost.
fn main() {
    let now = std::time::Instant::now;
    for f in std::env::args().skip(1) {
        let buf = std::fs::read(&f).unwrap();
        for s in aqr_pdf::scan::streams(&buf) {
            if aqr_pdf::kind::filter(&s.dict) != aqr_pdf::kind::Filter::Dct {
                continue;
            }
            let d = &buf[s.data.clone()];
            let Some((prog, blocks)) = aqr_pdf::image::jpeg_frame(d) else { continue };
            let (mut tc, mut td, mut out) = (f64::INFINITY, f64::INFINITY, 0);
            for _ in 0..2 {
                let t = now();
                let p = aqr_jpeg::container::compress(d);
                tc = tc.min(t.elapsed().as_secs_f64());
                out = p.len();
                if p.len() < d.len() {
                    let t = now();
                    let _ = aqr_jpeg::container::decompress(&p);
                    td = td.min(t.elapsed().as_secs_f64());
                } else {
                    td = 0.0;
                }
            }
            println!("{f}\t{}\t{blocks}\t{}\t{tc:.9}\t{td:.9}\t{out}", d.len(), prog as u8);
        }
    }
}
