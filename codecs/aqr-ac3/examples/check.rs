//! Codes the AC-3 syncframes in the `01wb` chunks of an AVI and decodes them again: `aqr-ac3-check FILE.avi [CHUNKS]`.
use aqr_ac3::{Ctx, Model};
use aqr_walk::rc::{Decoder, Encoder, SliceSource};
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
    // the syncframes in a row from the first one
    let mut frames = Vec::new();
    let mut p = 0;
    while p + 5 <= stream.len() {
        match aqr_ac3::frame_len(&stream[p..]) {
            Some(n) if p + n <= stream.len() => {
                frames.push(&stream[p..p + n]);
                p += n;
            }
            _ => p += 1,
        }
    }
    println!("{} bytes, {} frames", stream.len(), frames.len());
    let t = Instant::now();
    let mut model = Model::new(21);
    let mut ctx = Ctx::default();
    let mut enc = Encoder::new();
    let (mut bad, mut total) = (0, 0usize);
    let mut ok_frames = Vec::new();
    for f in &frames {
        total += f.len();
        if aqr_ac3::check(f) {
            aqr_ac3::encode(&mut enc, &mut model, &mut ctx, f).unwrap();
            ok_frames.push(*f);
        } else {
            bad += 1;
        }
    }
    let out = enc.finish();
    let te = t.elapsed();
    let t = Instant::now();
    let mut model = Model::new(21);
    let mut ctx = Ctx::default();
    let mut dec = Decoder::new(SliceSource::new(&out));
    let mut diff = 0;
    for f in &ok_frames {
        let g = aqr_ac3::decode(&mut dec, &mut model, &mut ctx, Vec::new()).unwrap();
        if g != *f {
            diff += 1;
            if diff < 3 {
                let at = g.iter().zip(f.iter()).position(|(a, b)| a != b);
                println!("differs at byte {at:?} (lengths {} {})", g.len(), f.len());
            }
        }
    }
    let td = t.elapsed();
    let okb: usize = ok_frames.iter().map(|f| f.len()).sum();
    println!("{} frames coded, {bad} not parsed; {okb} -> {} bytes ({:.2}%), mismatches {diff}; encode {:.1}s decode {:.1}s", ok_frames.len(), out.len(), 100.0 * out.len() as f64 / okb as f64, te.as_secs_f64(), td.as_secs_f64());
    let _ = total;
}
