//! Runs inputs through a target's body as plain code (no sanitizer, no coverage, checksums
//! checked), timed: `replay TARGET FILE...`
//! (decode, unpack, pack, modules, natives, wasm, compress, roundtrip), to tell a slow input of the fuzzer's from one of
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
            "wasm" => {
                let t = Instant::now();
                let n = aqr_archive::decode_value(&d, &aqr_fuzz::Known).map(|_| ());
                let native = t.elapsed().as_secs_f64();
                let t = Instant::now();
                let w = aqr_archive::decode_value(&d, &aqr_decompress::Wasm).map(|_| ());
                format!("natively {n:?} in {native:.2} s, in Wasm {w:?} in {:.2} s", t.elapsed().as_secs_f64())
            }
            "natives" => format!("{:?}", aqr_archive::decode_value(&d, &aqr_fuzz::Known).map(|_| ())),
            "modules" => aqr_fuzz::modules::run(&d),
            "pack" => {
                aqr_fuzz::pack::pack(&d);
                "read back alike".to_string()
            }
            "unpack" => {
                aqr_fuzz::unpack::unpack(&d);
                "unpacked alike".to_string()
            }
            "decode" => {
                aqr_fuzz::decode::decode(&d);
                "as decoded".to_string()
            }
            "roundtrip" => {
                let mut u = libfuzzer_sys::arbitrary::Unstructured::new(&d);
                let _ = u.choose_index(4);
                let _ = u.arbitrary::<bool>();
                let made = Instant::now();
                let f = aqr_fuzz::formula::formula(&mut u);
                let made = made.elapsed().as_secs_f64();
                let r = Instant::now();
                let want = f.as_ref().ok().and_then(aqr_fuzz::formula::reference);
                let r = r.elapsed().as_secs_f64();
                let all = Instant::now();
                aqr_fuzz::formula::roundtrip(&d);
                format!(
                    "given back (made in {made:.3} s, of {} bytes, the reference's in {r:.3} s; all {:.3} s)",
                    want.map_or(0, |w| w.len()),
                    all.elapsed().as_secs_f64()
                )
            }
            _ => panic!("no target {t}"),
        };
        eprintln!("{f}: {:.2} s, {what}", start.elapsed().as_secs_f64());
    }
}
