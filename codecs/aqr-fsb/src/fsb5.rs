//! The layout of an FSB5 sound bank: a header, the samples' headers, a table of names, then the
//! samples' data one after another (FORMAT.md §7).

use std::ops::Range;

/// The codec of a bank's samples (its `mode`), of those told apart here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    Pcm16,
    ImaAdpcm,
    Mpeg,
    Vorbis,
    Other(u32),
}

impl Codec {
    fn of(mode: u32) -> Codec {
        match mode {
            2 => Codec::Pcm16,
            7 => Codec::ImaAdpcm,
            11 => Codec::Mpeg,
            15 => Codec::Vorbis,
            m => Codec::Other(m),
        }
    }
}

/// A bank's layout. The bank may be followed by other bytes, which are not its.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub codec: Codec,
    /// The bytes before the samples' data: the header, the samples' headers and the names.
    pub head: usize,
    /// Each sample's data, from its offset to the next sample's (or to the end of the data), so
    /// with its padding; one after another from `head`.
    pub samples: Vec<Range<usize>>,
    /// What each sample's header tells.
    pub headers: Vec<Header>,
}

/// What a sample's header tells: its channels (those of the header's code, or of a channels
/// chunk) and its chunks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub channels: usize,
    /// Each chunk's type and its bytes (after its word) in the bank.
    pub chunks: Vec<(u32, Range<usize>)>,
}

impl Header {
    /// The bytes of the first chunk of type `kind`.
    pub fn chunk<'a>(&self, d: &'a [u8], kind: u32) -> Option<&'a [u8]> {
        self.chunks.iter().find(|c| c.0 == kind).map(|c| &d[c.1.clone()])
    }
}

/// The chunk types read here.
pub const CHUNK_CHANNELS: u32 = 1;
/// A Vorbis sample's: the CRC-32 of its setup header, then (optionally) a seek table.
pub const CHUNK_VORBIS: u32 = 11;

impl Layout {
    /// The layout of the bank at the start of `d`, or None where it is not one: the magic, a
    /// version of 0 or 1, at least one sample, the samples' headers (each with its chunks)
    /// taking exactly their declared size, and the samples' offsets in order within the data.
    pub fn of(d: &[u8]) -> Option<Layout> {
        let word = |p: usize| Some(u32::from_le_bytes(d.get(p..p + 4)?.try_into().ok()?));
        if d.get(..4)? != b"FSB5" {
            return None;
        }
        let version = word(4)?;
        let header = match version {
            0 => 0x40,
            1 => 0x3C,
            _ => return None,
        };
        let (n, headers, names, data, mode) =
            (word(8)? as usize, word(12)? as usize, word(16)? as usize, word(20)? as usize, word(24)?);
        let head = header + headers.checked_add(names)?;
        if n == 0 || head.checked_add(data)? > d.len() {
            return None;
        }
        let mut offsets = Vec::with_capacity(n.min(headers / 8));
        let mut infos = Vec::with_capacity(n.min(headers / 8));
        let mut p = header;
        for _ in 0..n {
            let h = u64::from_le_bytes(d.get(p..p + 8)?.try_into().ok()?);
            p += 8;
            offsets.push(((h >> 7) & ((1 << 27) - 1)) as usize * 32);
            let mut info = Header { channels: [1, 2, 6, 8][(h >> 5 & 3) as usize], chunks: Vec::new() };
            let mut more = h & 1 != 0;
            while more {
                let c = word(p)?;
                more = c & 1 != 0;
                let start = p + 4;
                p = p.checked_add(4 + ((c >> 1) & 0xFF_FFFF) as usize)?;
                if p > header + headers {
                    return None;
                }
                info.chunks.push((c >> 25, start..p));
                if c >> 25 == CHUNK_CHANNELS && p > start {
                    info.channels = d[start] as usize;
                }
            }
            infos.push(info);
        }
        if p != header + headers || offsets[0] != 0 || offsets.windows(2).any(|w| w[0] > w[1]) || offsets[n - 1] > data
        {
            return None;
        }
        let ends = offsets[1..].iter().copied().chain([data]);
        let samples = offsets.iter().zip(ends).map(|(&s, e)| head + s..head + e).collect();
        Some(Layout { codec: Codec::of(mode), head, samples, headers: infos })
    }

    /// The bank's bytes: to the end of its data.
    pub fn len(&self) -> usize {
        self.samples.last().map_or(self.head, |s| s.end)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Whether `d` starts with an FSB5 bank ([`Layout::of`]).
pub fn starts(d: &[u8]) -> bool {
    Layout::of(d).is_some()
}

/// The bytes of the bank at the start of `d`.
pub fn extent(d: &[u8]) -> Option<usize> {
    Layout::of(d).map(|l| l.len())
}

/// The CRC-32 and size of each sample's data of at least `min` bytes, for telling which banks
/// share samples.
pub fn sample_sums(d: &[u8], min: u64) -> Vec<(u32, u64)> {
    let Some(l) = Layout::of(d) else { return Vec::new() };
    l.samples
        .iter()
        .filter(|s| s.len() as u64 >= min)
        .map(|s| (aqr_common::crc32::hash(&d[s.clone()]), s.len() as u64))
        .collect()
}
