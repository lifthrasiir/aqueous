//! A block once inflated: a 256×256 tile of 8-bit planes, by its size.
//!
//! - colour (327680 bytes): a plane (the alpha), then 4 bytes a pixel interleaved (B, G, R and a
//!   fourth mostly 0 or 255, not the alpha);
//! - one plane (65536 bytes: masks, monochrome layers);
//! - two planes (131072 bytes).
//!
//! A pixel is all of its samples at once, packed into a `u64` (plane first, a byte each).

use std::collections::HashSet;

pub const SIDE: usize = 256;
pub const AREA: usize = SIDE * SIDE;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Layout {
    Color,
    One,
    Two,
}

impl Layout {
    pub fn of(len: usize) -> Option<Layout> {
        match len {
            327680 => Some(Layout::Color),
            65536 => Some(Layout::One),
            131072 => Some(Layout::Two),
            _ => None,
        }
    }

    /// Samples per pixel.
    pub fn samples(self) -> usize {
        match self {
            Layout::Color => 5,
            Layout::One => 1,
            Layout::Two => 2,
        }
    }

    /// Bytes of a tile.
    pub fn bytes(self) -> usize {
        AREA * self.samples()
    }

    pub fn name(self) -> &'static str {
        match self {
            Layout::Color => "color",
            Layout::One => "one",
            Layout::Two => "two",
        }
    }
}

/// The pixel `i` of tile `t`.
#[inline]
pub fn pixel(l: Layout, t: &[u8], i: usize) -> u64 {
    match l {
        Layout::Color => {
            let c = u32::from_le_bytes(*t[AREA + 4 * i..].first_chunk().unwrap());
            t[i] as u64 | (c as u64) << 8
        }
        Layout::One => t[i] as u64,
        Layout::Two => t[i] as u64 | (t[AREA + i] as u64) << 8,
    }
}

/// The pixels of tile `t`.
pub fn pixels(l: Layout, t: &[u8], out: &mut Vec<u64>) {
    out.clear();
    out.extend((0..AREA).map(|i| pixel(l, t, i)));
}

/// Tile `t` from its pixels.
pub fn unpixels(l: Layout, px: &[u64], t: &mut Vec<u8>) {
    t.clear();
    t.resize(l.bytes(), 0);
    for (i, &p) in px.iter().enumerate() {
        t[i] = p as u8;
        match l {
            Layout::Color => t[AREA + 4 * i..AREA + 4 * i + 4].copy_from_slice(&((p >> 8) as u32).to_le_bytes()),
            Layout::One => {}
            Layout::Two => t[AREA + i] = (p >> 8) as u8,
        }
    }
}

/// Tiles by how many distinct pixels they have.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Class {
    /// All bytes zero.
    Zero,
    /// One pixel all over.
    Const,
    /// Up to 16.
    Few,
    /// Up to 256.
    Palette,
    /// More.
    Full,
}

impl Class {
    pub const ALL: [Class; 5] = [Class::Zero, Class::Const, Class::Few, Class::Palette, Class::Full];

    pub fn name(self) -> &'static str {
        match self {
            Class::Zero => "zero",
            Class::Const => "const",
            Class::Few => "<=16",
            Class::Palette => "<=256",
            Class::Full => ">256",
        }
    }
}

pub fn classify(l: Layout, t: &[u8]) -> Class {
    if t.iter().all(|&b| b == 0) {
        return Class::Zero;
    }
    let mut seen = HashSet::new();
    let mut last = None;
    for i in 0..AREA {
        let p = pixel(l, t, i);
        if last != Some(p) {
            last = Some(p);
            seen.insert(p);
            if seen.len() > 256 {
                return Class::Full;
            }
        }
    }
    match seen.len() {
        1 => Class::Const,
        2..=16 => Class::Few,
        _ => Class::Palette,
    }
}
