//! Entropy pass alone over a file's LMS errors (two channels interleaved in 4096 blocks):
//! cycles per residual (at 4.4 GHz), both ways.
#![allow(clippy::needless_range_loop)]
use flac_recons::bits::{self, BitReader, FastWriter};
use flac_recons::{codec, flac, model, rans};

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let d = std::fs::read(&path).unwrap();
    let (si, start) = flac::parse_metadata(&d).unwrap();
    let pd = bits::padded(&d);
    let mut chans: [Vec<i32>; 2] = [Vec::new(), Vec::new()];
    let mut p = start;
    let mut f = flac::Frame::default();
    let mut lms = [model::Lms::default(), model::Lms::default()];
    while let Some(e) = flac::parse_frame(&pd, p, d.len(), si.bps, &mut f) {
        p = e;
        for c in 0..2.min(f.channels) {
            lms[c].run::<false>(&mut f.res[c]);
            chans[c].extend_from_slice(&f.res[c]);
        }
    }
    let n = chans[0].len().min(chans[1].len());
    let t = codec::tables();
    for _ in 0..3 {
        let tm = std::time::Instant::now();
        let mut enc = rans::Encoder::with_capacity(2 * n);
        let mut raw = [FastWriter::with_capacity(n * 4), FastWriter::with_capacity(n * 4)];
        let mut avg = [model::Scale::default(); 2];
        for b in (0..n).step_by(4096) {
            let e = (b + 4096).min(n);
            enc.reserve(2 * (e - b));
            raw[0].reserve((e - b) * 8);
            raw[1].reserve((e - b) * 8);
            let mut el = enc.local(t);
            let (mut r0, mut r1) = (raw[0].local(), raw[1].local());
            let (mut a0, mut a1) = (avg[0], avg[1]);
            for i in b..e {
                model::encode_one(&mut a0, 0, chans[0][i], &mut el, &mut r0);
                model::encode_one(&mut a1, 1, chans[1][i], &mut el, &mut r1);
            }
            avg = [a0, a1];
            enc.put_back(el);
            raw[0].put_back(r0);
            raw[1].put_back(r1);
        }
        let tput = tm.elapsed().as_secs_f64();
        let words = enc.finish(t);
        let tfin = tm.elapsed().as_secs_f64() - tput;
        let [w0, w1] = raw;
        let (b0, b1) = (bits::padded(&w0.finish()), bits::padded(&w1.finish()));
        let tm = std::time::Instant::now();
        let mut dec = rans::Decoder::new(t, &words);
        let mut dl = dec.local();
        let (mut r0, mut r1) = (BitReader::new(&b0, 0), BitReader::new(&b1, 0));
        let (mut a0, mut a1) = (model::Scale::default(), model::Scale::default());
        let mut out = [vec![0i32; n], vec![0i32; n]];
        for i in 0..n {
            out[0][i] = model::decode_one::<true>(&mut a0, 0, &mut dl, &mut r0);
            out[1][i] = model::decode_one::<true>(&mut a1, 1, &mut dl, &mut r1);
        }
        dec.put_back(dl);
        let td = tm.elapsed().as_secs_f64();
        assert!(out[0][..] == chans[0][..n] && out[1][..] == chans[1][..n]);
        let c = |s: f64| s * 4.4e9 / (2 * n) as f64;
        println!("enc {:.2} + finish {:.2}, dec {:.2} cycles/residual", c(tput), c(tfin), c(td));
    }
}
