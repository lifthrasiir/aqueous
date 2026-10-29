//! Images through the formula: BMP, PNM, TIFF and DICOM files taken apart, headerless rasters
//! and 16-bit samples after a header telling their size, evaluated back through the modules,
//! with trailers, cuts and corrupted inputs.

use aqr_formula::{Apply, Formula, Node};
use aqr_raw::container::{self, Layout};
use aqr_raw::formula::{self, raster, raster16};
use aqr_raw::image::raster::Format;

fn eval(f: &Formula) -> aqr_formula::Result<Vec<u8>> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| {
        assert!(a.resources.is_empty());
        let r = match a.module {
            m if m == raster::MODULE => raster::decode(&a.params, inputs, a.len, 1),
            m if m == raster16::MODULE => raster16::decode(&a.params, inputs, a.len),
            m => panic!("{m}"),
        };
        r.map_err(|e| aqr_formula::Error::new(e.0))
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

/// 16-bit samples of a smooth picture with noise and edges, of 12 bits, `ch` per pixel.
fn picture16(w: usize, h: usize, ch: usize, big: bool) -> Vec<u8> {
    let mut seed = 0x2545_f491u32;
    let mut v = Vec::with_capacity(2 * w * h * ch);
    for y in 0..h {
        for x in 0..w {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let base = (x * 5 + y * 3) as u32 + if (x / 20 + y / 20) % 2 == 0 { 300 } else { 2000 };
            for c in 0..ch {
                let s = ((base + c as u32 * 100 + (seed >> (24 - c)) % 40) & 0xfff) as u16;
                v.extend_from_slice(&if big { s.to_be_bytes() } else { s.to_le_bytes() });
            }
        }
    }
    v
}

/// A DICOM file of `frames` frames of `w` × `h` samples of `bits` bits, `ch` per pixel: with
/// the preamble and the file meta information, explicit VR little-endian, or a data set from
/// the start, implicit VR little-endian.
fn dicom(w: usize, h: usize, ch: usize, frames: usize, bits: usize, preamble: bool) -> Vec<u8> {
    let explicit = preamble;
    let el = |d: &mut Vec<u8>, g: u16, e: u16, vr: &[u8; 2], v: &[u8]| {
        d.extend_from_slice(&g.to_le_bytes());
        d.extend_from_slice(&e.to_le_bytes());
        if !explicit && g != 2 {
            d.extend_from_slice(&(v.len() as u32).to_le_bytes());
        } else if matches!(vr, b"OB" | b"OW") {
            d.extend_from_slice(vr);
            d.extend_from_slice(&[0, 0]);
            d.extend_from_slice(&(v.len() as u32).to_le_bytes());
        } else {
            d.extend_from_slice(vr);
            d.extend_from_slice(&(v.len() as u16).to_le_bytes());
        }
        d.extend_from_slice(v);
    };
    let mut d = Vec::new();
    if preamble {
        d.resize(128, 0);
        d.extend_from_slice(b"DICM");
        let mut meta = Vec::new();
        el(&mut meta, 2, 1, b"OB", &[0, 1]);
        el(&mut meta, 2, 0x10, b"UI", b"1.2.840.10008.1.2.1\0");
        el(&mut d, 2, 0, b"UL", &(meta.len() as u32).to_le_bytes());
        d.extend_from_slice(&meta);
    }
    el(&mut d, 8, 0x60, b"CS", b"MR");
    el(&mut d, 0x28, 2, b"US", &(ch as u16).to_le_bytes());
    el(&mut d, 0x28, 8, b"IS", format!("{frames} ").as_bytes());
    el(&mut d, 0x28, 0x10, b"US", &(h as u16).to_le_bytes());
    el(&mut d, 0x28, 0x11, b"US", &(w as u16).to_le_bytes());
    el(&mut d, 0x28, 0x100, b"US", &(bits as u16).to_le_bytes());
    let px = match bits {
        8 => picture(w, h * frames, ch),
        _ => picture16(w, h * frames, ch, false),
    };
    el(&mut d, 0x7fe0, 0x10, b"OW", &px);
    d
}

/// 16-bit samples after a header of 240 bytes telling their size (as silesia's `x-ray`): big-
/// endian 16-bit width and height at 4.
fn sized(w: usize, h: usize, big: bool) -> Vec<u8> {
    let mut d = vec![0xd0, 1, 0, 0x10];
    d.extend_from_slice(&(w as u16).to_be_bytes());
    d.extend_from_slice(&(h as u16).to_be_bytes());
    d.resize(240, 0);
    d.extend_from_slice(&picture16(w, h, 1, big));
    d
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

/// An uncompressed TIFF file: its IFD first (tags of SHORT values, BitsPerSample's and the
/// strips' at the end of the IFD), then the pixels in two strips.
fn tiff(w: usize, h: usize, ch: usize, big: bool) -> Vec<u8> {
    let (u16b, u32b) = if big {
        (u16::to_be_bytes as fn(u16) -> [u8; 2], u32::to_be_bytes as fn(u32) -> [u8; 4])
    } else {
        (u16::to_le_bytes as fn(u16) -> [u8; 2], u32::to_le_bytes as fn(u32) -> [u8; 4])
    };
    let n = 9;
    let extra = 8 + 2 + 12 * n + 4;
    let px_at = extra + 2 * ch + 8 + 8;
    let half = h / 2 * w * ch;
    let mut d = if big { b"MM\0*".to_vec() } else { b"II*\0".to_vec() };
    d.extend_from_slice(&u32b(8));
    d.extend_from_slice(&u16b(n as u16));
    // a tag of its value or offset, 4 bytes
    let tag = |d: &mut Vec<u8>, t: u16, ty: u16, count: u32, v: Vec<u8>| {
        d.extend_from_slice(&u16b(t));
        d.extend_from_slice(&u16b(ty));
        d.extend_from_slice(&u32b(count));
        d.extend_from_slice(&v);
    };
    let short = |v: usize| [u16b(v as u16), [0, 0]].concat();
    let long = |v: usize| u32b(v as u32).to_vec();
    tag(&mut d, 256, 3, 1, short(w));
    tag(&mut d, 257, 4, 1, long(h));
    let bits = match ch {
        1 => short(8),
        2 => [u16b(8), u16b(8)].concat(),
        _ => long(extra),
    };
    tag(&mut d, 258, 3, ch as u32, bits);
    tag(&mut d, 259, 3, 1, short(1));
    tag(&mut d, 262, 3, 1, short(if ch < 3 { 1 } else { 2 }));
    tag(&mut d, 273, 4, 2, long(extra + 2 * ch));
    tag(&mut d, 277, 3, 1, short(ch));
    tag(&mut d, 279, 4, 2, long(extra + 2 * ch + 8));
    tag(&mut d, 284, 3, 1, short(1));
    d.extend_from_slice(&u32b(0));
    for _ in 0..ch {
        d.extend_from_slice(&u16b(8));
    }
    d.extend_from_slice(&u32b(px_at as u32));
    d.extend_from_slice(&u32b((px_at + half) as u32));
    d.extend_from_slice(&u32b(half as u32));
    d.extend_from_slice(&u32b((h * w * ch - half) as u32));
    assert_eq!(d.len(), px_at);
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
            v.push((format!("tiff {w}x{h} {ch}"), tiff(w, h, ch, false)));
            v.push((format!("tiff {w}x{h} {ch}, big-endian"), tiff(w, h, ch, true)));
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
fn dicom_files() {
    for (w, h, ch, frames, bits) in [(64, 48, 1, 3, 16), (101, 7, 3, 1, 16), (64, 48, 1, 2, 8), (33, 20, 3, 1, 8)] {
        for preamble in [false, true] {
            let name = format!("dicom {w}x{h}x{ch} {frames} frames {bits} bits, preamble {preamble}");
            let d = dicom(w, h, ch, frames, bits, preamble);
            let c = container::detect(&d).expect(&name);
            assert_eq!(c.name, "dicom");
            assert_eq!(c.start + c.len, d.len(), "{name}");
            let f = check(&name, &d).expect(&name);
            let [a] = f.applies()[..] else { panic!("{name}") };
            assert_eq!(a.module, if bits == 8 { raster::MODULE } else { raster16::MODULE });
            // pixel data of another size than the image's: not taken
            let mut cut = d.clone();
            cut.truncate(d.len() - 1);
            let at = cut.len() - c.len - 4;
            cut[at..at + 4].copy_from_slice(&(c.len as u32 - 1).to_le_bytes());
            assert!(container::detect(&cut).is_none(), "{name}");
        }
    }
}

#[test]
fn sized_samples() {
    for (w, h, big) in [(300, 200, false), (64, 1000, true), (1000, 64, false)] {
        let d = sized(w, h, big);
        let c = container::sized(&d).unwrap();
        let Layout::Raster16(fmt) = c.layout else { panic!() };
        assert_eq!((fmt.width, fmt.height, fmt.big_endian, c.start), (w, h, big, 240));
        let f = formula::encode_sized(&d, 1).unwrap();
        assert!(eval(&f).unwrap() == d);
        // (cut by a byte, the samples are still smooth read the other way round from the byte
        // before: taken so, as any reading, they come back)
        if let Some(f) = formula::encode_sized(&d[..d.len() - 1], 1) {
            assert!(eval(&f).unwrap() == d[..d.len() - 1]);
        }
    }
    assert!(container::sized(&[0; 100_000]).is_none());
    // random samples after a header telling their size: not an image
    let mut d = sized(300, 200, false);
    let mut seed = 1u32;
    for b in &mut d[240..] {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        *b = (seed >> 24) as u8;
    }
    assert!(container::sized(&d).is_none());
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
            Node::Slice { stream, .. } => f.streams[*stream].data.to_vec(),
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
