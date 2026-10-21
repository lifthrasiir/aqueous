//! Why FlateDecode streams fail to parse: counts by error, with an example each.
use std::collections::BTreeMap;
fn main() {
    let mut by: BTreeMap<String, (usize, usize, String)> = BTreeMap::new();
    for f in std::env::args().skip(1) {
        let buf = std::fs::read(&f).unwrap();
        for s in aqr_pdf::scan::streams(&buf) {
            if aqr_pdf::kind::filter(&s.dict) != aqr_pdf::kind::Filter::Flate || s.data.len() < 64 {
                continue;
            }
            let d = &buf[s.data.clone()];
            let hdr = 2 * aqr_pdf::flate::zlib_header(d) as usize;
            if let Err(e) = aqr_flate::inflate::parse(&d[hdr..]) {
                let k = format!("{e:?} hdr={hdr} len_ok={}", s.length_ok);
                let ex = format!("{f}@{} {:02x?}", s.data.start, &d[..8.min(d.len())]);
                let v = by.entry(k).or_insert((0, 0, ex));
                v.0 += 1;
                v.1 += d.len();
            }
        }
    }
    for (k, v) in by {
        println!("{:>6} {:>11}  {k}  {}", v.0, v.1, v.2);
    }
}
