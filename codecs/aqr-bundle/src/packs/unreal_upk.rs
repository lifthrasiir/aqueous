//! Unreal Engine 3's packages (`.upk`, `.u`, `.umap`, `.udk`, a game's own extensions), file
//! versions 415 to 1000, and its files of compressed chunks alone (`.tfc`, the texture file
//! caches; fully compressed packages). Little endian throughout.
//!
//! A package starts with its summary: the magic `C1 83 2A 9E`, the file version and the
//! licensee's (u16 each), the total size of the header (i32), the folder name (an `FString`:
//! an i32 length, then that many bytes, a NUL last; a negative length counts UTF-16 units), the
//! package flags (u32), the count and offset of the names, the exports and the imports (i32
//! each), the offset of the depends map (i32), from version 623 the offset of the imports' and
//! exports' GUIDs and their two counts (i32 each), from version 584 the offset of the thumbnail
//! table (i32), the package's GUID (16 bytes), the generations (a count, i32, and each the
//! counts of exports, names and net objects, i32 each), the engine's and the cooker's versions
//! (u32 each), the compression flags (u32: 1 zlib, 2 LZO, 4 LZX) and the compressed chunks (a
//! count, i32, and each the offset and size of its data uncompressed, then of it compressed in
//! the file, i32 each).
//!
//! A compressed chunk (`FCompressedChunkInfo`, and the whole of a `.tfc`, chunk after chunk)
//! is a header, the magic, the block size, the sizes compressed and not of its data and of
//! each block (i32 each, a block for each block size of the data), then the blocks, each
//! compressed on its own. Each chunk of a package with chunks is a file of the chunk's bytes,
//! its header with them (`chunkN`): a chunk of zlib as it is, which aqr-compress's scan finds in
//! it, one of LZO or LZX of compressed contents ([`aqr_disk::Compressed::Opaque`]). A `.tfc`
//! does not tell its method: a chunk whose blocks all start with a zlib header is taken as
//! zlib, any other as opaque.
//!
//! The tables of a package without chunks are read: the names (each an `FString` and u64
//! flags), the imports (each the class's package and name, an i32 outer, the object's name: the
//! names `FName`s, a name's index and number, i32 each) and the exports (each the class, the
//! super and the outer (i32 each: an import's -1 - index, an export's 1 + index, 0 none, of the
//! class `Class`), the object's name (an `FName`), the archetype (i32), the object flags (u64),
//! the serialized size and offset (i32 each), the export flags (u32), the net objects (a count,
//! i32, and as many i32), the package's GUID (16 bytes) and flags (u32)). That layout is the
//! samples' (no component map, which Epic's earlier versions had after the offset); it is
//! checked against what the summary tells (the depends map right after the exports, each
//! export's bytes after the header and within the file), and a table that does not read so (an
//! earlier version's, a licensee's own fields) is not read. Each
//! export's serialized bytes are a file named by its outers' and its own names, its class's for
//! extension (`Outer/Name.Texture2D`); each thumbnail of the thumbnail table (the count, then
//! each the class's name and the object's path, `FString`s, and the offset of its data: width,
//! height, the size of its image, i32 each, then the image, a PNG) is a file of its image
//! (`thumbnails/Path.png`).
//!
//! The header but the files (the summary, the tables, the chunks' headers), the depends map and
//! what no export points at are the rest. Verified on samples of versions 865, 868 and 893 (UDK,
//! Hat in Time's licensee version 5); not read: UE2's packages (compact indices) and those
//! before version 415, UE4's (a negative legacy version first).

use super::{file, u32le, within};
use aqr_disk::{Compressed, File};

const MAGIC: [u8; 4] = [0xC1, 0x83, 0x2A, 0x9E];
const VERSIONS: std::ops::RangeInclusive<u32> = 415..=1000;
const ZLIB: u32 = 1;
/// The compression flags of LZO1X chunks.
const LZO: u32 = 2;
/// The blocks a chunk may have, at most (8 GiB of data in blocks of 128 KiB; the samples' biggest
/// chunk has 3774).
const BLOCKS: usize = 1 << 16;

/// Whether `u32le(d, 4)` is a block size (a chunk first): a power of two from 32 KiB to 16 MiB,
/// which is no file version (0 in its low 16 bits, or 0x8000).
fn block_size(v: u32) -> bool {
    v.is_power_of_two() && (1 << 15..=1 << 24).contains(&v)
}

pub fn starts(d: &[u8]) -> bool {
    d.starts_with(&MAGIC) && u32le(d, 4).is_some_and(|v| block_size(v) || VERSIONS.contains(&(v & 0xffff)))
}

/// A reader of a table.
struct R<'a> {
    d: &'a [u8],
    at: usize,
}

impl R<'_> {
    fn u32(&mut self) -> Option<u32> {
        let v = u32le(self.d, self.at)?;
        self.at += 4;
        Some(v)
    }

    fn i32(&mut self) -> Option<i32> {
        self.u32().map(|v| v as i32)
    }

    fn skip(&mut self, n: usize) -> Option<()> {
        self.at = self.at.checked_add(n).filter(|&e| e <= self.d.len())?;
        Some(())
    }

    /// A count of items of `size` bytes at least each, if they may fit in what is left.
    fn count(&mut self, size: usize) -> Option<usize> {
        let n = usize::try_from(self.i32()?).ok()?;
        (n <= (self.d.len() - self.at) / size).then_some(n)
    }

    /// `n`, if as many items of `size` bytes at least may fit in what is left.
    fn count_of(&self, n: usize, size: usize) -> Option<usize> {
        (self.at <= self.d.len() && n <= (self.d.len() - self.at) / size).then_some(n)
    }

    /// An `FString`, as UTF-8 (lossy, of UTF-16 ones), up to its NUL.
    fn string(&mut self) -> Option<Vec<u8>> {
        let n = self.i32()?;
        let s = match n {
            0.. => {
                let b = self.d.get(self.at..self.at.checked_add(n as usize)?)?;
                self.at += n as usize;
                b.to_vec()
            }
            _ => {
                let n = (n as i64).unsigned_abs() as usize;
                let b = self.d.get(self.at..self.at.checked_add(n.checked_mul(2)?)?)?;
                self.at += 2 * n;
                let units = b.as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c));
                char::decode_utf16(units).map(|c| c.unwrap_or('\u{fffd}')).collect::<String>().into_bytes()
            }
        };
        Some(s.split(|&b| b == 0).next().unwrap_or(&[]).to_vec())
    }
}

/// The compressed chunk at `at` of `d`: its extent (its header with its blocks), the size of
/// its data, whether its blocks all start with a zlib header.
fn chunk(d: &[u8], at: usize) -> Option<(std::ops::Range<usize>, u64, bool)> {
    if d.get(at..at.checked_add(4)?)? != MAGIC {
        return None;
    }
    let mut r = R { d, at: at + 4 };
    let bs = r.u32()? as u64;
    let (size, raw) = (r.u32()? as u64, r.u32()? as u64);
    if bs == 0 || raw > bs * BLOCKS as u64 {
        return None;
    }
    let n = raw.div_ceil(bs) as usize;
    let start = r.at.checked_add(n.checked_mul(8)?)?;
    let (mut sum, mut sum_raw, mut zlib, mut data) = (0u64, 0u64, true, start as u64);
    for _ in 0..n {
        let (s, u) = (r.u32()? as u64, r.u32()? as u64);
        let head = within(d, data, 2.min(s));
        zlib &= head.is_some_and(|h| {
            let h = &d[h];
            h.len() == 2 && h[0] & 0x0f == 8 && h[0] >> 4 <= 7 && u16::from_be_bytes([h[0], h[1]]).is_multiple_of(31)
        });
        (sum, sum_raw, data) = (sum + s, sum_raw + u, data + s);
    }
    if sum != size || sum_raw != raw {
        return None;
    }
    let extent = within(d, at as u64, (start - at) as u64 + size)?;
    Some((extent, raw, zlib && n > 0))
}

/// A file of the chunk at `at`, named `chunkK`, of zlib or else of compressed contents.
fn chunk_file(d: &[u8], at: usize, k: usize, zlib: Option<bool>) -> Option<File> {
    let (extent, raw, found) = chunk(d, at)?;
    let zlib = zlib.unwrap_or(found);
    let mut f = file(format!("chunk{k}{}", if zlib { ".zlib" } else { "" }).into_bytes(), extent);
    f.compressed = (!zlib).then_some(Compressed::Opaque { len: raw });
    Some(f)
}

/// The blocks of `d`, a file of compressed chunks alone (a `.tfc`, or a chunk of a package as a
/// content of its own), one after another from its start: each block's place, bytes and bytes of
/// data, as LZO1X streams (aqr-lzo's `encode_blocks`). None where it is no such file, or most of
/// its chunks are of zlib (found by aqr-compress's scan): a file's chunks are of one method, and
/// an LZO1X block may start as a zlib header does (one chunk of Goat Simulator's 48957).
pub fn lzo_blocks(d: &[u8]) -> Option<Vec<(usize, usize, usize)>> {
    if !(d.starts_with(&MAGIC) && u32le(d, 4).is_some_and(block_size)) {
        return None;
    }
    let (mut blocks, mut at, mut chunks, mut zlibs) = (Vec::new(), 0, 0usize, 0usize);
    while let Some((extent, _, zlib)) = chunk(d, at) {
        (chunks, zlibs) = (chunks + 1, zlibs + zlib as usize);
        // (the header: magic, block size, sizes; then a pair of sizes a block, then the blocks)
        let n = (u32le(d, at + 12)? as usize).div_ceil(u32le(d, at + 4)? as usize);
        let mut data = at + 16 + 8 * n;
        for k in 0..n {
            let (s, u) = (u32le(d, at + 16 + 8 * k)? as usize, u32le(d, at + 20 + 8 * k)? as usize);
            blocks.push((data, s, u));
            data += s;
        }
        at = extent.end;
    }
    (!blocks.is_empty() && zlibs * 2 <= chunks).then_some(blocks)
}

/// The chunks of a file of chunks alone (a `.tfc`), one after another from its start.
fn chunks(d: &[u8]) -> Vec<File> {
    let (mut files, mut at) = (Vec::new(), 0);
    while let Some(f) = chunk_file(d, at, files.len(), None) {
        at = f.extents[0].end;
        files.push(f);
    }
    files
}

/// An `FName` of `names`: its name, and `_N` where its number is N + 1.
fn name(names: &[Vec<u8>], index: i32, number: i32) -> Option<Vec<u8>> {
    let mut s = names.get(usize::try_from(index).ok()?)?.clone();
    if number > 0 {
        s.extend_from_slice(format!("_{}", number - 1).as_bytes());
    }
    Some(s)
}

/// An export as far as its file goes.
struct Export {
    class: i32,
    outer: i32,
    name: Vec<u8>,
    size: u32,
    offset: u32,
}

/// The files of package `d`; None where it is not one, or its tables are not read.
pub fn files(d: &[u8]) -> Option<Vec<File>> {
    if !starts(d) {
        return None;
    }
    let v = u32le(d, 4)?;
    if block_size(v) {
        let files = chunks(d);
        return (!files.is_empty()).then_some(files);
    }
    let version = v & 0xffff;
    let mut r = R { d, at: 8 };
    let header = r.u32()? as usize;
    r.string()?;
    r.skip(4)?;
    let (names, names_at) = (r.u32()? as usize, r.u32()? as usize);
    let (exports, exports_at) = (r.u32()? as usize, r.u32()? as usize);
    let (imports, imports_at) = (r.u32()? as usize, r.u32()? as usize);
    let depends = r.u32()? as usize;
    if version >= 623 {
        r.skip(12)?;
    }
    let thumbnails = if version >= 584 { r.u32()? as usize } else { 0 };
    r.skip(16)?;
    let generations = r.count(12)?;
    r.skip(generations * 12 + 8)?;
    let method = r.u32()?;
    let n = r.count(16)?;
    if n > 0 {
        let mut files = Vec::with_capacity(n);
        for k in 0..n {
            let at = u32le(d, r.at + 16 * k + 8)? as usize;
            // (an LZO chunk a file of its own, which aqr-compress rebuilds by aqr-lzo, as a file of
            // chunks alone: [`lzo_blocks`]; LZX's compressed contents)
            let mut f = chunk_file(d, at, k, Some(method == ZLIB));
            if method == LZO
                && let Some(f) = &mut f
            {
                f.compressed = None;
            }
            files.extend(f);
        }
        return Some(files);
    }
    if header > d.len() {
        return None;
    }

    let mut r = R { d, at: names_at };
    // (a name takes 12 bytes at least, an import 28, an export 68)
    let names = (0..r.count_of(names, 12)?).map(|_| r.string().and_then(|s| r.skip(8).map(|_| s)));
    let names = names.collect::<Option<Vec<_>>>()?;
    let mut r = R { d, at: imports_at };
    let mut classes = Vec::with_capacity(r.count_of(imports, 28)?);
    for _ in 0..imports {
        r.skip(20)?;
        let (index, number) = (r.i32()?, r.i32()?);
        classes.push(name(&names, index, number)?);
    }
    let mut r = R { d, at: exports_at };
    let mut table = Vec::with_capacity(r.count_of(exports, 68)?);
    for _ in 0..exports {
        let class = r.i32()?;
        r.skip(4)?;
        let outer = r.i32()?;
        let (index, number) = (r.i32()?, r.i32()?);
        r.skip(4 + 8)?;
        let (size, offset) = (r.u32()?, r.u32()?);
        r.skip(4)?;
        let net = r.count(4)?;
        r.skip(net * 4 + 20)?;
        table.push(Export { class, outer, name: name(&names, index, number)?, size, offset });
    }
    // (the layout holds where the depends map follows the exports, and every export's bytes are
    // after the header, within the file)
    if r.at != depends {
        return None;
    }
    if table.iter().any(|e| {
        e.size > 0 && (u64::from(e.offset) < header as u64 || within(d, e.offset.into(), e.size.into()).is_none())
    }) {
        return None;
    }
    let mut files = Vec::with_capacity(table.len());
    for e in &table {
        if e.size == 0 {
            continue;
        }
        let mut path = e.name.clone();
        let mut outer = e.outer;
        // (outers deeper than 64 are no real package's; a loop of them neither)
        for _ in 0..64 {
            let Some(o) = usize::try_from(outer).ok().and_then(|o| table.get(o.checked_sub(1)?)) else { break };
            path = [&o.name[..], b"/", &path].concat();
            outer = o.outer;
        }
        let class: &[u8] = match e.class {
            0 => b"Class",
            c @ 1.. => table.get(c as usize - 1).map_or(b"", |c| &c.name),
            c => classes.get((-1 - c as i64) as usize).map_or(b"", |c| c),
        };
        path.push(b'.');
        path.extend_from_slice(class);
        files.push(file(path, within(d, e.offset.into(), e.size.into())?));
    }
    if thumbnails > 0 {
        files.extend(thumbnail_files(d, thumbnails).unwrap_or_default());
    }
    Some(files)
}

/// The images of the thumbnail table at `at` of `d`.
fn thumbnail_files(d: &[u8], at: usize) -> Option<Vec<File>> {
    let mut r = R { d, at };
    let n = r.count(12)?;
    let mut files = Vec::with_capacity(n);
    for _ in 0..n {
        r.string()?;
        let path = r.string()?;
        let at = r.u32()? as u64;
        let size = u32le(d, usize::try_from(at.checked_add(8)?).ok()?)?;
        let Some(extent) = within(d, at + 12, size.into()) else { continue };
        if size > 0 {
            let ext: &[u8] = if d[extent.clone()].starts_with(b"\x89PNG") { b".png" } else { b"" };
            files.push(file([b"thumbnails/", &path[..], ext].concat(), extent));
        }
    }
    Some(files)
}
