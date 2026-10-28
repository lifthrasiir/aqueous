//! The layout of a pack (FORMAT.md, §3): for each entry a tag, telling its type and what of
//! it is derived, and the bytes of it that are not; [`build`] rebuilds the pack from it, the
//! entries' DEFLATE streams (rebuilt elsewhere) and the bytes kept.

use aqr_common::varint;

use crate::pack::{self, HEADER, ID, OFS_DELTA, REF_DELTA};
use crate::{Result, corrupt};

/// The bits of a tag past the type.
pub const REBUILT: u8 = 8;
pub const HEADER_KEPT: u8 = 16;
pub const ZLIB_SAME: u8 = 32;

/// The zlib header taken as the previous one before the first: git's at its default level.
pub const ZLIB_FIRST: [u8; 2] = [0x78, 0x9c];

/// The trailer: the SHA-1 of the pack, derived.
pub const TRAILER_DERIVED: u8 = 1;

/// Bytes of what [`build`] derives of each rebuilt entry, after the streams in `deflated`
/// (aqr-flate's `Derived::Zlib`).
pub const DERIVED: usize = 20;

/// The layout of a pack, and the bytes kept, as made entry by entry.
#[derive(Default)]
pub struct Writer {
    pub layout: Vec<u8>,
    pub kept: Vec<u8>,
    tags: Vec<u8>,
    zlib: Option<[u8; 2]>,
}

impl Writer {
    /// A layout starting with the pack header `header`.
    pub fn new(header: &[u8]) -> Writer {
        Writer { kept: header.to_vec(), ..Writer::default() }
    }

    /// An entry kept as it is, all of it.
    pub fn kept(&mut self, entry: &[u8]) {
        self.tags.push(0);
        varint::put(&mut self.layout, entry.len() as u64);
        self.kept.extend_from_slice(entry);
    }

    /// An entry of type `t` whose zlib stream is rebuilt from its data of `size` bytes: its
    /// header, base and zlib header.
    pub fn rebuilt(&mut self, t: u8, size: u64, header: &[u8], base: &[u8], zlib: [u8; 2]) {
        let mut tag = t | REBUILT;
        let mut h = Vec::new();
        pack::put_header(t, size, &mut h);
        if h != header {
            tag |= HEADER_KEPT;
            self.kept.extend_from_slice(header);
        }
        match t {
            OFS_DELTA => self.layout.extend_from_slice(base),
            REF_DELTA => self.kept.extend_from_slice(base),
            _ => {}
        }
        if self.zlib.unwrap_or(ZLIB_FIRST) == zlib {
            tag |= ZLIB_SAME;
        } else {
            self.layout.extend_from_slice(&zlib);
        }
        self.zlib = Some(zlib);
        self.tags.push(tag);
    }

    /// The layout and the bytes kept: `trailer` derived or not, and `rest` after it.
    pub fn finish(self, derived: bool, rest: &[u8]) -> (Vec<u8>, Vec<u8>) {
        // (the tags first, then the bytes of each: the tags alike, the rest alike)
        let mut layout = Vec::with_capacity(self.tags.len() + self.layout.len() + 11);
        varint::put(&mut layout, self.tags.len() as u64);
        layout.extend_from_slice(&self.tags);
        layout.extend_from_slice(&self.layout);
        layout.push(if derived { TRAILER_DERIVED } else { 0 });
        let mut kept = self.kept;
        kept.extend_from_slice(rest);
        (layout, kept)
    }
}

/// Reads bytes off the front of a slice.
struct Bytes<'a>(&'a [u8]);

impl<'a> Bytes<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > self.0.len() {
            return Err(corrupt());
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }

    fn varint(&mut self) -> Result<u64> {
        let mut at = 0;
        let v = varint::get(self.0, &mut at)?;
        self.0 = &self.0[at..];
        Ok(v)
    }

    /// Bytes while bit 7 is set, then one more, within 10.
    fn continued(&mut self) -> Result<&'a [u8]> {
        let n = pack::header_len(self.0).ok_or_else(corrupt)?;
        self.take(n)
    }
}

/// The tags of the entries of `layout`.
pub fn tags(layout: &[u8]) -> Result<&[u8]> {
    let mut l = Bytes(layout);
    let n = usize::try_from(l.varint()?).map_err(|_| corrupt())?;
    l.take(n)
}

/// The pack of `len` bytes from `layout`, `deflated` (the rebuilt entries' DEFLATE streams one
/// after another, then [`DERIVED`] bytes for each: the Adler-32 of its data, big endian, the
/// length of its data and the length of the stream) and `kept`.
pub fn build(layout: &[u8], deflated: &[u8], kept: &[u8], len: u64) -> Result<Vec<u8>> {
    let mut l = Bytes(layout);
    let mut k = Bytes(kept);
    let n = usize::try_from(l.varint()?).map_err(|_| corrupt())?;
    let tags = l.take(n)?;
    let rebuilt = tags.iter().filter(|&&t| t & REBUILT != 0).count();
    let table_at = deflated.len().checked_sub(rebuilt.checked_mul(DERIVED).ok_or_else(corrupt)?).ok_or_else(corrupt)?;
    let (mut streams, mut table) = (Bytes(&deflated[..table_at]), Bytes(&deflated[table_at..]));
    let cap = usize::try_from(len).map_err(|_| corrupt())?;
    let mut out = Vec::with_capacity(cap.min(kept.len() + deflated.len() + layout.len() * 4 + ID));
    out.extend_from_slice(k.take(HEADER)?);
    let mut zlib = ZLIB_FIRST;
    for &tag in tags {
        if tag & REBUILT == 0 {
            if tag != 0 {
                return Err(corrupt());
            }
            let n = usize::try_from(l.varint()?).map_err(|_| corrupt())?;
            out.extend_from_slice(k.take(n)?);
            continue;
        }
        if tag & !(7 | REBUILT | HEADER_KEPT | ZLIB_SAME) != 0 {
            return Err(corrupt());
        }
        let t = tag & 7;
        let e = table.take(DERIVED)?;
        let adler = &e[..4];
        let size = u64::from_le_bytes(e[4..12].try_into().unwrap());
        let stream = usize::try_from(u64::from_le_bytes(e[12..].try_into().unwrap())).map_err(|_| corrupt())?;
        match tag & HEADER_KEPT {
            0 => pack::put_header(t, size, &mut out),
            _ => out.extend_from_slice(k.continued()?),
        }
        match t {
            OFS_DELTA => out.extend_from_slice(l.continued()?),
            REF_DELTA => out.extend_from_slice(k.take(ID)?),
            _ => {}
        }
        if tag & ZLIB_SAME == 0 {
            zlib = l.take(2)?.try_into().unwrap();
        }
        out.extend_from_slice(&zlib);
        out.extend_from_slice(streams.take(stream)?);
        out.extend_from_slice(adler);
    }
    match l.take(1)? {
        [TRAILER_DERIVED] => {
            let sum = pack::sha1(&out);
            out.extend_from_slice(&sum);
        }
        [0] => {}
        _ => return Err(corrupt()),
    }
    out.extend_from_slice(k.0);
    if !l.0.is_empty() || !streams.0.is_empty() || out.len() as u64 != len {
        return Err(corrupt());
    }
    Ok(out)
}
