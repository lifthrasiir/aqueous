//! The GIF container (GIF89a, the specification of CompuServe, 1990; `FORMAT.md` §2): the
//! blocks of a file, as far as they hold, and where each image's LZW data is.

use std::ops::Range;

/// An image of a file: its descriptor and where its data is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// Where its descriptor (`0x2C`) is.
    pub at: usize,
    pub left: u16,
    pub top: u16,
    pub width: u16,
    pub height: u16,
    pub interlaced: bool,
    /// The LZW minimum code size.
    pub min: u8,
    /// Its data: the chain of sub-blocks, the terminator included, or up to the end of the file
    /// where it is cut.
    pub data: Range<usize>,
}

/// A file as far as its blocks hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gif {
    pub width: u16,
    pub height: u16,
    pub images: Vec<Image>,
    /// Bytes of whole blocks from the start: up to and with the trailer (`0x3B`), else up to
    /// where a block is not whole or not one.
    pub end: usize,
    /// Whether the blocks end with the trailer.
    pub trailer: bool,
}

pub fn le16(d: &[u8], p: usize) -> u16 {
    u16::from_le_bytes([d[p], d[p + 1]])
}

/// Bytes of a color table of the flags `packed` (its size in the low 3 bits, if bit 7 is set).
pub fn table_len(packed: u8) -> usize {
    if packed & 0x80 != 0 { 3 << ((packed & 7) + 1) } else { 0 }
}

/// Whether `d` starts with a GIF header and logical screen descriptor.
pub fn header(d: &[u8]) -> bool {
    d.len() >= 13 && (d.starts_with(b"GIF87a") || d.starts_with(b"GIF89a"))
}

/// The end of the chain of sub-blocks starting at `p` (after its terminator), if whole.
pub fn chain(d: &[u8], mut p: usize) -> Option<usize> {
    loop {
        let n = *d.get(p)? as usize;
        p += 1 + n;
        if n == 0 {
            return Some(p);
        }
        if p > d.len() {
            return None;
        }
    }
}

/// The blocks of `d`, a file starting with a GIF header; None where it does not.
pub fn parse(d: &[u8]) -> Option<Gif> {
    if !header(d) {
        return None;
    }
    let mut g = Gif { width: le16(d, 6), height: le16(d, 8), images: vec![], end: 0, trailer: false };
    let mut p = 13 + table_len(d[10]);
    if p > d.len() {
        return None;
    }
    loop {
        g.end = p;
        match d.get(p) {
            Some(0x3B) => {
                g.end = p + 1;
                g.trailer = true;
                break;
            }
            Some(0x21) => match d.get(p + 1).and_then(|_| chain(d, p + 2)) {
                Some(e) => p = e,
                None => break,
            },
            Some(0x2C) => {
                let Some(desc) = d.get(p..p + 10) else { break };
                let start = p + 10 + table_len(desc[9]) + 1;
                if start > d.len() {
                    break;
                }
                // (a chain cut by the end of the file ends the blocks)
                let (e, cut) = chain(d, start).map_or((d.len(), true), |e| (e, false));
                g.images.push(Image {
                    at: p,
                    left: le16(desc, 1),
                    top: le16(desc, 3),
                    width: le16(desc, 5),
                    height: le16(desc, 7),
                    interlaced: desc[9] & 0x40 != 0,
                    min: d[start - 1],
                    data: start..e,
                });
                p = e;
                if cut {
                    g.end = e;
                    break;
                }
            }
            _ => break,
        }
    }
    Some(g)
}

/// A chain of sub-blocks taken apart.
pub struct Unchained {
    /// The bytes of the sub-blocks, concatenated.
    pub bytes: Vec<u8>,
    /// The sizes of the whole sub-blocks.
    pub sizes: Vec<u8>,
    /// Where the chain is cut (has no terminator): the size of the sub-block the end cuts, of
    /// which the last bytes of `bytes` are; 0 where it is cut after a whole one.
    pub cut: Option<u8>,
}

/// The chain `d` (its terminator included, or cut by its end) taken apart.
pub fn unchain(d: &[u8]) -> Unchained {
    let mut u = Unchained { bytes: Vec::with_capacity(d.len()), sizes: vec![], cut: Some(0) };
    let mut p = 0;
    while p < d.len() {
        let n = d[p] as usize;
        if n == 0 {
            u.cut = None;
            break;
        }
        if p + 1 + n > d.len() {
            u.bytes.extend_from_slice(&d[p + 1..]);
            u.cut = Some(n as u8);
            break;
        }
        u.bytes.extend_from_slice(&d[p + 1..p + 1 + n]);
        u.sizes.push(n as u8);
        p += 1 + n;
    }
    u
}

/// The rows of an interlaced image of `h` rows in the order they are stored: every 8th from 0,
/// every 8th from 4, every 4th from 2, every 2nd from 1.
pub fn interlaced_rows(h: usize) -> impl Iterator<Item = usize> {
    (0..h).step_by(8).chain((4..h).step_by(8)).chain((2..h).step_by(4)).chain((1..h).step_by(2))
}
