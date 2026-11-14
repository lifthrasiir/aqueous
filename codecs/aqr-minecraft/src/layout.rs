//! The layout of a region file (FORMAT.md, §3): for each chunk, in the order of its sectors, a
//! tag telling how it is made and what of it is kept, then whether the bytes after the last
//! chunk are; [`build_stream`] makes the file again of the header, the layout, the chunks' data
//! and the bytes kept, all taken in order in pieces and the file put in order.

use aqr_api::stream::{Pull, Put, Reader};
use aqr_common::arith::ArithDecoder;
use aqr_common::varint;
use aqr_flate::zlib::{Params, Strategy};

use crate::model::{self, Coder, Model, Work};
use crate::nbt;
use crate::region::{self, Chunk, HEADER, NONE, SECTOR, ZLIB};
use crate::{Result, corrupt};

/// How a chunk is made (the low bits of its tag): its sectors kept as they are; its zlib stream
/// made of its data; its data as it is (compression type 3).
pub const KEPT: u64 = 0;
pub const REBUILT: u64 = 1;
pub const STORED: u64 = 2;
pub const HOW: u64 = 3;
/// Bits of a tag: the bytes between the chunk and the one before it (or the header) are kept,
/// else zero; the bytes after its stream in its last sector are kept, else zero.
pub const GAP_KEPT: u64 = 4;
pub const PAD_KEPT: u64 = 8;
/// A bit of a tag: the chunk's data is its NBT's skeleton, its holes coded (a varint after the
/// tag, of the bytes of `coded` they take).
pub const NBT: u64 = 16;
/// The tag's bits below the length of the chunk's data (of one made or stored).
pub const SHIFT: u32 = 5;

/// The last byte of the layout: the bytes after the last chunk are kept, else zero.
pub const TAIL_KEPT: u8 = 1;

/// Bytes of the pieces in which [`build_stream`] takes the data and the bytes kept, and the
/// layout.
pub const PIECE: usize = 1 << 16;
pub const SMALL_PIECE: usize = 1 << 12;

/// Bytes of the zlib header and Adler-32 around a chunk's DEFLATE stream, and of the length and
/// compression type before it.
pub const ZLIB_FRAME: usize = 6;
pub const CHUNK_HEADER: usize = 5;

/// The zlib parameters of the module's params (FORMAT.md, §2): level, strategy, window bits and
/// memory level, a byte each.
pub fn params(p: &[u8]) -> Result<Params> {
    let &[level, strategy, wbits, mem] = p else { return Err(corrupt()) };
    let strategy = *Strategy::ALL.get(strategy as usize).ok_or_else(corrupt)?;
    let p = Params { level, strategy, wbits, mem };
    match (1..=9).contains(&level) && (9..=15).contains(&wbits) && (1..=9).contains(&mem) {
        true => Ok(p),
        false => Err(corrupt()),
    }
}

/// [`params`] of `p`.
pub fn params_of(p: Params) -> Vec<u8> {
    vec![p.level, p.strategy as u8, p.wbits, p.mem]
}

/// The layout of a region file, and the bytes kept, as made chunk by chunk in the order of their
/// sectors.
pub struct Writer<'a> {
    file: &'a [u8],
    pub layout: Vec<u8>,
    pub kept: Vec<u8>,
    /// The bytes of the file laid out so far.
    at: usize,
}

/// How a chunk is made, as [`Writer::chunk`] is told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    Kept,
    /// Of `n` bytes of data, and its zlib stream (or its data) of `len` bytes after its header.
    Rebuilt {
        n: u64,
        len: usize,
    },
    Stored {
        n: u64,
    },
}

impl<'a> Writer<'a> {
    /// The layout of `file`, a region file, its header kept apart.
    pub fn new(file: &'a [u8]) -> Writer<'a> {
        Writer { file, layout: Vec::new(), kept: Vec::new(), at: HEADER }
    }

    /// Whether `d` is all zero bytes; else it is kept.
    fn zero_or_kept(&mut self, d: &[u8]) -> bool {
        let zero = d.iter().all(|&b| b == 0);
        if !zero {
            self.kept.extend_from_slice(d);
        }
        zero
    }

    /// The chunk `c`, the next by its sectors, made as `how` tells, its data's holes `coded` in
    /// so many bytes where its data is its NBT's skeleton.
    pub fn chunk(&mut self, c: &Chunk, how: How, coded: Option<u64>) {
        let (r, file) = (c.range(), self.file);
        let mut tag = 0;
        if !self.zero_or_kept(&file[self.at..r.start]) {
            tag |= GAP_KEPT;
        }
        let (how, n, end) = match how {
            How::Kept => (KEPT, 0, r.start),
            How::Rebuilt { n, len } => (REBUILT, n, r.start + CHUNK_HEADER + len),
            How::Stored { n } => (STORED, n, r.start + CHUNK_HEADER + n as usize),
        };
        if how == KEPT {
            self.kept.extend_from_slice(&file[r.clone()]);
        } else if !self.zero_or_kept(&file[end..r.end]) {
            tag |= PAD_KEPT;
        }
        if coded.is_some() {
            tag |= NBT;
        }
        varint::put(&mut self.layout, tag | how | n << SHIFT);
        if let Some(c) = coded {
            varint::put(&mut self.layout, c);
        }
        self.at = r.end;
    }

    /// The layout and the bytes kept, the bytes after the last chunk too.
    pub fn finish(mut self) -> (Vec<u8>, Vec<u8>) {
        let tail = self.file.get(self.at..).unwrap_or(&[]);
        let t = if self.zero_or_kept(tail) { 0 } else { TAIL_KEPT };
        self.layout.push(t);
        (self.layout, self.kept)
    }
}

/// What [`build_stream`] takes the chunks' data by.
pub trait Chunks {
    /// The zlib stream of the next rebuilt chunk, of `n` bytes of data (its holes `coded` in so
    /// many bytes, where it is of an NBT's skeleton), appended to `out` (the buffer of the chunk's
    /// sectors, of the length and compression type before the stream).
    fn rebuilt(&mut self, n: u64, coded: Option<u64>, out: Vec<u8>) -> Result<Vec<u8>>;
    /// The next `n` bytes of data (as [`Chunks::rebuilt`]), put to `out`.
    fn stored(&mut self, n: u64, coded: Option<u64>, out: &mut dyn Put) -> Result<()>;
    /// Whether every byte of data was taken.
    fn done(&mut self) -> bool;
}

/// The chunks' data taken in order of the streams `data` and `coded`: the data as it is, or an
/// NBT's skeleton and its holes coded, made again by the model of the file ([`Holes`]).
pub struct Source<'a> {
    pub data: Reader<'a>,
    pub coded: Reader<'a>,
    /// The bytes of each not taken yet.
    pub left: u64,
    pub coded_left: u64,
    pub holes: Holes,
}

/// The model of a file's holes, and the buffers of its walks.
pub struct Holes {
    pub model: Model,
    pub work: Work,
    pub sections: Vec<nbt::Section>,
    pub frames: Vec<nbt::Frame>,
}

impl Default for Holes {
    fn default() -> Self {
        Holes::new()
    }
}

impl Holes {
    pub fn new() -> Holes {
        Holes {
            model: Model::new(),
            work: Work::new(),
            sections: Vec::with_capacity(nbt::SECTIONS),
            frames: nbt::frames(),
        }
    }
}

/// The skeleton's bytes of a [`Reader`].
struct Skeleton<'r, 'a> {
    src: &'r mut Reader<'a>,
    left: &'r mut u64,
}

impl nbt::Fill for Skeleton<'_, '_> {
    fn fill(&mut self, d: &mut [u8]) -> Result<()> {
        *self.left = self.left.checked_sub(d.len() as u64).ok_or_else(corrupt)?;
        self.src.exact(d).ok_or_else(corrupt)
    }
}

impl Source<'_> {
    /// The next chunk's data, of `n` bytes, its holes `coded` in so many bytes where given.
    pub fn next(&mut self, n: u64, coded: Option<u64>) -> Result<Vec<u8>> {
        let mut d = vec![0; usize::try_from(n).map_err(|_| corrupt())?];
        match coded {
            None => {
                self.left = self.left.checked_sub(n).ok_or_else(corrupt)?;
                self.data.exact(&mut d).ok_or_else(corrupt)?;
            }
            Some(c) => {
                self.coded_left = self.coded_left.checked_sub(c).ok_or_else(corrupt)?;
                let mut z = vec![0; usize::try_from(c).map_err(|_| corrupt())?];
                self.coded.exact(&mut z).ok_or_else(corrupt)?;
                let mut skeleton = Skeleton { src: &mut self.data, left: &mut self.left };
                decode_holes(&mut self.holes, &z, &mut d, &mut skeleton)?;
            }
        }
        Ok(d)
    }

    /// Whether every byte of both was taken.
    pub fn done(&mut self) -> bool {
        self.left == 0 && self.coded_left == 0 && self.data.done() && self.coded.done()
    }
}

/// The NBT `d` made again of its skeleton (of `src`) and its holes coded in `z`.
pub fn decode_holes(h: &mut Holes, z: &[u8], d: &mut [u8], src: &mut dyn nbt::Fill) -> Result<()> {
    let mut c = ArithDecoder::new(z);
    let Holes { model, work, sections, frames } = h;
    nbt::parse(d, src, sections, frames, &mut |cand| Ok(c.bit(model.flag(cand.kind), false)))?;
    model::chunk(&mut c, model, work, d, sections, None)?;
    match c.failed() {
        false => Ok(()),
        true => Err(corrupt()),
    }
}

/// The chunks' data of a [`Source`], each rebuilt chunk's zlib stream made of its data as it
/// comes: a buffer of its sectors, then of its data (and of its holes coded), then zlib's
/// emulator, are what a chunk holds.
pub struct Streamed<'a> {
    pub src: Source<'a>,
    pub params: Params,
}

impl Chunks for Streamed<'_> {
    fn rebuilt(&mut self, n: u64, coded: Option<u64>, out: Vec<u8>) -> Result<Vec<u8>> {
        let d = self.src.next(n, coded)?;
        zlib_onto(&d, self.params, out)
    }

    fn stored(&mut self, n: u64, coded: Option<u64>, out: &mut dyn Put) -> Result<()> {
        if coded.is_some() {
            let d = self.src.next(n, coded)?;
            return out.put(&d).ok_or_else(corrupt);
        }
        self.src.left = self.src.left.checked_sub(n).ok_or_else(corrupt)?;
        self.src.data.each(n, |b| out.put(b)).ok_or_else(corrupt)
    }

    fn done(&mut self) -> bool {
        self.src.done()
    }
}

/// The zlib stream of `data` as zlib makes it with `p` (its header, the DEFLATE stream and the
/// Adler-32), appended to `out`.
pub fn zlib_onto(data: &[u8], p: Params, mut out: Vec<u8>) -> Result<Vec<u8>> {
    out.extend(region::zlib_header(p));
    let mut out = aqr_flate::zlib::compress_onto(data, p, out).ok_or_else(corrupt)?;
    out.extend(aqr_common::adler32::hash(data).to_be_bytes());
    Ok(out)
}

/// Zero bytes, put a piece at a time.
static ZEROS: [u8; 4096] = [0; 4096];

fn zeros(mut n: usize, out: &mut dyn Put) -> Result<()> {
    while n > 0 {
        let k = n.min(ZEROS.len());
        out.put(&ZEROS[..k]).ok_or_else(corrupt)?;
        n -= k;
    }
    Ok(())
}

/// `n` bytes kept, or zero (`kept` false), put to `out`.
fn zero_or_kept(kept: bool, n: usize, src: &mut Reader, out: &mut dyn Put) -> Result<()> {
    match kept {
        true => src.each(n as u64, |b| out.put(b)).ok_or_else(corrupt),
        false => zeros(n, out),
    }
}

/// The header (the first [`HEADER`] bytes) read whole of `src`.
pub fn read_header(src: &mut dyn Pull) -> Result<Vec<u8>> {
    let mut h = vec![0; HEADER];
    let mut at = 0;
    while at < HEADER {
        match src.pull(&mut h[at..]) {
            0 => return Err(corrupt()),
            k => at += k,
        }
    }
    Ok(h)
}

/// What the layout tells of a chunk made of its data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Made {
    /// The bytes of its data, whether it is rebuilt (else stored), and the bytes of its holes
    /// coded (where its data is an NBT's skeleton).
    pub n: u64,
    pub rebuilt: bool,
    pub coded: Option<u64>,
    /// Bytes of its sectors.
    pub sectors: u64,
}

/// What the layout tells of each chunk made of its data, in order.
pub fn made(header: &[u8], layout: &[u8], size: u64) -> Result<Vec<Made>> {
    let chunks = region::chunks(header, size / SECTOR as u64).ok_or_else(corrupt)?;
    let mut at = 0;
    let mut v = Vec::new();
    for c in &chunks {
        let tag = varint::get(layout, &mut at)?;
        let coded = match tag & NBT != 0 && tag & HOW != KEPT {
            true => Some(varint::get(layout, &mut at)?),
            false => None,
        };
        let sectors = c.count as u64 * SECTOR as u64;
        match tag & HOW {
            KEPT => {}
            h => v.push(Made { n: tag >> SHIFT, rebuilt: h == REBUILT, coded, sectors }),
        }
    }
    Ok(v)
}

/// The region file of `size` bytes put to `out` in order, of its header, its layout read of
/// `layout`, the chunks' data taken by `chunks` and the bytes kept of `kept`.
pub fn build_stream(
    header: &[u8],
    layout: &mut Reader,
    chunks: &mut dyn Chunks,
    kept: &mut Reader,
    size: u64,
    out: &mut dyn Put,
) -> Result<()> {
    let size = usize::try_from(size).map_err(|_| corrupt())?;
    let list = region::chunks(header, (size / SECTOR) as u64).ok_or_else(corrupt)?;
    out.put(header).ok_or_else(corrupt)?;
    let mut at = HEADER;
    for c in &list {
        let r = c.range();
        let tag = layout.num()?;
        zero_or_kept(tag & GAP_KEPT != 0, r.start - at, kept, out)?;
        let n = tag >> SHIFT;
        let sectors = r.len();
        let coded = match tag & NBT != 0 && tag & HOW != KEPT {
            true => Some(layout.num()?),
            false => None,
        };
        let used = match tag & HOW {
            KEPT if tag & !GAP_KEPT == 0 => {
                kept.each(sectors as u64, |b| out.put(b)).ok_or_else(corrupt)?;
                sectors
            }
            REBUILT => {
                let mut buf = Vec::with_capacity(sectors + aqr_flate::write::TOKEN_SLACK);
                buf.extend([0; 4]);
                buf.push(ZLIB);
                let mut buf = chunks.rebuilt(n, coded, buf)?;
                let len = buf.len().checked_sub(4).filter(|&l| l + 4 <= sectors).ok_or_else(corrupt)?;
                buf[..4].copy_from_slice(&(len as u32).to_be_bytes());
                out.put(&buf).ok_or_else(corrupt)?;
                buf.len()
            }
            STORED => {
                let len = usize::try_from(n).ok().filter(|&n| n <= sectors - CHUNK_HEADER).ok_or_else(corrupt)?;
                out.put(&((len + 1) as u32).to_be_bytes()).ok_or_else(corrupt)?;
                out.put(&[NONE]).ok_or_else(corrupt)?;
                chunks.stored(n, coded, out)?;
                len + CHUNK_HEADER
            }
            _ => return Err(corrupt()),
        };
        if tag & HOW != KEPT {
            zero_or_kept(tag & PAD_KEPT != 0, sectors - used, kept, out)?;
        }
        at = r.end;
    }
    let tail = match layout.byte().ok_or_else(corrupt)? {
        0 => false,
        TAIL_KEPT => true,
        _ => return Err(corrupt()),
    };
    zero_or_kept(tail, size.checked_sub(at).ok_or_else(corrupt)?, kept, out)?;
    match layout.done() && kept.done() && chunks.done() {
        true => Ok(()),
        false => Err(corrupt()),
    }
}
