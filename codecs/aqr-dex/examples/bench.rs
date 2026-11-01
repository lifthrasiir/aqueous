//! Times the module on DEX files: `aqr-dex-bench [-n RUNS] FILE...`.

fn main() {
    let mut runs = 10;
    let mut files = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-n" => runs = args.next().unwrap().parse().unwrap(),
            _ => files.push(std::fs::read(&a).unwrap()),
        }
    }
    let split: Vec<_> = files.iter().map(|d| aqr_dex::encode::split(d).unwrap()).collect();
    let bytes: usize = files.iter().map(|d| d.len()).sum();
    let t = std::time::Instant::now();
    for _ in 0..runs {
        for (d, st) in files.iter().zip(&split) {
            let refs: Vec<&[u8]> = st.iter().map(|v| v.as_slice()).collect();
            let out = aqr_dex::module::decode(&[], &refs, d.len() as u64).unwrap();
            assert_eq!(out.len(), d.len());
        }
    }
    let s = t.elapsed().as_secs_f64();
    println!("{} bytes x {runs}: {:.3} s, {:.0} MB/s", bytes, s, (bytes * runs) as f64 / s / 1e6);
}
