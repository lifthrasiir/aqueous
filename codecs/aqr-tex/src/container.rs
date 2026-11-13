//! Containers of blocks other than DDS ([`crate::dds`]), whose surfaces are not in the order of
//! a layout's: KTX ([`ktx`]: by mipmap, each its layers; sizes between), KTX 2 ([`ktx2`]: each
//! mipmap anywhere by its index, layers within), PVR 3 ([`pvr`]: by mipmap, each its surfaces and
//! faces) and astcenc's `.astc` ([`astc`]: one surface). Taking one apart is finding its layout
//! and where each surface of it is ([`Found`]); everything else is kept as it is.
//!
//! Taken: 2D textures (arrays of them, cube maps) of the formats of [`Format`], not
//! supercompressed (KTX 2's scheme 0), every surface whole in the file.

use crate::dds::u32le;
use crate::layout::{ASTC_FOOTPRINTS, Format, Layout};

/// Where a container's surfaces are: its layout, and the start of the blocks of each surface in
/// the order of the layout's data (for each layer, its mipmaps from the largest).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub layout: Layout,
    pub starts: Vec<usize>,
}

impl Found {
    /// The pieces (start, length) of the file that are the blocks, in the order of the
    /// layout's data; None where any is past `len` bytes or two overlap.
    pub fn pieces(&self, len: usize) -> Option<Vec<(usize, usize)>> {
        let l = &self.layout;
        let layer = surface_bytes(l);
        let sizes: Vec<usize> = (0..l.layers).flat_map(|_| layer.iter().copied()).collect();
        if sizes.len() != self.starts.len() {
            return None;
        }
        let p: Vec<(usize, usize)> = self.starts.iter().copied().zip(sizes).collect();
        let mut sorted = p.clone();
        sorted.sort();
        let mut end = 0;
        for &(s, n) in &sorted {
            if s < end || s.checked_add(n)? > len {
                return None;
            }
            end = s + n;
        }
        Some(p)
    }
}

fn u64le(d: &[u8], p: usize) -> Option<u64> {
    Some(u64::from_le_bytes(*d.get(p..)?.first_chunk()?))
}

/// The bytes of the blocks of each mipmap of `l`'s surfaces, from the largest.
fn surface_bytes(l: &Layout) -> Vec<usize> {
    l.mip_surfaces().map(|s| s.blocks() * l.format.block_bytes()).collect()
}

/// The layout of `width` × `height`, of `mips` mipmaps (0 for 1) and `layers` (0 for 1), if it is
/// valid.
fn layout(format: Format, width: u32, height: u32, mips: u32, layers: u32) -> Option<Layout> {
    let l = Layout { format, width, height, mips: mips.max(1), layers: layers.max(1) };
    l.valid().then_some(l)
}

const KTX: &[u8; 12] = b"\xabKTX 11\xbb\r\n\x1a\n";
const KTX2: &[u8; 12] = b"\xabKTX 20\xbb\r\n\x1a\n";

/// Whether `d` starts like a KTX file (of either version).
pub fn starts_ktx(d: &[u8]) -> bool {
    d.starts_with(KTX) || d.starts_with(KTX2)
}

/// The format of an OpenGL internal format.
fn gl_format(f: u32) -> Option<Format> {
    Some(match f {
        0x83f0 | 0x83f1 | 0x8c4c | 0x8c4d => Format::Bc1,
        0x83f2 | 0x8c4e => Format::Bc2,
        0x83f3 | 0x8c4f => Format::Bc3,
        0x8dbb | 0x8dbc => Format::Bc4,
        0x8dbd | 0x8dbe => Format::Bc5,
        0x8e8e => Format::Bc6h { signed: true },
        0x8e8f => Format::Bc6h { signed: false },
        0x8e8c | 0x8e8d => Format::Bc7,
        0x8d64 => Format::Etc1,
        0x9274 | 0x9275 => Format::Etc2,
        0x9276 | 0x9277 => Format::Etc2P,
        0x9278 | 0x9279 => Format::Etc2A,
        0x9270..=0x9273 => Format::Eac { rg: f >= 0x9272, signed: f & 1 != 0 },
        0x8c00 | 0x8c02 => Format::Pvrtc { two: false },
        0x8c01 | 0x8c03 => Format::Pvrtc { two: true },
        0x93b0..=0x93bd | 0x93d0..=0x93dd => {
            let (w, h) = ASTC_FOOTPRINTS[(f & 0xf) as usize];
            Format::Astc { w, h }
        }
        _ => return None,
    })
}

/// A KTX file (version 1): its header (64 bytes, of either byte order), its key and value data,
/// then each mipmap from the largest: its size (u32), then each layer's (the array's elements,
/// each its faces), padded to 4 bytes (each face of a cube map that is no array, and each
/// mipmap).
pub fn ktx(d: &[u8]) -> Option<Found> {
    if !d.starts_with(KTX) {
        return None;
    }
    let big = match u32le(d, 12)? {
        0x0403_0201 => false,
        0x0102_0304 => true,
        _ => return None,
    };
    let u = |p: usize| -> Option<u32> {
        let v = u32le(d, p)?;
        Some(if big { v.swap_bytes() } else { v })
    };
    let (gl_type, internal) = (u(16)?, u(28)?);
    let (width, height, depth) = (u(36)?, u(40)?, u(44)?);
    let (elements, faces, mips, kv) = (u(48)?, u(52)?, u(56)?, u(60)?);
    if gl_type != 0 || depth > 1 || height == 0 || !(faces == 1 || faces == 6) {
        return None;
    }
    let format = gl_format(internal)?;
    let l = layout(format, width, height, mips, elements.max(1).checked_mul(faces)?)?;
    let cube = faces == 6 && elements == 0;
    let surfaces = surface_bytes(&l);
    let mut at = 64usize.checked_add(kv as usize)?;
    let mut starts = vec![0; l.layers as usize * surfaces.len()];
    for (m, &n) in surfaces.iter().enumerate() {
        let size = u(at)? as usize;
        // (the size of a face of a cube map that is no array, else of the mipmap)
        let want = if cube { n } else { n * l.layers as usize };
        if size != want {
            return None;
        }
        at += 4;
        for k in 0..l.layers as usize {
            starts[k * surfaces.len() + m] = at;
            at = at.checked_add(n)?;
            if cube {
                at = at.next_multiple_of(4);
            }
        }
        at = at.next_multiple_of(4);
    }
    (at <= d.len()).then_some(Found { layout: l, starts })
}

/// The format of a Vulkan format.
fn vk_format(f: u32) -> Option<Format> {
    Some(match f {
        131..=134 => Format::Bc1,
        135 | 136 => Format::Bc2,
        137 | 138 => Format::Bc3,
        139 | 140 => Format::Bc4,
        141 | 142 => Format::Bc5,
        143 => Format::Bc6h { signed: false },
        144 => Format::Bc6h { signed: true },
        145 | 146 => Format::Bc7,
        147 | 148 => Format::Etc2,
        149 | 150 => Format::Etc2P,
        151 | 152 => Format::Etc2A,
        153..=156 => Format::Eac { rg: f >= 155, signed: f.is_multiple_of(2) },
        157..=184 => {
            let (w, h) = ASTC_FOOTPRINTS[(f - 157) as usize / 2];
            Format::Astc { w, h }
        }
        1_000_066_000..=1_000_066_013 => {
            let (w, h) = ASTC_FOOTPRINTS[(f - 1_000_066_000) as usize];
            Format::Astc { w, h }
        }
        1_000_054_000 | 1_000_054_004 => Format::Pvrtc { two: true },
        1_000_054_001 | 1_000_054_005 => Format::Pvrtc { two: false },
        _ => return None,
    })
}

/// A KTX 2 file: its header (80 bytes), the index of its mipmaps (offset, length and length
/// uncompressed, u64 each; from the largest), each mipmap its layers (the array's elements, each
/// its faces). Of no supercompression only.
pub fn ktx2(d: &[u8]) -> Option<Found> {
    if !d.starts_with(KTX2) {
        return None;
    }
    let format = vk_format(u32le(d, 12)?)?;
    let (width, height, depth) = (u32le(d, 20)?, u32le(d, 24)?, u32le(d, 28)?);
    let (elements, faces, mips, scheme) = (u32le(d, 32)?, u32le(d, 36)?, u32le(d, 40)?, u32le(d, 44)?);
    if scheme != 0 || depth > 1 || height == 0 || !(faces == 1 || faces == 6) {
        return None;
    }
    let l = layout(format, width, height, mips, elements.max(1).checked_mul(faces)?)?;
    let surfaces = surface_bytes(&l);
    let mut starts = vec![0; l.layers as usize * surfaces.len()];
    for (m, &n) in surfaces.iter().enumerate() {
        let at = 80 + 24 * m;
        let (offset, len) = (usize::try_from(u64le(d, at)?).ok()?, u64le(d, at + 8)?);
        if len != (n * l.layers as usize) as u64 {
            return None;
        }
        for k in 0..l.layers as usize {
            starts[k * surfaces.len() + m] = offset.checked_add(k * n)?;
        }
    }
    Some(Found { layout: l, starts })
}

/// Whether `d` starts like a PVR file (version 3).
pub fn starts_pvr(d: &[u8]) -> bool {
    d.starts_with(b"PVR\x03")
}

/// A PVR file (version 3): its header (52 bytes), its metadata, then each mipmap from the
/// largest: each surface (the array's elements), each its faces. The format by the pixel
/// format (signed EAC by the channel type).
pub fn pvr(d: &[u8]) -> Option<Found> {
    if !starts_pvr(d) {
        return None;
    }
    let (pf, channel) = (u64le(d, 8)?, u32le(d, 20)?);
    let (height, width, depth) = (u32le(d, 24)?, u32le(d, 28)?, u32le(d, 32)?);
    let (elements, faces, mips, meta) = (u32le(d, 36)?, u32le(d, 40)?, u32le(d, 44)?, u32le(d, 48)?);
    // (channel types: signed byte, short, int of 1, 3, 5, normalized or not)
    let signed = matches!(channel, 1 | 3 | 5 | 7 | 9 | 11);
    let format = match pf {
        0 | 1 => Format::Pvrtc { two: true },
        2 | 3 => Format::Pvrtc { two: false },
        6 => Format::Etc1,
        7 => Format::Bc1,
        8 | 9 => Format::Bc2,
        10 | 11 => Format::Bc3,
        12 => Format::Bc4,
        13 => Format::Bc5,
        14 => Format::Bc6h { signed },
        15 => Format::Bc7,
        22 => Format::Etc2,
        23 => Format::Etc2A,
        24 => Format::Etc2P,
        25 => Format::Eac { rg: false, signed },
        26 => Format::Eac { rg: true, signed },
        27..=40 => {
            let (w, h) = ASTC_FOOTPRINTS[pf as usize - 27];
            Format::Astc { w, h }
        }
        _ => return None,
    };
    if depth > 1 || !(faces == 1 || faces == 6) {
        return None;
    }
    let l = layout(format, width, height, mips, elements.max(1).checked_mul(faces)?)?;
    let surfaces = surface_bytes(&l);
    let mut at = 52usize.checked_add(meta as usize)?;
    let mut starts = vec![0; l.layers as usize * surfaces.len()];
    for (m, &n) in surfaces.iter().enumerate() {
        for k in 0..l.layers as usize {
            starts[k * surfaces.len() + m] = at;
            at = at.checked_add(n)?;
        }
    }
    (at <= d.len()).then_some(Found { layout: l, starts })
}

/// Whether `d` starts like an `.astc` file.
pub fn starts_astc(d: &[u8]) -> bool {
    d.starts_with(&[0x13, 0xab, 0xa1, 0x5c])
}

/// An `.astc` file (astcenc's): its header (16 bytes: the magic, the footprint's width, height
/// and depth, u8 each, the image's, u24 each), then the blocks of one surface. Of 2D footprints
/// and images only.
pub fn astc(d: &[u8]) -> Option<Found> {
    if !starts_astc(d) || d.len() < 16 {
        return None;
    }
    let u24 = |p: usize| d[p] as u32 | (d[p + 1] as u32) << 8 | (d[p + 2] as u32) << 16;
    let (bw, bh, bd) = (d[4], d[5], d[6]);
    if bd != 1 || u24(13) != 1 || !ASTC_FOOTPRINTS.contains(&(bw, bh)) {
        return None;
    }
    let l = layout(Format::Astc { w: bw, h: bh }, u24(7), u24(10), 1, 1)?;
    (16 + l.len() <= d.len() as u64).then(|| Found { layout: l, starts: vec![16] })
}

/// The container of `d`, if it is one of those taken: KTX, KTX 2, PVR or `.astc`.
pub fn detect(d: &[u8]) -> Option<Found> {
    let f = ktx(d).or_else(|| ktx2(d)).or_else(|| pvr(d)).or_else(|| astc(d))?;
    f.pieces(d.len()).is_some().then_some(f)
}

/// Whether `d` starts like any of the containers: a KTX file's identifier, a PVR file's of a
/// pixel format taken, an `.astc` file's of a footprint taken.
pub fn starts(d: &[u8]) -> bool {
    let pvr = starts_pvr(d) && u64le(d, 8).is_some_and(|f| matches!(f, 0..=3 | 6..=15 | 22..=40));
    let astc = starts_astc(d) && d.len() >= 7 && d[6] == 1 && ASTC_FOOTPRINTS.contains(&(d[4], d[5]));
    starts_ktx(d) || pvr || astc
}

/// The end of the container's data in `d` (the last of its blocks), if it is one taken.
pub fn extent(d: &[u8]) -> Option<usize> {
    let f = detect(d)?;
    f.pieces(d.len())?.iter().map(|&(s, n)| s + n).max()
}
