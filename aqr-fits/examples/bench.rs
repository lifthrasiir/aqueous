//! aqr-fits-bench FILE...: each FITS file through its formula, checked; the formula's size
//! (streams of the model as they are, others through zstd at `LEVEL`, 19 by default), against
//! the file through zstd alone, and the time of each way.

use aqr_fits::formula::{self, IMAGE, RICE, TABLE};
use aqr_formula::{Apply, Formula, Kind};
use std::time::Instant;

fn eval(f: &Formula) -> Vec<u8> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| {
        let r = match a.module {
            IMAGE => formula::decode_image(&a.params, inputs, a.len),
            RICE => formula::decode_rice(&a.params, inputs, a.len),
            TABLE => formula::decode_table(&a.params, inputs, a.len),
            m => panic!("{m}"),
        };
        r.map_err(|e| aqr_formula::Error::new(e.0))
    })
    .expect("eval")
}

fn main() {
    let level: i32 = std::env::var("LEVEL").ok().and_then(|l| l.parse().ok()).unwrap_or(19);
    let z = |d: &[u8]| if d.is_empty() { 0 } else { zstd::bulk::compress(d, level).unwrap().len() };
    let (mut tin, mut tout, mut tz, mut tenc, mut tdec) = (0, 0, 0, 0.0, 0.0);
    for path in std::env::args().skip(1) {
        let d = std::fs::read(&path).expect("read");
        let t = Instant::now();
        let Some(f) = formula::encode(&d, 1) else {
            println!("{path}: not FITS");
            continue;
        };
        let enc = t.elapsed().as_secs_f64();
        let t = Instant::now();
        let back = eval(&f);
        let dec = t.elapsed().as_secs_f64();
        assert!(back == d, "{path}: round trip");
        // streams by class
        let mut by: std::collections::BTreeMap<&str, (usize, usize)> = Default::default();
        for s in &f.streams {
            let e = by.entry(&s.class).or_default();
            e.0 += s.data.len();
        }
        let mut classes: std::collections::BTreeMap<&str, Vec<u8>> = Default::default();
        for s in &f.streams {
            if s.kind == Kind::Compressed {
                by.get_mut(&*s.class).unwrap().1 += s.data.len();
            } else {
                classes.entry(&s.class).or_default().extend(&s.data);
            }
        }
        for (c, data) in &classes {
            by.get_mut(c).unwrap().1 += z(data);
        }
        let out: usize = by.values().map(|v| v.1).sum();
        let zd = z(&d);
        let name = path.rsplit('/').next().unwrap();
        let parts: Vec<String> = by.iter().filter(|v| v.1.0 > 0).map(|(c, v)| format!("{}={}", &c[5..], v.1)).collect();
        println!(
            "{name:<48} {:>9} -> {:>9} ({:.4}; zstd {:.4}) {:>6.1} / {:>6.1} MB/s  {}",
            d.len(),
            out,
            out as f64 / d.len() as f64,
            zd as f64 / d.len() as f64,
            d.len() as f64 / enc / 1e6,
            d.len() as f64 / dec / 1e6,
            parts.join(" ")
        );
        tin += d.len();
        tout += out;
        tz += zd;
        tenc += enc;
        tdec += dec;
    }
    println!(
        "total {tin} -> {tout} ({:.4}; zstd -{level} {:.4}), {:.1} / {:.1} MB/s",
        tout as f64 / tin as f64,
        tz as f64 / tin as f64,
        tin as f64 / tenc / 1e6,
        tin as f64 / tdec / 1e6
    );
}
