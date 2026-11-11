//! `aqr-isc-check FILE...`: each InstallShield cabinet taken apart, evaluated natively and
//! compared with the file, with the sizes of what was made.

use std::time::Instant;

use aqr_decompress::Native;

fn main() {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    for path in std::env::args().skip(1) {
        let d = std::fs::read(&path).expect("read");
        let t = Instant::now();
        let opt = aqr_isc::formula::Options { threads, limit: aqr_isc::formula::limit(d.len()), ..Default::default() };
        let Some(e) = aqr_isc::formula::encode_with(&d, &opt) else {
            println!("{path}: not taken");
            continue;
        };
        let took = t.elapsed();
        let records = e.formula.streams.iter().find(|s| s.class == "flate/record").map_or(0, |s| s.data.len());
        let t = Instant::now();
        let back = e.formula.eval(&Native { threads });
        println!(
            "{path}: {} bytes, {}/{} chunks rebuilt, data {} bytes, records {} bytes ({:.2}% of the cabinet), \
             split in {took:.2?}, rebuilt in {:.2?}: {}",
            d.len(),
            e.rebuilt,
            e.chunks,
            e.inflated,
            records,
            100.0 * records as f64 / d.len() as f64,
            t.elapsed(),
            match back {
                Ok(b) if b == d => "the same".to_string(),
                Ok(_) => "DIFFERENT".to_string(),
                Err(err) => format!("error {err}"),
            }
        );
    }
}
