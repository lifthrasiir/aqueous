//! LZMA2 (of xz and 7z): LZMA's packets in chunks of at most 2 MiB of data and 64 KiB coded, each
//! chunk's range coder flushed at its end. From one chunk to the next the model goes on, or is
//! reset (its state, probabilities and last distances), with new properties or not; chunks of
//! data stored as it is come in between. A chunk resetting the dictionary starts a **segment**,
//! which no match reaches before: segments are split and rebuilt on their own (`split`, `join`),
//! the record holding the chunks' headers before the parse's (`recons`, with a break at each
//! chunk).

use std::ops::Range;

use aqr_common::varint;

use crate::lzma::{END, Error, Kind, Model, Packet, Props};
use crate::rc::{Decoder, Encoder};
use crate::recons::{self, Break, Split};

/// The most data a chunk holds.
pub const CHUNK_DATA: usize = 1 << 21;
/// The most bytes an LZMA chunk codes its packets in, and a stored chunk holds.
pub const CHUNK_CODED: usize = 1 << 16;

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
    fn resets_dict(&self) -> bool {
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
                    let src = pos.checked_sub(pk.dist as usize + 1).ok_or(Error::Corrupt)?;
                    if pos + pk.len as usize > end {
                        return Err(Error::Corrupt);
                    }
                    for j in 0..pk.len as usize {
                        data.push(data[src + j]);
                    }
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

/// Writes a segment's chunks, given its packets one after another.
pub struct Writer<'a> {
    chunks: &'a [Chunk],
    /// The next chunk to write, and its start in the data.
    next: usize,
    start: usize,
    /// The LZMA chunk being coded: its end in the data, its coder and itself.
    open: Option<(usize, Encoder, Chunk)>,
    model: Option<Model>,
    props: Option<Props>,
    /// The position of the next packet.
    pos: usize,
    out: Vec<u8>,
}

impl<'a> Writer<'a> {
    pub fn new(chunks: &'a [Chunk], capacity: usize) -> Self {
        let out = Vec::with_capacity(capacity);
        Writer { chunks, next: 0, start: 0, open: None, model: None, props: None, pos: 0, out }
    }

    /// Writes the chunks that end by the position, and opens the LZMA chunk at it.
    fn reach(&mut self, data: &[u8]) -> Option<()> {
        loop {
            if let Some((end, ..)) = self.open {
                if self.pos < end {
                    return Some(());
                }
                let (_, c, ch) = self.open.take()?;
                let coded = c.finish();
                if coded.len() > CHUNK_CODED {
                    return None;
                }
                let (u, p) = (ch.len - 1, coded.len() - 1);
                self.out.extend([ch.control | (u >> 16) as u8, (u >> 8) as u8, u as u8, (p >> 8) as u8, p as u8]);
                self.out.extend(ch.props.map(Props::byte));
                self.out.extend(coded);
            }
            let Some(&ch) = self.chunks.get(self.next) else { return Some(()) };
            if self.pos != self.start {
                return None;
            }
            self.next += 1;
            let span = self.start..self.start + ch.len;
            self.start = span.end;
            if ch.stored() {
                let u = ch.len - 1;
                self.out.extend([ch.control, (u >> 8) as u8, u as u8]);
                self.out.extend_from_slice(data.get(span.clone())?);
                self.pos = span.end;
                continue;
            }
            if ch.control >= 0xA0 {
                self.props = ch.props.or(self.props);
                self.model = Some(Model::new(self.props?));
            }
            self.model.as_ref()?;
            self.open = Some((span.end, Encoder::new(Vec::with_capacity(CHUNK_CODED + 16)), ch));
        }
    }

    /// Codes the next packet of `data`. None where it is not in an LZMA chunk.
    pub fn push(&mut self, data: &[u8], pk: Packet) -> Option<()> {
        self.reach(data)?;
        let (end, c, _) = self.open.as_mut()?;
        if self.pos + pk.len as usize > *end {
            return None;
        }
        self.model.as_mut()?.code(c, data, self.pos, pk);
        self.pos += pk.len as usize;
        Some(())
    }

    /// The chunks, once every one is written (None otherwise, or where one codes in too many
    /// bytes).
    pub fn finish(mut self, data: &[u8]) -> Option<Vec<u8>> {
        self.reach(data)?;
        (self.open.is_none() && self.next == self.chunks.len() && self.pos == data.len()).then_some(self.out)
    }
}

/// The chunks of the packets of `data`. None where they do not fit.
pub fn encode(data: &[u8], chunks: &[Chunk], packets: &[Packet]) -> Option<Vec<u8>> {
    let mut w = Writer::new(chunks, data.len() / 4);
    for &pk in packets {
        w.push(data, pk)?;
    }
    w.finish(data)
}

/// The breaks of the record's walk: at each chunk, where the state is reset by it, and its data
/// skipped where it is stored.
fn breaks(chunks: &[Chunk]) -> Vec<Break> {
    let mut at = 0;
    chunks
        .iter()
        .map(|c| {
            let b = Break { at, reset: c.control >= 0xA0, stored: if c.stored() { c.len } else { 0 } };
            at += c.len;
            b
        })
        .collect()
}

/// The flag of a record telling that the stream's end byte follows the segment.
const END_BYTE: u8 = 1;

/// The record of a segment: its header (a flags byte, bit 0 telling that the stream's end byte
/// follows; the dictionary size, le32; the number of chunks, then for each its control byte,
/// its data's length less one and the properties it sets, varints but the bytes), then the
/// record of its packets ([`recons::deconstruct`]); and the cost of that by [`recons::Cat`] in
/// bits. None where the packets do not fit.
pub fn deconstruct(seg: &Segment, dict: u32, end: bool) -> Option<(Vec<u8>, [f64; recons::CATS])> {
    let (body, cost) = recons::deconstruct(&seg.data, &seg.packets, dict, &breaks(&seg.chunks))?;
    let mut r = Vec::with_capacity(16 + 4 * seg.chunks.len() + body.len());
    r.push(if end { END_BYTE } else { 0 });
    r.extend(dict.to_le_bytes());
    varint::put(&mut r, seg.chunks.len() as u64);
    for c in &seg.chunks {
        r.push(c.control);
        varint::put(&mut r, c.len as u64 - 1);
        r.extend(c.props.map(Props::byte));
    }
    r.extend(body);
    Some((r, cost))
}

/// Splits the stream at the start of `input` (of a dictionary of `dict` bytes) into the data
/// and record of each segment, each segment's bytes its `used`, on up to `threads` threads.
/// Fails where the stream does not decode or is not what encoding its packets gives.
pub fn split(input: &[u8], dict: u32, limit: usize, threads: usize) -> Result<Vec<Split>, Error> {
    let dec = decode(input, limit)?;
    let n = dec.segments.len();
    let records = aqr_common::pool::par_map(n, threads, |i| {
        let s = &dec.segments[i];
        let last = i + 1 == n;
        let chunks = &input[s.span.start..s.span.end - last as usize];
        if encode(&s.data, &s.chunks, &s.packets).as_deref() != Some(chunks) {
            return Err(Error::Encoded);
        }
        deconstruct(s, dict, last).map(|r| r.0).ok_or(Error::Corrupt)
    });
    let mut out = Vec::with_capacity(n);
    for (s, r) in dec.segments.into_iter().zip(records) {
        out.push(Split { record: r?, used: s.span.len(), data: s.data });
    }
    Ok(out)
}

/// The bytes of a segment (and the stream's end byte, where its record says so) from its data
/// and record. None where the record is not one of `data`'s.
pub fn join(data: &[u8], record: &[u8]) -> Option<Vec<u8>> {
    let (&flags, rest) = record.split_first()?;
    let (dict, rest) = rest.split_first_chunk::<4>()?;
    if flags & !END_BYTE != 0 {
        return None;
    }
    let mut at = 0;
    let n = varint::get(rest, &mut at).ok()?;
    let mut chunks = Vec::new();
    let mut sum = 0usize;
    for i in 0..n {
        let &control = rest.get(at)?;
        at += 1;
        let len = usize::try_from(varint::get(rest, &mut at).ok()?).ok()?.checked_add(1)?;
        let props = match control {
            0xC0.. => {
                let p = Props::from_byte(*rest.get(at)?).filter(|p| p.lc + p.lp <= 4)?;
                at += 1;
                Some(p)
            }
            _ => None,
        };
        let c = Chunk { control, len, props };
        let max = if c.stored() { CHUNK_CODED } else { CHUNK_DATA };
        let valid = matches!(control, 1 | 2 | 0x80 | 0xA0 | 0xC0 | 0xE0) && (i == 0) == c.resets_dict();
        if !valid || len > max {
            return None;
        }
        sum = sum.checked_add(len)?;
        chunks.push(c);
    }
    if sum != data.len() {
        return None;
    }
    let mut w = Writer::new(&chunks, data.len() / 4 + 16);
    let mut ok = true;
    recons::reconstruct(data, &rest[at..], u32::from_le_bytes(*dict), &breaks(&chunks), |pk| {
        ok = ok && w.push(data, pk).is_some();
    })?;
    let mut out = w.finish(data).filter(|_| ok)?;
    if flags & END_BYTE != 0 {
        out.push(0);
    }
    Some(out)
}
