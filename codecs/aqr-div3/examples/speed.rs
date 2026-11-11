//! Where the time goes: `aqr-div3-speed FILE.avi [FRAMES]` times the parse alone (check), the coding, and the decoding of the
//! video frames, in MB/s of the frames' bytes.
use aqr_div3::{Codec, Decoder, Encoder, SliceSource};
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let data = std::fs::read(&args[1]).unwrap();
    let n: usize = args.get(2).map_or(20000, |s| s.parse().unwrap());
    let chunk: usize = args.get(3).map_or(usize::MAX, |s| s.parse::<usize>().unwrap() << 20);
    let v = aqr_div3::formula::video(&data).unwrap();
    let frames: Vec<&[u8]> = v.frames.iter().take(n).map(|r| &data[r.clone()]).collect();
    let total: usize = frames.iter().map(|f| f.len()).sum();
    let mb = |s: f64| total as f64 / 1e6 / s;
    let codec = Codec::new(v.width as usize, v.height as usize);
    let t = Instant::now();
    let ok = frames.iter().filter(|f| codec.check(f)).count();
    let tc = t.elapsed().as_secs_f64();
    let t = Instant::now();
    let mut codec = Codec::new(v.width as usize, v.height as usize);
    let mut enc = Encoder::new();
    for f in &frames {
        codec.encode(&mut enc, f);
    }
    let out = enc.finish();
    let te = t.elapsed().as_secs_f64();
    let t = Instant::now();
    let mut codec = Codec::new(v.width as usize, v.height as usize);
    let mut dec = Decoder::new(SliceSource::new(&out));
    let mut buf = Vec::with_capacity(5 << 20);
    for f in &frames {
        buf = codec.decode(&mut dec, f.len(), buf).unwrap();
        assert_eq!(&buf[..], *f);
    }
    let td = t.elapsed().as_secs_f64();
    // the size with a restart every `chunk` bytes of frames
    let mut tot = 0usize;
    let (mut codec2, mut enc2, mut acc) = (Codec::new(v.width as usize, v.height as usize), Encoder::new(), 0usize);
    for f in &frames {
        codec2.encode(&mut enc2, f);
        acc += f.len();
        if acc >= chunk {
            tot += std::mem::replace(&mut enc2, Encoder::new()).finish().len();
            codec2 = Codec::new(v.width as usize, v.height as usize);
            acc = 0;
        }
    }
    tot += enc2.finish().len();
    println!("chunked every {} MB: {} bytes ({:+.3}% against one stream)", chunk >> 20, tot, 100.0 * (tot as f64 / out.len() as f64 - 1.0));
    println!("{} frames, {:.1} MB, {ok} parse; check {:.2}s ({:.1} MB/s)  encode incl. check {:.2}s ({:.1} MB/s)  decode {:.2}s ({:.1} MB/s); {} -> {}", frames.len(), total as f64 / 1e6, tc, mb(tc), te, mb(te), td, mb(td), total, out.len());
}
