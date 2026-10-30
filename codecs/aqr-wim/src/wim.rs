//! WIM files (Microsoft's "Windows Imaging File Format"): a header of 208 bytes, then
//! resources anywhere, each found by a resource header (its size in the file, flags, offset and
//! size of its data). The lookup table, of 50-byte entries (a resource header, a part number, a
//! reference count and the SHA-1 of the data), lists the resources of files and of images'
//! metadata; the header points at the lookup table, the XML data, the boot metadata and the
//! integrity table.
//!
//! A compressed resource is a table of where its chunks start (all but the first, from the
//! table's end; 4 bytes each, 8 where the data is over 4 GiB), then the chunks, each of
//! [`CHUNK`] bytes of data but the last, compressed on its own: a chunk of as many bytes as its
//! data is stored. The file's flags say how chunks are compressed; only LZX is taken here.

use std::ops::Range;

pub const MAGIC: &[u8; 8] = b"MSWIM\0\0\0";
pub const HEADER: usize = 208;
pub const CHUNK: usize = 32768;

/// Flags of the header: compressed, and by XPRESS, LZX or LZMS.
pub const COMPRESSED: u32 = 0x2;
pub const XPRESS: u32 = 0x2_0000;
pub const LZX: u32 = 0x4_0000;
pub const LZMS: u32 = 0x8_0000;

/// Flags of a resource: metadata (an image's), compressed.
pub const RES_METADATA: u8 = 0x2;
pub const RES_COMPRESSED: u8 = 0x4;
pub const RES_SPANNED: u8 = 0x8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reshdr {
    pub size: u64,
    pub flags: u8,
    pub offset: u64,
    pub orig: u64,
}

impl Reshdr {
    pub fn read(b: &[u8]) -> Reshdr {
        let mut s = [0u8; 8];
        s[..7].copy_from_slice(&b[..7]);
        let q = |p: usize| u64::from_le_bytes(b[p..p + 8].try_into().unwrap());
        Reshdr { size: u64::from_le_bytes(s), flags: b[7], offset: q(8), orig: q(16) }
    }

    /// Its bytes in a file of `len` bytes, if they are within it.
    pub fn span(&self, len: usize) -> Option<Range<usize>> {
        let start = usize::try_from(self.offset).ok()?;
        let end = start.checked_add(usize::try_from(self.size).ok()?)?;
        (end <= len).then_some(start..end)
    }
}

/// A resource taken apart: its chunk table and its chunks (their bytes and their data's
/// lengths), or its data as it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resource {
    Chunks { table: Range<usize>, chunks: Vec<(Range<usize>, usize)> },
    Data(Range<usize>),
}

impl Resource {
    pub fn span(&self) -> Range<usize> {
        match self {
            Resource::Chunks { table, chunks } => table.start..chunks.last().map_or(table.end, |c| c.0.end),
            Resource::Data(r) => r.clone(),
        }
    }
}

/// The WIM file at the start of `d`: its header's flags, and the resources of its lookup table
/// that are taken apart, by where they are in the file. None where it is not one of LZX or of
/// no compression, chunks of [`CHUNK`] bytes, whose lookup table is within it.
pub struct Wim {
    pub flags: u32,
    pub resources: Vec<Resource>,
}

pub fn starts(d: &[u8]) -> bool {
    d.len() >= HEADER && d.starts_with(MAGIC) && u32::from_le_bytes(d[8..12].try_into().unwrap()) as usize == HEADER
}

pub fn parse(d: &[u8]) -> Option<Wim> {
    if !starts(d) {
        return None;
    }
    let flags = u32::from_le_bytes(d[16..20].try_into().unwrap());
    let chunk = u32::from_le_bytes(d[20..24].try_into().unwrap()) as usize;
    let compressed = flags & COMPRESSED != 0;
    if compressed && (flags & (XPRESS | LZX | LZMS) != LZX || chunk != CHUNK) {
        return None;
    }
    let lookup = Reshdr::read(&d[48..72]);
    if lookup.flags & RES_COMPRESSED != 0 {
        return None;
    }
    let table = &d[lookup.span(d.len())?];
    let mut entries: Vec<Reshdr> = table
        .as_chunks::<50>()
        .0
        .iter()
        .map(|e| Reshdr::read(&e[..24]))
        .filter(|r| r.flags & RES_SPANNED == 0)
        .collect();
    entries.sort_by_key(|r| (r.offset, std::cmp::Reverse(r.size)));
    let mut resources = Vec::new();
    let mut end = HEADER;
    for r in entries {
        let Some(span) = r.span(d.len()).filter(|s| s.start >= end && !s.is_empty()) else { continue };
        let res = if r.flags & RES_COMPRESSED != 0 {
            if !compressed {
                continue;
            }
            match chunks(d, &r, span.clone()) {
                Some(res) => res,
                None => continue,
            }
        } else {
            if r.orig != r.size {
                continue;
            }
            Resource::Data(span.clone())
        };
        end = span.end;
        resources.push(res);
    }
    Some(Wim { flags, resources })
}

/// The chunks of compressed resource `r` at `span`.
fn chunks(d: &[u8], r: &Reshdr, span: Range<usize>) -> Option<Resource> {
    let orig = usize::try_from(r.orig).ok()?;
    let n = orig.div_ceil(CHUNK);
    if n == 0 {
        return None;
    }
    let es = if r.orig > u32::MAX as u64 { 8 } else { 4 };
    let table = span.start..span.start.checked_add((n - 1).checked_mul(es)?)?;
    if table.end > span.end {
        return None;
    }
    let base = table.end;
    let mut starts = vec![0usize];
    for e in d[table.clone()].chunks_exact(es) {
        let mut v = [0u8; 8];
        v[..es].copy_from_slice(e);
        starts.push(usize::try_from(u64::from_le_bytes(v)).ok()?);
    }
    starts.push(span.end - base);
    let mut chunks = Vec::with_capacity(n);
    for i in 0..n {
        let (a, b) = (starts[i], starts[i + 1]);
        let len = CHUNK.min(orig - i * CHUNK);
        // (each chunk at most as long as its data, as stored chunks are)
        if b <= a || b - a > len {
            return None;
        }
        chunks.push((base + a..base + b, len));
    }
    Some(Resource::Chunks { table, chunks })
}
