//! Images through the formula: BMP and PNM files taken apart and headerless rasters, evaluated
//! back through the module, with trailers, cuts and corrupted inputs.

use aqr_formula::{Apply, Formula, Node};
use aqr_raw::container::{self, Layout};
use aqr_raw::formula::{self, raster};
use aqr_raw::image::raster::Format;

fn eval(f: &Formula) -> aqr_formula::Result<Vec<u8>> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| {
        assert_eq!(a.module, raster::MODULE);
        assert!(a.resources.is_empty());
        raster::decode(&a.params, inputs, a.len, 1).map_err(|e| aqr_formula::Error::new(e.0))
    })
}

fn check(name: &str, d: &[u8]) -> Option<Formula> {
    let f = formula::encode(d, 1)?;
    assert_eq!(f.len(), d.len() as u64, "{name}");
    assert!(eval(&f).unwrap() == d, "{name}");
    Some(f)
}

/// Samples of a smooth picture with noise and edges, `ch` per pixel.
fn picture(w: usize, h: usize, ch: usize) -> Vec<u8> {
    let mut seed = 0x2545_f491u32;
    let mut v = Vec::with_capacity(w * h * ch);
    for y in 0..h {
        for x in 0..w {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let base = (x * 3 + y * 2) as u32 + if (x / 16 + y / 16) % 2 == 0 { 0 } else { 80 };
            for c in 0..ch {
                v.push((base + c as u32 * 20 + (seed >> (28 - c)) % 5) as u8);
            }
        }
    }
    v
}

/// A BMP of `bpp` bits (8, 24, 32) from `ch`-sample pixels, rows padded to 4 bytes,
/// bottom-up unless `top_down`.
fn bmp(w: usize, h: usize, bpp: usize, top_down: bool) -> Vec<u8> {
    let ch = bpp / 8;
    let px = picture(w, h, ch);
    let stride = (w * ch + 3) & !3;
    let palette = if bpp == 8 { 1024 } else { 0 };
    let off = 14 + 40 + palette;
    let mut d = Vec::new();
    d.extend_from_slice(b"BM");
    d.extend_from_slice(&((off + stride * h) as u32).to_le_bytes());
    d.extend_from_slice(&[0; 4]);
    d.extend_from_slice(&(off as u32).to_le_bytes());
    d.extend_from_slice(&40u32.to_le_bytes());
    d.extend_from_slice(&(w as i32).to_le_bytes());
    d.extend_from_slice(&(if top_down { -(h as i32) } else { h as i32 }).to_le_bytes());
    d.extend_from_slice(&1u16.to_le_bytes());
    d.extend_from_slice(&(bpp as u16).to_le_bytes());
    d.extend_from_slice(&[0; 24]);
    for i in 0..palette / 4 {
        d.extend_from_slice(&[i as u8, i as u8, i as u8, 0]);
    }
    for y in 0..h {
        d.extend_from_slice(&px[y * w * ch..(y + 1) * w * ch]);
        d.resize(d.len() + stride - w * ch, 0);
    }
    d
}

fn pnm(w: usize, h: usize, ch: usize) -> Vec<u8> {
    let mut d = match ch {
        1 => format!("P5\n# a comment\n{w} {h}\n255\n").into_bytes(),
        3 => format!("P6 {w}\t{h} 255 ").into_bytes(),
        _ => format!("P7\nWIDTH {w}\nHEIGHT {h}\nDEPTH {ch}\nMAXVAL 255\nTUPLTYPE X\nENDHDR\n").into_bytes(),
    };
    d.extend_from_slice(&picture(w, h, ch));
    d
}

fn images() -> Vec<(String, Vec<u8>)> {
    let mut v = vec![];
    for (w, h) in [(1, 1), (7, 5), (101, 64), (640, 480)] {
        for bpp in [8, 24, 32] {
            v.push((format!("bmp {w}x{h} {bpp}"), bmp(w, h, bpp, false)));
        }
        v.push((format!("bmp {w}x{h} 24, top-down"), bmp(w, h, 24, true)));
        for ch in [1, 2, 3, 4] {
            v.push((format!("pnm {w}x{h} {ch}"), pnm(w, h, ch)));
        }
    }
    v
}

#[test]
fn files() {
    for (name, d) in images() {
        let c = container::detect(&d).expect(&name);
        let f = check(&name, &d).expect(&name);
        let [a] = f.applies()[..] else { panic!("{name}") };
        let Layout::Raster(fmt) = c.layout else { panic!("{name}") };
        assert_eq!(a.params, fmt.to_bytes());
        assert_eq!(c.start + c.len, d.len(), "{name}");
        if d.len() > 100_000 {
            let coded: usize = f.streams.iter().map(|s| s.data.len()).sum();
            assert!(coded < d.len() / 2, "{name}: {coded} of {}", d.len());
        }
    }
}

#[test]
fn trailers_and_cuts() {
    for (name, d) in images() {
        let mut t = d.clone();
        t.extend_from_slice(b"trailing bytes");
        check(&name, &t).expect(&name);
        // The samples are not all there: not coded.
        assert!(formula::encode(&d[..d.len() - 1], 1).is_none(), "{name}");
    }
}

#[test]
fn raw_samples() {
    let px = picture(300, 200, 3);
    let fmt = Format { width: 300, height: 200, channels: 3, stride: 900 };
    let mut d = px.clone();
    d.extend_from_slice(b"rest");
    let f = formula::encode_samples(&d, Layout::Raster(fmt), 1).unwrap();
    assert!(eval(&f).unwrap() == d);
    assert!(formula::encode_samples(&px[..px.len() - 1], Layout::Raster(fmt), 1).is_none());
}

#[test]
fn not_coded() {
    for d in [
        &b"BM"[..],
        b"P5\n1 1\n65535\n\0\0",
        b"P6 0 0 255 ",
        b"P7\nWIDTH 1\nHEIGHT 1\nDEPTH 5\nMAXVAL 255\nENDHDR\n12345",
    ] {
        assert!(formula::encode(d, 1).is_none(), "{d:?}");
    }
}

#[test]
fn corrupt_inputs() {
    let d = bmp(101, 64, 24, false);
    let f = formula::encode(&d, 1).unwrap();
    let a = f.applies()[0];
    let ins: Vec<Vec<u8>> = a
        .inputs
        .iter()
        .map(|n| match n {
            Node::Slice { stream, .. } => f.streams[*stream].data.clone(),
            _ => panic!(),
        })
        .collect();
    let run = |p: &[u8], i: &[Vec<u8>], len: u64| {
        let r: Vec<&[u8]> = i.iter().map(|v| &v[..]).collect();
        raster::decode(p, &r, len, 1)
    };
    assert!(run(&[], &ins, a.len).is_err());
    assert!(run(&a.params[1..], &ins, a.len).is_err());
    assert!(run(&a.params, &ins[..1], a.len).is_err());
    assert!(run(&a.params, &ins, a.len + 1).is_err());
    assert!(run(&a.params, &ins, u64::MAX).is_err());
    for k in 0..ins.len() {
        let mut m = ins.clone();
        let n = m[k].len();
        m[k].truncate(n / 2);
        assert!(run(&a.params, &m, a.len).is_err());
        for i in (0..n).step_by(37) {
            let mut m = ins.clone();
            m[k][i] ^= 0x5a;
            let _ = run(&a.params, &m, a.len);
        }
    }
}
