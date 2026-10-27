//! DDS files through the formula: blocks of every format and layout (mipmaps, cube maps, DX10
//! arrays, sides not of whole blocks), uncompressed pixels, trailers, files not taken, and
//! corrupted coded blocks.

use aqr_formula::{Apply, Formula};
use aqr_tex::layout::{Format, Layout};
use aqr_tex::{dds, formula, module, texture};

fn eval(f: &Formula) -> aqr_formula::Result<Vec<u8>> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| {
        assert!(a.resources.is_empty());
        if a.module == formula::MODULE {
            module::decode(&a.params, inputs, a.len, 1).map_err(|e| aqr_formula::Error::new(e.0))
        } else {
            assert_eq!(a.module, aqr_raw::formula::raster::MODULE);
            aqr_raw::formula::raster::decode(&a.params, inputs, a.len, 1).map_err(|e| aqr_formula::Error::new(e.0))
        }
    })
}

fn check(name: &str, d: &[u8]) -> Formula {
    let f = formula::encode(d, 1).unwrap_or_else(|| panic!("{name}: not taken"));
    assert_eq!(f.len(), d.len() as u64, "{name}");
    assert!(eval(&f).unwrap() == d, "{name}");
    f
}

struct Rng(u32);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }
}

/// Blocks of every kind the models tell apart: repeated, endpoints of a neighbour or of a
/// block before, flat, either mode, and random ones.
fn blocks(format: Format, n: usize, seed: u32) -> Vec<u8> {
    let mut r = Rng(seed | 1);
    let mut seen: Vec<[u8; 8]> = vec![];
    let mut half = |r: &mut Rng, value: bool| -> [u8; 8] {
        let mut b = [0u8; 8];
        for x in b.iter_mut() {
            *x = r.next() as u8;
        }
        match r.next() % 8 {
            0 if !seen.is_empty() => b = seen[r.next() as usize % seen.len()],
            1 if !seen.is_empty() => {
                let s = seen[r.next() as usize % seen.len()];
                let k = if value { 2 } else { 4 };
                b[..k].copy_from_slice(&s[..k]);
            }
            2 => {
                // flat: the endpoints alike
                let k = if value { 1 } else { 2 };
                let (a, c) = b.split_at_mut(k);
                c[..k].copy_from_slice(a);
            }
            3 => b[4..].fill(0),
            _ => {}
        }
        seen.push(b);
        b
    };
    let mut out = Vec::with_capacity(n * format.block_bytes());
    for _ in 0..n {
        match format {
            Format::Bc1 => out.extend(half(&mut r, false)),
            Format::Bc2 => {
                let a = (r.next() as u64) << 32 | r.next() as u64;
                out.extend(a.to_le_bytes());
                out.extend(half(&mut r, false));
            }
            Format::Bc3 => {
                out.extend(half(&mut r, true));
                out.extend(half(&mut r, false));
            }
            Format::Bc4 => out.extend(half(&mut r, true)),
            Format::Bc5 => {
                out.extend(half(&mut r, true));
                out.extend(half(&mut r, true));
            }
        }
    }
    out
}

const DDSD_MIPMAPCOUNT: u32 = 0x2_0000;

/// A DDS header: `fourcc` (or none, for pixels of `bits`), the size, mipmaps, `caps2`, and a
/// DX10 header of `dx10` (DXGI format, misc flag, array size).
fn header(
    fourcc: Option<&[u8; 4]>,
    bits: u32,
    (w, h): (u32, u32),
    mips: u32,
    caps2: u32,
    dx10: Option<(u32, u32, u32)>,
) -> Vec<u8> {
    let mut d = b"DDS ".to_vec();
    let u = |d: &mut Vec<u8>, v: u32| d.extend(v.to_le_bytes());
    u(&mut d, 124);
    u(&mut d, 0x1007 | if mips > 1 { DDSD_MIPMAPCOUNT } else { 0 });
    u(&mut d, h);
    u(&mut d, w);
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
    if let Some((format, misc, array)) = dx10 {
        for v in [format, 3, misc, array, 0] {
            u(&mut d, v);
        }
    }
    d
}

fn file(format: Format, size: (u32, u32), mips: u32, layers: u32, cube: bool, seed: u32) -> Vec<u8> {
    let fourcc: &[u8; 4] = match format {
        Format::Bc1 => b"DXT1",
        Format::Bc2 => b"DXT3",
        Format::Bc3 => b"DXT5",
        Format::Bc4 => b"ATI1",
        Format::Bc5 => b"ATI2",
    };
    let l = Layout { format, width: size.0, height: size.1, mips, layers };
    let mut d = header(Some(fourcc), 0, size, mips, if cube { 0xfe00 } else { 0 }, None);
    d.extend(blocks(format, (l.len() / format.block_bytes() as u64) as usize, seed));
    d
}

#[test]
fn every_format_and_layout() {
    for format in Format::ALL {
        for (size, mips) in [((1, 1), 1), ((4, 4), 1), ((5, 3), 1), ((64, 64), 7), ((37, 13), 6), ((256, 8), 9)] {
            let d = file(format, size, mips, 1, false, size.0 * 7 + mips);
            let f = check(&format!("{format:?} {size:?} {mips}"), &d);
            let found = dds::detect(&d).unwrap();
            assert_eq!(
                found.data,
                dds::Data::Blocks(Layout { format, width: size.0, height: size.1, mips, layers: 1 })
            );
            assert!(f.streams.iter().any(|s| s.class == "bcn/coded"));
        }
        let d = file(format, (32, 32), 6, 6, true, 3);
        check(&format!("{format:?} cube"), &d);
    }
}

#[test]
fn dx10_arrays_and_trailers() {
    let l = Layout { format: Format::Bc3, width: 16, height: 8, mips: 3, layers: 3 };
    let mut d = header(Some(b"DX10"), 0, (16, 8), 3, 0, Some((77, 0, 3)));
    d.extend(blocks(Format::Bc3, (l.len() / 16) as usize, 9));
    assert_eq!(dds::detect(&d).unwrap(), dds::Found { data: dds::Data::Blocks(l), start: 148 });
    check("dx10 array", &d);
    // a cube map of arrays: six layers each
    let l = Layout { format: Format::Bc1, width: 8, height: 8, mips: 1, layers: 12 };
    let mut c = header(Some(b"DX10"), 0, (8, 8), 1, 0, Some((71, 4, 2)));
    c.extend(blocks(Format::Bc1, (l.len() / 8) as usize, 5));
    assert_eq!(dds::detect(&c).unwrap().data, dds::Data::Blocks(l));
    check("dx10 cube array", &c);
    d.extend_from_slice(b"what follows");
    check("trailer", &d);
}

#[test]
fn smooth_textures_code_small() {
    // blocks of a smooth picture: the models predict them well
    let (w, h) = (128u32, 128u32);
    let l = Layout { format: Format::Bc1, width: w, height: h, mips: 1, layers: 1 };
    let mut b = vec![];
    for by in 0..h / 4 {
        for bx in 0..w / 4 {
            let c0 = ((bx / 2) << 11 | (by / 2) << 5 | (31 - bx / 2)) as u16;
            let c1 = c0.saturating_sub(0x0841);
            b.extend(c0.max(c1 + 1).to_le_bytes());
            b.extend(c1.to_le_bytes());
            b.extend([0x00u8, 0x55, 0xaa, 0xff]);
        }
    }
    let (main, apart, _) = texture::encode(&l, &b, 1);
    assert!(apart.is_empty());
    assert!(main.len() * 10 < b.len(), "{} of {}", main.len(), b.len());
    assert_eq!(texture::decode(&l, &main, &apart, 1).unwrap(), b);
}

/// Surfaces large enough to be coded in stripes: the same coded form whatever the threads, and
/// decoded on any number of them.
#[test]
fn stripes() {
    for (format, size, mips, layers) in [(Format::Bc1, (1024, 1024), 3, 1), (Format::Bc5, (1024, 520), 2, 2)] {
        let l = Layout { format, width: size.0, height: size.1, mips, layers };
        assert!(texture::stripes(l.mip_surfaces().next().unwrap().blocks()) > 1);
        let b = blocks(format, (l.len() / format.block_bytes() as u64) as usize, 21);
        let (main, apart, _) = texture::encode(&l, &b, 1);
        for threads in [2, 3, 8] {
            let (m, a, _) = texture::encode(&l, &b, threads);
            assert!(m == main && a == apart, "{format:?} on {threads} threads");
            assert!(texture::decode(&l, &main, &apart, threads).unwrap() == b, "{format:?} on {threads} threads");
        }
        assert!(texture::decode(&l, &main, &apart, 1).unwrap() == b);
        // the streams of the stripes, one too few or too many, or none
        let mut streams = aqr_common::packed::unpack(&apart).unwrap();
        assert!(streams.len() > 1);
        assert!(texture::decode(&l, &main, &[], 4).is_err());
        let last = streams.pop().unwrap();
        assert!(texture::decode(&l, &main, &aqr_common::packed::pack(&streams), 4).is_err());
        streams.extend([last, last]);
        assert!(texture::decode(&l, &main, &aqr_common::packed::pack(&streams), 4).is_err());
        // a stripe's stream cut
        streams.pop();
        let n = streams.len();
        streams[n - 1] = &last[..last.len() / 2];
        let cut = texture::decode(&l, &main, &aqr_common::packed::pack(&streams), 4);
        assert!(!matches!(cut, Ok(o) if o == b));
    }
}

#[test]
fn pixels() {
    for (bits, mips) in [(32, 5), (24, 1), (8, 4)] {
        let (w, h) = (24u32, 16u32);
        let mut d = header(None, bits, (w, h), mips, 0, None);
        let n: u64 = (0..mips).map(|m| ((w >> m).max(1) * (h >> m).max(1)) as u64).sum();
        let mut r = Rng(bits);
        for i in 0..n * bits as u64 / 8 {
            d.push((i / 5) as u8 ^ (r.next() % 4) as u8);
        }
        let f = check(&format!("{bits} bits"), &d);
        assert!(matches!(dds::detect(&d).unwrap().data, dds::Data::Pixels { .. }));
        assert!(f.streams.iter().all(|s| s.class != "bcn/coded"));
    }
}

#[test]
fn not_taken() {
    let d = file(Format::Bc1, (16, 16), 1, 1, false, 1);
    // cut short, a volume, some faces of a cube, an unknown FourCC, more mipmaps than halvings
    assert!(formula::encode(&d[..d.len() - 1], 1).is_none());
    let mut v = d.clone();
    v[8..12].copy_from_slice(&(0x80_1007u32).to_le_bytes());
    v[24..28].copy_from_slice(&4u32.to_le_bytes());
    assert!(dds::detect(&v).is_none());
    let mut c = d.clone();
    c[112..116].copy_from_slice(&0x0600u32.to_le_bytes());
    assert!(dds::detect(&c).is_none());
    let mut u = d.clone();
    u[84..88].copy_from_slice(b"BC7U");
    assert!(dds::detect(&u).is_none());
    let mut m = d.clone();
    m[8..12].copy_from_slice(&(0x2_1007u32).to_le_bytes());
    m[28..32].copy_from_slice(&6u32.to_le_bytes());
    assert!(dds::detect(&m).is_none());
    assert!(dds::detect(b"DDS ").is_none());
    assert!(formula::encode(b"", 1).is_none());
}

#[test]
fn corrupted() {
    for format in Format::ALL {
        let l = Layout { format, width: 32, height: 16, mips: 6, layers: 1 };
        let b = blocks(format, (l.len() / format.block_bytes() as u64) as usize, 11);
        let (coded, apart, _) = texture::encode(&l, &b, 1);
        let p = l.to_bytes();
        assert_eq!(module::decode(&p, &[&coded, &apart], l.len(), 1).unwrap(), b);
        // the wrong length, inputs or params; streams apart where there are none
        assert!(module::decode(&p, &[&coded, &apart], l.len() + 1, 1).is_err());
        assert!(module::decode(&p, &[&coded], l.len(), 1).is_err());
        assert!(module::decode(&p, &[&coded, &aqr_common::packed::pack::<&[u8]>(&[])], l.len(), 1).is_err());
        assert!(module::decode(&p[..p.len() - 1], &[&coded, &apart], l.len(), 1).is_err());
        // cut, or bits flipped: an error or other blocks, never a panic
        let mut r = Rng(format as u32 + 1);
        for _ in 0..200 {
            let mut c = coded.clone();
            match r.next() % 3 {
                0 => c.truncate(r.next() as usize % c.len()),
                1 => {
                    let i = r.next() as usize % c.len();
                    c[i] ^= 1 << (r.next() % 8);
                }
                _ => c.extend_from_slice(&[0xff; 9]),
            }
            if let Ok(o) = module::decode(&p, &[&c, &apart], l.len(), 1) {
                assert_eq!(o.len() as u64, l.len());
            }
        }
    }
    // a large texture of a few bytes is not decoded
    let l = Layout { format: Format::Bc1, width: 1 << 16, height: 1 << 16, mips: 1, layers: 1 };
    assert!(module::decode(&l.to_bytes(), &[&[0; 4], &[]], l.len(), 1).is_err());
}
