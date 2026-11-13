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

    /// A string of a NUL, its bytes.
    fn cstr(&mut self) -> Option<&[u8]> {
        let n = self.d.get(self.at..)?.iter().position(|&b| b == 0)?;
        let s = &self.d[self.at..self.at + n];
        self.at += n + 1;
        Some(s)
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
    Some(
        objects(d)?
            .objects
            .into_iter()
            .map(|o| file(format!("{}.{}", o.id, extension(o.class)).into_bytes(), o.extent))
            .collect(),
    )
}

/// An object of a serialized file: its id, its class, where its bytes are in the file.
#[derive(Debug, Clone)]
pub struct Object {
    pub id: i64,
    pub class: i32,
    pub extent: std::ops::Range<usize>,
}

/// What [`objects`] reads: the engine's version (its major and minor numbers, as `2019.4`), the
/// objects' byte order, and the objects (of bytes, in the order of the table).
#[derive(Debug, Clone)]
pub struct Objects {
    pub engine: (u32, u32),
    pub big: bool,
    pub objects: Vec<Object>,
}

/// The major and minor numbers of an engine's version (`2019.4.31f1`, `5.6.3p1`).
fn engine(v: &[u8]) -> (u32, u32) {
    let mut n = v.split(|&b| !b.is_ascii_digit()).map(|s| std::str::from_utf8(s).ok().and_then(|s| s.parse().ok()));
    (n.next().flatten().unwrap_or(0), n.next().flatten().unwrap_or(0))
}

/// The objects of serialized file `d`; None where it is not one, or its metadata is not read.
pub fn objects(d: &[u8]) -> Option<Objects> {
    if !starts(d) {
        return None;
    }
    let Header { version: v, meta, data, big } = header(d)?;
    let mut r = Reader { d, at: meta, big };
    let engine = engine(r.cstr()?);
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
    let mut objects = Vec::with_capacity(n);
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
        objects.push(Object { id, class, extent });
    }
    Some(Objects { engine, big, objects })
}

/// Where a texture's blocks are: in its object, or in a resource file (`.resS`, by its path as
/// the object names it) at an offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Data {
    Inline(std::ops::Range<usize>),
    Stream { path: Vec<u8>, offset: u64, size: u64 },
}

/// What a texture's pixels are: blocks that aqr-tex takes (BC1, BC3 to BC7, ETC1, ETC2, EAC,
/// PVRTC and ASTC), or uncompressed pixels of 8-bit samples that aqr-raw takes ([`Raw`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pixels {
    Blocks(aqr_tex::layout::Layout),
    Raw(Raw),
}

/// Uncompressed pixels of `channels` 8-bit samples (Alpha8, R8, RG16, RGB24, RGBA32, ARGB32,
/// BGRA32): of each of the `layers`, the mipmaps from the largest, each halving the one before
/// (rounded down, at least 1), its rows one after another, of no padding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Raw {
    pub channels: usize,
    pub width: u32,
    pub height: u32,
    pub mips: u32,
    pub layers: u32,
}

impl Raw {
    /// The surfaces of a layer: their widths and heights.
    pub fn surfaces(&self) -> impl Iterator<Item = (u32, u32)> + use<> {
        let (w, h) = (self.width, self.height);
        (0..self.mips).map(move |m| ((w >> m).max(1), (h >> m).max(1)))
    }

    fn valid(&self) -> bool {
        let max_mips = 32 - self.width.max(self.height).leading_zeros();
        (1..=1 << 16).contains(&self.width)
            && (1..=1 << 16).contains(&self.height)
            && (1..=max_mips).contains(&self.mips)
            && (1..=6).contains(&self.layers)
    }
}

impl Pixels {
    /// Bytes of all the pixels, of a valid one.
    pub fn len(&self) -> u64 {
        match self {
            Pixels::Blocks(l) => l.len(),
            Pixels::Raw(r) => {
                let layer: u64 = r.surfaces().map(|(w, h)| w as u64 * h as u64).sum();
                layer * r.channels as u64 * r.layers as u64
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn valid(&self) -> bool {
        match self {
            Pixels::Blocks(l) => l.valid(),
            Pixels::Raw(r) => r.valid(),
        }
    }

    fn set_layers(&mut self, n: u32) {
        match self {
            Pixels::Blocks(l) => l.layers = n,
            Pixels::Raw(r) => r.layers = n,
        }
    }
}

/// The node rebuilding `bytes`, all the pixels of `p`: aqr-tex's `bcn-recons` of blocks; of
/// uncompressed pixels, aqr-raw's `raster-recons` of each surface (one too small for it kept as
/// it is, of kind `Pixels`). None where aqr-tex turns the blocks down.
pub fn apply_pixels(
    f: &mut aqr_formula::Formula,
    bytes: &[u8],
    p: &Pixels,
    threads: usize,
) -> Option<aqr_formula::Node> {
    use aqr_formula::{Kind, Node};
    match p {
        Pixels::Blocks(l) => aqr_tex::formula::apply(f, bytes, l, threads),
        Pixels::Raw(r) => {
            let (mut parts, mut at) = (Vec::new(), 0);
            for _ in 0..r.layers {
                for (w, h) in r.surfaces() {
                    let n = (w * h) as usize * r.channels;
                    let samples = &bytes[at..at + n];
                    let fmt = aqr_raw::image::raster::Format {
                        width: w as usize,
                        height: h as usize,
                        channels: r.channels,
                        stride: w as usize * r.channels,
                    };
                    let node = aqr_raw::formula::raster::apply(f, samples, fmt)
                        .unwrap_or_else(|| f.push_as("unity/pixels", Kind::Pixels, samples.to_vec()));
                    parts.push(node);
                    at += n;
                }
            }
            Some(Node::concat(parts))
        }
    }
}

/// A `Texture2D` taken: what its pixels are, where they are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Texture {
    pub pixels: Pixels,
    pub data: Data,
}

/// The texture of object `o` (of class 28, `Texture2D`) of a serialized file of `engine`'s, in its
/// byte order `big`: where its blocks are (positions in `o`, or in a resource file) and their
/// layout; None where it is not of the formats taken, or does not read.
///
/// Its first fields (AssetStudio's description of the class, by the engine's version): the
/// name (a u32 length, the bytes, to 4 bytes), from 2017.3 the fallback format (i32) and whether
/// to downscale (a byte; from 2020.2 another, whether alpha is optional; then to 4 bytes), the
/// width, the height and the image's size (i32 each), from 2020.1 the mipmaps stripped (i32),
/// the format (i32) and from 5.2 the mipmaps (i32). Its last: the image's data (a u32 length,
/// the bytes), then the streamed data's offset (u32, u64 from 2020.1), size (u32) and path (a
/// string as the name's): read from the object's end, as the fields between vary by version. The
/// blocks' size as the layout makes it must be the image's size (times the images).
pub fn texture(o: &[u8], engine: (u32, u32), big: bool) -> Option<Texture> {
    use aqr_tex::layout::{Format, Layout};
    let at_least = |v: (u32, u32)| engine >= v;
    let u32_at = |at: usize| -> Option<u32> {
        let b: [u8; 4] = o.get(at..at.checked_add(4)?)?.try_into().ok()?;
        Some(if big { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) })
    };
    let name = u32_at(0)? as usize;
    let mut base = (4usize.checked_add(name)?).next_multiple_of(4);
    if at_least((2017, 3)) {
        base = (base + 4 + 1 + at_least((2020, 2)) as usize).next_multiple_of(4);
    }
    // the fields at `at`: the pixels, and the image's size (one image)
    let head = |at: usize| -> Option<(Pixels, u64)> {
        let (width, height, size) = (u32_at(at)?, u32_at(at + 4)?, u32_at(at + 8)? as u64);
        let at = at + 12 + 4 * at_least((2020, 1)) as usize;
        // (the blocks' format, or the samples of an uncompressed pixel)
        let format = match u32_at(at)? {
            10 => Ok(Format::Bc1),
            12 => Ok(Format::Bc3),
            24 => Ok(Format::Bc6h { signed: false }),
            25 => Ok(Format::Bc7),
            26 => Ok(Format::Bc4),
            27 => Ok(Format::Bc5),
            30 | 31 => Ok(Format::Pvrtc { two: true }),
            32 | 33 => Ok(Format::Pvrtc { two: false }),
            34 => Ok(Format::Etc1),
            41..=44 => Ok(Format::Eac { rg: u32_at(at)? >= 43, signed: u32_at(at)? % 2 == 0 }),
            45 => Ok(Format::Etc2),
            46 => Ok(Format::Etc2P),
            47 => Ok(Format::Etc2A),
            // (ASTC's of LDR, of the old RGBA formats and of HDR)
            c @ (48..=59 | 66..=71) => {
                let s = [4, 5, 6, 8, 10, 12][((c - 48) % 6) as usize];
                Ok(Format::Astc { w: s, h: s })
            }
            1 | 63 => Err(1),
            62 => Err(2),
            3 => Err(3),
            4 | 5 | 14 => Err(4),
            _ => return None,
        };
        // (before 5.2 whether there are mipmaps, a byte: then all of them)
        let mips = match at_least((5, 2)) {
            true => u32_at(at + 4)?,
            false => match *o.get(at + 4)? {
                0 => 1,
                _ => 32 - width.max(height).leading_zeros(),
            },
        };
        let pixels = match format {
            Ok(format) => Pixels::Blocks(Layout { format, width, height, mips, layers: 1 }),
            Err(channels) => Pixels::Raw(Raw { channels, width, height, mips, layers: 1 }),
        };
        (pixels.valid() && size > 0 && pixels.len() == size).then_some((pixels, size))
    };
    // (the fields as of the version; or a field before or after, as later versions have them:
    // what the image's size checks)
    let (mut pixels, size) = [0, -4, 4, 8].into_iter().find_map(|k: isize| head(base.checked_add_signed(k)?))?;
    // before 5.3 the image's data is last: its length, the bytes
    if !at_least((5, 3)) {
        for images in 1..=6u64 {
            let n = usize::try_from(size * images).ok()?;
            let Some(len_at) = o.len().checked_sub(n + 4) else { break };
            if u32_at(len_at)? as usize == n {
                pixels.set_layers(images as u32);
                return Some(Texture { pixels, data: Data::Inline(len_at + 4..o.len()) });
            }
        }
        return None;
    }
    // the streamed data at the end: the path (its length and bytes, to 4 bytes), the size and the
    // offset before them
    let wide = at_least((2020, 1));
    let tail = |path: usize| -> Option<(usize, u64, u64)> {
        let path_at = o.len().checked_sub(path.next_multiple_of(4) + 4)?;
        (u32_at(path_at)? as usize == path).then_some(())?;
        let s = u32_at(path_at.checked_sub(4)?)? as u64;
        let (offset, start) = match wide {
            true => {
                let at = path_at.checked_sub(12)?;
                (u32_at(at)? as u64 | (u32_at(at + 4)? as u64) << 32, at)
            }
            false => (u32_at(path_at.checked_sub(8)?)? as u64, path_at - 8),
        };
        Some((start, offset, s))
    };
    // streamed: a path that names a resource file, of the image's size (or of whole images)
    for path in (1..=256).rev() {
        let Some((_, offset, s)) = tail(path) else { continue };
        let p = &o[o.len() - path.next_multiple_of(4)..][..path];
        if s > 0 && s.is_multiple_of(size) && p.ends_with(b".resS") {
            pixels.set_layers(u32::try_from(s / size).ok()?);
            return pixels
                .valid()
                .then(|| Texture { pixels, data: Data::Stream { path: p.to_vec(), offset, size: s } });
        }
    }
    // inline: the image's data before an empty stream (no path, size 0), its length before it
    let (start, _, 0) = tail(0)? else { return None };
    for images in 1..=6u64 {
        let n = usize::try_from(size * images).ok()?;
        let Some(len_at) = start.checked_sub(n + 4) else { break };
        if u32_at(len_at)? as usize == n {
            pixels.set_layers(images as u32);
            return Some(Texture { pixels, data: Data::Inline(len_at + 4..len_at + 4 + n) });
        }
    }
    None
}

/// The formula of serialized file `d` where any of its objects is a texture whose blocks are in
/// it ([`texture`]): those blocks rebuilt by aqr-tex's `bcn-recons`, the rest of their objects
/// (and the headers, the table, the padding) the stream `unity-assets/rest`, stream 1 (as
/// aqr-disk's formula has it), the other objects contents of their kinds by `kind`, a stream
/// each. None where no texture's blocks are in it (the file is then aqr-disk's formula).
pub fn encode_with(d: &[u8], kind: fn(&[u8]) -> aqr_formula::Kind, threads: usize) -> Option<aqr_formula::Formula> {
    let o = objects(d)?;
    let mut objects: Vec<(Object, Option<Texture>)> = o
        .objects
        .into_iter()
        .map(|ob| {
            let t = (ob.class == 28).then(|| texture(&d[ob.extent.clone()], o.engine, o.big)).flatten();
            (ob, t.filter(|t| matches!(t.data, Data::Inline(_))))
        })
        .collect();
    if !objects.iter().any(|x| x.1.is_some()) {
        return None;
    }
    objects.sort_by_key(|x| x.0.extent.start);
    let mut p = super::Pieces::new(d, "unity-assets", kind);
    for (ob, t) in &objects {
        let r = ob.extent.clone();
        // (objects overlapping one before are the rest's)
        if r.start < p.at() {
            continue;
        }
        let pixels = t.as_ref().and_then(|t| {
            let Data::Inline(i) = &t.data else { return None };
            let i = r.start + i.start..r.start + i.end;
            Some((i.clone(), apply_pixels(&mut p.f, &d[i], &t.pixels, threads)?))
        });
        match pixels {
            Some((i, node)) => p.node(node, i),
            None => p.content(format!("{}.{}", ob.id, extension(ob.class)).as_bytes(), r),
        }
    }
    Some(p.finish())
}
/// The textures whose blocks are in resource file `name` (a `.resS`, by its name: the last part
/// of the textures' paths) of serialized file `d`: where their blocks are in it, their layouts.
pub fn streamed(d: &[u8], name: &[u8]) -> Vec<(std::ops::Range<u64>, Pixels)> {
    let base = |p: &[u8]| -> Vec<u8> {
        let p = p.rsplit(|&b| b == b'/' || b == b'\\').next().unwrap_or(p);
        p.to_ascii_lowercase()
    };
    let want = base(name);
    let Some(o) = objects(d) else { return Vec::new() };
    let mut out = Vec::new();
    for ob in o.objects.iter().filter(|ob| ob.class == 28) {
        let Some(t) = texture(&d[ob.extent.clone()], o.engine, o.big) else { continue };
        if let Data::Stream { path, offset, size } = t.data
            && base(&path) == want
        {
            out.push((offset..offset + size, t.pixels));
        }
    }
    out
}

/// The formula of resource file `d` (a `.resS`) of the textures `textures` (where their blocks are
/// in it, their layouts: [`streamed`]): their blocks rebuilt by aqr-tex's `bcn-recons`, the rest
/// of the file the stream `unity-ress/rest`, stream 1. Textures overlapping one before, or past
/// the end, are the rest's. None where none is rebuilt.
pub fn encode_resource(
    d: &[u8],
    textures: &[(std::ops::Range<u64>, Pixels)],
    threads: usize,
) -> Option<aqr_formula::Formula> {
    let mut p = super::Pieces::new(d, "unity-ress", |_| aqr_formula::Kind::Unknown);
    (resource_pieces(&mut p, 0..d.len(), textures, threads) > 0).then(|| p.finish())
}

/// The pieces of the resource file at `at` of what `p` makes (a file, or a node of a bundle)
/// of the textures `textures` (their places in it): their pixels rebuilt ([`apply_pixels`]), the
/// rest the rest's. Textures overlapping one before, or past its end, are the rest's. Returns how
/// many are rebuilt.
pub fn resource_pieces(
    p: &mut super::Pieces,
    at: std::ops::Range<usize>,
    textures: &[(std::ops::Range<u64>, Pixels)],
    threads: usize,
) -> usize {
    let mut t: Vec<_> = textures.iter().filter(|(r, _)| r.end <= at.len() as u64).collect();
    t.sort_by_key(|(r, _)| r.start);
    let mut made = 0;
    for (r, pixels) in t {
        let r = at.start + r.start as usize..at.start + r.end as usize;
        if r.start < p.at() {
            continue;
        }
        let d = p.data();
        let Some(node) = apply_pixels(&mut p.f, &d[r.clone()], pixels, threads) else { continue };
        p.node(node, r);
        made += 1;
    }
    p.rest_to(at.end);
    made
}
