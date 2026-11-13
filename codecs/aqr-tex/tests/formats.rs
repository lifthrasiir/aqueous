//! The families of formats beyond BC1 to BC5 through the walk: blocks of every layout of theirs
//! (random bits: every mode, reserved ones too), of surfaces in stripes on any number of
//! threads, of odd sizes and mipmaps, given back; smooth textures coded small; corrupted streams
//! turned down.

use aqr_tex::layout::{Format, Layout};
use aqr_tex::texture;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// The coded size of `b`, the blocks of `l`, checked given back on 1 and 4 threads (the streams
/// the same).
fn round(l: &Layout, b: &[u8]) -> usize {
    let (main, apart, _) = texture::encode_any(l, b, 1);
    assert!(texture::decode_any(l, &main, &apart, 1).unwrap() == b, "{l:?}");
    let (m4, a4, _) = texture::encode_any(l, b, 4);
    assert!(m4 == main && a4 == apart, "{l:?}: not the same on 4 threads");
    assert!(texture::decode_any(l, &main, &apart, 4).unwrap() == b, "{l:?} on 4 threads");
    // (cut short, or corrupted: an error or other blocks, never a panic)
    for cut in [main.len() / 2, main.len().saturating_sub(5)] {
        let _ = texture::decode_any(l, &main[..cut], &apart, 2);
    }
    let mut bad = main.clone();
    if let Some(x) = bad.get_mut(main.len() / 3) {
        *x ^= 0x40;
    }
    let _ = texture::decode_any(l, &bad, &apart, 2);
    main.len() + apart.len()
}

/// Random blocks of BC7: of every mode in turn, the reserved one too, some repeated.
fn bc7_random(n: usize, seed: u64) -> Vec<u8> {
    let mut r = Rng(seed | 1);
    let mut out: Vec<u8> = Vec::with_capacity(n * 16);
    for i in 0..n {
        if i > 0 && r.next().is_multiple_of(5) {
            let k = (r.next() as usize % i) * 16;
            let b: [u8; 16] = out[k..k + 16].try_into().unwrap();
            out.extend(b);
            continue;
        }
        let mut b = [0u8; 16];
        for x in b.iter_mut() {
            *x = r.next() as u8;
        }
        let m = (r.next() % 9) as u32;
        b[0] = if m == 8 { 0 } else { (b[0] | 1) << m };
        out.extend(b);
    }
    out
}

/// A smooth RGBA image of `w` × `h`.
fn image(w: usize, h: usize, seed: u64) -> Vec<[u8; 4]> {
    let s = seed as f64;
    (0..w * h)
        .map(|i| {
            let (x, y) = ((i % w) as f64, (i / w) as f64);
            let v = |a: f64, b: f64| (127.5 + 120.0 * ((x * a + y * b) / 40.0 + s).sin()) as u8;
            [v(1.0, 0.3), v(0.4, 1.1), v(-0.7, 0.5), v(0.2, 0.2)]
        })
        .collect()
}

/// BC7 blocks of `img` (`w` × `h`) by mode 6 alone: each channel's least and most values of a
/// block as its endpoints (p-bits 0), its pixels' indices by where they project between them.
fn bc7_encode(img: &[[u8; 4]], w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![];
    for by in 0..h.div_ceil(4) {
        for bx in 0..w.div_ceil(4) {
            let px: Vec<[u8; 4]> =
                (0..16).map(|p| img[(by * 4 + p / 4).min(h - 1) * w + (bx * 4 + p % 4).min(w - 1)]).collect();
            let lo: [u8; 4] = std::array::from_fn(|c| px.iter().map(|v| v[c]).min().unwrap() & !1);
            let hi: [u8; 4] = std::array::from_fn(|c| px.iter().map(|v| v[c]).max().unwrap() & !1);
            let d: [i32; 4] = std::array::from_fn(|c| hi[c] as i32 - lo[c] as i32);
            let l2: i32 = d.iter().map(|v| v * v).sum::<i32>().max(1);
            let mut ix: Vec<u32> = px
                .iter()
                .map(|v| {
                    let t: i32 = (0..4).map(|c| (v[c] as i32 - lo[c] as i32) * d[c]).sum();
                    ((t * 15 + l2 / 2) / l2).clamp(0, 15) as u32
                })
                .collect();
            let (mut e0, mut e1) = (lo, hi);
            // (the anchor's index of 3 bits: below 8, else the endpoints swapped)
            if ix[0] >= 8 {
                (e0, e1) = (e1, e0);
                ix.iter_mut().for_each(|i| *i = 15 - *i);
            }
            let mut v: u128 = 1 << 6;
            let mut at = 7;
            let mut put = |n: u32, x: u32| {
                v |= (x as u128) << at;
                at += n;
            };
            for c in 0..4 {
                put(7, e0[c] as u32 >> 1);
                put(7, e1[c] as u32 >> 1);
            }
            put(1, 0);
            put(1, 0);
            for (p, &i) in ix.iter().enumerate() {
                put(if p == 0 { 3 } else { 4 }, i);
            }
            out.extend(v.to_le_bytes());
        }
    }
    out
}

#[test]
fn bc7_layouts() {
    for (size, mips, layers, seed) in [((4, 4), 1, 1, 1), ((13, 7), 3, 2, 2), ((64, 64), 7, 1, 3), ((1, 1), 1, 3, 4)] {
        let l = Layout { format: Format::Bc7, width: size.0, height: size.1, mips, layers };
        let b = bc7_random(l.len() as usize / 16, seed);
        round(&l, &b);
    }
}

#[test]
fn bc7_stripes() {
    // a largest mipmap of 4 stripes, its second of one
    let l = Layout { format: Format::Bc7, width: 1024, height: 520, mips: 2, layers: 1 };
    assert!(texture::stripes_of(l.format, l.mip_surfaces().next().unwrap().blocks()) > 1);
    let b = bc7_random(l.len() as usize / 16, 7);
    round(&l, &b);
}

#[test]
fn bc7_smooth_codes_small() {
    let (w, h) = (256, 128);
    let mut blocks = vec![];
    let mut mips = 0;
    let (mut mw, mut mh) = (w, h);
    while mw >= 1 && mh >= 1 && mips < 6 {
        blocks.extend(bc7_encode(&image(mw, mh, mips as u64), mw, mh));
        mips += 1;
        (mw, mh) = (mw / 2, mh / 2);
    }
    let l = Layout { format: Format::Bc7, width: w as u32, height: h as u32, mips, layers: 1 };
    assert_eq!(l.len() as usize, blocks.len());
    let n = round(&l, &blocks);
    let z = zstd::bulk::compress(&blocks, 19).unwrap().len();
    eprintln!("bc7 smooth: {} -> {n} (zstd -19 {z})", blocks.len());
    assert!(n * 10 < z * 9, "{n} against zstd's {z}");
}

fn eval(f: &aqr_formula::Formula) -> Vec<u8> {
    f.eval(&|a: &aqr_formula::Apply, inputs: &[&[u8]]| aqr_tex::formula::decode_any(a, inputs, 2)).unwrap()
}

/// The layout's surfaces (blocks of `b`) as each layer's mipmaps from the largest: the bytes of
/// surface (layer `k`, mipmap `m`).
fn surfaces(l: &Layout, b: &[u8]) -> Vec<Vec<Vec<u8>>> {
    let sizes: Vec<usize> = l.mip_surfaces().map(|s| s.blocks() * l.format.block_bytes()).collect();
    let mut at = 0;
    (0..l.layers)
        .map(|_| {
            sizes
                .iter()
                .map(|&n| {
                    at += n;
                    b[at - n..at].to_vec()
                })
                .collect()
        })
        .collect()
}

/// A KTX file of BC7 blocks `b` of layout `l` (`faces` of each array element; `elements` 0 for
/// none), of a key and value pair, in either byte order.
fn ktx(l: &Layout, b: &[u8], elements: u32, faces: u32, big: bool) -> Vec<u8> {
    let s = surfaces(l, b);
    let u = |v: u32| if big { v.to_be_bytes() } else { v.to_le_bytes() };
    let mut d = b"\xabKTX 11\xbb\r\n\x1a\n".to_vec();
    for v in [0x0403_0201, 0, 1, 0, 0x8e8c, 0x1908, l.width, l.height, 0, elements, faces, l.mips, 12] {
        d.extend(u(v));
    }
    d.extend(u(7));
    d.extend(b"a\0b\0\0\0\0\0");
    for m in 0..l.mips as usize {
        let cube = faces == 6 && elements == 0;
        let n = s[0][m].len() * if cube { 1 } else { l.layers as usize };
        d.extend(u(n as u32));
        for layer in &s {
            d.extend(&layer[m]);
        }
    }
    d
}

/// A KTX 2 file of BC7 blocks `b` of layout `l`, its mipmaps from the smallest in the file.
fn ktx2(l: &Layout, b: &[u8]) -> Vec<u8> {
    let s = surfaces(l, b);
    let mut d = b"\xabKTX 20\xbb\r\n\x1a\n".to_vec();
    for v in [145, 1, l.width, l.height, 0, l.layers, 1, l.mips, 0] {
        d.extend(u32::to_le_bytes(v));
    }
    d.resize(80 + 24 * l.mips as usize, 0);
    d.extend(b"(a descriptor)");
    for m in (0..l.mips as usize).rev() {
        d.resize(d.len().next_multiple_of(16), 0);
        let (at, n) = (d.len(), s[0][m].len() * l.layers as usize);
        for layer in &s {
            d.extend(&layer[m]);
        }
        let i = 80 + 24 * m;
        d[i..i + 8].copy_from_slice(&(at as u64).to_le_bytes());
        d[i + 8..i + 16].copy_from_slice(&(n as u64).to_le_bytes());
        d[i + 16..i + 24].copy_from_slice(&(n as u64).to_le_bytes());
    }
    d
}

/// A PVR file (version 3) of BC7 blocks `b` of layout `l`.
fn pvr(l: &Layout, b: &[u8]) -> Vec<u8> {
    let s = surfaces(l, b);
    let mut d = b"PVR\x03".to_vec();
    d.extend(0u32.to_le_bytes());
    d.extend(15u64.to_le_bytes());
    for v in [0, 0, l.height, l.width, 1, l.layers, 1, l.mips, 4] {
        d.extend(u32::to_le_bytes(v));
    }
    d.extend(b"meta");
    for m in 0..l.mips as usize {
        for layer in &s {
            d.extend(&layer[m]);
        }
    }
    d
}

#[test]
fn containers() {
    use aqr_tex::{container, formula};
    let l = Layout { format: Format::Bc7, width: 40, height: 24, mips: 4, layers: 3 };
    let b = bc7_random(l.len() as usize / 16, 11);
    let s = surfaces(&l, &b);
    for (name, d) in [
        ("ktx", ktx(&l, &b, 3, 1, false)),
        ("ktx big", ktx(&l, &b, 3, 1, true)),
        ("ktx2", ktx2(&l, &b)),
        ("pvr", pvr(&l, &b)),
    ] {
        let found = container::detect(&d).unwrap_or_else(|| panic!("{name}: not found"));
        assert_eq!(found.layout, l, "{name}");
        // each surface where the file has it
        for (i, &(at, n)) in found.pieces(d.len()).unwrap().iter().enumerate() {
            let (k, m) = (i / l.mips as usize, i % l.mips as usize);
            assert!(d[at..at + n] == s[k][m], "{name}: surface {k}, {m}");
        }
        let f = formula::encode_container(&d, 2).unwrap();
        assert!(eval(&f) == d, "{name}");
        // (a trailer kept, a file cut short not taken)
        let mut t = d.clone();
        t.extend(b"trailer");
        assert!(eval(&formula::encode_container(&t, 1).unwrap()) == t, "{name}");
        assert!(container::detect(&d[..d.len() - 1]).is_none(), "{name}");
    }
    // a cube map of KTX: each face padded (a whole number of blocks: none)
    let c = Layout { format: Format::Bc7, width: 8, height: 8, mips: 2, layers: 6 };
    let cb = bc7_random(c.len() as usize / 16, 12);
    let d = ktx(&c, &cb, 0, 6, false);
    assert_eq!(container::detect(&d).unwrap().layout, c);
    assert!(eval(&formula::encode_container(&d, 1).unwrap()) == d);
    // an `.astc` file is one surface: ASTC not taken yet, its layout found
    let mut a = vec![0x13, 0xab, 0xa1, 0x5c, 6, 5, 1, 13, 0, 0, 7, 0, 0, 1, 0, 0];
    a.extend(vec![0u8; 3 * 2 * 16]);
    assert_eq!(
        container::detect(&a).unwrap().layout,
        Layout { format: Format::Astc { w: 6, h: 5 }, width: 13, height: 7, mips: 1, layers: 1 }
    );
}
