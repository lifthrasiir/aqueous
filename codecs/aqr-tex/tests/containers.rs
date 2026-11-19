//! KTX, KTX 2, PVR and `.astc` files that are not what they say: offsets past the end, blocks
//! overlapping, supercompression, volumes, sizes of other layouts, and damaged files. Taken or
//! not, never a panic, and what is taken evaluates back to the file.

use std::time::Duration;

use aqr_archive::Evaluate;
use aqr_formula::{Apply, Formula};
use aqr_tex::{container, formula};

#[global_allocator]
static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(1 << 30);

fn eval(f: &Formula) -> aqr_formula::Result<Vec<u8>> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| {
        assert!(a.resources.is_empty());
        formula::decode_any(a, inputs, 1)
    })
}

fn blocks(n: usize) -> Vec<u8> {
    (0..n * 8).map(|i| (i * 7 + i / 8 * 3) as u8).collect()
}

fn le(v: &mut Vec<u8>, x: &[u32]) {
    for &x in x {
        v.extend(x.to_le_bytes());
    }
}

/// A KTX file of BC1 (0x83f0) of the header's fields, then `payload`.
#[allow(clippy::too_many_arguments)]
fn ktx(w: u32, h: u32, depth: u32, mips: u32, elements: u32, faces: u32, kv: u32, payload: &[u8]) -> Vec<u8> {
    let mut d = b"\xabKTX 11\xbb\r\n\x1a\n".to_vec();
    le(&mut d, &[0x0403_0201, 0, 1, 0, 0x83f0, 0x1908, w, h, depth, elements, faces, mips, kv]);
    d.extend(std::iter::repeat_n(0, kv as usize));
    d.extend(payload);
    d
}

#[allow(clippy::too_many_arguments)]
fn ktx2(format: u32, w: u32, h: u32, depth: u32, elements: u32, faces: u32, mips: u32, scheme: u32) -> Vec<u8> {
    let mut d = b"\xabKTX 20\xbb\r\n\x1a\n".to_vec();
    le(&mut d, &[format, 1, w, h, depth, elements, faces, mips, scheme]);
    d.resize(80, 0);
    d
}

#[allow(clippy::too_many_arguments)]
fn pvr(pf: u64, w: u32, h: u32, depth: u32, elements: u32, faces: u32, mips: u32, meta: u32) -> Vec<u8> {
    let mut d = b"PVR\x03".to_vec();
    le(&mut d, &[0]);
    d.extend(pf.to_le_bytes());
    le(&mut d, &[0, 0, h, w, depth, elements, faces, mips, meta]);
    d
}

/// Taken or not; if taken, evaluated back to `d`.
fn check(name: &str, d: &[u8]) {
    if let Some(f) = formula::encode_container(d, 1) {
        assert!(eval(&f).unwrap() == d, "{name}");
        let found = container::detect(d).unwrap();
        assert!(found.pieces(d.len()).is_some(), "{name}");
    }
}

/// A well-formed KTX of 8x8 BC1 with 4 mipmaps.
fn good_ktx() -> Vec<u8> {
    let mut p = vec![];
    for n in [4u32, 1, 1, 1] {
        le(&mut p, &[n * 8]);
        p.extend(blocks(n as usize));
    }
    ktx(8, 8, 0, 4, 0, 1, 0, &p)
}

#[test]
fn good_is_taken() {
    let d = good_ktx();
    assert!(container::detect(&d).is_some());
    check("good", &d);
}

#[test]
fn ktx_negatives() {
    let good = good_ktx();
    // a size field that is not the mipmap's
    let mut d = good.clone();
    d[64] = 33;
    assert!(container::detect(&d).is_none());
    // a key-value block past the end
    let mut d = good.clone();
    d[60..64].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(container::detect(&d).is_none());
    // a volume, five faces, a zero height, a width past the limit, more mipmaps than the side
    // allows, a type that is not compressed, an unknown format
    for (at, v) in [(44usize, 4u32), (52, 5), (40, 0), (36, 1 << 17), (56, 5), (16, 0x1401), (28, 0x1234)] {
        let mut d = good.clone();
        d[at..at + 4].copy_from_slice(&v.to_le_bytes());
        assert!(container::detect(&d).is_none(), "{at}: {v}");
    }
    // as many layers as the limit allows, or more, in a file of no data
    assert!(container::detect(&ktx(8, 8, 0, 1, 1 << 16, 1, 0, &[])).is_none());
    assert!(container::detect(&ktx(8, 8, 0, 1, u32::MAX, 6, 0, &[])).is_none());
    // the largest sides, every mipmap, no data
    assert!(container::detect(&ktx(1 << 16, 1 << 16, 0, 17, 0, 1, 0, &[])).is_none());
    // cut at every place
    for n in 0..good.len() {
        assert!(container::detect(&good[..n]).is_none(), "{n}");
    }
}

#[test]
fn ktx2_negatives() {
    // BC1 (131), 8x8, one level: the index of a level points anywhere
    let level = |off: u64, len: u64| {
        let mut d = ktx2(131, 8, 8, 0, 0, 1, 1, 0);
        for v in [off, len, len] {
            d.extend(v.to_le_bytes());
        }
        d
    };
    let mut ok = level(104, 32);
    ok.extend(blocks(4));
    assert!(container::detect(&ok).is_some());
    check("ktx2", &ok);
    // offset past the end, at the end of the address space, a length of another size
    for (off, len) in [(1000, 32), (u64::MAX, 32), (u64::MAX - 31, 32), (104, 31), (104, u64::MAX)] {
        let mut d = level(off, len);
        d.extend(blocks(4));
        assert!(container::detect(&d).is_none(), "{off} {len}");
    }
    // the blocks over the header: whatever is taken must evaluate back
    let mut d = level(0, 32);
    d.extend(blocks(4));
    check("ktx2 offset 0", &d);
    // a file cut anywhere
    for n in 0..ok.len() {
        assert!(container::detect(&ok[..n]).is_none(), "{n}");
    }
    // supercompression, a volume, a cube of five faces, a zero height, unknown formats, mipmaps
    // beyond those of the side, as many layers as can be
    for (name, mut d) in [
        ("zstd", ktx2(131, 8, 8, 0, 0, 1, 1, 2)),
        ("basis", ktx2(131, 8, 8, 0, 0, 1, 1, 1)),
        ("volume", ktx2(131, 8, 8, 4, 0, 1, 1, 0)),
        ("five faces", ktx2(131, 8, 8, 0, 0, 5, 1, 0)),
        ("height 0", ktx2(131, 8, 0, 0, 0, 1, 1, 0)),
        ("format 0", ktx2(0, 8, 8, 0, 0, 1, 1, 0)),
        ("format max", ktx2(u32::MAX, 8, 8, 0, 0, 1, 1, 0)),
        ("mips", ktx2(131, 8, 8, 0, 0, 1, 99, 0)),
        ("layers", ktx2(131, 8, 8, 0, u32::MAX, 6, 1, 0)),
        ("layers limit", ktx2(131, 8, 8, 0, 1 << 16, 1, 1, 0)),
    ] {
        d.extend(std::iter::repeat_n(0u8, 24 * 100));
        assert!(container::detect(&d).is_none(), "{name}");
    }
    // two layers of one level over one layer's data
    let mut d = ktx2(131, 8, 8, 0, 2, 1, 1, 0);
    for v in [104u64, 64, 64] {
        d.extend(v.to_le_bytes());
    }
    d.extend(blocks(4));
    assert!(container::detect(&d).is_none());
    // two levels at one place
    let mut d = ktx2(131, 8, 8, 0, 0, 1, 2, 0);
    for (off, len) in [(128u64, 32u64), (128, 8)] {
        for v in [off, len, len] {
            d.extend(v.to_le_bytes());
        }
    }
    d.resize(128, 0);
    d.extend(blocks(4));
    assert!(container::detect(&d).is_none());
}

#[test]
fn pvr_negatives() {
    // BC1 = 7; a good file: 8x8, one mipmap, 4 blocks
    let mut good = pvr(7, 8, 8, 1, 1, 1, 1, 0);
    good.extend(blocks(4));
    assert!(container::detect(&good).is_some());
    check("pvr", &good);
    for n in 0..good.len() {
        assert!(container::detect(&good[..n]).is_none(), "{n}");
    }
    // metadata past the end, or of the largest size
    for meta in [1000, u32::MAX] {
        let mut d = pvr(7, 8, 8, 1, 1, 1, 1, meta);
        d.extend(blocks(4));
        assert!(container::detect(&d).is_none(), "{meta}");
    }
    for (name, d) in [
        ("volume", pvr(7, 8, 8, 4, 1, 1, 1, 0)),
        ("five faces", pvr(7, 8, 8, 1, 1, 5, 1, 0)),
        ("pf", pvr(99, 8, 8, 1, 1, 1, 1, 0)),
        ("pf big", pvr(u64::MAX, 8, 8, 1, 1, 1, 1, 0)),
        ("surfaces", pvr(7, 8, 8, 1, u32::MAX, 6, 1, 0)),
        ("big", pvr(7, 1 << 16, 1 << 16, 1, 1 << 16, 1, 17, 0)),
        ("mips", pvr(7, 8, 8, 1, 1, 1, 9, 0)),
        ("zero width", pvr(7, 0, 8, 1, 1, 1, 1, 0)),
    ] {
        let mut d = d;
        d.extend(blocks(8));
        assert!(container::detect(&d).is_none(), "{name}");
    }
    // PVRTC of sides that are not powers of two
    let mut d = pvr(2, 12, 8, 1, 1, 1, 1, 0);
    d.extend(blocks(100));
    assert!(container::detect(&d).is_none());
    // a cube map: six faces of each mipmap
    let mut cube = pvr(7, 8, 8, 1, 1, 6, 1, 0);
    cube.extend(blocks(4 * 6));
    assert_eq!(container::detect(&cube).unwrap().layout.layers, 6);
    check("cube", &cube);
}

#[test]
fn astc_negatives() {
    let head = |bw: u8, bh: u8, bd: u8, w: u32, h: u32, d: u32| {
        let mut v = vec![0x13, 0xab, 0xa1, 0x5c, bw, bh, bd];
        for x in [w, h, d] {
            v.extend(&x.to_le_bytes()[..3]);
        }
        v
    };
    // (ASTC blocks are 16 bytes: a 4x4 footprint of 8x8 pixels has 4)
    let b16 = |n: usize| blocks(2 * n);
    let mut good = head(4, 4, 1, 8, 8, 1);
    good.extend(b16(4));
    assert!(container::detect(&good).is_some());
    check("astc", &good);
    for n in 0..good.len() {
        assert!(container::detect(&good[..n]).is_none(), "{n}");
    }
    for (name, mut d) in [
        ("3d footprint", head(4, 4, 4, 8, 8, 1)),
        ("3d image", head(4, 4, 1, 8, 8, 4)),
        ("footprint 7x7", head(7, 7, 1, 8, 8, 1)),
        ("zero footprint", head(0, 0, 1, 8, 8, 1)),
        ("zero size", head(4, 4, 1, 0, 8, 1)),
        ("huge", head(4, 4, 1, 0xff_ffff, 0xff_ffff, 1)),
        ("past the limit", head(4, 4, 1, 1 << 17, 4, 1)),
    ] {
        d.extend(b16(8));
        assert!(container::detect(&d).is_none(), "{name}");
    }
}

#[test]
fn damaged_files() {
    let mut files = vec![("ktx", good_ktx())];
    let mut k2 = ktx2(131, 8, 8, 0, 0, 1, 1, 0);
    for v in [104u64, 32, 32] {
        k2.extend(v.to_le_bytes());
    }
    k2.extend(blocks(4));
    files.push(("ktx2", k2));
    let mut p = pvr(7, 8, 8, 1, 1, 6, 2, 4);
    p.extend(b"meta");
    p.extend(blocks(6 * 5));
    files.push(("pvr", p));
    let mut a = vec![0x13, 0xab, 0xa1, 0x5c, 4, 4, 1, 8, 0, 0, 8, 0, 0, 1, 0, 0];
    a.extend(blocks(8));
    files.push(("astc", a));
    for (name, d) in files {
        assert!(container::detect(&d).is_some(), "{name}");
        aqr_testkit::damaged(&d, 64, 128, Duration::from_secs(20), |v| {
            let _ = container::starts(v);
            let _ = container::extent(v);
            check(name, v);
        });
    }
}

/// The format each container's format codes mean (OpenGL's in KTX, Vulkan's in KTX 2, PVR's
/// pixel formats), and none for the codes of formats not taken.
#[test]
fn format_codes() {
    use aqr_tex::layout::{ASTC_FOOTPRINTS, Format, Layout};
    let bytes_of = |format: Format| -> usize {
        let l = Layout { format, width: 32, height: 32, mips: 1, layers: 1 };
        l.mip_surfaces().map(|s| s.blocks() * l.format.block_bytes()).sum()
    };
    let (b6u, b6s) = (Format::Bc6h { signed: false }, Format::Bc6h { signed: true });
    let astc = |i: usize| Format::Astc { w: ASTC_FOOTPRINTS[i].0, h: ASTC_FOOTPRINTS[i].1 };
    let pvrtc = |two| Format::Pvrtc { two };
    let eac = |rg, signed| Format::Eac { rg, signed };
    let formats_of = |d: &[u8]| container::detect(d).map(|f| f.layout.format);
    // KTX
    let gl: &[(u32, Option<Format>)] = &[
        (0x83f0, Some(Format::Bc1)),
        (0x8c4d, Some(Format::Bc1)),
        (0x83f2, Some(Format::Bc2)),
        (0x8c4f, Some(Format::Bc3)),
        (0x8dbb, Some(Format::Bc4)),
        (0x8dbe, Some(Format::Bc5)),
        (0x8e8e, Some(b6s)),
        (0x8e8f, Some(b6u)),
        (0x8e8d, Some(Format::Bc7)),
        (0x8d64, Some(Format::Etc1)),
        (0x9275, Some(Format::Etc2)),
        (0x9277, Some(Format::Etc2P)),
        (0x9279, Some(Format::Etc2A)),
        (0x9270, Some(eac(false, false))),
        (0x9271, Some(eac(false, true))),
        (0x9272, Some(eac(true, false))),
        (0x9273, Some(eac(true, true))),
        (0x8c00, Some(pvrtc(false))),
        (0x8c03, Some(pvrtc(true))),
        (0x93b0, Some(astc(0))),
        (0x93bd, Some(astc(13))),
        (0x93d5, Some(astc(5))),
        (0x1234, None),
    ];
    for &(code, want) in gl {
        let format = want.unwrap_or(Format::Bc1);
        let mut p = vec![];
        le(&mut p, &[bytes_of(format) as u32]);
        p.extend(vec![0u8; bytes_of(format)]);
        let mut d = ktx(32, 32, 0, 1, 0, 1, 0, &p);
        d[28..32].copy_from_slice(&code.to_le_bytes());
        assert_eq!(formats_of(&d), want, "KTX {code:#x}");
    }
    // KTX 2
    let vk: &[(u32, Option<Format>)] = &[
        (131, Some(Format::Bc1)),
        (134, Some(Format::Bc1)),
        (136, Some(Format::Bc2)),
        (138, Some(Format::Bc3)),
        (140, Some(Format::Bc4)),
        (142, Some(Format::Bc5)),
        (143, Some(b6u)),
        (144, Some(b6s)),
        (146, Some(Format::Bc7)),
        (147, Some(Format::Etc2)),
        (150, Some(Format::Etc2P)),
        (152, Some(Format::Etc2A)),
        (153, Some(eac(false, false))),
        (154, Some(eac(false, true))),
        (155, Some(eac(true, false))),
        (156, Some(eac(true, true))),
        (157, Some(astc(0))),
        (184, Some(astc(13))),
        (1_000_066_005, Some(astc(5))),
        (1_000_054_000, Some(pvrtc(true))),
        (1_000_054_005, Some(pvrtc(false))),
        (37, None),
        (185, None),
    ];
    for &(code, want) in vk {
        let format = want.unwrap_or(Format::Bc1);
        let n = bytes_of(format);
        let mut d = ktx2(code, 32, 32, 0, 0, 1, 1, 0);
        d.extend(104u64.to_le_bytes());
        d.extend((n as u64).to_le_bytes());
        d.extend((n as u64).to_le_bytes());
        d.extend(vec![0u8; n]);
        assert_eq!(formats_of(&d), want, "KTX 2 {code}");
    }
    // PVR (signed EAC and BC6H by the channel type)
    let pf: &[(u64, u32, Option<Format>)] = &[
        (0, 0, Some(pvrtc(true))),
        (3, 0, Some(pvrtc(false))),
        (6, 0, Some(Format::Etc1)),
        (7, 0, Some(Format::Bc1)),
        (9, 0, Some(Format::Bc2)),
        (11, 0, Some(Format::Bc3)),
        (12, 0, Some(Format::Bc4)),
        (13, 0, Some(Format::Bc5)),
        (14, 0, Some(b6u)),
        (14, 3, Some(b6s)),
        (15, 0, Some(Format::Bc7)),
        (22, 0, Some(Format::Etc2)),
        (23, 0, Some(Format::Etc2A)),
        (24, 0, Some(Format::Etc2P)),
        (25, 0, Some(eac(false, false))),
        (25, 1, Some(eac(false, true))),
        (26, 0, Some(eac(true, false))),
        (26, 5, Some(eac(true, true))),
        (27, 0, Some(astc(0))),
        (40, 0, Some(astc(13))),
        (41, 0, None),
        (4, 0, None),
    ];
    for &(code, channel, want) in pf {
        let format = want.unwrap_or(Format::Bc1);
        let mut d = pvr(code, 32, 32, 1, 1, 1, 1, 0);
        d[20..24].copy_from_slice(&channel.to_le_bytes());
        d.extend(vec![0u8; bytes_of(format)]);
        assert_eq!(formats_of(&d), want, "PVR {code} channel {channel}");
    }
}
