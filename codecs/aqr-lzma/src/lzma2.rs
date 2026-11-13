//! LZMA2 (of xz and 7z): LZMA's packets in chunks of at most 2 MiB of data and 64 KiB coded, each
//! chunk's range coder flushed at its end. From one chunk to the next the model goes on, or is
//! reset (its state, probabilities and last distances), with new properties or not; chunks of
//! data stored as it is come in between. A chunk resetting the dictionary starts a **segment**,
//! which no match reaches before: segments are split and rebuilt on their own (`split`, `join`),
//! the record holding the chunks' headers before the parse's (`recons`, with a break at each
//! chunk).

use std::ops::Range;

use crate::lzma::{self, END, Error, Kind, Model, Packet, Props};
use crate::mf::Mf;
use crate::rc::{Decoder, Encoder};
use crate::recons::{self, Ev, Split};
use crate::stream::{Body, Pull, Put, Reader};
use crate::window::{View, Window};

/// The most data a chunk holds.
pub const CHUNK_DATA: usize = 1 << 21;
/// The most bytes an LZMA chunk codes its packets in, and a stored chunk holds.
pub const CHUNK_CODED: usize = 1 << 16;
/// The room an LZMA chunk's coder is given, more than it codes in ([`CHUNK_CODED`]) by its flush.
pub const CHUNK_ROOM: usize = CHUNK_CODED + 16;
/// The literal probabilities of the largest properties of LZMA2 (`lc + lp` = 4).
pub const LIT_MAX: usize = 0x300 << 4;

/// A chunk: its control byte (1 or 2 for stored data, else that of an LZMA chunk without the
/// bits of its size: 0x80 resetting nothing, 0xA0 the state, 0xC0 the state with new
/// properties, 0xE0 those and the dictionary; 1 resets the dictionary too), the bytes of data
/// it holds, and the properties it sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chunk {
    pub control: u8,
    pub len: usize,
    pub props: Option<Props>,
}

impl Chunk {
    pub fn stored(&self) -> bool {
        self.control < 0x80
    }

    /// Whether it starts a segment.
    pub fn resets_dict(&self) -> bool {
        self.control == 1 || self.control == 0xE0
    }
}

/// The dictionary size that LZMA2's properties byte (of xz's filter and 7z's coder) tells.
pub fn dict_size(b: u8) -> Option<u32> {
    match b {
        0..40 => Some((2 | (b as u32 & 1)) << (b / 2 + 11)),
        40 => Some(u32::MAX),
        _ => None,
    }
}

/// A segment decoded: its data, chunks and packets, and its bytes in the stream (the stream's
/// end byte with the last).
pub struct Segment {
    pub data: Vec<u8>,
    pub chunks: Vec<Chunk>,
    pub packets: Vec<Packet>,
    pub span: Range<usize>,
}

/// A stream decoded into its segments (one, empty, for a stream of its end byte alone); `used`
/// is the bytes of the stream, its end byte included.
pub struct Decoded {
    pub segments: Vec<Segment>,
    pub used: usize,
}

/// Decodes the stream at the start of `input`, its data at most `limit` bytes. A match may not
/// go past its chunk (no encoder makes one), and a segment's first LZMA chunk must give its
/// properties (as 7-Zip's decoder wants), so that segments stand alone.
pub fn decode(input: &[u8], limit: usize) -> Result<Decoded, Error> {
    let mut segments: Vec<Segment> = Vec::new();
    let (mut at, mut total) = (0, 0usize);
    let mut model: Option<Model> = None;
    let mut props = None;
    loop {
        let &first = input.get(at).ok_or(Error::Corrupt)?;
        if first == 0 {
            at += 1;
            break;
        }
        let lzma = first >= 0x80;
        let control = if lzma { first & 0xE0 } else { first };
        if !lzma && control > 2 {
            return Err(Error::Corrupt);
        }
        let head = if !lzma {
            3
        } else if control >= 0xC0 {
            6
        } else {
            5
        };
        let h = input.get(at..at + head).ok_or(Error::Corrupt)?;
        let len = if lzma { (first as usize & 0x1F) << 16 } else { 0 } | (h[1] as usize) << 8 | h[2] as usize;
        let len = len + 1;
        if control == 1 || control == 0xE0 {
            segments.push(Segment { data: Vec::new(), chunks: Vec::new(), packets: Vec::new(), span: at..at });
            (model, props) = (None, None);
        }
        let seg = segments.last_mut().ok_or(Error::Corrupt)?;
        total += len;
        if total > limit {
            return Err(Error::Corrupt);
        }
        if !lzma {
            seg.data.extend_from_slice(input.get(at + 3..at + 3 + len).ok_or(Error::Corrupt)?);
            seg.chunks.push(Chunk { control, len, props: None });
            at += 3 + len;
            seg.span.end = at;
            continue;
        }
        let packed = ((h[3] as usize) << 8 | h[4] as usize) + 1;
        let set = if control >= 0xC0 {
            Some(Props::from_byte(h[5]).filter(|p| p.lc + p.lp <= 4).ok_or(Error::Corrupt)?)
        } else {
            None
        };
        if control >= 0xA0 {
            props = set.or(props);
            model = Some(Model::new(props.ok_or(Error::Corrupt)?));
        }
        let m = model.as_mut().ok_or(Error::Corrupt)?;
        let coded = input.get(at + head..at + head + packed).ok_or(Error::Corrupt)?;
        let mut c = Decoder::new(coded).ok_or(Error::Corrupt)?;
        let data = &mut seg.data;
        let end = data.len() + len;
        while data.len() < end {
            let pos = data.len();
            let pk = m.code(&mut c, data, pos, Packet::LIT);
            if c.overrun > 4 {
                return Err(Error::Corrupt);
            }
            match pk.kind {
                Kind::Lit => {
                    data.push(pk.dist as u8);
                    seg.packets.push(Packet::LIT);
                }
                _ if pk.dist == END => return Err(Error::Corrupt),
                _ => {
                    if pos + pk.len as usize > end {
                        return Err(Error::Corrupt);
                    }
                    lzma::copy_match(data, pk).ok_or(Error::Corrupt)?;
                    seg.packets.push(if pk.kind == Kind::ShortRep { Packet::SHORT_REP } else { pk });
                }
            }
        }
        if !c.finished() || c.overrun > 0 || c.pos != packed {
            return Err(Error::Corrupt);
        }
        seg.chunks.push(Chunk { control, len, props: set });
        at += head + packed;
        seg.span.end = at;
    }
    match segments.last_mut() {
        Some(s) => s.span.end = at,
        None => segments.push(Segment { data: Vec::new(), chunks: Vec::new(), packets: Vec::new(), span: 0..at }),
    }
    Ok(Decoded { segments, used: at })
}

/// Writes a segment's chunks, given each one's header and then its packets.
pub struct Writer<'a> {
    sink: &'a mut dyn Put,
    /// The LZMA chunk being coded: its end in the data, its coder and itself.
    open: Option<(usize, Encoder<'static>, Chunk)>,
    model: Option<Model>,
    /// The memory of the model's literal probabilities until it has one: room for the largest
    /// ([`LIT_MAX`]).
    lit: Vec<u16>,
    props: Option<Props>,
    /// The position of the next packet.
    pos: usize,
}

impl<'a> Writer<'a> {
    /// Writing to `sink`.
    pub fn to(sink: &'a mut dyn Put) -> Self {
        Writer { sink, open: None, model: None, lit: Vec::with_capacity(LIT_MAX), props: None, pos: 0 }
    }

    /// Writes the LZMA chunk open, if any, once the packets reach its end.
    fn close(&mut self) -> Option<()> {
        let Some((end, c, ch)) = self.open.take() else { return Some(()) };
        if self.pos != end {
            return None;
        }
        let coded = c.finish();
        if coded.len() > CHUNK_CODED {
            return None;
        }
        let (u, p) = (ch.len - 1, coded.len() - 1);
        let head = [ch.control | (u >> 16) as u8, (u >> 8) as u8, u as u8, (p >> 8) as u8, p as u8];
        self.sink.put(&head)?;
        if let Some(p) = ch.props {
            self.sink.put(&[p.byte()])?;
        }
        self.sink.put(&coded)
    }

    /// Starts the chunk `ch` at the position of the next packet, after the one before it: a
    /// stored chunk is written from `v`, which holds its data, an LZMA chunk is opened.
    pub fn begin(&mut self, v: &View, ch: Chunk) -> Option<()> {
        self.close()?;
        let end = self.pos + ch.len;
        if ch.stored() {
            let u = ch.len - 1;
            self.sink.put(&[ch.control, (u >> 8) as u8, u as u8])?;
            self.sink.put(v.d.get(self.pos.checked_sub(v.base)?..end.checked_sub(v.base)?)?)?;
            self.pos = end;
            return Some(());
        }
        if ch.control >= 0xA0 {
            self.props = ch.props.or(self.props);
            match &mut self.model {
                Some(m) => m.reset(self.props?),
                None => self.model = Some(Model::with_lit(self.props?, std::mem::take(&mut self.lit))),
            }
        }
        self.model.as_ref()?;
        self.open = Some((end, Encoder::new(Vec::with_capacity(CHUNK_ROOM)), ch));
        Some(())
    }

    /// Codes the next packet, of the data seen through `v`. None where it is not in an LZMA
    /// chunk.
    pub fn push(&mut self, v: &View, pk: Packet) -> Option<()> {
        let (end, c, _) = self.open.as_mut()?;
        if self.pos + pk.len as usize > *end {
            return None;
        }
        self.model.as_mut()?.code_at(c, v.d, v.base, self.pos, pk);
        self.pos += pk.len as usize;
        Some(())
    }

    /// Ends the segment: the last chunk is written (None where it codes in too many bytes, or
    /// the packets stop short of its end), and the stream's end byte where `end`.
    pub fn finish(mut self, end: bool) -> Option<()> {
        self.close()?;
        if end { self.sink.put(&[0]) } else { Some(()) }
    }
}

/// The chunks of the packets of `data`. None where they do not fit.
pub fn encode(data: &[u8], chunks: &[Chunk], packets: &[Packet]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() / 4);
    let v = View::whole(data);
    let mut w = Writer::to(&mut out);
    let (mut pos, mut next) = (0, 0);
    for &c in chunks {
        w.begin(&v, c)?;
        pos += c.len;
        if c.stored() {
            continue;
        }
        let start = pos - c.len;
        let mut at = start;
        while at < pos {
            let pk = *packets.get(next)?;
            next += 1;
            w.push(&v, pk)?;
            at += pk.len as usize;
        }
    }
    w.finish(false)?;
    (pos == data.len() && next == packets.len()).then_some(out)
}

/// The flag of a record telling that the stream's end byte follows the segment.
const END_BYTE: u8 = 1;

/// The record of a segment: its header (a flags byte, bit 0 telling that the stream's end byte
/// follows; the dictionary size, le32; the match finder's reach `fin`, [`recons::finder_of`]),
/// then the record of its packets and chunks ([`recons::deconstruct`]); and the cost of that by
/// [`recons::Cat`] in bits. None where the packets do not fit, or `fin` is not a reach.
pub fn deconstruct(seg: &Segment, dict: u32, fin: u8, end: bool) -> Option<(Vec<u8>, [f64; recons::CATS])> {
    let (body, cost) = recons::deconstruct(&seg.data, &seg.packets, dict, fin, Some(&seg.chunks))?;
    let mut r = Vec::with_capacity(HEADER + body.len());
    r.push(if end { END_BYTE } else { 0 });
    r.extend(dict.to_le_bytes());
    r.push(fin);
    r.extend(body);
    Some((r, cost))
}

/// Splits the stream at the start of `input` (of a dictionary of `dict` bytes) into the data
/// and record of each segment, each segment's bytes its `used`, on up to `threads` threads.
/// Fails where the stream does not decode or is not what encoding its packets gives.
pub fn split(input: &[u8], dict: u32, limit: usize, threads: usize) -> Result<Vec<Split>, Error> {
    split_at(input, dict, limit, threads, None)
}

/// [`split`] with the match finder of the reach `fin` ([`recons::finder_of`]) for each segment;
/// None for the largest that the memory bound allows for the segment ([`crate::plan::finder_for`]).
pub fn split_at(input: &[u8], dict: u32, limit: usize, threads: usize, fin: Option<u8>) -> Result<Vec<Split>, Error> {
    let dec = decode(input, limit)?;
    let n = dec.segments.len();
    let records = aqr_common::pool::par_map(n, threads, |i| {
        let s = &dec.segments[i];
        let last = i + 1 == n;
        let chunks = &input[s.span.start..s.span.end - last as usize];
        if encode(&s.data, &s.chunks, &s.packets).as_deref() != Some(chunks) {
            return Err(Error::Encoded);
        }
        let fin = fin.unwrap_or_else(|| crate::plan::finder_for(s.data.len(), dict, crate::plan::Own::Lzma2));
        deconstruct(s, dict, fin, last).map(|r| r.0).ok_or(Error::Corrupt)
    });
    let mut out = Vec::with_capacity(n);
    for (s, r) in dec.segments.into_iter().zip(records) {
        out.push(Split { record: r?, used: s.span.len(), data: s.data });
    }
    Ok(out)
}

/// The bytes of a segment's record before its body: the flags, the dictionary size and the match
/// finder's reach.
pub const HEADER: usize = 6;

/// What a segment's record tells before the segment is rebuilt: whether the stream's end byte
/// follows it, the dictionary size, the match finder's reach ([`recons::finder_of`]), and the
/// record of the packets and chunks.
pub struct Head<'a> {
    pub end: bool,
    pub dict: u32,
    pub fin: u8,
    pub body: &'a [u8],
}

/// The [`Head`] of the record of a segment. None where it is not one.
pub fn head_of(record: &[u8]) -> Option<Head<'_>> {
    let (head, body) = record.split_at_checked(HEADER)?;
    if head[0] & !END_BYTE != 0 {
        return None;
    }
    let dict = u32::from_le_bytes(head[1..5].try_into().unwrap());
    recons::finder_of(dict, head[5])?;
    Some(Head { end: head[0] & END_BYTE != 0, dict, fin: head[5], body })
}

/// The bytes of a segment (and the stream's end byte, where its record says so) from its data
/// and record. None where the record is not one of `data`'s.
pub fn join(data: &[u8], record: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() / 4 + 16);
    join_onto(data, record, &mut out)?;
    Some(out)
}

/// [`join`] written onto the end of `out`.
pub fn join_onto(data: &[u8], record: &[u8], out: &mut Vec<u8>) -> Option<()> {
    let h = head_of(record)?;
    // (an empty segment has no chunks to code: the coder's flush alone)
    if data.is_empty() && h.body.len() != 1 {
        return None;
    }
    let mut w = Writer::to(out);
    recons::reconstruct(data, h.body, h.dict, h.fin, true, |v, ev| match ev {
        Ev::Chunk(c) => w.begin(v, c),
        Ev::Packet(pk) => w.push(v, pk),
    })?;
    w.finish(h.end)
}

/// [`join`] as a part module does it: the `n` bytes of data read in order from `data` into a
/// window, the record read in order from `rec` (`rec_len` bytes of it), the segment put to
/// `sink` in pieces; nothing of the lengths is held. The same bytes.
pub fn join_stream(
    data: &mut dyn Pull,
    n: usize,
    rec: &mut Reader,
    rec_len: u64,
    sink: &mut dyn Put,
) -> crate::Result<()> {
    let mut ok = || -> Option<()> {
        let flags = rec.byte()?;
        let mut dict = [0; 4];
        for b in &mut dict {
            *b = rec.byte()?;
        }
        let dict = u32::from_le_bytes(dict);
        let fin = rec.byte()?;
        if flags & !END_BYTE != 0 {
            return None;
        }
        let reach = recons::reach(dict);
        let mut win = Window::fed(data, n, reach);
        let mut mf = Mf::new(n, recons::finder_of(dict, fin)?);
        let mut w = Writer::to(sink);
        let body = rec_len.checked_sub(HEADER as u64)?;
        if n == 0 && body != 1 {
            return None;
        }
        let mut d = Body::new(rec, body);
        recons::walk_chunks(&mut d, &mut mf, &mut win, reach, |v, ev| match ev {
            Ev::Chunk(c) => w.begin(v, c),
            Ev::Packet(pk) => w.push(v, pk),
        })?;
        d.finish()?;
        win.finish()?;
        w.finish(flags & END_BYTE != 0)
    };
    ok().ok_or_else(crate::corrupt)
}
