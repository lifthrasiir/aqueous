//! Runs inputs through a target's body as plain code (no sanitizer, no coverage, checksums
//! checked), timed: `replay TARGET FILE...`, to tell a slow input of the fuzzer's from one of
//! the code's.

use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [t, files @ ..] = &args[..] else { panic!("usage: replay TARGET FILE...") };
    for f in files {
        let d = std::fs::read(f).unwrap();
        let start = Instant::now();
        let what = match t.as_str() {
            "compress" => {
                let c = aqr_compress::compress(&d, &aqr_fuzz::options());
                let back = aqr_archive::decode(&c, &aqr_fuzz::Known);
                format!(
                    "{} -> {} bytes, back: {}",
                    d.len(),
                    c.len(),
                    back.map_or_else(|e| e.to_string(), |b| (b == d).to_string())
                )
            }
            "natives" => format!("{:?}", aqr_archive::decode_value(&d, &aqr_fuzz::Known).map(|_| ())),
            _ => panic!("no target {t}"),
        };
        eprintln!("{f}: {:.2} s, {what}", start.elapsed().as_secs_f64());
    }
}
