//! Round trips of synthetic PSD files built here, so that the tests need no corpus.

use psd_recons::packbits::{self, Encoder};

/// A layer: rectangle, channels (id, planar data), compression of the channels.
struct Layer {
    top: i32,
    left: i32,
    w: usize,
    h: usize,
    channels: Vec<(i16, Vec<u8>)>,
}

fn be16(o: &mut Vec<u8>, v: u16) {
    o.extend_from_slice(&v.to_be_bytes());
}

fn be32(o: &mut Vec<u8>, v: u32) {
    o.extend_from_slice(&v.to_be_bytes());
}

/// PackBits data of `rows` rows of `w` bytes: counts, then rows. `mangle` re-encodes some
/// rows all as literals, which no known encoder does.
fn rle(data: &[u8], w: usize, enc: Encoder, mangle: bool) -> Vec<u8> {
    let rows = data.len() / w;
    let mut enc_rows: Vec<Vec<u8>> = Vec::new();
    for r in 0..rows {
        let row = &data[r * w..(r + 1) * w];
        let mut e = Vec::new();
        if mangle && r % 3 == 1 {
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

fn image(w: usize, h: usize, seed: u32) -> Vec<u8> {
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
fn psd(width: usize, height: usize, layers: &[Layer], enc: Encoder, mangle: bool, raw: &[usize]) -> Vec<u8> {
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
            if raw.contains(&k) {
                be16(&mut c, 0);
                c.extend_from_slice(data);
            } else {
                be16(&mut c, 1);
                c.extend_from_slice(&rle(data, l.w, enc, mangle));
            }
            be16(&mut li, *id as u16);
            be32(&mut li, c.len() as u32);
            chan_data.push(c);
        }
        li.extend_from_slice(b"8BIMnorm");
        li.extend_from_slice(&[255, 0, 8, 0]);
        let mut extra = Vec::new();
        be32(&mut extra, 0); // mask
        be32(&mut extra, 0); // blending ranges
        let name = format!("Layer {k}");
        extra.push(name.len() as u8);
        extra.extend_from_slice(name.as_bytes());
        while (extra.len() - 8) % 4 != 0 {
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
    o.extend_from_slice(&rle(&planes, width, enc, false));
    o
}

fn layers(w: usize, h: usize, alpha: bool) -> Vec<Layer> {
    let mut v = Vec::new();
    for k in 0..2 {
        let mut channels = vec![(0, image(w, h, k)), (1, image(w, h, k + 10)), (2, image(w, h, k + 20))];
        if alpha {
            let a: Vec<u8> = image(w, h, k + 30).iter().map(|&b| if b < 90 { 0 } else { b }).collect();
            channels.insert(0, (-1, a));
        }
        v.push(Layer { top: k as i32 * 3, left: -(k as i32) * 5, w, h, channels });
    }
    v
}

fn check(file: &[u8]) {
    let packed = psd_recons::compress(file);
    assert_eq!(packed[5], 1, "not coded");
    assert!(packed.len() < file.len(), "{} >= {}", packed.len(), file.len());
    assert_eq!(psd_recons::decompress(&packed).unwrap(), file);
}

#[test]
fn photoshop_rle() {
    check(&psd(300, 40, &layers(300, 40, true), packbits::NO_TILES, false, &[]));
}

#[test]
fn tiles_and_exceptions() {
    let enc = Encoder::Photoshop { phase: 77, period: 368 };
    check(&psd(500, 30, &layers(500, 30, true), enc, true, &[]));
}

#[test]
fn greedy_writer_and_raw_layer() {
    check(&psd(260, 35, &layers(260, 35, false), Encoder::Greedy { min_run: 3, split_rest: true }, false, &[0]));
}

#[test]
fn not_a_psd_is_stored() {
    let junk: Vec<u8> = (0..5000u32).map(|i| (i * 7 % 13) as u8).collect();
    let packed = psd_recons::compress(&junk);
    assert_eq!(packed[5], 0);
    assert_eq!(psd_recons::decompress(&packed).unwrap(), junk);
}

#[test]
fn corrupt_input_is_an_error() {
    let file = psd(300, 40, &layers(300, 40, true), packbits::NO_TILES, false, &[]);
    let packed = psd_recons::compress(&file);
    for cut in [7, 20, packed.len() / 2, packed.len() - 1] {
        assert!(psd_recons::decompress(&packed[..cut]).is_err());
    }
    let mut flipped = packed.clone();
    let k = flipped.len() - 10;
    flipped[k] ^= 0x40;
    assert!(psd_recons::decompress(&flipped).is_err());
}

#[test]
fn duplicate_layers_and_gray_layer() {
    let (w, h) = (200, 30);
    let mut ls = layers(w, h, true);
    // A copy of the first layer, moved; and a gray layer (equal color planes).
    let mut copy = Layer { top: 7, left: 2, w, h, channels: ls[0].channels.clone() };
    copy.channels.swap(1, 2);
    ls.push(copy);
    let g = image(w, h, 99);
    ls.push(Layer { top: 0, left: 0, w, h, channels: vec![(0, g.clone()), (1, g.clone()), (2, g)] });
    let file = psd(w, h, &ls, packbits::NO_TILES, false, &[]);
    check(&file);
    // Copies cost next to nothing.
    let single = psd(w, h, &ls[..2], packbits::NO_TILES, false, &[]);
    let (a, b) = (psd_recons::compress(&file).len(), psd_recons::compress(&single).len());
    assert!(a < b + b / 4, "{a} vs {b}");
}

#[test]
fn same_bytes_other_size_is_no_copy() {
    // Raw channels with equal bytes but different sizes (100 x 2 and 50 x 4).
    let data = image(100, 2, 5);
    let l = |w, h| Layer { top: 0, left: 0, w, h, channels: vec![(0, data.clone()), (1, data.clone()), (2, data.clone())] };
    check(&psd(100, 4, &[l(100, 2), l(50, 4)], packbits::NO_TILES, false, &[0, 1]));
}

#[test]
fn layer_outside_the_canvas() {
    // Layers right of, below and left of the canvas; the composite is rendered as hint.
    let (w, h) = (120, 20);
    let mut ls = layers(w, h, true);
    for (k, (top, left)) in [(0, 500), (300, 0), (0, -400)].into_iter().enumerate() {
        ls.push(Layer { top, left, w: 50, h: 10, channels: ls[k % 2].channels.iter().map(|(id, d)| (*id, d[..500].to_vec())).collect() });
    }
    // A composite the renderer predicts: the hint is used.
    check(&psd(w, h, &ls, packbits::NO_TILES, false, &[]));
}
