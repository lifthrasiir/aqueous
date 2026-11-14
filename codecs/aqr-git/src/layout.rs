//! The layout of a pack (FORMAT.md, §3): for each entry a tag, telling its type and what of
//! it is derived, and the bytes of it that are not; [`build_stream`] rebuilds the pack from it,
//! the entries' DEFLATE streams (rebuilt elsewhere) and the bytes kept, all taken in order in
//! pieces and the pack put in order: nothing is held but a piece of each.

use aqr_api::stream::{Pull, Put, Reader, Slice};
use aqr_common::crc32::Hasher;
use aqr_common::varint;
use sha1::{Digest, Sha1};

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

/// Bytes of what [`build_stream`] derives of each rebuilt entry, which the table gives apart from
/// the streams (aqr-flate's `Derived::Zlib`).
pub const DERIVED: usize = 20;

/// Bytes of the pieces in which [`build_stream`] takes the streams and the bytes kept, and of
/// those of the table and the layout.
pub const PIECE: usize = 1 << 16;
pub const SMALL_PIECE: usize = 1 << 12;

/// The layout of a pack, and the bytes kept, as made entry by entry.
#[derive(Default)]
pub struct Writer {
    /// Each entry's tag, then what it tells.
    layout: Vec<u8>,
    pub kept: Vec<u8>,
    count: usize,
    zlib: Option<[u8; 2]>,
}

impl Writer {
    /// A layout starting with the pack header `header`.
    pub fn new(header: &[u8]) -> Writer {
        Writer { kept: header.to_vec(), ..Writer::default() }
    }

    /// An entry kept as it is, all of it.
    pub fn kept(&mut self, entry: &[u8]) {
        self.count += 1;
        self.layout.push(0);
        varint::put(&mut self.layout, entry.len() as u64);
        self.kept.extend_from_slice(entry);
    }

    /// An entry of type `t` whose zlib stream is rebuilt from its data of `size` bytes: its
    /// header, base and zlib header.
    pub fn rebuilt(&mut self, t: u8, size: u64, header: &[u8], base: &[u8], zlib: [u8; 2]) {
        self.count += 1;
        let mut tag = t | REBUILT;
        let (h, n) = pack::header(t, size);
        if h[..n] != *header {
            tag |= HEADER_KEPT;
            self.kept.extend_from_slice(header);
        }
        let same = self.zlib.unwrap_or(ZLIB_FIRST) == zlib;
        if same {
            tag |= ZLIB_SAME;
        }
        self.layout.push(tag);
        match t {
            OFS_DELTA => self.layout.extend_from_slice(base),
            REF_DELTA => self.kept.extend_from_slice(base),
            _ => {}
        }
        if !same {
            self.layout.extend_from_slice(&zlib);
        }
        self.zlib = Some(zlib);
    }

    /// The layout and the bytes kept: `trailer` derived or not, and `rest` after it.
    pub fn finish(self, derived: bool, rest: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let mut layout = Vec::with_capacity(self.layout.len() + 11);
        varint::put(&mut layout, self.count as u64);
        layout.push(if derived { TRAILER_DERIVED } else { 0 });
        layout.extend_from_slice(&self.layout);
        let mut kept = self.kept;
        kept.extend_from_slice(rest);
        (layout, kept)
    }
}

/// The tags of the entries of `layout`.
pub fn tags(layout: &[u8]) -> Result<Vec<u8>> {
    let mut at = 0;
    let n = varint::get(layout, &mut at)?;
    at += 1;
    let mut tags = Vec::new();
    for _ in 0..n {
        let tag = *layout.get(at).ok_or_else(corrupt)?;
        at += 1;
        if tag == 0 {
            varint::get(layout, &mut at)?;
        } else {
            if tag & 7 == OFS_DELTA {
                at += pack::header_len(layout.get(at..).ok_or_else(corrupt)?).ok_or_else(corrupt)?;
            }
            if tag & ZLIB_SAME == 0 {
                at += 2;
            }
        }
        tags.push(tag);
    }
    Ok(tags)
}

/// Whether `layout` derives the pack's trailer.
pub fn trailer_derived(layout: &[u8]) -> bool {
    let mut at = 0;
    varint::get(layout, &mut at).is_ok() && layout.get(at) == Some(&TRAILER_DERIVED)
}

/// What the pack's streams are taken from: [`Reader`] of the input, or, for [`check`], the pack
/// itself.
pub trait Streams {
    /// Hands `f` the next `n` bytes (`at` is where the pack stands) in pieces.
    fn take(&mut self, at: u64, n: usize, f: &mut dyn FnMut(&[u8]) -> Result<()>) -> Result<()>;
    /// Whether every byte was taken.
    fn done(&mut self) -> bool;
}

/// `n` bytes of `r` handed to `f`.
fn each(r: &mut Reader, n: u64, f: &mut dyn FnMut(&[u8]) -> Result<()>) -> Result<()> {
    let mut res = Ok(());
    let ok = r.each(n, |b| match f(b) {
        Ok(()) => Some(()),
        Err(e) => {
            res = Err(e);
            None
        }
    });
    res?;
    ok.ok_or_else(corrupt)
}

impl Streams for Reader<'_> {
    fn take(&mut self, _: u64, n: usize, f: &mut dyn FnMut(&[u8]) -> Result<()>) -> Result<()> {
        each(self, n as u64, f)
    }

    fn done(&mut self) -> bool {
        Reader::done(self)
    }
}

/// The pack itself as the streams, for [`check`].
struct Own<'a>(&'a [u8]);

impl Streams for Own<'_> {
    fn take(&mut self, at: u64, n: usize, f: &mut dyn FnMut(&[u8]) -> Result<()>) -> Result<()> {
        let at = usize::try_from(at).map_err(|_| corrupt())?;
        f(self.0.get(at..at.checked_add(n).ok_or_else(corrupt)?).ok_or_else(corrupt)?)
    }

    fn done(&mut self) -> bool {
        true
    }
}

/// Where each entry of a pack starts (and the end of the last), and the CRC-32 of each.
#[derive(Default)]
pub struct Entries {
    pub starts: Vec<u64>,
    pub crcs: Vec<u32>,
}

/// The pack as it is put: counting, hashing and noting what the indexes want of it.
struct Tee<'a> {
    out: &'a mut dyn Put,
    at: u64,
    /// The SHA-1 of the pack so far, where its trailer is derived.
    sha: Option<Sha1>,
    entries: Option<&'a mut Entries>,
    crc: Hasher,
    /// Whether an entry is open (its CRC-32 running), and the entries have ended.
    open: bool,
    ended: bool,
    /// The bytes after the last entry that the trailer takes.
    trailer: [u8; ID],
    got: usize,
}

impl Tee<'_> {
    fn put(&mut self, b: &[u8]) -> Result<()> {
        self.out.put(b).ok_or_else(corrupt)?;
        self.at += b.len() as u64;
        if let Some(s) = &mut self.sha {
            s.update(b);
        }
        if self.entries.is_some() {
            if self.ended {
                let k = (ID - self.got).min(b.len());
                self.trailer[self.got..self.got + k].copy_from_slice(&b[..k]);
                self.got += k;
            } else if self.open {
                self.crc.update(b);
            }
        }
        Ok(())
    }

    /// An entry starts.
    fn begin(&mut self) {
        self.close();
        if let Some(e) = &mut self.entries {
            e.starts.push(self.at);
            self.open = true;
        }
    }

    fn close(&mut self) {
        if let (true, Some(e)) = (self.open, &mut self.entries) {
            e.crcs.push(std::mem::take(&mut self.crc).finalize());
            self.open = false;
        }
    }

    /// The entries have ended.
    fn end(&mut self) {
        self.close();
        if let Some(e) = &mut self.entries {
            e.starts.push(self.at);
        }
        self.ended = true;
    }
}

/// The pack compared with one as it is put.
struct CheckPut<'a> {
    want: &'a [u8],
    at: usize,
}

impl Put for CheckPut<'_> {
    fn put(&mut self, b: &[u8]) -> Option<()> {
        match self.want.get(self.at..self.at + b.len()) {
            Some(w) if w == b => {
                self.at += b.len();
                Some(())
            }
            _ => None,
        }
    }
}

/// The inputs of [`build_into`], all in order.
struct Inputs<'a, 'b> {
    streams: &'b mut dyn Streams,
    table: Reader<'a>,
    layout: Reader<'a>,
    kept: Reader<'a>,
    layout_len: u64,
    kept_len: u64,
    /// The most entries the layout may tell (a bound on what is reserved for them).
    max_entries: u64,
}

/// A continued field off `r` into the front of `buf`; its length.
fn continued(r: &mut Reader, buf: &mut [u8; 10]) -> Result<usize> {
    for (i, slot) in buf.iter_mut().enumerate() {
        let b = r.byte().ok_or_else(corrupt)?;
        *slot = b;
        if b & 0x80 == 0 {
            return Ok(i + 1);
        }
    }
    Err(corrupt())
}

/// The pack of `len` bytes from the inputs, put to `tee`: see [`build_stream`].
fn build_into(inp: &mut Inputs, tee: &mut Tee, len: u64) -> Result<()> {
    let Inputs { streams, table, layout, kept, .. } = inp;
    let n = layout.num().map_err(|_| corrupt())?;
    if n > inp.layout_len.min(inp.max_entries) {
        return Err(corrupt());
    }
    tee.sha = match layout.byte().ok_or_else(corrupt)? {
        TRAILER_DERIVED => Some(Sha1::new()),
        0 => None,
        _ => return Err(corrupt()),
    };
    if let Some(e) = &mut tee.entries {
        e.starts.reserve_exact(n as usize + 1);
        e.crcs.reserve_exact(n as usize);
    }
    let piece = |r: &mut Reader, n: u64, tee: &mut Tee| each(r, n, &mut |b| tee.put(b));
    piece(kept, HEADER as u64, tee)?;
    let mut zlib = ZLIB_FIRST;
    let mut buf = [0; 10];
    for _ in 0..n {
        tee.begin();
        let tag = layout.byte().ok_or_else(corrupt)?;
        if tag & REBUILT == 0 {
            if tag != 0 {
                return Err(corrupt());
            }
            let n = layout.num().map_err(|_| corrupt())?;
            piece(kept, n, tee)?;
            continue;
        }
        if tag & !(7 | REBUILT | HEADER_KEPT | ZLIB_SAME) != 0 {
            return Err(corrupt());
        }
        let t = tag & 7;
        let mut e = [0; DERIVED];
        table.exact(&mut e).ok_or_else(corrupt)?;
        let adler = &e[..4];
        let size = u64::from_le_bytes(e[4..12].try_into().unwrap());
        let stream = usize::try_from(u64::from_le_bytes(e[12..].try_into().unwrap())).map_err(|_| corrupt())?;
        match tag & HEADER_KEPT {
            0 => {
                let (h, k) = pack::header(t, size);
                tee.put(&h[..k])?;
            }
            _ => {
                let k = continued(kept, &mut buf)?;
                tee.put(&buf[..k])?;
            }
        }
        match t {
            OFS_DELTA => {
                let k = continued(layout, &mut buf)?;
                tee.put(&buf[..k])?;
            }
            REF_DELTA => piece(kept, ID as u64, tee)?,
            _ => {}
        }
        if tag & ZLIB_SAME == 0 {
            layout.exact(&mut zlib).ok_or_else(corrupt)?;
        }
        tee.put(&zlib)?;
        let at = tee.at;
        streams.take(at, stream, &mut |b| tee.put(b))?;
        tee.put(adler)?;
    }
    tee.end();
    if let Some(s) = tee.sha.take() {
        let sum: [u8; ID] = s.finalize().into();
        tee.put(&sum)?;
    }
    let rest = inp.kept_len.checked_sub(kept.taken()).ok_or_else(corrupt)?;
    piece(kept, rest, tee)?;
    if !layout.done() || !table.done() || !streams.done() || !kept.done() || tee.at != len {
        return Err(corrupt());
    }
    Ok(())
}

/// The pack of `len` bytes from `layout`, `table` (what the streams of the rebuilt entries
/// derive, [`DERIVED`] bytes each: the Adler-32 of its data, big endian, the length of its data
/// and the length of the stream) and `kept`, and the entries' DEFLATE streams `streams` one
/// after another, each of `lens` bytes in all: put to `out` in order, the inputs taken in order
/// in pieces. Where `entries` is given, it is filled with the entries' offsets and CRC-32s, and
/// the trailer (the 20 bytes after the last entry) is returned; the layout may then tell at most
/// `max_entries`.
pub fn build_stream(
    src: [&mut dyn Pull; 4],
    lens: [u64; 4],
    len: u64,
    out: &mut dyn Put,
    entries: Option<&mut Entries>,
    max_entries: u64,
) -> Result<Option<[u8; ID]>> {
    let [streams, table, layout, kept] = src;
    let mut streams = Reader::new(streams, PIECE);
    let mut inp = Inputs {
        streams: &mut streams,
        table: Reader::new(table, SMALL_PIECE),
        layout: Reader::new(layout, SMALL_PIECE),
        kept: Reader::new(kept, PIECE),
        layout_len: lens[2],
        kept_len: lens[3],
        max_entries,
    };
    run(&mut inp, out, entries, len)
}

fn run(inp: &mut Inputs, out: &mut dyn Put, entries: Option<&mut Entries>, len: u64) -> Result<Option<[u8; ID]>> {
    let recording = entries.is_some();
    let mut tee =
        Tee { out, at: 0, sha: None, entries, crc: Hasher::new(), open: false, ended: false, trailer: [0; ID], got: 0 };
    build_into(inp, &mut tee, len)?;
    match (recording, tee.got) {
        (false, _) => Ok(None),
        (true, ID) => Ok(Some(tee.trailer)),
        _ => Err(corrupt()),
    }
}

/// The pack of `len` bytes from slices (see [`build_stream`]), into a vector.
pub fn build(layout: &[u8], streams: &[u8], table: &[u8], kept: &[u8], len: u64) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(usize::try_from(len).map_err(|_| corrupt())?);
    let src: [&mut dyn Pull; 4] = [&mut Slice(streams), &mut Slice(table), &mut Slice(layout), &mut Slice(kept)];
    let lens = [streams.len(), table.len(), layout.len(), kept.len()].map(|l| l as u64);
    build_stream(src, lens, len, &mut out, None, u64::MAX)?;
    Ok(out)
}

/// Whether `layout`, `table` (what [`build`] takes after the streams) and `kept` give `pack`
/// back with the streams taken as they are in it: all but aqr-flate's part, without a copy.
pub fn check(layout: &[u8], table: &[u8], kept: &[u8], pack: &[u8]) -> bool {
    let (mut table_src, mut layout_src, mut kept_src) = (Slice(table), Slice(layout), Slice(kept));
    let mut inp = Inputs {
        streams: &mut Own(pack),
        table: Reader::new(&mut table_src, SMALL_PIECE),
        layout: Reader::new(&mut layout_src, SMALL_PIECE),
        kept: Reader::new(&mut kept_src, PIECE),
        layout_len: layout.len() as u64,
        kept_len: kept.len() as u64,
        max_entries: u64::MAX,
    };
    run(&mut inp, &mut CheckPut { want: pack, at: 0 }, None, pack.len() as u64).is_ok()
}
