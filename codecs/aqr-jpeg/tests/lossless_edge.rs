//! Edge cases of lossless JPEG (SOF3: `lossless`, `lformula`, `lmodule`): what `check` leaves out
//! (data the encoder would not write as it is), the limits of the headers, frames of one sample
//! and of 2 and 16 bits, headers cut, frames beyond the data, damaged inputs.

use std::time::Duration;

use aqr_archive::Evaluate;
use aqr_formula::Apply;
use aqr_jpeg::lformula::{self, MODULE};
use aqr_jpeg::lmodule;
use aqr_jpeg::lossless::{self, Samples, synthetic};
use aqr_testkit::{Refusing, damaged, most};

#[global_allocator]
static ALLOC: Refusing = Refusing(1 << 30);

fn module_fn(a: &Apply, inputs: &[&[u8]]) -> aqr_formula::Result<Vec<u8>> {
    assert_eq!(a.module, MODULE);
    lformula::decode(&a.params, inputs, a.len, 1).map_err(|e| aqr_formula::Error::new(e.0))
}

/// Codes `d`: where it is coded, it comes back as it was.
fn roundtrip(name: &str, d: &[u8]) -> bool {
    let Some(f) = lformula::encode(d, 1) else { return false };
    assert_eq!(f.eval(&module_fn).unwrap(), d, "{name}: roundtrip");
    true
}

fn seg(out: &mut Vec<u8>, m: u8, body: &[u8]) {
    out.extend_from_slice(&[0xFF, m]);
    out.extend_from_slice(&(body.len() as u16 + 2).to_be_bytes());
    out.extend_from_slice(body);
}

/// The headers of a one-component frame with the table of the 17 categories as 5-bit codes, and
/// the scan's own bytes `sos_tail` (predictor, 0, point transform), then `data` and EOI.
fn file(precision: u8, w: u16, h: u16, sos_tail: [u8; 3], restart: u16, data: &[u8]) -> Vec<u8> {
    let mut d = vec![0xFF, 0xD8];
    let mut t = vec![0x00];
    let mut counts = [0u8; 16];
    counts[4] = 17;
    t.extend_from_slice(&counts);
    t.extend(0..=16u8);
    seg(&mut d, 0xC4, &t);
    let mut sof = vec![precision];
    sof.extend_from_slice(&h.to_be_bytes());
    sof.extend_from_slice(&w.to_be_bytes());
    sof.extend_from_slice(&[1, 1, 0x11, 0]);
    seg(&mut d, 0xC3, &sof);
    if restart > 0 {
        seg(&mut d, 0xDD, &restart.to_be_bytes());
    }
    let mut sos = vec![1, 1, 0];
    sos.extend_from_slice(&sos_tail);
    seg(&mut d, 0xDA, &sos);
    d.extend_from_slice(data);
    d.extend_from_slice(&[0xFF, 0xD9]);
    d
}

/// The headers that `parse` refuses: a predictor outside 1..=7, a precision outside 2..=16, a
/// point transform of the whole sample, no area, selection bits, and each of them cut.
#[test]
fn headers_refused() {
    let ok = file(8, 4, 4, [1, 0, 0], 0, &[0; 8]);
    assert!(lossless::parse(&ok, 0).is_ok());
    for p in [0u8, 8, 9, 255] {
        assert!(lossless::parse(&file(8, 4, 4, [p, 0, 0], 0, &[0; 8]), 0).is_err(), "predictor {p}");
    }
    for p in [0u8, 1, 17, 255] {
        assert!(lossless::parse(&file(p, 4, 4, [1, 0, 0], 0, &[0; 8]), 0).is_err(), "precision {p}");
    }
    for al in [8u8, 9, 15] {
        assert!(lossless::parse(&file(8, 4, 4, [1, 0, al], 0, &[0; 8]), 0).is_err(), "point transform {al}");
    }
    assert!(lossless::parse(&file(8, 4, 4, [1, 0, 0x10], 0, &[0; 8]), 0).is_err(), "Ah");
    assert!(lossless::parse(&file(8, 4, 4, [1, 63, 0], 0, &[0; 8]), 0).is_err(), "Se");
    assert!(lossless::parse(&file(8, 0, 4, [1, 0, 0], 0, &[0; 8]), 0).is_err(), "width 0");
    assert!(lossless::parse(&file(8, 4, 0, [1, 0, 0], 0, &[0; 8]), 0).is_err(), "height 0");
    // a restart interval that is not of whole rows
    assert!(lossless::parse(&file(8, 4, 4, [1, 0, 0], 6, &[0; 8]), 0).is_err(), "restart 6 of rows of 4");
    for al in 0..8u8 {
        assert!(lossless::parse(&file(8, 4, 4, [1, 0, al], 0, &[0; 8]), 0).is_ok(), "point transform {al}");
    }
    // cut at every offset: an error, never a panic
    for n in 0..ok.len() {
        let _ = lossless::parse(&ok[..n], 0);
        let _ = lossless::find(&ok[..n]);
        let _ = lformula::encode(&ok[..n], 1);
    }
}

/// The widths of the samples and the extremes of the frame.
#[test]
fn precisions_and_sizes() {
    for precision in [2u8, 3, 8, 15, 16] {
        for (w, h) in [(1u16, 1u16), (1, 300), (300, 1), (7, 5), (301, 97)] {
            for pred in [1u8, 4, 7] {
                let d = synthetic(precision, w, h, &[(1, 1)], pred, 0, 0, w as u32 + pred as u32);
                let hd = lossless::parse(&d, 0).unwrap();
                let s = lossless::decode(&hd, &d[hd.start..hd.end]).unwrap();
                assert_eq!(lossless::encode(&hd, &s).unwrap(), &d[hd.start..hd.end], "{precision}-bit {w}x{h} p{pred}");
                roundtrip("frame", &d);
            }
        }
    }
    // the larger ones are coded
    for precision in [2u8, 16] {
        let d = synthetic(precision, 400, 300, &[(1, 1)], 1, 0, 0, 5);
        assert!(roundtrip("coded", &d), "{precision}-bit");
    }
    // a point transform of up to precision - 1
    for pt in [1u8, 7, 15] {
        let d = synthetic(16, 200, 100, &[(1, 1)], 2, pt, 0, 6);
        assert!(roundtrip("point transform", &d), "point transform {pt}");
    }
}

/// A frame of the headers' size over a few bytes of data: no plane of its size is made, and
/// nothing is coded.
#[test]
fn frames_beyond_the_data() {
    for (w, h) in [(32768u16, 32768u16), (65535, 65535), (65535, 1), (1, 65535), (32769, 32768)] {
        let d = file(12, w, h, [1, 0, 0], 0, &[0x55; 32]);
        aqr_testkit::case(&format!("{w}x{h}"));
        assert!(lformula::encode(&d, 1).is_none(), "{w}x{h}");
        assert!(most() < (64 << 20), "{w}x{h}: {} bytes reserved", most());
        if let Ok(hd) = lossless::parse(&d, 0) {
            assert!(lossless::decode(&hd, &d[hd.start..hd.end]).is_err());
            assert!(most() < (64 << 20), "{w}x{h}: {} bytes reserved decoding", most());
        }
    }
    // with restart intervals (a number of whole rows) and the other component counts
    let mut d = file(12, 32768, 32768, [1, 0, 0], 32768, &[0x55; 32]);
    d.extend_from_slice(&[0xFF, 0xD0]);
    assert!(lformula::encode(&d, 1).is_none());
    let d = synthetic(12, 64, 64, &[(1, 1), (1, 1), (1, 1), (1, 1)], 1, 0, 0, 1);
    let mut hd = lossless::parse(&d, 0).unwrap();
    (hd.width, hd.height) = (32768, 32768);
    aqr_testkit::case("four planes");
    assert!(lossless::decode(&hd, &d[hd.start..hd.end]).is_err());
    assert!(most() < (64 << 20), "{} bytes reserved", most());
}

/// The bytes of the first interval's end: the last data byte before `FF Dn`.
fn interval_ends(d: &[u8], start: usize, end: usize) -> Vec<usize> {
    (start + 1..end)
        .filter(|&i| d[i - 1] != 0xFF && d[i] == 0xFF && (0xD0..=0xD7).contains(&d[i + 1]))
        .map(|i| i - 1)
        .collect()
}

/// What `check` leaves out: the data that decodes, and that the encoder would not write as it
/// is, is not coded (the file is as it was), and it does not take the good streams of the file
/// with it.
#[test]
fn data_not_as_the_encoder_writes_it() {
    let good = synthetic(14, 400, 200, &[(1, 1), (1, 1)], 1, 0, 0, 3);
    let good2 = synthetic(12, 400, 200, &[(1, 1)], 4, 0, 0, 4);
    assert!(roundtrip("good", &good));
    let restarted = synthetic(14, 300, 200, &[(1, 1), (1, 1)], 1, 0, 300, 5);
    let hd = lossless::parse(&restarted, 0).unwrap();
    let ends = interval_ends(&restarted, hd.start, hd.end);
    assert!(ends.len() >= 3, "{} intervals", ends.len() + 1);

    // a padding bit of an interval cleared: the data is refused whole, never coded
    let mut padded = None;
    'find: for &e in &ends {
        for bit in 0..7 {
            let mut v = restarted.clone();
            v[e] &= !(1 << bit);
            if let Err(e) = lossless::decode(&hd, &v[hd.start..hd.end])
                && e.0.contains("padding")
            {
                padded = Some(v);
                break 'find;
            }
        }
    }
    let padded = padded.expect("an interval ends with padding bits");
    assert!(!roundtrip("padding", &padded), "zero padding is coded");

    // restart markers numbered out of order
    let mut v = restarted.clone();
    let at = ends[1] + 2;
    assert_eq!(v[at - 1], 0xFF);
    v[at] = if v[at] == 0xD3 { 0xD4 } else { 0xD3 };
    if lossless::decode(&hd, &v[hd.start..hd.end]).is_ok() {
        assert!(!roundtrip("bad RST numbers", &v), "bad RST numbers are coded");
    }

    // a symbol of the table listed twice: whichever code the encoder writes is coded, the data
    // of neither is lost
    let mut d = vec![0xFF, 0xD8];
    let mut t = vec![0x00];
    let mut counts = [0u8; 16];
    counts[4] = 16;
    counts[6] = 1;
    t.extend_from_slice(&counts);
    t.extend((0..16u8).chain([0]));
    seg(&mut d, 0xC4, &t);
    seg(&mut d, 0xC3, &[8, 0, 200, 1, 144, 1, 1, 0x11, 0]);
    seg(&mut d, 0xDA, &[1, 1, 0, 1, 0, 0]);
    let start = d.len();
    d.extend_from_slice(&[0xFF, 0xD9]);
    let mut h = lossless::parse(&d, 0).unwrap();
    let planes = vec![(0..200 * 400u32).map(|k| ((k / 400 + k % 400 / 40) % 4) as u16).collect::<Vec<_>>()];
    let s = Samples { planes };
    for last in [false, true] {
        h.last_code = last;
        let data = lossless::encode(&h, &s).unwrap();
        let mut v = d.clone();
        v.splice(start..start, data);
        assert!(roundtrip(if last { "last code" } else { "first code" }, &v));
    }

    // a corrupt stream among good ones: the others are coded (the corrupt one is kept as it is)
    let mut corrupt = synthetic(14, 400, 200, &[(1, 1), (1, 1)], 1, 0, 0, 9);
    let ch = lossless::parse(&corrupt, 0).unwrap();
    let mid = (ch.start + ch.end) / 2;
    corrupt.truncate(mid);
    corrupt.extend_from_slice(&[0xFF, 0xD9]);
    let mut v = good.clone();
    v.extend_from_slice(b" gap ");
    v.extend_from_slice(&corrupt);
    v.extend_from_slice(b" gap ");
    v.extend_from_slice(&good2);
    v.extend_from_slice(&padded);
    let f = lformula::encode(&v, 1).expect("the good streams are coded");
    assert_eq!(f.eval(&module_fn).unwrap(), v);
    let coded: usize = f.streams.iter().map(|s| s.data.len()).sum();
    assert!(coded < v.len() * 3 / 4, "{coded} of {}", v.len());
}

/// Damage to the file and to the inputs of the application: no panic, hang or huge allocation;
/// what is coded comes back.
#[test]
fn damaged_inputs() {
    let d = synthetic(14, 64, 40, &[(1, 1), (1, 1)], 1, 0, 64, 11);
    damaged(&d, 150, 140, Duration::from_secs(20), |v| {
        if let Some(f) = lformula::encode(v, 1) {
            assert_eq!(f.eval(&module_fn).unwrap(), v);
        }
    });
    let f = lformula::encode(&d, 1).expect("coded");
    let ins: Vec<Vec<u8>> = f.streams.iter().map(|s| s.data.to_vec()).collect();
    assert_eq!(ins.len(), 4);
    let run = |ins: &[Vec<u8>]| {
        let r: Vec<&[u8]> = ins.iter().map(|v| &v[..]).collect();
        let _ = lmodule::decode(&[], &r, d.len() as u64, 1);
    };
    for k in 0..4 {
        damaged(&ins[k], 80, 64, Duration::from_secs(20), |v| {
            let mut c = ins.clone();
            c[k] = v.to_vec();
            run(&c)
        });
    }
}
