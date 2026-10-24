//! Peak heap of `formula::encode` and of evaluating the formula, per file, and the process's
//! peak resident set. `LOG=MB` logs the allocations of at least that many MB; `T=N` threads.

use aqr_common::mem::{self, Counting};

#[global_allocator]
static A: Counting = Counting;

fn main() {
    let log = std::env::var("LOG").ok().and_then(|s| s.parse::<f64>().ok()).map(|mb| (mb * 1e6) as usize);
    let threads: usize = std::env::var("T").ok().and_then(|s| s.parse().ok()).unwrap_or(1);
    let mb = |x: usize| x as f64 / 1e6;
    let rss = || mem::max_rss() as f64 / 1e6;
    for path in std::env::args().skip(1) {
        let data = std::fs::read(&path).unwrap();
        println!("{path}: {:.2} MB", mb(data.len()));
        mem::log_large(log);
        let (f, enc) = mem::peak(|| aqr_mp3::formula::encode(&data, threads).expect("not coded"));
        mem::log_large(None);
        let enc_rss = rss();
        if log.is_some() {
            println!("  -- decode");
        }
        mem::log_large(log);
        let (back, dec) = mem::peak(|| {
            f.eval(&|a: &aqr_formula::Apply, i: &[&[u8]]| {
                let r: Vec<&[u8]> = a.resources.iter().map(|r| &**r).collect();
                aqr_mp3::formula::decode(&a.params, &r, i, a.len, threads).map_err(|e| aqr_formula::Error(e.0))
            })
            .unwrap()
        });
        mem::log_large(None);
        assert!(back == data);
        let x = |n: usize| n as f64 / data.len() as f64;
        println!(
            "  enc {:.1} MB ({:.1}x, rss {enc_rss:.1})  dec {:.1} MB ({:.1}x, rss {:.1})",
            mb(enc),
            x(enc),
            mb(dec),
            x(dec),
            rss()
        );
    }
}
