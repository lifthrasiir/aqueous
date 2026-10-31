//! The module's two inputs (FORMAT.md §3): the packets of the streams, each framed in chunks
//! ([`frame`], [`Packets`]), and the layout record of the pages ([`Layout`]); and the walk over
//! the pages that the encoder and the decoder both make ([`Walk`], §5), which gives each page
//! its lacing values and body from its stream's packets, and what its header type, sequence
//! number and granule position are predicted to be.

use aqr_common::varint;

use crate::page::{BOS, CONTINUED, EOS, HEADER};
use crate::{Result, corrupt};

/// The bytes of a whole chunk of a packet: a chunk of fewer ends it.
pub const CHUNK: usize = 0xffff;

/// The granule position of a page on which no packet ends.
pub const NONE: u64 = u64::MAX;

/// Puts packet `p` framed: chunks of [`CHUNK`] bytes each after the 16-bit length 0xffff while
/// that many are left, then the rest (fewer, perhaps none) after its length.
pub fn frame(out: &mut Vec<u8>, p: &[u8]) {
    let (chunks, rest) = p.as_chunks::<CHUNK>();
    for c in chunks {
        out.extend_from_slice(&(CHUNK as u16).to_le_bytes());
        out.extend_from_slice(c);
    }
    out.extend_from_slice(&(rest.len() as u16).to_le_bytes());
    out.extend_from_slice(rest);
}

/// A stream's packets: their bytes one after another, and where each ends.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Packets {
    pub bytes: Vec<u8>,
    pub ends: Vec<usize>,
}

impl Packets {
    /// The packets framed in `d` ([`frame`]), all of it.
    pub fn unframe(d: &[u8]) -> Result<Packets> {
        let mut p = Packets { bytes: Vec::with_capacity(d.len()), ends: Vec::new() };
        let mut at = 0;
        while at < d.len() {
            let n = u16::from_le_bytes(d.get(at..at + 2).ok_or_else(corrupt)?.try_into().unwrap()) as usize;
            p.bytes.extend_from_slice(d.get(at + 2..at + 2 + n).ok_or_else(corrupt)?);
            at += 2 + n;
            if n < CHUNK {
                p.ends.push(p.bytes.len());
            }
        }
        if p.ends.last() != Some(&p.bytes.len()) && !p.bytes.is_empty() {
            return Err(corrupt());
        }
        Ok(p)
    }

    pub fn push(&mut self, p: &[u8]) {
        self.bytes.extend_from_slice(p);
        self.ends.push(self.bytes.len());
    }

    pub fn len(&self) -> usize {
        self.ends.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ends.is_empty()
    }

    pub fn get(&self, i: usize) -> &[u8] {
        let start = if i == 0 { 0 } else { self.ends[i - 1] };
        &self.bytes[start..self.ends[i]]
    }

    /// The packets framed, one after another.
    pub fn framed(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.bytes.len() + 2 * self.len());
        (0..self.len()).for_each(|i| frame(&mut out, self.get(i)));
        out
    }

    /// The lacing values of the packets: of a packet of `n` bytes, `n / 255` values 255, then
    /// `n mod 255`, but for the last packet of a stream left `open` (unended), of a multiple of
    /// 255 bytes, more than none.
    pub fn lacing(&self, open: bool) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(self.bytes.len() / 255 + self.len());
        for i in 0..self.len() {
            let n = self.get(i).len();
            out.resize(out.len() + n / 255, 255);
            if !(open && i + 1 == self.len()) {
                out.push((n % 255) as u8);
            } else if n == 0 || !n.is_multiple_of(255) {
                return Err(corrupt());
            }
        }
        if open && self.is_empty() {
            return Err(corrupt());
        }
        Ok(out)
    }
}

/// How a stream's granule positions are predicted: by the samples of its packets (§5.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Predictor {
    /// None: each page with a packet ending on it as the one before.
    None,
    /// Vorbis I, of the block flags of the setup header's modes (1 to 64).
    Vorbis(Vec<bool>),
    /// Opus (RFC 7845).
    Opus,
}

impl Predictor {
    /// The samples each packet of `p` ends, as the predictor counts them.
    pub fn durations(&self, p: &Packets) -> Result<Vec<u64>> {
        let mut out = vec![0; p.len()];
        match self {
            Predictor::None => {}
            Predictor::Vorbis(flags) => {
                let id = if p.is_empty() { &[][..] } else { p.get(0) };
                if id.len() < 30 || &id[..7] != b"\x01vorbis" {
                    return Err(corrupt());
                }
                let (short, long) = (id[28] & 15, id[28] >> 4);
                if !(6..=13).contains(&short) || !(6..=13).contains(&long) || short > long {
                    return Err(corrupt());
                }
                let sizes = [1u64 << short, 1u64 << long];
                let bits = u32::BITS - (flags.len() as u32 - 1).leading_zeros();
                let mut prev = None;
                for (i, d) in out.iter_mut().enumerate() {
                    let Some(&b) = p.get(i).first() else { continue };
                    let mode = (b >> 1) as usize & ((1 << bits) - 1);
                    if b & 1 != 0 || mode >= flags.len() {
                        continue;
                    }
                    let size = sizes[flags[mode] as usize];
                    *d = prev.map_or(0, |prev| prev / 4 + size / 4);
                    prev = Some(size);
                }
            }
            Predictor::Opus => {
                for (i, d) in out.iter_mut().enumerate().skip(2) {
                    let q = p.get(i);
                    let Some(&toc) = q.first() else { continue };
                    let config = toc as u64 >> 3;
                    let size = match config {
                        0..12 => [480, 960, 1920, 2880][config as usize % 4],
                        12..16 => [480, 960][config as usize % 2],
                        _ => [120, 240, 480, 960][config as usize % 4],
                    };
                    let frames = match toc & 3 {
                        0 => 1,
                        1 | 2 => 2,
                        _ => q.get(1).map_or(0, |&c| c as u64 & 63),
                    };
                    *d = frames * size;
                }
            }
        }
        Ok(out)
    }
}

/// A logical stream of the layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stream {
    pub serial: u32,
    /// The bytes of its packets framed, in the packets input.
    pub bytes: u64,
    /// Whether its last packet is not ended (its last lacing value 255).
    pub open: bool,
    pub predictor: Predictor,
}

/// A page of the layout: its stream and lacing values' count, and its header's fields as they
/// differ from the walk's prediction: the header type XOR it, the sequence number and granule
/// position less it (wrapping).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Page {
    pub stream: usize,
    pub segments: u8,
    pub kind: u8,
    pub seq: u32,
    pub granule: u64,
}

/// The layout record (FORMAT.md §4).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Layout {
    pub streams: Vec<Stream>,
    pub pages: Vec<Page>,
}

impl Layout {
    pub fn write(&self) -> Vec<u8> {
        let mut out = Vec::new();
        varint::put(&mut out, self.streams.len() as u64);
        for s in &self.streams {
            out.extend_from_slice(&s.serial.to_le_bytes());
            varint::put(&mut out, s.bytes);
            let kind = match s.predictor {
                Predictor::None => 0,
                Predictor::Vorbis(_) => 1,
                Predictor::Opus => 2,
            };
            out.push(s.open as u8 | kind << 1);
            if let Predictor::Vorbis(flags) = &s.predictor {
                varint::put(&mut out, flags.len() as u64);
                for c in flags.chunks(8) {
                    out.push(c.iter().enumerate().fold(0, |a, (i, &f)| a | (f as u8) << i));
                }
            }
        }
        varint::put(&mut out, self.pages.len() as u64);
        self.pages.iter().for_each(|p| varint::put(&mut out, p.stream as u64));
        out.extend(self.pages.iter().map(|p| p.segments));
        out.extend(self.pages.iter().map(|p| p.kind));
        self.pages.iter().for_each(|p| varint::put(&mut out, varint::zz(p.seq as i32 as i64)));
        self.pages.iter().for_each(|p| varint::put(&mut out, varint::zz(p.granule as i64)));
        out
    }

    /// The layout of `d`, of a file of `len` bytes.
    pub fn read(d: &[u8], len: u64) -> Result<Layout> {
        let mut at = 0;
        let get = |at: &mut usize| varint::get(d, at).map_err(|_| corrupt());
        let byte = |at: &mut usize| {
            let b = *d.get(*at).ok_or_else(corrupt)?;
            *at += 1;
            Ok(b)
        };
        let most = len / HEADER as u64;
        let n = get(&mut at)?;
        if n == 0 || n > most {
            return Err(corrupt());
        }
        let mut streams = Vec::with_capacity(n as usize);
        for _ in 0..n {
            let serial = u32::from_le_bytes(d.get(at..at + 4).ok_or_else(corrupt)?.try_into().unwrap());
            at += 4;
            let bytes = get(&mut at)?;
            let flags = byte(&mut at)?;
            let predictor = match flags >> 1 {
                0 => Predictor::None,
                1 => {
                    let m = get(&mut at)? as usize;
                    if !(1..=64).contains(&m) {
                        return Err(corrupt());
                    }
                    let mut f = Vec::with_capacity(m);
                    for i in 0..m.div_ceil(8) {
                        let b = byte(&mut at)?;
                        let k = (m - 8 * i).min(8);
                        if k < 8 && b >> k != 0 {
                            return Err(corrupt());
                        }
                        f.extend((0..k).map(|j| b >> j & 1 != 0));
                    }
                    Predictor::Vorbis(f)
                }
                2 => Predictor::Opus,
                _ => return Err(corrupt()),
            };
            streams.push(Stream { serial, bytes, open: flags & 1 != 0, predictor });
        }
        let n = get(&mut at)?;
        if n > most {
            return Err(corrupt());
        }
        let n = n as usize;
        let mut pages = vec![Page { stream: 0, segments: 0, kind: 0, seq: 0, granule: 0 }; n];
        for p in &mut pages {
            p.stream = get(&mut at)? as usize;
            if p.stream >= streams.len() {
                return Err(corrupt());
            }
        }
        let column = d.get(at..at + 2 * n).ok_or_else(corrupt)?;
        for (i, p) in pages.iter_mut().enumerate() {
            (p.segments, p.kind) = (column[i], column[n + i]);
        }
        at += 2 * n;
        for p in &mut pages {
            let s = varint::unzz(get(&mut at)?);
            p.seq = i32::try_from(s).map_err(|_| corrupt())? as u32;
        }
        for p in &mut pages {
            p.granule = varint::unzz(get(&mut at)?) as u64;
        }
        if at != d.len() {
            return Err(corrupt());
        }
        Ok(Layout { streams, pages })
    }
}

/// A stream as the walk takes its pages.
struct Walked<'a> {
    packets: &'a Packets,
    lacing: &'a [u8],
    durations: &'a [u64],
    /// The lacing values, bytes and packets taken so far.
    lace: usize,
    byte: usize,
    packet: usize,
    /// The granule position predicted, at the end of the last packet ended.
    pos: u64,
    /// The sequence number of its last page, if any.
    seq: Option<u32>,
    /// Whether its last page ended in a packet.
    within: bool,
    /// Its pages not taken.
    left: usize,
}

/// A page as the walk predicts it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub kind: u8,
    pub seq: u32,
    pub granule: u64,
    pub lacing: std::ops::Range<usize>,
    pub body: std::ops::Range<usize>,
    /// Whether a packet ends on it.
    ends: bool,
}

/// The walk over the pages (FORMAT.md §5): each page in turn takes its lacing values and body
/// from its stream, and is predicted from the pages of the stream before it.
pub struct Walk<'a> {
    streams: Vec<Walked<'a>>,
}

impl<'a> Walk<'a> {
    /// The walk of streams of `packets`, their `lacing` and `durations`, of the pages of
    /// `streams` (each page's stream).
    pub fn new(
        packets: &'a [Packets],
        lacing: &'a [Vec<u8>],
        durations: &'a [Vec<u64>],
        streams: &[usize],
    ) -> Walk<'a> {
        let mut w = Walk {
            streams: (0..packets.len())
                .map(|i| Walked {
                    packets: &packets[i],
                    lacing: &lacing[i],
                    durations: &durations[i],
                    lace: 0,
                    byte: 0,
                    packet: 0,
                    pos: 0,
                    seq: None,
                    within: false,
                    left: 0,
                })
                .collect(),
        };
        streams.iter().for_each(|&s| w.streams[s].left += 1);
        w
    }

    /// The next page, of stream `s` and `n` lacing values, as predicted.
    pub fn page(&mut self, s: usize, n: usize) -> Result<Step> {
        let w = &mut self.streams[s];
        let lacing = w.lace..w.lace + n;
        let values = w.lacing.get(lacing.clone()).ok_or_else(corrupt)?;
        let body = w.byte..w.byte + values.iter().map(|&v| v as usize).sum::<usize>();
        let first = w.seq.is_none();
        let mut ends = false;
        for _ in values.iter().filter(|&&v| v < 255) {
            w.pos = w.pos.wrapping_add(w.durations[w.packet]);
            w.packet += 1;
            ends = true;
        }
        w.left -= 1;
        let kind = (w.within as u8 * CONTINUED) | (first as u8 * BOS) | ((w.left == 0) as u8 * EOS);
        let seq = w.seq.map_or(0, |s| s.wrapping_add(1));
        let granule = if ends { w.pos } else { NONE };
        if let Some(&v) = values.last() {
            w.within = v == 255;
        }
        (w.lace, w.byte) = (lacing.end, body.end);
        Ok(Step { kind, seq, granule, lacing, body, ends })
    }

    /// Takes the page `step` of stream `s` to have sequence number `seq` and granule position
    /// `granule`.
    pub fn settle(&mut self, s: usize, step: &Step, seq: u32, granule: u64) {
        let w = &mut self.streams[s];
        w.seq = Some(seq);
        if step.ends && granule != NONE {
            w.pos = granule;
        }
    }

    /// The bytes of the lacing values and body of `step` of stream `s`.
    pub fn parts(&self, s: usize, step: &Step) -> (&'a [u8], &'a [u8]) {
        let w = &self.streams[s];
        (&w.lacing[step.lacing.clone()], &w.packets.bytes[step.body.clone()])
    }

    /// Whether every stream's lacing values are all taken.
    pub fn done(&self) -> bool {
        self.streams.iter().all(|w| w.lace == w.lacing.len())
    }
}
