//! Where the blocks of a texture are: its format, its size, its mipmaps and its layers (the
//! faces of a cube map, the elements of an array), and the params of the module that say so
//! (FORMAT.md, "The module").

use aqr_common::varint;

/// The block formats taken: blocks of a fixed footprint of pixels each (4×4 but for PVRTC's
/// 2 bits a pixel and ASTC's).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    /// BC1 (DXT1): a colour block of two RGB565 endpoints and 2-bit indices, 8 bytes.
    Bc1,
    /// BC2 (DXT2, DXT3): 4-bit alpha values, then a colour block, 16 bytes.
    Bc2,
    /// BC3 (DXT4, DXT5): an alpha block of BC4, then a colour block, 16 bytes.
    Bc3,
    /// BC4 (ATI1): a block of two 8-bit endpoints and 3-bit indices, 8 bytes.
    Bc4,
    /// BC5 (ATI2): two blocks of BC4 (red, then green), 16 bytes.
    Bc5,
    /// BC6H: RGB half floats (unsigned or signed), 16 bytes of 14 modes.
    Bc6h { signed: bool },
    /// BC7: RGBA, 16 bytes of 8 modes.
    Bc7,
    /// ETC1: RGB, 8 bytes.
    Etc1,
    /// ETC2's RGB: ETC1 and its T, H and planar modes, 8 bytes.
    Etc2,
    /// ETC2's RGBA: an alpha block of EAC, then an RGB block of ETC2, 16 bytes.
    Etc2A,
    /// ETC2's RGB with punch-through alpha, 8 bytes.
    Etc2P,
    /// EAC's R11 (8 bytes) or RG11 (two blocks of R11, red then green), unsigned or signed.
    Eac { rg: bool, signed: bool },
    /// PVRTC1 of 4 bits a pixel (4×4 pixels a block) or 2 (8×4), 8 bytes, the blocks of a
    /// surface in Morton order.
    Pvrtc { two: bool },
    /// ASTC of a 2D footprint of `w` × `h` pixels (4×4 to 12×12), 16 bytes.
    Astc { w: u8, h: u8 },
}

/// The groups of formats, each of a module of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Group {
    /// BC1 to BC5: `bcn-recons`.
    Bcn,
    /// `bc6h-recons`.
    Bc6h,
    /// `bc7-recons`.
    Bc7,
    /// ETC1, ETC2 and EAC: `etc-recons`.
    Etc,
    /// `pvrtc-recons`.
    Pvrtc,
    /// `astc-recons`.
    Astc,
}

/// What the walk needs of a format's blocks: their footprint, the fewest of them across and
/// down a surface, and their bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shape {
    pub fw: usize,
    pub fh: usize,
    pub least: usize,
    pub bytes: usize,
}

impl Shape {
    /// The pixels of a block.
    pub fn pixels(self) -> usize {
        self.fw * self.fh
    }
}

/// The 2D footprints of ASTC.
pub const ASTC_FOOTPRINTS: [(u8, u8); 14] = [
    (4, 4),
    (5, 4),
    (5, 5),
    (6, 5),
    (6, 6),
    (8, 5),
    (8, 6),
    (8, 8),
    (10, 5),
    (10, 6),
    (10, 8),
    (10, 10),
    (12, 10),
    (12, 12),
];

impl Format {
    /// The formats of BC1 to BC5, of `bcn-recons`.
    pub const ALL: [Format; 5] = [Format::Bc1, Format::Bc2, Format::Bc3, Format::Bc4, Format::Bc5];

    /// Every format but ASTC, and ASTC of 4×4, 6×5, 8×8 and 12×12.
    pub const SOME: [Format; 22] = [
        Format::Bc1,
        Format::Bc2,
        Format::Bc3,
        Format::Bc4,
        Format::Bc5,
        Format::Bc6h { signed: false },
        Format::Bc6h { signed: true },
        Format::Bc7,
        Format::Etc1,
        Format::Etc2,
        Format::Etc2A,
        Format::Etc2P,
        Format::Eac { rg: false, signed: false },
        Format::Eac { rg: false, signed: true },
        Format::Eac { rg: true, signed: false },
        Format::Eac { rg: true, signed: true },
        Format::Pvrtc { two: false },
        Format::Pvrtc { two: true },
        Format::Astc { w: 4, h: 4 },
        Format::Astc { w: 6, h: 5 },
        Format::Astc { w: 8, h: 8 },
        Format::Astc { w: 12, h: 12 },
    ];

    pub fn group(self) -> Group {
        match self {
            Format::Bc1 | Format::Bc2 | Format::Bc3 | Format::Bc4 | Format::Bc5 => Group::Bcn,
            Format::Bc6h { .. } => Group::Bc6h,
            Format::Bc7 => Group::Bc7,
            Format::Etc1 | Format::Etc2 | Format::Etc2A | Format::Etc2P | Format::Eac { .. } => Group::Etc,
            Format::Pvrtc { .. } => Group::Pvrtc,
            Format::Astc { .. } => Group::Astc,
        }
    }

    pub fn block_bytes(self) -> usize {
        match self {
            Format::Bc1 | Format::Bc4 | Format::Etc1 | Format::Etc2 | Format::Etc2P | Format::Pvrtc { .. } => 8,
            Format::Eac { rg, .. } => 8 << rg as usize,
            _ => 16,
        }
    }

    /// The pixels of a block across and down.
    pub fn footprint(self) -> (usize, usize) {
        match self {
            Format::Pvrtc { two: true } => (8, 4),
            Format::Astc { w, h } => (w as usize, h as usize),
            _ => (4, 4),
        }
    }

    /// The fewest blocks across and down of a surface: 2 of PVRTC (whose blocks are between
    /// those around), else 1.
    pub fn min_blocks(self) -> usize {
        match self {
            Format::Pvrtc { .. } => 2,
            _ => 1,
        }
    }

    pub fn shape(self) -> Shape {
        let (fw, fh) = self.footprint();
        Shape { fw, fh, least: self.min_blocks(), bytes: self.block_bytes() }
    }

    pub fn name(self) -> &'static str {
        match self {
            Format::Bc1 => "bc1",
            Format::Bc2 => "bc2",
            Format::Bc3 => "bc3",
            Format::Bc4 => "bc4",
            Format::Bc5 => "bc5",
            Format::Bc6h { signed: false } => "bc6h",
            Format::Bc6h { signed: true } => "bc6h-signed",
            Format::Bc7 => "bc7",
            Format::Etc1 => "etc1",
            Format::Etc2 => "etc2",
            Format::Etc2A => "etc2-rgba",
            Format::Etc2P => "etc2-a1",
            Format::Eac { rg: false, signed: false } => "eac-r11",
            Format::Eac { rg: false, signed: true } => "eac-r11-signed",
            Format::Eac { rg: true, signed: false } => "eac-rg11",
            Format::Eac { rg: true, signed: true } => "eac-rg11-signed",
            Format::Pvrtc { two: false } => "pvrtc-4bpp",
            Format::Pvrtc { two: true } => "pvrtc-2bpp",
            Format::Astc { .. } => "astc",
        }
    }

    fn code(self) -> u8 {
        match self {
            Format::Bc1 => 1,
            Format::Bc2 => 2,
            Format::Bc3 => 3,
            Format::Bc4 => 4,
            Format::Bc5 => 5,
            Format::Bc6h { signed } => 6 + signed as u8,
            Format::Bc7 => 8,
            Format::Etc1 => 16,
            Format::Etc2 => 17,
            Format::Etc2A => 18,
            Format::Etc2P => 19,
            Format::Eac { rg, signed } => 20 + 2 * rg as u8 + signed as u8,
            Format::Pvrtc { two } => 32 + two as u8,
            Format::Astc { .. } => 48,
        }
    }

    /// The format of code `c` (of ASTC, the footprint's bytes first in `rest`, taken).
    fn from_code(c: u8, rest: &mut &[u8]) -> Option<Format> {
        Some(match c {
            1 => Format::Bc1,
            2 => Format::Bc2,
            3 => Format::Bc3,
            4 => Format::Bc4,
            5 => Format::Bc5,
            6 | 7 => Format::Bc6h { signed: c == 7 },
            8 => Format::Bc7,
            16 => Format::Etc1,
            17 => Format::Etc2,
            18 => Format::Etc2A,
            19 => Format::Etc2P,
            20..=23 => Format::Eac { rg: c & 2 != 0, signed: c & 1 != 0 },
            32 | 33 => Format::Pvrtc { two: c == 33 },
            48 => {
                let (&[w, h], r) = rest.split_first_chunk::<2>()?;
                *rest = r;
                ASTC_FOOTPRINTS.contains(&(w, h)).then_some(Format::Astc { w, h })?
            }
            _ => return None,
        })
    }
}

/// The surfaces of a texture, in the order of their data: for each layer, its mipmaps from
/// the largest, each its rows of blocks from the top, each row from the left (PVRTC's in Morton
/// order: [`crate::pvrtc`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub format: Format,
    /// Of the largest mipmap, in pixels; at least 1.
    pub width: u32,
    pub height: u32,
    /// Mipmaps per layer, at least 1; each halves the one before (rounded down, at least 1).
    pub mips: u32,
    /// At least 1.
    pub layers: u32,
}

/// A surface's size in blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Surface {
    pub bw: usize,
    pub bh: usize,
}

impl Surface {
    pub fn blocks(self) -> usize {
        self.bw * self.bh
    }
}

/// Bounds on what a layout may be, so that its sizes fit (in u64, and in usize where they are
/// held): no side over 2^16 pixels, no more mipmaps than halvings to 1×1, at most 2^16 layers.
const MAX_SIDE: u32 = 1 << 16;
const MAX_LAYERS: u32 = 1 << 16;

impl Layout {
    /// Whether the layout is within its bounds (and of PVRTC, its sides powers of two).
    pub fn valid(&self) -> bool {
        let max_mips = 32 - self.width.max(self.height).leading_zeros();
        (1..=MAX_SIDE).contains(&self.width)
            && (1..=MAX_SIDE).contains(&self.height)
            && (1..=max_mips).contains(&self.mips)
            && (1..=MAX_LAYERS).contains(&self.layers)
            && (self.format.group() != Group::Pvrtc || self.width.is_power_of_two() && self.height.is_power_of_two())
    }

    /// The surfaces of a layer, from the largest mipmap.
    pub fn mip_surfaces(&self) -> impl Iterator<Item = Surface> + use<> {
        self.surfaces_in(self.format.shape())
    }

    /// [`Layout::mip_surfaces`] of blocks of shape `s` (the format's).
    #[inline]
    pub fn surfaces_in(&self, s: Shape) -> impl Iterator<Item = Surface> + use<> {
        let (w, h) = (self.width, self.height);
        (0..self.mips).map(move |m| {
            let (w, h) = ((w >> m).max(1) as usize, (h >> m).max(1) as usize);
            Surface { bw: w.div_ceil(s.fw).max(s.least), bh: h.div_ceil(s.fh).max(s.least) }
        })
    }

    /// Bytes of the blocks of every surface; of a valid layout, under 2^56.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> u64 {
        self.len_in(self.format.shape())
    }

    /// [`Layout::len`] of blocks of shape `s` (the format's).
    #[inline]
    pub fn len_in(&self, s: Shape) -> u64 {
        let layer: u64 = self.surfaces_in(s).map(|s| s.blocks() as u64).sum();
        layer * self.layers as u64 * s.bytes as u64
    }

    /// The module's params: the format (u8; of ASTC, then the footprint's width and height,
    /// u8 each), then the width, the height, the mipmaps and the layers (varints).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut p = vec![self.format.code()];
        if let Format::Astc { w, h } = self.format {
            p.extend([w, h]);
        }
        for v in [self.width, self.height, self.mips, self.layers] {
            varint::put(&mut p, v as u64);
        }
        p
    }

    /// The layout of params `p`, which must be exactly those of a valid layout.
    pub fn from_bytes(p: &[u8]) -> Option<Layout> {
        let (&code, mut rest) = p.split_first()?;
        let format = Format::from_code(code, &mut rest)?;
        let mut at = 0;
        let mut next = || -> Option<u32> { u32::try_from(varint::get(rest, &mut at).ok()?).ok() };
        let (width, height, mips, layers) = (next()?, next()?, next()?, next()?);
        rest = &rest[at..];
        let l = Layout { format, width, height, mips, layers };
        (rest.is_empty() && l.valid() && l.to_bytes() == p).then_some(l)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        let l = Layout { format: Format::Bc1, width: 256, height: 64, mips: 9, layers: 1 };
        assert!(l.valid());
        // 64x16, 32x8, 16x4, 8x2, 4x1, 2x1, 1x1, 1x1, 1x1 blocks
        assert_eq!(l.len(), (1024 + 256 + 64 + 16 + 4 + 2 + 1 + 1 + 1) * 8);
        assert!(!Layout { mips: 10, ..l }.valid());
        let c = Layout { format: Format::Bc3, width: 6, height: 5, mips: 1, layers: 6 };
        assert_eq!(c.len(), 4 * 16 * 6);
        // 2bpp PVRTC: 4x4, 2x2, then 2x2 blocks at least (of 8x4 pixels)
        let p = Layout { format: Format::Pvrtc { two: true }, width: 32, height: 16, mips: 4, layers: 1 };
        assert_eq!(p.len(), (16 + 4 + 4 + 4) * 8);
        assert!(!Layout { width: 24, ..p }.valid());
        // ASTC 6x5: 11x7, then 6x4 blocks
        let a = Layout { format: Format::Astc { w: 6, h: 5 }, width: 64, height: 32, mips: 2, layers: 1 };
        assert_eq!(a.len(), (77 + 24) * 16);
        for l in [l, c, p, a] {
            assert_eq!(Layout::from_bytes(&l.to_bytes()), Some(l));
        }
        for f in Format::SOME {
            let l = Layout { format: f, width: 16, height: 16, mips: 1, layers: 1 };
            assert_eq!(Layout::from_bytes(&l.to_bytes()), Some(l));
        }
        let mut p = l.to_bytes();
        p.push(0);
        assert_eq!(Layout::from_bytes(&p), None);
        assert_eq!(Layout::from_bytes(&[9, 4, 4, 1, 1]), None);
        assert_eq!(Layout::from_bytes(&[48, 7, 7, 4, 4, 1, 1]), None);
    }
}
