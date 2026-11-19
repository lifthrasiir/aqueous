//! Damaged coded streams of every family (BC7, BC6H, ETC and EAC, PVRTC, ASTC, as BC1 to BC5's
//! are in `dds.rs`), blocks of random bits (every mode, reserved ones too) of every format and
//! of every ASTC footprint given back, and DDS files damaged. An error or other blocks, never a
//! panic, a hang or an allocation the data cannot hold.

use std::time::Duration;

use aqr_tex::layout::{ASTC_FOOTPRINTS, Format, Layout};
use aqr_tex::{dds, formula, texture};

#[global_allocator]
static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(1 << 30);

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// `n` blocks of random bits, some repeated, some of few bits set, some void-extent ASTC ones.
fn random_blocks(format: Format, n: usize, seed: u64) -> Vec<u8> {
    let bytes = format.block_bytes();
    let mut r = Rng(seed | 1);
    let mut out: Vec<u8> = Vec::with_capacity(n * bytes);
    for i in 0..n {
        let k = r.next() % 8;
        if i > 0 && k == 0 {
            let at = (r.next() as usize % i) * bytes;
            out.extend_from_within(at..at + bytes);
            continue;
        }
        let mut b: Vec<u8> = (0..bytes).map(|_| r.next() as u8).collect();
        match k {
            1 => b.iter_mut().for_each(|x| *x &= r.next() as u8 & 0x11),
            2 => b.fill(0),
            3 => b.fill(0xff),
            4 if matches!(format, Format::Astc { .. }) => {
                let mut v = u128::from_le_bytes(b.clone().try_into().unwrap());
                v = v & !0xfff | 0x1fc | ((r.next() % 2) as u128) << 9 | ((1u128 << 52) - 1) << 12;
                b = v.to_le_bytes().to_vec();
            }
            5 if matches!(format, Format::Bc7) => b[0] = (b[0] | 1) << (r.next() % 8),
            _ => {}
        }
        out.extend(b);
    }
    out
}

#[test]
fn random_blocks_of_every_format() {
    let mut formats: Vec<Format> = Format::SOME.to_vec();
    formats.extend(ASTC_FOOTPRINTS.iter().map(|&(w, h)| Format::Astc { w, h }));
    formats.extend([
        Format::Bc6h { signed: true },
        Format::Eac { rg: true, signed: true },
        Format::Pvrtc { two: true },
    ]);
    for format in formats {
        for (size, mips, layers) in [((23u32, 17u32), 3, 2), ((1, 1), 1, 1), ((5, 130), 4, 1)] {
            let (width, height) = if matches!(format, Format::Pvrtc { .. }) {
                (size.0.next_power_of_two(), size.1.next_power_of_two())
            } else {
                size
            };
            let l = Layout { format, width, height, mips, layers };
            if !l.valid() {
                continue;
            }
            let b = random_blocks(format, (l.len() / format.block_bytes() as u64) as usize, width as u64 * 7 + 1);
            let (main, apart, _) = texture::encode_any(&l, &b, 1);
            assert!(texture::decode_any(&l, &main, &apart, 1).unwrap() == b, "{l:?}");
        }
    }
}

/// Every damage of the coded stream of a small texture of `format` decodes to an error or to
/// blocks of the right size.
fn damaged_stream(format: Format) {
    let (width, height) = if matches!(format, Format::Pvrtc { .. }) { (32, 16) } else { (27, 14) };
    let l = Layout { format, width, height, mips: 3, layers: 1 };
    let b = random_blocks(format, (l.len() / format.block_bytes() as u64) as usize, 5);
    let (main, apart, _) = texture::encode_any(&l, &b, 1);
    let p = l.to_bytes();
    let check = |main: &[u8], apart: &[u8]| {
        if let Ok(o) = texture::decode_any(&l, main, apart, 1) {
            assert_eq!(o.len() as u64, l.len());
        }
        // (the module's own checks too)
        let aqr_formula::Node::Apply(a) =
            aqr_formula::Node::apply(formula::module(l.format.group()), p.clone(), vec![], l.len())
        else {
            unreachable!()
        };
        if let Ok(o) = formula::decode_any(&a, &[main, apart], 1) {
            assert_eq!(o.len() as u64, l.len());
        }
    };
    let t = Duration::from_secs(30);
    aqr_testkit::damaged(&main, 150, 48, t, |m| check(m, &apart));
    if !apart.is_empty() {
        aqr_testkit::damaged(&apart, 50, 48, t, |a| check(&main, a));
    }
}

#[test]
fn damaged_bc7() {
    damaged_stream(Format::Bc7);
}

#[test]
fn damaged_bc6h() {
    damaged_stream(Format::Bc6h { signed: false });
    damaged_stream(Format::Bc6h { signed: true });
}

#[test]
fn damaged_etc() {
    for f in [Format::Etc1, Format::Etc2, Format::Etc2P, Format::Etc2A, Format::Eac { rg: false, signed: true }] {
        damaged_stream(f);
    }
    damaged_stream(Format::Eac { rg: true, signed: false });
}

#[test]
fn damaged_pvrtc() {
    damaged_stream(Format::Pvrtc { two: false });
    damaged_stream(Format::Pvrtc { two: true });
}

#[test]
fn damaged_astc() {
    for (w, h) in [(4, 4), (6, 5), (12, 12)] {
        damaged_stream(Format::Astc { w, h });
    }
}

#[test]
fn damaged_bc1_to_bc5() {
    for f in aqr_tex::layout::Format::ALL {
        damaged_stream(f);
    }
}

/// A DDS file of header fields as `dds.rs` of the tests builds it.
fn dds_file(
    fourcc: Option<&[u8; 4]>,
    bits: u32,
    size: (u32, u32),
    mips: u32,
    caps2: u32,
    dx10: Option<(u32, u32)>,
) -> Vec<u8> {
    let u = |d: &mut Vec<u8>, v: u32| d.extend(v.to_le_bytes());
    let mut d = b"DDS ".to_vec();
    u(&mut d, 124);
    u(&mut d, 0x2_1007);
    u(&mut d, size.1);
    u(&mut d, size.0);
    u(&mut d, 0);
    u(&mut d, 0);
    u(&mut d, mips);
    d.resize(76, 0);
    u(&mut d, 32);
    u(&mut d, if fourcc.is_some() { 4 } else { 0x41 });
    d.extend(fourcc.copied().unwrap_or([0; 4]));
    u(&mut d, bits);
    for m in [0xff0000, 0xff00, 0xff, 0xff000000] {
        u(&mut d, if fourcc.is_some() { 0 } else { m });
    }
    u(&mut d, 0x1000);
    u(&mut d, caps2);
    d.resize(128, 0);
    if let Some((format, array)) = dx10 {
        for v in [format, 3, if caps2 & 0x200 != 0 { 4 } else { 0 }, array, 0] {
            u(&mut d, v);
        }
    }
    d
}

#[test]
fn dds_corners() {
    let body = |n: usize| (0..n).map(|i| (i * 31 % 251) as u8).collect::<Vec<u8>>();
    // the DX10 header cut, or its format unknown, a dimension of 1 or 2 (not a 2D texture)
    let mut d = dds_file(Some(b"DX10"), 0, (8, 8), 1, 0, Some((98, 1)));
    d.extend(body(64));
    assert!(dds::detect(&d).is_some());
    assert!(dds::detect(&d[..130]).is_none());
    assert!(dds::detect(&d[..127]).is_none());
    for (at, v) in
        [(128usize, 0u32), (128, 1), (128, 200), (128, u32::MAX), (132, 2), (132, 4), (132, 0), (140, u32::MAX)]
    {
        let mut x = d.clone();
        x[at..at + 4].copy_from_slice(&v.to_le_bytes());
        assert!(dds::detect(&x).is_none(), "{at}: {v}");
    }
    // pixels of 16 or 0 bits, with a cube map, with mipmaps past the halvings, with sides of 0
    for (name, bits, caps2, mips, size) in [
        ("16 bits", 16, 0, 1, (8, 8)),
        ("0 bits", 0, 0, 1, (8, 8)),
        ("mips", 24, 0, 9, (8, 8)),
        ("width 0", 24, 0, 1, (0, 8)),
        ("height 0", 24, 0, 1, (8, 0)),
        ("wide", 24, 0, 1, (1 << 17, 1)),
        ("huge", 32, 0, 1, (u32::MAX, u32::MAX)),
    ] {
        let mut d = dds_file(None, bits, size, mips, caps2, None);
        d.extend(body(4096));
        assert!(dds::detect(&d).is_none(), "{name}");
    }
    // a DX10 array of the most elements in a file of none
    let d = dds_file(Some(b"DX10"), 0, (8, 8), 1, 0, Some((98, u32::MAX)));
    assert!(dds::detect(&d).is_none());
    let d = dds_file(Some(b"DX10"), 0, (8, 8), 1, 0x200 | 0xfc00, Some((98, u32::MAX / 3)));
    assert!(dds::detect(&d).is_none());
    // pixels of a cube map: six faces
    let mut c = dds_file(None, 32, (4, 4), 1, 0x200 | 0xfc00, None);
    c.extend(body(6 * 4 * 4 * 4));
    let f = dds::detect(&c).expect("a cube of pixels");
    assert_eq!(f.data.len(), 6 * 64);
}

#[test]
fn damaged_dds_files() {
    let body = |n: usize| (0..n).map(|i| (i * 13 % 247) as u8).collect::<Vec<u8>>();
    let mut files = vec![];
    let mut d = dds_file(Some(b"DX10"), 0, (12, 8), 3, 0, Some((98, 2)));
    d.extend(random_blocks(Format::Bc7, 12 * 2 * 2, 3));
    d.extend(b"tail");
    files.push(d);
    let mut d = dds_file(Some(b"DXT5"), 0, (12, 8), 3, 0, None);
    d.extend(random_blocks(Format::Bc3, 6 + 2 + 1, 4));
    files.push(d);
    let mut d = dds_file(None, 24, (5, 3), 3, 0, None);
    d.extend(body(15 * 3 + 6 + 3));
    files.push(d);
    let mut d = dds_file(Some(b"DX10"), 0, (8, 8), 1, 0x200 | 0xfc00, Some((95, 1)));
    d.extend(random_blocks(Format::Bc6h { signed: false }, 4 * 6, 5));
    files.push(d);
    for d in files {
        assert!(dds::detect(&d).is_some());
        aqr_testkit::damaged(&d, 60, 148, Duration::from_secs(30), |v| {
            let _ = dds::starts(v);
            if let Some(f) = formula::encode(v, 1) {
                assert_eq!(f.len(), v.len() as u64);
                let back = aqr_archive::Evaluate::eval(&f, &|a: &aqr_formula::Apply, inputs: &[&[u8]]| {
                    if a.module.name.ends_with("-recons") && a.module.name != "raster-recons" {
                        formula::decode_any(a, inputs, 1)
                    } else {
                        assert_eq!(a.module, aqr_raw::formula::raster::MODULE);
                        aqr_raw::formula::raster::decode(&a.params, inputs, a.len, 1)
                            .map_err(|e| aqr_formula::Error::new(e.0))
                    }
                });
                assert!(back.is_ok_and(|b| b == v));
            }
        });
    }
}
