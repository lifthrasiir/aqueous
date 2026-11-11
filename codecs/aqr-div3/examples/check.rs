//! Codes the video frames of an AVI (`00dc` chunks of a DIV3 stream), decodes them again and
//! compares: `aqr-div3-check FILE.avi [FIRST [COUNT]]`. Reports the sizes by frame kind.
use aqr_div3::{Codec, FrameKind};
use std::time::Instant;

fn frames(data: &[u8]) -> Vec<&[u8]> {
    let mi = data.windows(4).position(|w| w == b"movi").expect("movi") + 4;
    let mut pos = mi;
    let mut out = Vec::new();
    while pos + 8 <= data.len() {
        let id = &data[pos..pos + 4];
        let sz = u32::from_le_bytes(data[pos + 4..pos + 8].try_into().unwrap()) as usize;
        if id == b"LIST" {
            pos += 12;
            continue;
        }
        let ok = id == b"JUNK" || (id[0].is_ascii_digit() && id[1].is_ascii_digit() && matches!(&id[2..], b"dc" | b"wb" | b"db"));
        if !ok || pos + 8 + sz > data.len() {
            break;
        }
        if id == b"00dc" {
            out.push(&data[pos + 8..pos + 8 + sz]);
        }
        pos += 8 + sz + (sz & 1);
    }
    out
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let data = std::fs::read(&args[1]).expect("read");
    let first: usize = args.get(2).map_or(0, |s| s.parse().unwrap());
    let count: usize = args.get(3).map_or(usize::MAX, |s| s.parse().unwrap());
    let all = frames(&data);
    let fr: Vec<&[u8]> = all.iter().skip(first).take(count).copied().collect();
    println!("{} frames of {}", fr.len(), all.len());

    let t = Instant::now();
    let mut codec = Codec::new(640, 352);
    let mut enc = aqr_div3::Encoder::new();
    let mut sizes = Vec::new();
    let mut kinds = Vec::new();
    let mut last = 0;
    for f in &fr {
        let k = codec.encode(&mut enc, f);
        kinds.push(k);
        let now = enc.len_so_far();
        sizes.push(now - last);
        last = now;
    }
    let out = enc.finish();
    let te = t.elapsed();

    let t = Instant::now();
    let mut codec = Codec::new(640, 352);
    let mut dec = aqr_div3::Decoder::new(&out);
    let mut bad = 0;
    for (i, f) in fr.iter().enumerate() {
        match codec.decode(&mut dec, f.len()) {
            Ok(v) if v == *f => {}
            _ => {
                bad += 1;
                if bad < 5 {
                    println!("frame {} differs", first + i);
                }
            }
        }
    }
    let td = t.elapsed();

    let (mut oi, mut ci, mut op, mut cp, mut or, mut cr) = (0, 0, 0, 0, 0, 0);
    for (i, f) in fr.iter().enumerate() {
        match kinds[i] {
            Some(FrameKind::I) => (oi, ci) = (oi + f.len(), ci + sizes[i]),
            Some(FrameKind::P) => (op, cp) = (op + f.len(), cp + sizes[i]),
            None => (or, cr) = (or + f.len(), cr + sizes[i]),
        }
    }
    let total: usize = fr.iter().map(|f| f.len()).sum();
    println!("I {oi} -> {ci}   P {op} -> {cp}   raw {or} -> {cr}");
    println!("total {total} -> {} ({:.2}%)  mismatches {bad}  encode {:.1}s decode {:.1}s", out.len(), 100.0 * out.len() as f64 / total as f64, te.as_secs_f64(), td.as_secs_f64());
}
