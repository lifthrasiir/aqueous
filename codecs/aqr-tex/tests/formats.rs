//! The families of formats beyond BC1 to BC5 through the walk: blocks of every layout of theirs
//! (random bits: every mode, reserved ones too), of surfaces in stripes on any number of
//! threads, of odd sizes and mipmaps, given back; smooth textures coded small; corrupted streams
//! turned down.

#![allow(clippy::needless_range_loop)]

use aqr_tex::layout::{Format, Layout};
use aqr_tex::texture::{self, Family};

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

/// A smooth RGBA image of `w` × `h`, its pixels `scale` times as large (a mipmap of the image of
/// scale 1).
fn image(w: usize, h: usize, scale: f64) -> Vec<[u8; 4]> {
    let s = 0.5;
    (0..w * h)
        .map(|i| {
            let (x, y) = (((i % w) as f64 + 0.5) * scale, ((i / w) as f64 + 0.5) * scale);
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

/// What each family says of its formats' blocks is what the format says.
#[test]
fn families_shapes() {
    fn agree<F: Family>() {
        for f in Format::SOME.into_iter().filter(|f| f.group() == F::GROUP) {
            assert_eq!(F::shape(f), f.shape(), "{}", f.name());
        }
    }
    agree::<aqr_tex::bcn::Bcn>();
    agree::<aqr_tex::bc7::Bc7>();
    agree::<aqr_tex::bc6h::Bc6h>();
    agree::<aqr_tex::etcm::Etc>();
    agree::<aqr_tex::pvrtc::Pvrtc>();
    agree::<aqr_tex::astcm::Astc>();
}

#[test]
fn bc7_stripes() {
    // a largest mipmap of 4 stripes, its second of one
    let l = Layout { format: Format::Bc7, width: 1024, height: 520, mips: 2, layers: 1 };
    assert!(texture::stripes::<aqr_tex::bcn::Bcn>(l.mip_surfaces().next().unwrap().blocks()) > 1);
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
        blocks.extend(bc7_encode(&image(mw, mh, (1 << mips) as f64), mw, mh));
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
    // an `.astc` file is one surface
    let mut a = vec![0x13, 0xab, 0xa1, 0x5c, 6, 5, 1, 13, 0, 0, 7, 0, 0, 1, 0, 0];
    a.extend(astc_random(3 * 2, 5, (6, 5)));
    let l = Layout { format: Format::Astc { w: 6, h: 5 }, width: 13, height: 7, mips: 1, layers: 1 };
    assert_eq!(container::detect(&a).unwrap().layout, l);
    assert!(eval(&formula::encode_container(&a, 1).unwrap()) == a);
}

/// Random blocks of BC6H: of every header in turn (the reserved ones too), some repeated.
fn bc6h_random(n: usize, seed: u64) -> Vec<u8> {
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
        b[0] = b[0] & !31 | (r.next() % 32) as u8;
        out.extend(b);
    }
    out
}

/// BC6H blocks (unsigned) of mode 10 (endpoints of 10 bits, as they are) of a smooth image of
/// `w` × `h` of the values the endpoints unquantize to, its pixels `scale` times as large.
fn bc6h_encode(w: usize, h: usize, scale: f64) -> Vec<u8> {
    use aqr_tex::bptc::quantize;
    let s = 0.5;
    let px = |x: usize, y: usize| -> [i32; 3] {
        let (x, y) = ((x as f64 + 0.5) * scale, (y as f64 + 0.5) * scale);
        let v = |a: f64, b: f64| (30000.0 + 25000.0 * ((x * a + y * b) / 30.0 + s).sin()) as i32;
        [v(1.0, 0.2), v(0.3, 0.9), v(-0.5, 0.6)]
    };
    let mut out = vec![];
    for by in 0..h.div_ceil(4) {
        for bx in 0..w.div_ceil(4) {
            let p: Vec<[i32; 3]> =
                (0..16).map(|q| px((bx * 4 + q % 4).min(w - 1), (by * 4 + q / 4).min(h - 1))).collect();
            let lo: [i32; 3] = std::array::from_fn(|c| quantize(p.iter().map(|v| v[c]).min().unwrap(), 10, false));
            let hi: [i32; 3] = std::array::from_fn(|c| quantize(p.iter().map(|v| v[c]).max().unwrap(), 10, false));
            let un = |q: i32| aqr_tex::bptc::unquantize(q, 10, false);
            let d: [i64; 3] = std::array::from_fn(|c| (un(hi[c]) - un(lo[c])) as i64);
            let l2: i64 = d.iter().map(|v| v * v).sum::<i64>().max(1);
            let mut ix: Vec<u32> = p
                .iter()
                .map(|v| {
                    let t: i64 = (0..3).map(|c| (v[c] - un(lo[c])) as i64 * d[c]).sum();
                    ((t * 15 + l2 / 2) / l2).clamp(0, 15) as u32
                })
                .collect();
            let (mut e0, mut e1) = (lo, hi);
            if ix[0] >= 8 {
                (e0, e1) = (e1, e0);
                ix.iter_mut().for_each(|i| *i = 15 - *i);
            }
            let mut v: u128 = 0b00011;
            let mut at = 5;
            let mut put = |n: u32, x: u32| {
                v |= (x as u128) << at;
                at += n;
            };
            for e in [e0, e1] {
                for c in e {
                    put(10, c as u32);
                }
            }
            for (q, &i) in ix.iter().enumerate() {
                put(if q == 0 { 3 } else { 4 }, i);
            }
            out.extend(v.to_le_bytes());
        }
    }
    out
}

#[test]
fn bc6h_layouts() {
    for signed in [false, true] {
        for (size, mips, layers, seed) in [((4, 4), 1, 1, 1), ((13, 7), 3, 2, 2), ((64, 64), 7, 1, 3)] {
            let l = Layout { format: Format::Bc6h { signed }, width: size.0, height: size.1, mips, layers };
            let b = bc6h_random(l.len() as usize / 16, seed);
            round(&l, &b);
        }
    }
    let l = Layout { format: Format::Bc6h { signed: false }, width: 1024, height: 520, mips: 2, layers: 1 };
    round(&l, &bc6h_random(l.len() as usize / 16, 7));
}

#[test]
fn bc6h_smooth_codes_small() {
    let (w, h) = (256, 128);
    let (mut blocks, mut mips, mut mw, mut mh) = (vec![], 0, w, h);
    while mips < 6 {
        blocks.extend(bc6h_encode(mw, mh, (1 << mips) as f64));
        mips += 1;
        (mw, mh) = (mw / 2, mh / 2);
    }
    let l = Layout { format: Format::Bc6h { signed: false }, width: w as u32, height: h as u32, mips, layers: 1 };
    assert_eq!(l.len() as usize, blocks.len());
    let n = round(&l, &blocks);
    let z = zstd::bulk::compress(&blocks, 19).unwrap().len();
    let cost = texture::encode_any(&l, &blocks, 1).2.map(|c| c / 65536 / 8);
    eprintln!("bc6h smooth: {} -> {n} (zstd -19 {z}) {cost:?}", blocks.len());
    assert!(n * 10 < z * 9, "{n} against zstd's {z}");
}

#[test]
fn dds_of_dx10() {
    // BC7 (DXGI 98), BC6H unsigned (95) and signed (96), of mipmaps and an array of 2
    for (format, dxgi) in
        [(Format::Bc7, 98u32), (Format::Bc6h { signed: false }, 95), (Format::Bc6h { signed: true }, 96)]
    {
        let l = Layout { format, width: 24, height: 20, mips: 3, layers: 2 };
        let b = if dxgi == 98 { bc7_random(l.len() as usize / 16, 3) } else { bc6h_random(l.len() as usize / 16, 4) };
        let mut d = vec![0u8; 148];
        d[..4].copy_from_slice(b"DDS ");
        for (at, v) in [(4, 124u32), (8, 0x2_1007), (12, l.height), (16, l.width), (28, l.mips), (76, 32), (80, 4)] {
            d[at..at + 4].copy_from_slice(&v.to_le_bytes());
        }
        d[84..88].copy_from_slice(b"DX10");
        for (at, v) in [(128, dxgi), (132, 3), (136, 0), (140, 2), (144, 0)] {
            d[at..at + 4].copy_from_slice(&v.to_le_bytes());
        }
        d.extend(&b);
        assert_eq!(aqr_tex::dds::detect(&d).unwrap().data, aqr_tex::dds::Data::Blocks(l));
        let f = aqr_tex::formula::encode(&d, 2).unwrap();
        assert!(eval(&f) == d, "{format:?}");
    }
}

/// The formats of ETC and EAC.
const ETC: [Format; 8] = [
    Format::Etc1,
    Format::Etc2,
    Format::Etc2P,
    Format::Etc2A,
    Format::Eac { rg: false, signed: false },
    Format::Eac { rg: false, signed: true },
    Format::Eac { rg: true, signed: false },
    Format::Eac { rg: true, signed: true },
];

/// Random blocks of `bytes` bytes (any 64 bits are a block of ETC or EAC), some repeated, some of
/// their first 4 bytes (all but the indices of most of RGB's) repeated.
fn etc_random(n: usize, bytes: usize, seed: u64) -> Vec<u8> {
    let mut r = Rng(seed | 1);
    let mut out: Vec<u8> = Vec::with_capacity(n * bytes);
    for i in 0..n {
        let mut b: Vec<u8> = (0..bytes).map(|_| r.next() as u8).collect();
        if i > 0 && r.next().is_multiple_of(4) {
            let k = (r.next() as usize % i) * bytes;
            let (all, half) = (r.next().is_multiple_of(2), bytes / 2);
            let take = if all { bytes } else { half };
            b[..take].copy_from_slice(&out[k..k + take]);
        }
        out.extend(b);
    }
    out
}

#[test]
fn etc_layouts() {
    for (k, format) in ETC.into_iter().enumerate() {
        for (size, mips, layers) in [((4, 4), 1, 1), ((13, 7), 3, 2), ((64, 64), 7, 1)] {
            let l = Layout { format, width: size.0, height: size.1, mips, layers };
            let bb = format.block_bytes();
            let b = etc_random(l.len() as usize / bb, bb, k as u64 * 7 + mips as u64);
            round(&l, &b);
        }
    }
    let l = Layout { format: Format::Etc2, width: 1024, height: 520, mips: 2, layers: 1 };
    round(&l, &etc_random(l.len() as usize / 8, 8, 99));
}

/// ETC1 blocks (differential, or individual where the bases are far apart) of `img`: each
/// subblock's base the mean of its pixels (split left and right), the codeword of the least error.
fn etc1_encode(img: &[[u8; 4]], w: usize, h: usize) -> Vec<u8> {
    use aqr_tex::etc;
    let mut out = vec![];
    for by in 0..h.div_ceil(4) {
        for bx in 0..w.div_ceil(4) {
            let px: Vec<[i32; 3]> = (0..16)
                .map(|p| {
                    let v = img[(by * 4 + p / 4).min(h - 1) * w + (bx * 4 + p % 4).min(w - 1)];
                    [v[0] as i32, v[1] as i32, v[2] as i32]
                })
                .collect();
            let sub = |k: usize| -> Vec<usize> { (0..16).filter(|&p| etc::subblock(p, false) == k).collect() };
            let means: [[i32; 3]; 2] =
                std::array::from_fn(|k| std::array::from_fn(|c| sub(k).iter().map(|&p| px[p][c]).sum::<i32>() / 8));
            let q5 = means.map(|m| m.map(|v| (v * 31 + 127) / 255));
            let diff = (0..3).all(|c| (-4..=3).contains(&(q5[1][c] - q5[0][c])));
            let mut word: u64 = 0;
            let bases: [[i32; 3]; 2] = if diff {
                word |= 1 << 33;
                for c in 0..3 {
                    word |= (q5[0][c] as u64) << (59 - 8 * c) | (((q5[1][c] - q5[0][c]) & 7) as u64) << (56 - 8 * c);
                }
                q5.map(|q| q.map(|v| etc::ext(v, 5)))
            } else {
                let q4 = means.map(|m| m.map(|v| (v * 15 + 127) / 255));
                for c in 0..3 {
                    word |= (q4[0][c] as u64) << (60 - 8 * c) | (q4[1][c] as u64) << (56 - 8 * c);
                }
                q4.map(|q| q.map(|v| etc::ext(v, 4)))
            };
            for k in 0..2 {
                let err = |cw: usize, p: usize| {
                    (0..4u32)
                        .map(|i| {
                            let c = etc::modified(bases[k], cw, i, true);
                            ((0..3).map(|ch| (c[ch] as i32 - px[p][ch]).pow(2)).sum::<i32>(), i)
                        })
                        .min()
                        .unwrap()
                };
                let cw = (0..8).min_by_key(|&cw| sub(k).iter().map(|&p| err(cw, p).0).sum::<i32>()).unwrap();
                word |= (cw as u64) << (37 - 3 * k);
                for p in sub(k) {
                    let i = err(cw, p).1 as u64;
                    let col = etc::column(p);
                    word |= (i >> 1) << (16 + col) | (i & 1) << col;
                }
            }
            out.extend(word.to_be_bytes());
        }
    }
    out
}

#[test]
fn etc_smooth_codes_small() {
    let (w, h) = (256, 128);
    let (mut blocks, mut mips, mut mw, mut mh) = (vec![], 0, w, h);
    while mips < 6 {
        blocks.extend(etc1_encode(&image(mw, mh, (1 << mips) as f64), mw, mh));
        mips += 1;
        (mw, mh) = (mw / 2, mh / 2);
    }
    for format in [Format::Etc1, Format::Etc2] {
        let l = Layout { format, width: w as u32, height: h as u32, mips, layers: 1 };
        assert_eq!(l.len() as usize, blocks.len());
        let n = round(&l, &blocks);
        let z = zstd::bulk::compress(&blocks, 19).unwrap().len();
        let cost = texture::encode_any(&l, &blocks, 1).2.map(|c| c / 65536 / 8);
        eprintln!("{format:?} smooth: {} -> {n} (zstd -19 {z}) {cost:?}", blocks.len());
        assert!(n * 10 < z * 9, "{n} against zstd's {z}");
    }
}

/// EAC R11 blocks (unsigned) of channel 0 of `img` (its 8 bits as 11, times 8): the base the mean,
/// the table and multiplier of the least error.
fn eac_encode(img: &[[u8; 4]], w: usize, h: usize) -> Vec<u8> {
    use aqr_tex::etc::{self, Eac};
    let mut out = vec![];
    for by in 0..h.div_ceil(4) {
        for bx in 0..w.div_ceil(4) {
            let px: Vec<i32> = (0..16)
                .map(|p| img[(by * 4 + p / 4).min(h - 1) * w + (bx * 4 + p % 4).min(w - 1)][0] as i32 * 8)
                .collect();
            let base = ((px.iter().sum::<i32>() / 16 - 4) / 8).clamp(0, 255) as u32;
            let best = |t: u32, m: u32| -> (i32, [u32; 16]) {
                let mut e = 0;
                let ix = std::array::from_fn(|p| {
                    let (d, i) = (0..8).map(|i| ((Eac::R11.value(base, m, t, i) - px[p]).abs(), i)).min().unwrap();
                    e += d;
                    i
                });
                (e, ix)
            };
            let (t, m) = (0..16).flat_map(|t| (1..16).map(move |m| (t, m))).min_by_key(|&(t, m)| best(t, m).0).unwrap();
            let ix = best(t, m).1;
            let mut word = (base as u64) << 56 | (m as u64) << 52 | (t as u64) << 48;
            for p in 0..16 {
                word |= (ix[p] as u64) << (45 - 3 * etc::column(p));
            }
            out.extend(word.to_be_bytes());
        }
    }
    out
}

#[test]
fn eac_smooth_codes_small() {
    let (w, h) = (128, 64);
    let (mut blocks, mut mips, mut mw, mut mh) = (vec![], 0, w, h);
    while mips < 5 {
        blocks.extend(eac_encode(&image(mw, mh, (1 << mips) as f64), mw, mh));
        mips += 1;
        (mw, mh) = (mw / 2, mh / 2);
    }
    let l =
        Layout { format: Format::Eac { rg: false, signed: false }, width: w as u32, height: h as u32, mips, layers: 1 };
    assert_eq!(l.len() as usize, blocks.len());
    let n = round(&l, &blocks);
    let z = zstd::bulk::compress(&blocks, 19).unwrap().len();
    let cost = texture::encode_any(&l, &blocks, 1).2.map(|c| c / 65536 / 8);
    eprintln!("eac smooth: {} -> {n} (zstd -19 {z}) {cost:?}", blocks.len());
    assert!(n * 10 < z * 9, "{n} against zstd's {z}");
}

#[test]
fn pvrtc_layouts() {
    for two in [false, true] {
        let format = Format::Pvrtc { two };
        for (size, mips, layers) in [((8, 8), 1, 1), ((64, 32), 6, 2), ((16, 128), 8, 1), ((256, 256), 2, 1)] {
            let l = Layout { format, width: size.0, height: size.1, mips, layers };
            let b = etc_random(l.len() as usize / 8, 8, size.0 as u64 + two as u64);
            round(&l, &b);
        }
    }
}

/// PVRTC1 blocks of 4 bits a pixel (in Morton order) of `img` (`w` × `h`, powers of two, at
/// least 8): of each word, colour A the least of each channel of its pixels (opaque, RGB 554), B
/// the most (RGB 555), each pixel's modulation the weight nearest it (of flag 0).
fn pvrtc_encode(img: &[[u8; 4]], w: usize, h: usize) -> Vec<u8> {
    use aqr_tex::pvrtc;
    let (bw, bh) = (w / 4, h / 4);
    let mut words = vec![0u64; bw * bh];
    for by in 0..bh {
        for bx in 0..bw {
            let px: Vec<[u8; 4]> = (0..16).map(|p| img[(by * 4 + p / 4) * w + bx * 4 + p % 4]).collect();
            let lo: [u64; 3] = std::array::from_fn(|c| px.iter().map(|v| v[c]).min().unwrap() as u64);
            let hi: [u64; 3] = std::array::from_fn(|c| px.iter().map(|v| v[c]).max().unwrap() as u64);
            let a = 1 << 15 | (lo[0] >> 3) << 10 | (lo[1] >> 3) << 5 | (lo[2] >> 4) << 1;
            let b = 1 << 15 | (hi[0] >> 3) << 10 | (hi[1] >> 3) << 5 | hi[2] >> 3;
            words[by * bw + bx] = b << 48 | a << 32;
        }
    }
    let colours = words.clone();
    for by in 0..bh {
        for bx in 0..bw {
            let mut m = 0u64;
            for p in 0..16 {
                let (x, y) = (bx * 4 + p % 4, by * 4 + p / 4);
                let ab = pvrtc::images(&colours, (bw, bh), 4, x, y);
                let v = img[y * w + x];
                let err = |k: usize| {
                    let c = pvrtc::blend(&ab, pvrtc::WEIGHTS[0][k], false);
                    (0..3).map(|ch| (c[ch] as i32 - v[ch] as i32).pow(2)).sum::<i32>()
                };
                m |= ((0..4).min_by_key(|&k| err(k)).unwrap() as u64) << (2 * p);
            }
            words[by * bw + bx] |= m;
        }
    }
    let mut out = vec![0u8; bw * bh * 8];
    for by in 0..bh {
        for bx in 0..bw {
            let k = pvrtc::morton(bx, by, bw, bh) * 8;
            out[k..k + 8].copy_from_slice(&words[by * bw + bx].to_le_bytes());
        }
    }
    out
}

#[test]
fn pvrtc_smooth_codes_small() {
    let (w, h) = (128, 128);
    let (mut blocks, mut mips, mut mw, mut mh) = (vec![], 0, w, h);
    while mips < 5 {
        blocks.extend(pvrtc_encode(&image(mw, mh, (1 << mips) as f64), mw, mh));
        mips += 1;
        (mw, mh) = (mw / 2, mh / 2);
    }
    let l = Layout { format: Format::Pvrtc { two: false }, width: w as u32, height: h as u32, mips, layers: 1 };
    assert_eq!(l.len() as usize, blocks.len());
    let n = round(&l, &blocks);
    let z = zstd::bulk::compress(&blocks, 19).unwrap().len();
    let cost = texture::encode_any(&l, &blocks, 1).2.map(|c| c / 65536 / 8);
    eprintln!("pvrtc smooth: {} -> {n} (zstd -19 {z}) {cost:?}", blocks.len());
    assert!(n * 10 < z * 9, "{n} against zstd's {z}");
}

/// Random blocks of ASTC (any bits: most are error blocks, some void-extent), and blocks of
/// legal fields: of each random block a normal one of its block mode where it is one.
fn astc_random(n: usize, seed: u64, fp: (u32, u32)) -> Vec<u8> {
    let mut b = etc_random(n, 16, seed);
    // (some made void-extent blocks, of all-1 extents)
    for (i, blk) in b.chunks_mut(16).enumerate() {
        if i % 7 == 3 {
            let mut v = u128::from_le_bytes(blk.try_into().unwrap());
            v = v & !0xfff | 0x1fc | 3 << 10 | ((1u128 << 52) - 1) << 12;
            blk.copy_from_slice(&v.to_le_bytes());
        }
        let _ = fp;
    }
    b
}

#[test]
fn astc_layouts() {
    for (fp, size, mips, layers) in
        [((4u8, 4u8), (16, 16), 5, 1), ((6, 5), (37, 23), 3, 2), ((8, 8), (64, 64), 7, 1), ((12, 12), (100, 50), 2, 1)]
    {
        let l = Layout { format: Format::Astc { w: fp.0, h: fp.1 }, width: size.0, height: size.1, mips, layers };
        let b = astc_random(l.len() as usize / 16, fp.0 as u64 * 31 + fp.1 as u64, (fp.0 as u32, fp.1 as u32));
        round(&l, &b);
    }
    let l = Layout { format: Format::Astc { w: 4, h: 4 }, width: 1024, height: 520, mips: 2, layers: 1 };
    round(&l, &astc_random(l.len() as usize / 16, 77, (4, 4)));
}

/// ASTC blocks of 4x4 of `img` (`w` × `h`): one partition of CEM 8 (RGB direct, 8 bits each: the
/// least and most of each channel), a grid of 4x4 weights of 3 bits (nearest each pixel's place
/// between the endpoints).
fn astc_encode(img: &[[u8; 4]], w: usize, h: usize) -> Vec<u8> {
    use aqr_tex::astc::{Block, Kind};
    let mut out = vec![];
    for by in 0..h.div_ceil(4) {
        for bx in 0..w.div_ceil(4) {
            let px: Vec<[u8; 4]> =
                (0..16).map(|p| img[(by * 4 + p / 4).min(h - 1) * w + (bx * 4 + p % 4).min(w - 1)]).collect();
            let lo: [u8; 3] = std::array::from_fn(|c| px.iter().map(|v| v[c]).min().unwrap());
            let hi: [u8; 3] = std::array::from_fn(|c| px.iter().map(|v| v[c]).max().unwrap());
            let mut b = Block {
                kind: Kind::Normal,
                mode: 83,
                wx: 4,
                wy: 4,
                wr: 5,
                np: 1,
                cem: [8; 4],
                cr: 16,
                ..Default::default()
            };
            b.colours.n = 6;
            for c in 0..3 {
                (b.colours.v[2 * c], b.colours.v[2 * c + 1]) = (lo[c], hi[c]);
            }
            let d: [i32; 3] = std::array::from_fn(|c| hi[c] as i32 - lo[c] as i32);
            let l2 = d.iter().map(|v| v * v).sum::<i32>().max(1);
            b.weights.n = 16;
            for p in 0..16 {
                let t: i32 = (0..3).map(|c| (px[p][c] as i32 - lo[c] as i32) * d[c]).sum();
                b.weights.v[p] = ((t * 7 + l2 / 2) / l2).clamp(0, 7) as u8;
            }
            (b.unused_at, b.unused_n) = (17 + 48, 128 - 48 - 17 - 48);
            let raw = b.write();
            assert_eq!(Block::read(&raw, (4, 4)).write(), raw);
            assert_eq!(Block::read(&raw, (4, 4)).kind, Kind::Normal);
            out.extend(raw);
        }
    }
    out
}

#[test]
fn astc_smooth_codes_small() {
    let (w, h) = (128, 64);
    let (mut blocks, mut mips, mut mw, mut mh) = (vec![], 0, w, h);
    while mips < 5 {
        blocks.extend(astc_encode(&image(mw, mh, (1 << mips) as f64), mw, mh));
        mips += 1;
        (mw, mh) = (mw / 2, mh / 2);
    }
    let l = Layout { format: Format::Astc { w: 4, h: 4 }, width: w as u32, height: h as u32, mips, layers: 1 };
    assert_eq!(l.len() as usize, blocks.len());
    let n = round(&l, &blocks);
    let z = zstd::bulk::compress(&blocks, 19).unwrap().len();
    let cost = texture::encode_any(&l, &blocks, 1).2.map(|c| c / 65536 / 8);
    eprintln!("astc smooth: {} -> {n} (zstd -19 {z}) {cost:?}", blocks.len());
    assert!(n * 10 < z * 9, "{n} against zstd's {z}");
}
