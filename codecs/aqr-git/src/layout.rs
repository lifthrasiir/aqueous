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

/// Where `build_into` puts the pack.
pub trait Out {
    fn put(&mut self, b: &[u8]) -> Result<()>;
    /// `n` bytes of a stream that is not given (see [`check`]).
    fn skip(&mut self, n: usize) -> Result<()>;
    /// The pack so far.
    fn so_far(&self) -> &[u8];
}

impl Out for Vec<u8> {
    fn put(&mut self, b: &[u8]) -> Result<()> {
        self.extend_from_slice(b);
        Ok(())
    }

    fn skip(&mut self, _: usize) -> Result<()> {
        Err(corrupt())
    }

    fn so_far(&self) -> &[u8] {
        self
    }
}

/// The pack compared with one as it is built.
struct Check<'a> {
    want: &'a [u8],
    at: usize,
}

impl Out for Check<'_> {
    fn put(&mut self, b: &[u8]) -> Result<()> {
        match self.want.get(self.at..self.at + b.len()) {
            Some(w) if w == b => {
                self.at += b.len();
                Ok(())
            }
            _ => Err(corrupt()),
        }
    }

    fn skip(&mut self, n: usize) -> Result<()> {
        self.want.get(self.at..self.at + n).ok_or_else(corrupt)?;
        self.at += n;
        Ok(())
    }

    fn so_far(&self) -> &[u8] {
        &self.want[..self.at]
    }
}

/// The pack of `len` bytes from `layout`, `deflated` (the rebuilt entries' DEFLATE streams one
/// after another, then [`DERIVED`] bytes for each: the Adler-32 of its data, big endian, the
/// length of its data and the length of the stream) and `kept`.
pub fn build(layout: &[u8], deflated: &[u8], kept: &[u8], len: u64) -> Result<Vec<u8>> {
    build_with(layout, deflated, kept, len, 0, None)
}

/// [`build`] into a vector of `more` bytes of room past the pack, and where `starts` is given,
/// the offset of each entry pushed to it, then that of the end of the entries.
pub fn build_with(
    layout: &[u8],
    deflated: &[u8],
    kept: &[u8],
    len: u64,
    more: usize,
    starts: Option<&mut Vec<usize>>,
) -> Result<Vec<u8>> {
    let rebuilt = tags(layout)?.iter().filter(|&&t| t & REBUILT != 0).count();
    let table_at = deflated.len().checked_sub(rebuilt.checked_mul(DERIVED).ok_or_else(corrupt)?).ok_or_else(corrupt)?;
    let cap = usize::try_from(len).map_err(|_| corrupt())?;
    let mut out = Vec::with_capacity(cap.min(kept.len() + deflated.len() + layout.len() * 4 + ID).saturating_add(more));
    build_into(layout, Some(&deflated[..table_at]), &deflated[table_at..], kept, len, &mut out, starts)?;
    Ok(out)
}

/// Whether `layout`, `table` (what [`build`] takes after the streams) and `kept` give `pack`
/// back with the streams taken as they are in it: all but aqr-flate's part, without a copy.
pub fn check(layout: &[u8], table: &[u8], kept: &[u8], pack: &[u8]) -> bool {
    build_into(layout, None, table, kept, pack.len() as u64, &mut Check { want: pack, at: 0 }, None).is_ok()
}

/// [`build`] into `out`, of `streams` (or, where not given, of what `out` has there) and
/// `table` (their [`DERIVED`] bytes each, all of it); the entries' offsets pushed to `starts`
/// as [`build_with`] does.
fn build_into(
    layout: &[u8],
    streams: Option<&[u8]>,
    table: &[u8],
    kept: &[u8],
    len: u64,
    out: &mut impl Out,
    mut starts: Option<&mut Vec<usize>>,
) -> Result<()> {
    let mut l = Bytes(layout);
    let mut k = Bytes(kept);
    let n = usize::try_from(l.varint()?).map_err(|_| corrupt())?;
    let tags = l.take(n)?;
    let (mut streams, mut table) = (streams.map(Bytes), Bytes(table));
    let mut header = Vec::with_capacity(10);
    out.put(k.take(HEADER)?)?;
    let mut zlib = ZLIB_FIRST;
    if let Some(s) = &mut starts {
        s.reserve(n + 1);
    }
    for &tag in tags {
        if let Some(s) = &mut starts {
            s.push(out.so_far().len());
        }
        if tag & REBUILT == 0 {
            if tag != 0 {
                return Err(corrupt());
            }
            let n = usize::try_from(l.varint()?).map_err(|_| corrupt())?;
            out.put(k.take(n)?)?;
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
            0 => {
                header.clear();
                pack::put_header(t, size, &mut header);
                out.put(&header)?;
            }
            _ => out.put(k.continued()?)?,
        }
        match t {
            OFS_DELTA => out.put(l.continued()?)?,
            REF_DELTA => out.put(k.take(ID)?)?,
            _ => {}
        }
        if tag & ZLIB_SAME == 0 {
            zlib = l.take(2)?.try_into().unwrap();
        }
        out.put(&zlib)?;
        match &mut streams {
            Some(s) => out.put(s.take(stream)?)?,
            None => out.skip(stream)?,
        }
        out.put(adler)?;
    }
    if let Some(s) = &mut starts {
        s.push(out.so_far().len());
    }
    match l.take(1)? {
        [TRAILER_DERIVED] => {
            let sum = pack::sha1(out.so_far());
            out.put(&sum)?;
        }
        [0] => {}
        _ => return Err(corrupt()),
    }
    out.put(k.0)?;
    let streams_left = streams.is_some_and(|s| !s.0.is_empty());
    if !l.0.is_empty() || !table.0.is_empty() || streams_left || out.so_far().len() as u64 != len {
        return Err(corrupt());
    }
    Ok(())
}
