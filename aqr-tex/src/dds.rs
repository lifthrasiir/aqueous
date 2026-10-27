//! DDS files: a header (`DDS `, 124 bytes, and 20 more of DX10 where the pixel format says
//! so), then the data of every surface, layer by layer, each layer's mipmaps from the largest.
//! Taking one apart is finding the data and its layout; the header, and whatever follows the
//! data, is kept as it is.
//!
//! Taken: BC1 to BC5 by their FourCC (`DXT1` to `DXT5`, `ATI1`, `ATI2`, `BC4U`, `BC4S`,
//! `BC5U`, `BC5S`) or their DXGI format (typeless, unsigned, sRGB and signed: their blocks are
//! alike), and uncompressed pixels of 8, 24 or 32 bits without a FourCC (their channels as
//! they are, a byte each; [`Data::Pixels`]); 2D textures, arrays of them and cube maps of all
//! six faces. Volume textures are not.

use crate::layout::{Format, Layout};

/// What a DDS file's data is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Data {
    /// Blocks of a layout.
    Blocks(Layout),
    /// Uncompressed pixels of `channels` bytes each, rows unpadded, of the surfaces of a
    /// layout (its format aside).
    Pixels { channels: usize, layout: Layout },
}

impl Data {
    /// Bytes of the data.
    pub fn len(&self) -> u64 {
        match *self {
            Data::Blocks(l) => l.len(),
            Data::Pixels { channels, layout } => {
                pixel_surfaces(&layout).map(|(w, h)| w * h).sum::<u64>() * channels as u64
            }
        }
    }
}

/// The sizes in pixels of the surfaces of `l`, in the order of their data.
pub fn pixel_surfaces(l: &Layout) -> impl Iterator<Item = (u64, u64)> + use<> {
    let (w, h, mips) = (l.width, l.height, l.mips);
    (0..l.layers).flat_map(move |_| (0..mips).map(move |m| ((w >> m).max(1) as u64, (h >> m).max(1) as u64)))
}

/// Where a DDS file's data is, and what it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Found {
    pub data: Data,
    /// The data is `d[start..start + data.len()]`.
    pub start: usize,
}

const MAGIC: &[u8; 4] = b"DDS ";
const HEADER: usize = 128;
const DX10: usize = 20;

// dwFlags
const DDSD_MIPMAPCOUNT: u32 = 0x2_0000;
const DDSD_DEPTH: u32 = 0x80_0000;
// ddspf.dwFlags
const DDPF_FOURCC: u32 = 0x4;
// dwCaps2
const DDSCAPS2_CUBEMAP: u32 = 0x200;
const DDSCAPS2_ALLFACES: u32 = 0xfc00;
const DDSCAPS2_VOLUME: u32 = 0x20_0000;
// DX10 resourceDimension and miscFlag
const DIMENSION_TEXTURE2D: u32 = 3;
const RESOURCE_MISC_TEXTURECUBE: u32 = 0x4;

fn u32le(d: &[u8], p: usize) -> Option<u32> {
    Some(u32::from_le_bytes(*d.get(p..)?.first_chunk()?))
}

/// Whether `d` starts like a DDS file.
pub fn starts(d: &[u8]) -> bool {
    d.starts_with(MAGIC) && u32le(d, 4) == Some(124)
}

/// The data of DDS file `d`, if it is of those taken, and all of it is there.
pub fn detect(d: &[u8]) -> Option<Found> {
    if !starts(d) {
        return None;
    }
    let flags = u32le(d, 8)?;
    let (height, width) = (u32le(d, 12)?, u32le(d, 16)?);
    let mips = if flags & DDSD_MIPMAPCOUNT != 0 { u32le(d, 28)?.max(1) } else { 1 };
    let (pf_flags, fourcc, bits) = (u32le(d, 80)?, d.get(84..88)?, u32le(d, 88)?);
    let caps2 = u32le(d, 112)?;
    if flags & DDSD_DEPTH != 0 && u32le(d, 24)? > 1 || caps2 & DDSCAPS2_VOLUME != 0 {
        return None;
    }
    // (BC1 stands for the format of uncompressed pixels, which has none)
    let (format, channels, mut layers, start) = if pf_flags & DDPF_FOURCC == 0 {
        match bits {
            8 | 24 | 32 => (Format::Bc1, Some(bits as usize / 8), 1, HEADER),
            _ => return None,
        }
    } else if fourcc == b"DX10" {
        let format = match u32le(d, HEADER)? {
            70..=72 => Format::Bc1,
            73..=75 => Format::Bc2,
            76..=78 => Format::Bc3,
            79..=81 => Format::Bc4,
            82..=84 => Format::Bc5,
            _ => return None,
        };
        let (dimension, misc, array) = (u32le(d, HEADER + 4)?, u32le(d, HEADER + 8)?, u32le(d, HEADER + 12)?);
        if dimension != DIMENSION_TEXTURE2D {
            return None;
        }
        let faces = if misc & RESOURCE_MISC_TEXTURECUBE != 0 { 6 } else { 1 };
        (format, None, array.max(1).checked_mul(faces)?, HEADER + DX10)
    } else {
        let format = match fourcc {
            b"DXT1" => Format::Bc1,
            b"DXT2" | b"DXT3" => Format::Bc2,
            b"DXT4" | b"DXT5" => Format::Bc3,
            b"ATI1" | b"BC4U" | b"BC4S" => Format::Bc4,
            b"ATI2" | b"BC5U" | b"BC5S" => Format::Bc5,
            _ => return None,
        };
        (format, None, 1, HEADER)
    };
    if caps2 & DDSCAPS2_CUBEMAP != 0 {
        // (a cube map of some faces only is not taken)
        if caps2 & DDSCAPS2_ALLFACES != DDSCAPS2_ALLFACES {
            return None;
        }
        if fourcc != b"DX10" || channels.is_some() {
            layers = 6;
        }
    }
    let layout = Layout { format, width, height, mips, layers };
    if !layout.valid() {
        return None;
    }
    let data = match channels {
        Some(channels) => Data::Pixels { channels, layout },
        None => Data::Blocks(layout),
    };
    (data.len() <= (d.len() - start.min(d.len())) as u64).then_some(Found { data, start })
}
