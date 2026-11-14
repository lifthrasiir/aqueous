//! FlateDecode streams by class: the actual expansion against the class prior and against
//! the estimate from the first block header (`flate::expansion`), as byte-weighted errors.
use aqr_flate::inflate::{self, Sink};
use aqr_pdf::flate::{self, zlib_header};
use aqr_pdf::kind;

/// The shape of a FlateDecode stream's data, from a parse that keeps nothing.
struct Shape {
    /// Bytes of DEFLATE data.
    deflate: usize,
    /// Bytes of inflated data.
    plain: usize,
}

#[derive(Default)]
struct Count {
    plain: usize,
}

impl Sink for Count {
    fn lit(&mut self, _: u8) {
        self.plain += 1;
    }
    fn mat(&mut self, len: u16, _: u16, _: bool) {
        self.plain += len as usize;
    }
    fn stored(&mut self, b: &[u8]) {
        self.plain += b.len();
    }
}

/// Measures a stream without inflating it.
fn shape(data: &[u8]) -> Option<Shape> {
    let hdr = if zlib_header(data) { 2 } else { 0 };
    let mut c = Count::default();
    let l = inflate::parse_into(&data[hdr..], usize::MAX, &mut c).ok()?;
    Some(Shape { deflate: data.len() - hdr - l.stream.trailing.len(), plain: c.plain })
}

fn main() {
    // Per class: bytes, inflated, and |log2(estimate / actual)| sums (byte-weighted) for
    // the prior and the header estimate; streams without an estimate.
    let prior = [4.5, 2.2, 11.8, 4.6, 4.5, 1.4];
    let mut acc = [[0f64; 6]; 6];
    for f in std::env::args().skip(1) {
        let buf = std::fs::read(&f).unwrap();
        for s in aqr_pdf::scan::streams(&buf) {
            if kind::filter(&s.dict) != kind::Filter::Flate {
                continue;
            }
            let d = &buf[s.data.clone()];
            let Some(sh) = shape(d) else { continue };
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
                kind::CLASSES[c].name(),
                a[0],
                a[1] / a[0],
                a[2] / a[0],
                a[3] / a[0],
                100.0 * a[4] / a[0]
            );
        }
    }
}
