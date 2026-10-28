//! Builders of synthetic PSD files, shared by the tests.

#![allow(dead_code)]

use aqr_psd::packbits::{self, Encoder};

/// A layer: rectangle, channels (id, planar data; a -2 channel is the user mask, of
/// `mask`'s rectangle: top, left, width, height, default value).
pub struct Layer {
    pub top: i32,
    pub left: i32,
    pub w: usize,
    pub h: usize,
    pub channels: Vec<(i16, Vec<u8>)>,
    pub mask: Option<(i32, i32, usize, usize, u8)>,
}

pub fn be16(o: &mut Vec<u8>, v: u16) {
    o.extend_from_slice(&v.to_be_bytes());
}

pub fn be32(o: &mut Vec<u8>, v: u32) {
    o.extend_from_slice(&v.to_be_bytes());
}

/// PackBits data of `rows` rows of `w` bytes: counts, then rows. `mangle` (if not 0)
/// re-encodes every `mangle`th row from row 1 on all as literals, which no known encoder does.
pub fn rle(data: &[u8], w: usize, enc: Encoder, mangle: usize) -> Vec<u8> {
    let rows = data.len() / w;
    let mut enc_rows: Vec<Vec<u8>> = Vec::new();
    for r in 0..rows {
        let row = &data[r * w..(r + 1) * w];
        let mut e = Vec::new();
        if mangle > 0 && r % mangle == 1 % mangle {
            for chunk in row.chunks(128) {
                e.push(chunk.len() as u8 - 1);
                e.extend_from_slice(chunk);
            }
        } else {
            packbits::encode(enc, row, &mut e);
        }
        enc_rows.push(e);
    }
    let mut o = Vec::new();
    for e in &enc_rows {
        be16(&mut o, e.len() as u16);
    }
    for e in &enc_rows {
        o.extend_from_slice(e);
    }
    o
}

pub fn image(w: usize, h: usize, seed: u32) -> Vec<u8> {
    (0..w * h)
        .map(|i| {
            let (x, y) = ((i % w) as u32, (i / w) as u32);
            // Smooth areas, flat areas and some noise.
            if x < w as u32 / 3 {
                200
            } else {
                ((x * 3 + y * 5 + seed) % 256) as u8 ^ ((x * y + seed).wrapping_mul(2654435761) >> 29) as u8
            }
        })
        .collect()
}

/// `raw`: which layers are stored raw (the others RLE).
pub fn psd(width: usize, height: usize, layers: &[Layer], enc: Encoder, mangle: usize, raw: &[usize]) -> Vec<u8> {
    let mut o = Vec::new();
    o.extend_from_slice(b"8BPS");
    be16(&mut o, 1);
    o.extend_from_slice(&[0; 6]);
    be16(&mut o, 3);
    be32(&mut o, height as u32);
    be32(&mut o, width as u32);
    be16(&mut o, 8);
    be16(&mut o, 3);
    be32(&mut o, 0); // color mode data
    // One resource.
    let mut res = Vec::new();
    res.extend_from_slice(b"8BIM");
    be16(&mut res, 1005);
    res.extend_from_slice(&[0, 0]);
    be32(&mut res, 16);
    res.extend_from_slice(&[0, 72, 0, 0, 0, 1, 0, 1, 0, 72, 0, 0, 0, 1, 0, 1]);
    be32(&mut o, res.len() as u32);
    o.extend_from_slice(&res);
    // Layer info.
    let mut li = Vec::new();
    be16(&mut li, layers.len() as u16);
    let mut chan_data = Vec::new();
    for (k, l) in layers.iter().enumerate() {
        be32(&mut li, l.top as u32);
        be32(&mut li, l.left as u32);
        be32(&mut li, (l.top + l.h as i32) as u32);
        be32(&mut li, (l.left + l.w as i32) as u32);
        be16(&mut li, l.channels.len() as u16);
        for (id, data) in &l.channels {
            let mut c = Vec::new();
            let cw = if *id == -2 { l.mask.unwrap().2 } else { l.w };
            if raw.contains(&k) {
                be16(&mut c, 0);
                c.extend_from_slice(data);
            } else {
                be16(&mut c, 1);
                c.extend_from_slice(&rle(data, cw, enc, mangle));
            }
            be16(&mut li, *id as u16);
            be32(&mut li, c.len() as u32);
            chan_data.push(c);
        }
        li.extend_from_slice(b"8BIMnorm");
        li.extend_from_slice(&[255, 0, 8, 0]);
        let mut extra = Vec::new();
        match l.mask {
            Some((top, left, mw, mh, def)) => {
                be32(&mut extra, 20);
                for v in [top, left, top + mh as i32, left + mw as i32] {
                    be32(&mut extra, v as u32);
                }
                extra.extend_from_slice(&[def, 0, 0, 0]);
            }
            None => be32(&mut extra, 0),
        }
        be32(&mut extra, 0); // blending ranges
        let name = format!("Layer {k}");
        extra.push(name.len() as u8);
        extra.extend_from_slice(name.as_bytes());
        while (extra.len() - 8 - if l.mask.is_some() { 20 } else { 0 }) % 4 != 0 {
            extra.push(0);
        }
        extra.extend_from_slice(b"8BIMluni");
        be32(&mut extra, 4);
        be32(&mut extra, 0);
        be32(&mut li, extra.len() as u32);
        li.extend_from_slice(&extra);
    }
    for c in chan_data {
        li.extend_from_slice(&c);
    }
    if li.len() % 2 == 1 {
        li.push(0);
    }
    let mut lm = Vec::new();
    be32(&mut lm, li.len() as u32);
    lm.extend_from_slice(&li);
    be32(&mut lm, 0); // global mask
    be32(&mut o, lm.len() as u32);
    o.extend_from_slice(&lm);
    // Composite: the last layer's planes over white, cropped.
    be16(&mut o, 1);
    let mut planes = Vec::new();
    for k in 0..3 {
        planes.extend(image(width, height, 77 + k));
    }
    o.extend_from_slice(&rle(&planes, width, enc, 0));
    o
}

pub fn layers(w: usize, h: usize, alpha: bool) -> Vec<Layer> {
    let mut v = Vec::new();
    for k in 0..2 {
        let mut channels = vec![(0, image(w, h, k)), (1, image(w, h, k + 10)), (2, image(w, h, k + 20))];
        if alpha {
            let a: Vec<u8> = image(w, h, k + 30).iter().map(|&b| if b < 90 { 0 } else { b }).collect();
            channels.insert(0, (-1, a));
        }
        v.push(Layer { top: k as i32 * 3, left: -(k as i32) * 5, w, h, channels, mask: None });
    }
    v
}

/// A file whose composite is its layers rendered (so the composite is coded with the hint).
pub fn hinted_file(w: usize, h: usize) -> Vec<u8> {
    let file = psd(w, h, &layers(w, h, true), packbits::NO_TILES, 0, &[]);
    let p = aqr_psd::parse(&file).unwrap();
    let rgb = aqr_psd::composite::render(&p, &file, false, &mut Default::default()).unwrap();
    let mut planes = Vec::new();
    for k in 0..3 {
        planes.extend(rgb.iter().skip(k).step_by(3));
    }
    let mut out = file[..p.composite.start].to_vec();
    be16(&mut out, 1);
    out.extend_from_slice(&rle(&planes, w, packbits::NO_TILES, 0));
    out
}
