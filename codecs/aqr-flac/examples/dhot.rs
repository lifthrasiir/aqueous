//! Repeated decoding of one file (profiling without encoding in the profile).
use aqr_flac::formula::{self, MODULE};
use aqr_formula::Apply;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let iters: usize = args[0].parse().unwrap();
    let f = formula::encode(&std::fs::read(&args[1]).unwrap(), 1).unwrap();
    let t = std::time::Instant::now();
    let mut n = 0;
    for _ in 0..iters {
        let out = f
            .eval(&|a: &Apply, inputs: &[&[u8]]| {
                assert_eq!(a.module, MODULE);
                let res: Vec<&[u8]> = a.resources.iter().map(|r| &r[..]).collect();
                formula::decode(&a.params, &res, inputs, a.len, 1).map_err(|e| aqr_formula::Error::new(e.0))
            })
            .unwrap();
        n += out.len();
    }
    eprintln!("{:.1} MB/s", n as f64 / t.elapsed().as_secs_f64() / 1e6);
}
