//! Image XObjects: their geometry, and FlateDecode images with PNG predictors coded by
//! aqr-png, wrapped as PNG files.

use crate::lex::Obj;
use crate::objects::Objects;

/// What the data of a FlateDecode image is, after inflating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub width: u32,
    pub height: u32,
    pub colors: u32,
    pub bpc: u32,
    /// The DecodeParms predictor (1: none, 2: TIFF, 10-15: PNG).
    pub predictor: u32,
}

impl Geometry {
    /// Bytes per row (without a PNG filter byte).
    pub fn row(&self) -> usize {
        (self.width as usize * self.colors as usize * self.bpc as usize).div_ceil(8)
    }

    /// Bytes per pixel for prediction (at least 1).
    pub fn bpp(&self) -> usize {
        (self.colors as usize * self.bpc as usize).div_ceil(8).max(1)
    }
}

/// The number of color components of a color space.
fn colors<'a>(cs: &Obj<'a>, objs: &mut Objects<'a>, depth: usize) -> Option<u32> {
    if depth > 4 {
        return None;
    }
    match cs {
        Obj::Name(n) => match *n {
            b"DeviceGray" | b"CalGray" | b"G" => Some(1),
            b"DeviceRGB" | b"CalRGB" | b"RGB" => Some(3),
            b"DeviceCMYK" | b"CMYK" => Some(4),
            _ => None,
        },
        Obj::Array(a) => match a.first()?.as_name()? {
            b"ICCBased" => {
                let s = objs.deref(a.get(1)?)?;
                s.get(b"N").and_then(Obj::as_int).map(|n| n as u32)
            }
            b"Indexed" | b"I" | b"Separation" => Some(1),
            b"DeviceN" => match objs.deref(a.get(1)?)? {
                Obj::Array(names) => Some(names.len() as u32),
                _ => None,
            },
            b"CalRGB" | b"Lab" => Some(3),
            b"CalGray" => Some(1),
            _ => None,
        },
        Obj::Ref(..) => {
            let o = objs.deref(cs)?;
            colors(&o, objs, depth + 1)
        }
        _ => None,
    }
}

/// The geometry of an image with dictionary `d`, if it is consistent.
pub fn geometry<'a>(d: &Obj<'a>, objs: &mut Objects<'a>) -> Option<Geometry> {
    // After a second filter, the data is not pixels.
    if d.get(b"Filter").is_some_and(|f| f.names().len() != 1) {
        return None;
    }
    let int = |o: Option<&Obj>| o.and_then(Obj::as_int);
    let parms = match d.get(b"DecodeParms") {
        Some(Obj::Array(a)) => a.first().cloned(),
        Some(p) => Some(p.clone()),
        None => None,
    };
    let parms = match parms {
        Some(p @ Obj::Ref(..)) => objs.deref(&p),
        p => p,
    };
    let pget = |k: &[u8]| int(parms.as_ref().and_then(|p| p.get(k)));
    let width = int(d.get(b"Width"))?;
    let height = int(d.get(b"Height"))?;
    let mask = d.get(b"ImageMask") == Some(&Obj::Bool(true));
    let bpc = if mask { 1 } else { int(d.get(b"BitsPerComponent")).unwrap_or(8) };
    let predictor = pget(b"Predictor").unwrap_or(1);
    let colors = if mask {
        1
    } else {
        match d.get(b"ColorSpace") {
            Some(cs) => colors(cs, objs, 0)? as i64,
            None => 1,
        }
    };
    let g = if predictor >= 2 {
        // The predictor's own parameters describe the rows. They should match the image;
        // where they do not (some writers get them wrong), the image's are more likely
        // the data's, and a wrong guess only costs ratio.
        if pget(b"Columns").unwrap_or(1) != width {
            return None;
        }
        (width, height, colors, bpc, predictor)
    } else {
        (width, height, colors, bpc, 1)
    };
    let ok = |v: i64, max: i64| (1..=max).contains(&v);
    if !ok(g.0, 1 << 20)
        || !ok(g.1, 1 << 20)
        || !ok(g.2, 32)
        || ![1, 2, 4, 8, 16].contains(&g.3)
        || !(g.4 == 1 || g.4 == 2 || (10..=15).contains(&g.4))
    {
        return None;
    }
    Some(Geometry { width: g.0 as u32, height: g.1 as u32, colors: g.2 as u32, bpc: g.3 as u32, predictor: g.4 as u32 })
}

/// The PNG color type and bit depth for PNG-predicted rows of `g`, if PNG has them.
pub fn png_format(g: &Geometry) -> Option<(u8, u8)> {
    if g.predictor < 10 {
        return None;
    }
    let ct = match (g.colors, g.bpc) {
        (1, 1 | 2 | 4 | 8 | 16) => 0,
        (2, 8 | 16) => 4,
        (3, 8 | 16) => 2,
        (4, 8 | 16) => 6,
        _ => return None,
    };
    Some((ct, g.bpc as u8))
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32fast::hash(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// Bytes of the PNG wrapper before the stream's data.
const PRE: usize = 8 + 25 + 8;

/// A PNG file holding `data` (a zlib stream of PNG rows) as its one IDAT chunk.
pub fn wrap(width: u32, height: u32, ct: u8, depth: u8, data: &[u8]) -> Vec<u8> {
    let mut png = Vec::with_capacity(data.len() + PRE + 16);
    png.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut ih = vec![];
    ih.extend_from_slice(&width.to_be_bytes());
    ih.extend_from_slice(&height.to_be_bytes());
    ih.extend_from_slice(&[depth, ct, 0, 0, 0]);
    chunk(&mut png, b"IHDR", &ih);
    chunk(&mut png, b"IDAT", data);
    chunk(&mut png, b"IEND", &[]);
    png
}

/// The data of a PNG file made by [`wrap`].
pub fn unwrap(png: &[u8]) -> Option<&[u8]> {
    let n = u32::from_be_bytes(png.get(PRE - 8..PRE - 4)?.try_into().ok()?) as usize;
    png.get(PRE..PRE.checked_add(n)?)
}

/// Whether a JPEG is progressive (its first frame header is SOF2, SOF6, SOF10 or SOF14),
/// and its number of 8×8 blocks, from its markers; `None` if no frame header is found.
pub fn jpeg_frame(d: &[u8]) -> Option<(bool, u64)> {
    let mut i = 2;
    if d.get(..2)? != [0xff, 0xd8] {
        return None;
    }
    while i + 4 <= d.len() {
        if d[i] != 0xff {
            return None;
        }
        let m = d[i + 1];
        if m == 0xff {
            i += 1;
            continue;
        }
        if (0xc0..=0xcf).contains(&m) && !matches!(m, 0xc4 | 0xc8 | 0xcc) {
            return Some((matches!(m, 0xc2 | 0xc6 | 0xca | 0xce), jpeg_blocks(d.get(i + 4..)?).unwrap_or(0)));
        }
        if matches!(m, 0xd8 | 0x01 | 0xd0..=0xd7) {
            i += 2;
            continue;
        }
        let len = u16::from_be_bytes([d[i + 2], d[i + 3]]) as usize;
        i += 2 + len;
    }
    None
}

/// The number of 8×8 blocks of a frame, from its header (after the length).
fn jpeg_blocks(h: &[u8]) -> Option<u64> {
    let height = u16::from_be_bytes([*h.get(1)?, *h.get(2)?]) as u64;
    let width = u16::from_be_bytes([*h.get(3)?, *h.get(4)?]) as u64;
    let n = *h.get(5)? as usize;
    let comps = h.get(6..6 + 3 * n)?;
    let samp = |c: &[u8]| ((c[1] >> 4).max(1) as u64, (c[1] & 15).max(1) as u64);
    let hmax = comps.chunks_exact(3).map(|c| samp(c).0).max()?;
    let vmax = comps.chunks_exact(3).map(|c| samp(c).1).max()?;
    Some(
        comps
            .chunks_exact(3)
            .map(|c| {
                let (hs, vs) = samp(c);
                (width * hs).div_ceil(8 * hmax) * (height * vs).div_ceil(8 * vmax)
            })
            .sum(),
    )
}
