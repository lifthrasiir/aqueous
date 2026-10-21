//! LMS stage alone over a file's residuals: cycles per residual (at 4.4 GHz), both ways.
#![allow(clippy::needless_range_loop)]
use aqr_flac::{
    bits, flac,
    model::{self, Lms},
};

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let d = std::fs::read(&path).unwrap();
    let (si, start) = flac::parse_metadata(&d).unwrap();
    let pd = bits::padded(&d);
    let mut chans: [Vec<i32>; 2] = [Vec::new(), Vec::new()];
    let mut p = start;
    let mut f = flac::Frame::default();
    while let Some(e) = flac::parse_frame(&pd, p, d.len(), si.bps, &mut f) {
        p = e;
        for c in 0..2.min(f.channels) {
            chans[c].extend_from_slice(&f.res[c]);
        }
    }
    let n = chans[0].len() + chans[1].len();
    let orig = chans.clone();
    for _ in 0..3 {
        let t = std::time::Instant::now();
        let mut st = [Lms::default(), Lms::default()];
        let [c0, c1] = &mut chans;
        let [s0, s1] = &mut st;
        for (b0, b1) in c0.chunks_mut(4096).zip(c1.chunks_mut(4096)) {
            model::lms_run2::<false>(s0, b0, s1, b1);
        }
        let te = t.elapsed().as_secs_f64();
        let t = std::time::Instant::now();
        let mut st = [Lms::default(), Lms::default()];
        let [c0, c1] = &mut chans;
        let [s0, s1] = &mut st;
        for (b0, b1) in c0.chunks_mut(4096).zip(c1.chunks_mut(4096)) {
            model::lms_run2::<true>(s0, b0, s1, b1);
        }
        let td = t.elapsed().as_secs_f64();
        assert!(chans == orig);
        println!("2ch: enc {:.2} dec {:.2} cycles/residual", te * 4.4e9 / n as f64, td * 4.4e9 / n as f64);
        let t = std::time::Instant::now();
        let mut st = [Lms::default(), Lms::default()];
        for (c, s) in chans.iter_mut().zip(&mut st) {
            for blk in c.chunks_mut(4096) {
                s.run::<false>(blk);
            }
        }
        let te = t.elapsed().as_secs_f64();
        let t = std::time::Instant::now();
        let mut st = [Lms::default(), Lms::default()];
        for (c, s) in chans.iter_mut().zip(&mut st) {
            for blk in c.chunks_mut(4096) {
                s.run::<true>(blk);
            }
        }
        let td = t.elapsed().as_secs_f64();
        assert!(chans == orig);
        println!("enc {:.2} dec {:.2} cycles/residual", te * 4.4e9 / n as f64, td * 4.4e9 / n as f64);
    }
}
