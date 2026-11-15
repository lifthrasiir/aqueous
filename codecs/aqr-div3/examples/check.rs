//! Codes the video frames of an AVI (the chunks of its DivX 3 stream), decodes them again and
//! compares: `aqr-div3-check FILE.avi [FIRST [COUNT]]`. Reports the sizes by frame kind.
use aqr_div3::{Codec, FrameKind};
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let data = std::fs::read(&args[1]).expect("read");
    let first: usize = args.get(2).map_or(0, |s| s.parse().unwrap());
    let count: usize = args.get(3).map_or(usize::MAX, |s| s.parse().unwrap());
    let v = aqr_div3::formula::video(&data).expect("no DivX 3 video");
    let (w, h) = (v.width as usize, v.height as usize);
    let all: Vec<&[u8]> = v.frames.iter().map(|r| &data[r.clone()]).collect();
    let fr: Vec<&[u8]> = all.iter().skip(first).take(count).copied().collect();
    println!("{w}x{h}, {} frames of {}", fr.len(), all.len());

    let t = Instant::now();
    let mut codec = Codec::new(w, h);
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
    let mut codec = Codec::new(w, h);
    let mut dec = aqr_div3::Decoder::new(aqr_div3::SliceSource::new(&out));
    let mut bad = 0;
    for (i, f) in fr.iter().enumerate() {
        match codec.decode(&mut dec, f.len(), Vec::new()) {
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
    println!(
        "total {total} -> {} ({:.2}%)  mismatches {bad}  encode {:.1}s decode {:.1}s",
        out.len(),
        100.0 * out.len() as f64 / total as f64,
        te.as_secs_f64(),
        td.as_secs_f64()
    );
}
