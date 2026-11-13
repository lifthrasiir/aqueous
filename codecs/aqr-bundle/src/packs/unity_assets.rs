//! Unity's serialized files (`*.assets`, `sharedassetsN.assets`, `levelN`, `globalgamemanagers`,
//! and the `CAB-…` nodes of AssetBundles), versions 9 to 22 of the format (Unity 3.x on).
//!
//! The header, big endian: the metadata's size, the file's size, the format's version and the
//! data's offset (u32 each), from version 9 the endianness of the rest (a byte, 1 big endian)
//! and 3 reserved bytes; from version 22 the first, second and fourth fields are 0 and follow
//! as the metadata's size (u32), the file's size and the data's offset (u64 each) and 8 reserved
//! bytes. The metadata follows (from byte 20, or 48), in the file's endianness: the engine's
//! version (a string of a NUL), the target platform (u32), from version 13 whether the types
//! have their trees (a byte), and the types: a count (u32), each its class (i32), from version
//! 16 whether it is stripped (a byte), from version 17 the index of its script (i16), from
//! version 13 a hash of its script (16 bytes: where the class is negative before version 16, or
//! is 114, MonoBehaviour, on) and of its type (16 bytes), and its tree where there are trees
//! (always before version 13): from version 12 (and in 10) a count of nodes (u32), the size of
//! the strings (u32), the nodes (24 bytes each, 32 from version 19) and the strings; else a node
//! as strings of its type and name, its fields (i32 each: size, index, flags, version, meta
//! flags) and its children recursively; from version 21 the classes it depends on (a count and
//! i32 each). From version 7 to 13 whether the objects' ids are 64 bits (u32). Then the objects:
//! a count (u32), each its id (i32, or i64 where so said; from version 14 always i64, aligned to
//! 4 bytes of the file), its offset in the data (u32, u64 from version 22) and size (u32), its
//! type (i32: the index of its type from version 16), before version 16 its class (u16), before
//! version 11 whether it is destroyed (u16), from version 11 to 16 the index of its script
//! (i16), in versions 15 and 16 whether it is stripped (a byte). The scripts, the external files
//! and the rest of the metadata follow and are not read.
//!
//! Each object is a file named by its id and an extension telling its class (`.tex2d`, `.mesh`,
//! `.mono`, … and `.c<class>` for the others). The header, the metadata and the padding between
//! objects are the rest. Versions 9, 14, 15, 17, 19, 20, 21 and 22 were seen in games; 10 to 13,
//! 16 and 18 are read by the description only.

use super::{file, u32be, u64be, within};
use aqr_disk::File;

/// The versions read.
const VERSIONS: std::ops::RangeInclusive<u32> = 9..=22;
/// The deepest tree of the older layout read.
const MAX_DEPTH: u32 = 64;

/// Reads of the metadata in the file's endianness.
struct Reader<'a> {
    d: &'a [u8],
    at: usize,
    big: bool,
}

impl Reader<'_> {
    fn bytes(&mut self, n: usize) -> Option<&[u8]> {
        let b = self.d.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(b)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.bytes(1)?[0])
    }

    fn u16(&mut self) -> Option<u16> {
        let b = self.bytes(2)?.try_into().ok()?;
        Some(if self.big { u16::from_be_bytes(b) } else { u16::from_le_bytes(b) })
    }

    fn u32(&mut self) -> Option<u32> {
        let b = self.bytes(4)?.try_into().ok()?;
        Some(if self.big { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) })
    }

    fn u64(&mut self) -> Option<u64> {
        let b = self.bytes(8)?.try_into().ok()?;
        Some(if self.big { u64::from_be_bytes(b) } else { u64::from_le_bytes(b) })
    }

    fn cstr(&mut self) -> Option<()> {
        let n = self.d.get(self.at..)?.iter().position(|&b| b == 0)?;
        self.at += n + 1;
        Some(())
    }

    /// A count of items of `min` bytes at least each, if they may fit in what is left.
    fn count(&mut self, min: usize) -> Option<usize> {
        let n = self.u32()? as usize;
        (n <= self.d.len().saturating_sub(self.at) / min.max(1)).then_some(n)
    }

    /// Skips a type tree of the older layout (before version 12, but 10).
    fn tree(&mut self, depth: u32) -> Option<()> {
        if depth > MAX_DEPTH {
            return None;
        }
        self.cstr()?;
        self.cstr()?;
        // (size, index, flags, version, meta flags; versions 9 to 11 have all)
        self.bytes(20)?;
        let children = self.count(26)?;
        for _ in 0..children {
            self.tree(depth + 1)?;
        }
        Some(())
    }
}

/// The header's version, metadata's offset and size, data's offset, file's size and endianness.
struct Header {
    version: u32,
    meta: usize,
    data: u64,
    big: bool,
}

fn header(d: &[u8]) -> Option<Header> {
    let version = u32be(d, 8)?;
    if !VERSIONS.contains(&version) {
        return None;
    }
    let big = match *d.get(16)? {
        0 => false,
        1 => true,
        _ => return None,
    };
    let (meta_size, size, data, meta) = match version {
        22.. => (u32be(d, 20)? as u64, u64be(d, 24)?, u64be(d, 32)?, 48),
        _ => (u32be(d, 0)? as u64, u32be(d, 4)? as u64, u32be(d, 12)? as u64, 20),
    };
    // (the metadata between the header and the data; the file of its size at most, as a bundle's
    // node may be followed by others)
    if meta_size == 0 || size > d.len() as u64 || data > size || (meta as u64).checked_add(meta_size)? > data {
        return None;
    }
    Some(Header { version, meta, data, big })
}

pub fn starts(d: &[u8]) -> bool {
    d.len() >= 48 && header(d).is_some()
}

/// The extension of the files of objects of class `class`.
fn extension(class: i32) -> String {
    let known = match class {
        21 => "mat",
        28 => "tex2d",
        43 => "mesh",
        48 => "shader",
        49 => "textasset",
        74 => "anim",
        83 => "audioclip",
        89 => "cubemap",
        114 => "mono",
        115 => "monoscript",
        117 => "tex3d",
        128 => "font",
        152 => "movie",
        187 => "tex2darray",
        213 => "sprite",
        329 => "videoclip",
        _ => return format!("c{class}"),
    };
    known.to_string()
}

/// The objects of serialized file `d`; None where it is not one, or its metadata is not read.
pub fn files(d: &[u8]) -> Option<Vec<File>> {
    if !starts(d) {
        return None;
    }
    let Header { version: v, meta, data, big } = header(d)?;
    let mut r = Reader { d, at: meta, big };
    r.cstr()?;
    r.u32()?;
    let trees = v < 13 || r.u8()? != 0;
    let n = r.count(4)?;
    let mut classes = Vec::with_capacity(n);
    for _ in 0..n {
        let class = r.u32()? as i32;
        if v >= 16 {
            r.u8()?;
        }
        if v >= 17 {
            r.u16()?;
        }
        if v >= 13 {
            if (v < 16 && class < 0) || (v >= 16 && class == 114) {
                r.bytes(16)?;
            }
            r.bytes(16)?;
        }
        if trees {
            if v >= 12 || v == 10 {
                let nodes = r.count(24)?;
                let strings = r.u32()? as usize;
                r.bytes(nodes.checked_mul(if v >= 19 { 32 } else { 24 })?)?;
                r.bytes(strings)?;
            } else {
                r.tree(0)?;
            }
            if v >= 21 {
                let deps = r.count(4)?;
                r.bytes(deps * 4)?;
            }
        }
        classes.push(class);
    }
    let big_ids = (7..14).contains(&v) && r.u32()? != 0;
    let n = r.count(12)?;
    let mut files = Vec::with_capacity(n);
    for _ in 0..n {
        if v >= 14 {
            r.at = r.at.next_multiple_of(4);
        }
        let id = match v >= 14 || big_ids {
            true => r.u64()? as i64,
            false => r.u32()? as i32 as i64,
        };
        let start = match v {
            22.. => r.u64()?,
            _ => r.u32()? as u64,
        };
        let size = r.u32()? as u64;
        let ty = r.u32()? as i32;
        let class = match v {
            ..16 => r.u16()? as i32,
            _ => *classes.get(usize::try_from(ty).ok()?)?,
        };
        if v < 11 {
            r.u16()?;
        }
        if (11..17).contains(&v) {
            r.u16()?;
        }
        if v == 15 || v == 16 {
            r.u8()?;
        }
        if size == 0 {
            continue;
        }
        let extent = within(d, data.checked_add(start)?, size)?;
        files.push(file(format!("{id}.{}", extension(class)).into_bytes(), extent));
    }
    Some(files)
}
