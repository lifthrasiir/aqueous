//! Codes the audio of an AVI (the `01wb` chunks as one byte stream) and rebuilds it by the module's code, natively:
//! `aqr-ac3-check FILE.avi [CHUNKS]`.
use std::time::Instant;

fn chunks(data: &[u8], limit: usize) -> Vec<u8> {
    let mi = data.windows(4).position(|w| w == b"movi").expect("movi") + 4;
    let mut pos = mi;
    let mut out = Vec::new();
    let mut n = 0;
    while pos + 8 <= data.len() && n < limit {
        let id = &data[pos..pos + 4];
        let sz = u32::from_le_bytes(data[pos + 4..pos + 8].try_into().unwrap()) as usize;
        if id == b"LIST" {
            pos += 12;
            continue;
        }
        if pos + 8 + sz > data.len() {
            break;
        }
        if id == b"01wb" {
            out.extend_from_slice(&data[pos + 8..pos + 8 + sz]);
            n += 1;
        }
        pos += 8 + sz + (sz & 1);
    }
    out
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let data = std::fs::read(&args[1]).unwrap();
    let limit: usize = args.get(2).map_or(usize::MAX, |s| s.parse().unwrap());
    let stream = chunks(&data, limit);
    let t = Instant::now();
    let coded = aqr_ac3::formula::encode_with(&stream, 4).expect("compressible");
    let te = t.elapsed();
    let t = Instant::now();
    let back = aqr_ac3::formula::decode(&[], &[&coded], stream.len() as u64, 4).unwrap();
    let serial = aqr_ac3::module::decode_serial(&[], &[&coded], stream.len() as u64, 1).unwrap();
    assert!(serial == stream, "serial decode differs");
    let td = t.elapsed();
    println!(
        "{} -> {} bytes ({:.2}%), {}; encode {:.1}s decode {:.1}s",
        stream.len(),
        coded.len(),
        100.0 * coded.len() as f64 / stream.len() as f64,
        if back == stream { "identical" } else { "DIFFERENT" },
        te.as_secs_f64(),
        td.as_secs_f64()
    );
}
