//! Where the blocks of a texture are: its format, its size, its mipmaps and its layers (the
//! faces of a cube map, the elements of an array), and the params of the module that say so
//! (FORMAT.md, "The module").

use aqr_common::varint;

/// The block formats taken: 4×4 blocks of pixels each.
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
}

impl Format {
    pub const ALL: [Format; 5] = [Format::Bc1, Format::Bc2, Format::Bc3, Format::Bc4, Format::Bc5];

    pub fn block_bytes(self) -> usize {
        match self {
            Format::Bc1 | Format::Bc4 => 8,
            Format::Bc2 | Format::Bc3 | Format::Bc5 => 16,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Format::Bc1 => "bc1",
            Format::Bc2 => "bc2",
            Format::Bc3 => "bc3",
            Format::Bc4 => "bc4",
            Format::Bc5 => "bc5",
        }
    }

    fn code(self) -> u8 {
        match self {
            Format::Bc1 => 1,
            Format::Bc2 => 2,
            Format::Bc3 => 3,
            Format::Bc4 => 4,
            Format::Bc5 => 5,
        }
    }

    fn from_code(c: u8) -> Option<Format> {
        Format::ALL.into_iter().find(|f| f.code() == c)
    }
}

/// The surfaces of a texture, in the order of their data: for each layer, its mipmaps from
/// the largest, each its rows of blocks from the top, each row from the left.
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
    /// Whether the layout is within its bounds.
    pub fn valid(&self) -> bool {
        let max_mips = 32 - self.width.max(self.height).leading_zeros();
        (1..=MAX_SIDE).contains(&self.width)
            && (1..=MAX_SIDE).contains(&self.height)
            && (1..=max_mips).contains(&self.mips)
            && (1..=MAX_LAYERS).contains(&self.layers)
    }

    /// The surfaces of a layer, from the largest mipmap.
    pub fn mip_surfaces(&self) -> impl Iterator<Item = Surface> + use<> {
        let (w, h) = (self.width, self.height);
        (0..self.mips).map(move |m| {
            let (w, h) = ((w >> m).max(1) as usize, (h >> m).max(1) as usize);
            Surface { bw: w.div_ceil(4), bh: h.div_ceil(4) }
        })
    }

    /// Bytes of the blocks of every surface; of a valid layout, under 2^56.
    pub fn len(&self) -> u64 {
        let layer: u64 = self.mip_surfaces().map(|s| s.blocks() as u64).sum();
        layer * self.layers as u64 * self.format.block_bytes() as u64
    }

    /// The module's params: the format (u8), then the width, the height, the mipmaps and the
    /// layers (varints).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut p = vec![self.format.code()];
        for v in [self.width, self.height, self.mips, self.layers] {
            varint::put(&mut p, v as u64);
        }
        p
    }

    /// The layout of params `p`, which must be exactly those of a valid layout.
    pub fn from_bytes(p: &[u8]) -> Option<Layout> {
        let (&code, mut rest) = p.split_first()?;
        let format = Format::from_code(code)?;
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
        for l in [l, c] {
            assert_eq!(Layout::from_bytes(&l.to_bytes()), Some(l));
        }
        let mut p = l.to_bytes();
        p.push(0);
        assert_eq!(Layout::from_bytes(&p), None);
        assert_eq!(Layout::from_bytes(&[9, 4, 4, 1, 1]), None);
    }
}
