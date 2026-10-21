//! Corpus survey: bytes by stream class and filter, FlateDecode expansion, the cost and
//! metadata of aqr-flate's fast representation, and zstd on the inflated data.
//!
//! `RAYON_NUM_THREADS=1 cargo run --release --example survey -- FILE...`

use aqr_pdf::kind::{self, CLASSES, Class, Filter};
use aqr_pdf::scan;
use std::collections::BTreeMap;
use std::time::Instant;

#[derive(Default)]
struct Acc {
    n: usize,
    raw: usize,
    plain: usize,
    meta: usize,
    failed: usize,
    t_parse: f64,
    t_dec: f64,
    t_rec: f64,
}

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();
    let mut by: BTreeMap<(Class, Filter), Acc> = BTreeMap::new();
    let mut plains: BTreeMap<Class, Vec<u8>> = BTreeMap::new();
    let mut skel = vec![];
    let (mut total, mut jpeg_in, mut jpeg_out, mut t_jc, mut t_jd) = (0usize, 0usize, 0usize, 0f64, 0f64);
    let opts = aqr_flate::Options { threads: 1, ..Default::default() };
    for f in &files {
        let buf = std::fs::read(f).unwrap();
        total += buf.len();
        let ss = scan::streams(&buf);
        let mut at = 0;
        let mut fstat: BTreeMap<(Class, Filter), (usize, usize)> = BTreeMap::new();
        for s in &ss {
            skel.extend_from_slice(&buf[at..s.data.start]);
            at = s.data.end;
            let c = kind::class(&s.dict);
            let fl = kind::filter(&s.dict);
            let data = &buf[s.data.clone()];
            let e = fstat.entry((c, fl)).or_default();
            e.0 += 1;
            e.1 += data.len();
            let a = by.entry((c, fl)).or_default();
            a.n += 1;
            a.raw += data.len();
            match fl {
                Filter::Flate => {
                    let raw = if data.len() > 2 && data[0] & 0xf == 8 { &data[2..] } else { data };
                    let t0 = Instant::now();
                    let Ok(flat) = aqr_flate::inflate::parse(raw) else {
                        a.failed += 1;
                        continue;
                    };
                    a.t_parse += t0.elapsed().as_secs_f64();
                    let trail = flat.stream.trailing.len();
                    let raw = &raw[..raw.len() - trail.saturating_sub(4).min(raw.len())];
                    a.plain += flat.data.len();
                    plains.entry(c).or_default().extend_from_slice(&flat.data);
                    let t1 = Instant::now();
                    match aqr_flate::deconstruct_with(raw, &opts) {
                        Ok((d, m)) => {
                            let t2 = Instant::now();
                            a.t_dec += (t2 - t1).as_secs_f64();
                            a.meta += m.len();
                            let back = aqr_flate::reconstruct_with_threads(&d, &m, 1).unwrap();
                            a.t_rec += t2.elapsed().as_secs_f64();
                            assert_eq!(back, raw);
                        }
                        Err(_) => a.failed += 1,
                    }
                }
                Filter::Dct => {
                    let t0 = Instant::now();
                    let p = aqr_jpeg::container::compress(data);
                    let t1 = Instant::now();
                    let back = aqr_jpeg::container::decompress(&p).unwrap();
                    t_jd += t1.elapsed().as_secs_f64();
                    t_jc += (t1 - t0).as_secs_f64();
                    assert_eq!(back, data);
                    jpeg_in += data.len();
                    jpeg_out += p.len();
                }
                _ => {}
            }
        }
        skel.extend_from_slice(&buf[at..]);
        let streams: usize = fstat.values().map(|v| v.1).sum();
        print!("{:>10} {:>6} skel {:>9}", buf.len(), ss.len(), buf.len() - streams);
        for ((c, fl), (n, b)) in &fstat {
            print!(" {}/{:?}:{}:{}", c.name(), fl, n, b);
        }
        println!("  {}", f.rsplit('/').next().unwrap());
    }
    println!("\ntotal {total}, skeleton {} ({:.1}%)", skel.len(), 100.0 * skel.len() as f64 / total as f64);
    for lvl in [3, 9, 19] {
        let z = zstd::bulk::compress(&skel, lvl).unwrap().len();
        print!("  skel zstd-{lvl} {z}");
    }
    println!();
    println!(
        "{:<16} {:>6} {:>11} {:>11} {:>5} {:>9} {:>5} {:>7} {:>7} {:>7} {:>4}",
        "class/filter", "n", "raw", "plain", "x", "meta", "m%", "parse", "dec", "rec", "fail"
    );
    for ((c, fl), a) in &by {
        let mbps = |t: f64| if t > 0.0 { a.plain as f64 / t / 1e6 } else { 0.0 };
        println!(
            "{:<16} {:>6} {:>11} {:>11} {:>5.2} {:>9} {:>5.2} {:>7.0} {:>7.0} {:>7.0} {:>4}",
            format!("{}/{:?}", c.name(), fl),
            a.n,
            a.raw,
            a.plain,
            a.plain as f64 / a.raw.max(1) as f64,
            a.meta,
            100.0 * a.meta as f64 / a.raw.max(1) as f64,
            mbps(a.t_parse),
            mbps(a.t_dec),
            mbps(a.t_rec),
            a.failed
        );
    }
    println!("(speeds in MB/s of inflated data)");
    for c in CLASSES {
        let Some(p) = plains.get(&c) else { continue };
        let raw: usize = by.iter().filter(|((cc, f), _)| *cc == c && *f == Filter::Flate).map(|(_, a)| a.raw).sum();
        print!("{:<8} raw {raw:>11} plain {:>11}", c.name(), p.len());
        for lvl in [1, 3, 9, 19] {
            let t = Instant::now();
            let z = zstd::bulk::compress(p, lvl).unwrap().len();
            print!("  z{lvl} {z} ({:.0} MB/s)", p.len() as f64 / t.elapsed().as_secs_f64() / 1e6);
        }
        println!();
    }
    if jpeg_in > 0 {
        println!(
            "jpeg {jpeg_in} -> {jpeg_out} ({:.3}), {:.0} / {:.0} MB/s",
            jpeg_out as f64 / jpeg_in as f64,
            jpeg_in as f64 / t_jc / 1e6,
            jpeg_in as f64 / t_jd / 1e6
        );
    }
}
