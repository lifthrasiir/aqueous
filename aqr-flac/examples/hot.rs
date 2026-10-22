//! Repeated encoding (`c`) or decoding (`d`) of files to formulas, for profiling.
use aqr_flac::formula::{self, MODULE};
use aqr_formula::Apply;

fn eval(f: &aqr_formula::Formula) -> Vec<u8> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| {
        assert_eq!(a.module, MODULE);
        let res: Vec<&[u8]> = a.resources.iter().map(|r| &r[..]).collect();
        formula::decode(&a.params, &res, inputs, a.len, 1).map_err(|e| aqr_formula::Error::new(e.0))
    })
    .unwrap()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let iters: usize = args[1].parse().unwrap();
    let data: Vec<Vec<u8>> = args[2..].iter().map(|p| std::fs::read(p).unwrap()).collect();
    let formulas: Vec<_> = data.iter().map(|d| formula::encode(d, 1).unwrap()).collect();
    let t = std::time::Instant::now();
    let mut n = 0;
    for _ in 0..iters {
        for (d, f) in data.iter().zip(&formulas) {
            if args[0] == "c" {
                n += formula::encode(d, 1).unwrap().len() as usize;
            } else {
                n += eval(f).len();
            }
        }
    }
    let tot: usize = data.iter().map(|d| d.len()).sum::<usize>() * iters;
    eprintln!("{n} {:.1} MB/s", tot as f64 / t.elapsed().as_secs_f64() / 1e6);
}
