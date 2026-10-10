//! FlateDecode streams by class: the actual expansion against the class prior and against
//! the estimate from the first block header (`flate::expansion`), as byte-weighted errors.
use pdf_recons::{flate, kind};
fn main() {
    // Per class: bytes, inflated, and |log2(estimate / actual)| sums (byte-weighted) for
    // the prior and the header estimate; streams without an estimate.
    let prior = [4.5, 2.2, 11.8, 4.6, 4.5, 1.4];
    let mut acc = [[0f64; 6]; 6];
    for f in std::env::args().skip(1) {
        let buf = std::fs::read(&f).unwrap();
        for s in pdf_recons::scan::streams(&buf) {
            if kind::filter(&s.dict) != kind::Filter::Flate {
                continue;
            }
            let d = &buf[s.data.clone()];
            let Some(sh) = flate::shape(d) else { continue };
            if sh.deflate < 64 {
                continue;
            }
            let c = kind::class(&s.dict) as usize;
            let actual = sh.plain as f64 / d.len() as f64;
            let w = d.len() as f64;
            let a = &mut acc[c];
            a[0] += w;
            a[1] += sh.plain as f64;
            a[2] += w * (prior[c] / actual).log2().abs();
            match flate::expansion(d) {
                Some(e) => a[3] += w * (e / actual).log2().abs(),
                None => {
                    a[3] += w * (prior[c] / actual).log2().abs();
                    a[4] += w;
                }
            }
        }
    }
    for (c, a) in acc.iter().enumerate() {
        if a[0] > 0.0 {
            println!(
                "{:<8} {:>12.0} bytes {:>5.2}x  mean |log2 err|: prior {:.3}, header {:.3}  (no header estimate {:.1}%)",
                kind::CLASSES[c].name(), a[0], a[1] / a[0], a[2] / a[0], a[3] / a[0], 100.0 * a[4] / a[0]
            );
        }
    }
}
