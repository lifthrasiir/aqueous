//! The 7z archive: a signature header, the packed streams, then the header, which may itself be
//! packed (an encoded header: the streams of one folder, whose output is the header). The
//! header tells the packed streams, the folders (chains of coders) taking them, the substreams
//! each folder's output is cut into, and the files, those with data taking the substreams in
//! order.

use std::ops::Range;

use aqr_common::crc32;

pub const MAGIC: [u8; 6] = [b'7', b'z', 0xbc, 0xaf, 0x27, 0x1c];
/// The bytes of the signature header.
pub const SIGNATURE: usize = 32;

/// Coder ids.
pub const COPY: &[u8] = &[0];
pub const LZMA: &[u8] = &[3, 1, 1];
pub const LZMA2: &[u8] = &[0x21];

mod id {
    pub const END: u64 = 0;
    pub const HEADER: u64 = 1;
    pub const ARCHIVE_PROPERTIES: u64 = 2;
    pub const ADDITIONAL_STREAMS: u64 = 3;
    pub const MAIN_STREAMS: u64 = 4;
    pub const FILES: u64 = 5;
    pub const PACK_INFO: u64 = 6;
    pub const UNPACK_INFO: u64 = 7;
    pub const SUBSTREAMS: u64 = 8;
    pub const SIZE: u64 = 9;
    pub const CRC: u64 = 10;
    pub const FOLDER: u64 = 11;
    pub const UNPACK_SIZE: u64 = 12;
    pub const NUM_UNPACK_STREAM: u64 = 13;
    pub const EMPTY_STREAM: u64 = 14;
    pub const NAME: u64 = 17;
    pub const ENCODED_HEADER: u64 = 23;
}

fn le32(d: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(d[at..at + 4].try_into().unwrap())
}

fn le64(d: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(d[at..at + 8].try_into().unwrap())
}

/// The span of the header that the signature header at the start of `d` tells, and its CRC-32.
/// None where `d` does not start with one (its magic, version 0.x and the CRC-32 of the rest).
pub fn signature(d: &[u8]) -> Option<(Range<u64>, u32)> {
    let h = d.get(..SIGNATURE)?;
    if h[..6] != MAGIC || h[6] != 0 || le32(h, 8) != crc32::hash(&h[12..32]) {
        return None;
    }
    let start = (SIGNATURE as u64).checked_add(le64(h, 12))?;
    Some((start..start.checked_add(le64(h, 20))?, le32(h, 28)))
}

/// A coder of a folder: its id, its streams (packed in, unpacked out) and its properties.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Coder {
    pub id: Vec<u8>,
    pub ins: u64,
    pub outs: u64,
    pub props: Vec<u8>,
}

/// A folder: its coders, its bind pairs (a coder's input from another's output), the indices of
/// its inputs that packed streams feed, the sizes of its coders' outputs, and the CRC-32 of its
/// output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub coders: Vec<Coder>,
    pub binds: Vec<(u64, u64)>,
    pub packed: Vec<u64>,
    pub sizes: Vec<u64>,
    pub crc: Option<u32>,
}

impl Folder {
    /// The size of its output: of the output no bind pair takes.
    pub fn size(&self) -> Option<u64> {
        let o = (0..self.sizes.len() as u64).find(|o| !self.binds.iter().any(|b| b.1 == *o))?;
        Some(self.sizes[o as usize])
    }

    /// The coder of a folder of one coder, of one packed stream.
    pub fn single(&self) -> Option<&Coder> {
        match &self.coders[..] {
            [c] if c.ins == 1 && c.outs == 1 => Some(c),
            _ => None,
        }
    }
}

/// Streams info: the packed streams (from `pack_pos`, after the signature header), the folders
/// taking them in order, and the substreams of each folder (their sizes) with their CRC-32s
/// (one after another, where known).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Streams {
    pub pack_pos: u64,
    pub pack_sizes: Vec<u64>,
    pub folders: Vec<Folder>,
    pub subs: Vec<Vec<u64>>,
    pub sub_crcs: Vec<Option<u32>>,
}

impl Streams {
    /// The span of the packed streams of each folder in the archive, its first and its count.
    pub fn folder_packs(&self) -> Option<Vec<Range<usize>>> {
        let mut at = 0;
        self.folders
            .iter()
            .map(|f| {
                let r = at..at + f.packed.len();
                at = r.end;
                (r.end <= self.pack_sizes.len()).then_some(r)
            })
            .collect()
    }

    /// The span of each packed stream in the archive.
    pub fn pack_spans(&self) -> Option<Vec<Range<u64>>> {
        let mut at = (SIGNATURE as u64).checked_add(self.pack_pos)?;
        self.pack_sizes
            .iter()
            .map(|&n| {
                let r = at..at.checked_add(n)?;
                at = r.end;
                Some(r)
            })
            .collect()
    }
}

/// A file: its name (UTF-8, from UTF-16) and whether it has data (a substream).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    pub name: Vec<u8>,
    pub stream: bool,
}

/// The header: the main streams and the files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Header {
    pub main: Streams,
    pub files: Vec<File>,
}

/// What the header bytes are: the header, or the streams of the folder whose output is it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Top {
    Plain(Header),
    Encoded(Streams),
}

struct Reader<'a> {
    d: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn byte(&mut self) -> Option<u8> {
        let b = *self.d.get(self.at)?;
        self.at += 1;
        Some(b)
    }

    fn bytes(&mut self, n: u64) -> Option<&[u8]> {
        let n = usize::try_from(n).ok()?;
        let r = self.d.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(r)
    }

    /// A NUMBER: the first byte's high 1 bits count the bytes after it (little-endian), the
    /// rest of it the high part.
    fn number(&mut self) -> Option<u64> {
        let first = self.byte()?;
        let mut v = 0u64;
        for i in 0..8 {
            let mask = 0x80u8 >> i;
            if first & mask == 0 {
                return Some(v | ((first & mask.wrapping_sub(1)) as u64) << (8 * i));
            }
            v |= (self.byte()? as u64) << (8 * i);
        }
        Some(v)
    }

    fn count(&mut self) -> Option<usize> {
        // (no more items than bytes left)
        usize::try_from(self.number()?).ok().filter(|&n| n <= self.d.len() - self.at)
    }

    /// `n` bits, highest first.
    fn bits(&mut self, n: usize) -> Option<Vec<bool>> {
        let b = self.bytes(n.div_ceil(8) as u64)?;
        Some((0..n).map(|i| b[i / 8] >> (7 - i % 8) & 1 != 0).collect())
    }

    fn digests(&mut self, n: usize) -> Option<Vec<Option<u32>>> {
        let all = self.byte()? != 0;
        let defined = if all { vec![true; n] } else { self.bits(n)? };
        defined
            .into_iter()
            .map(|d| Some(if d { Some(u32::from_le_bytes(self.bytes(4)?.try_into().ok()?)) } else { None }))
            .collect()
    }

    fn expect(&mut self, id: u64) -> Option<()> {
        (self.number()? == id).then_some(())
    }

    fn folder(&mut self) -> Option<Folder> {
        let n = self.count()?;
        let mut coders = Vec::with_capacity(n);
        for _ in 0..n {
            let flags = self.byte()?;
            if flags & 0xc0 != 0 {
                return None;
            }
            let id = self.bytes((flags & 15) as u64)?.to_vec();
            let (ins, outs) = if flags & 0x10 != 0 { (self.number()?, self.number()?) } else { (1, 1) };
            let props = if flags & 0x20 != 0 {
                let n = self.number()?;
                self.bytes(n)?.to_vec()
            } else {
                Vec::new()
            };
            coders.push(Coder { id, ins, outs, props });
        }
        let ins: u64 = coders.iter().try_fold(0u64, |a, c| a.checked_add(c.ins))?;
        let outs: u64 = coders.iter().try_fold(0u64, |a, c| a.checked_add(c.outs))?;
        if outs == 0 || outs > 64 || ins > 64 || ins < outs - 1 {
            return None;
        }
        let binds = (0..outs - 1).map(|_| Some((self.number()?, self.number()?))).collect::<Option<Vec<_>>>()?;
        let n_packed = ins - (outs - 1);
        let packed = if n_packed == 1 {
            let i = (0..ins).find(|i| !binds.iter().any(|b| b.0 == *i))?;
            vec![i]
        } else {
            (0..n_packed).map(|_| self.number()).collect::<Option<Vec<_>>>()?
        };
        Some(Folder { coders, binds, packed, sizes: Vec::new(), crc: None })
    }

    fn streams(&mut self) -> Option<Streams> {
        let mut s = Streams::default();
        loop {
            match self.number()? {
                id::END => {
                    // (without substreams info, a substream for each folder)
                    if s.subs.is_empty() {
                        s.subs = s.folders.iter().map(|f| f.size().map(|n| vec![n])).collect::<Option<_>>()?;
                        s.sub_crcs = s.folders.iter().map(|f| f.crc).collect();
                    }
                    return Some(s);
                }
                id::PACK_INFO => {
                    s.pack_pos = self.number()?;
                    let n = self.count()?;
                    loop {
                        match self.number()? {
                            id::END => break,
                            id::SIZE => s.pack_sizes = (0..n).map(|_| self.number()).collect::<Option<_>>()?,
                            id::CRC => drop(self.digests(n)?),
                            _ => return None,
                        }
                    }
                    if s.pack_sizes.len() != n {
                        return None;
                    }
                }
                id::UNPACK_INFO => {
                    self.expect(id::FOLDER)?;
                    let n = self.count()?;
                    if self.byte()? != 0 {
                        return None;
                    }
                    s.folders = (0..n).map(|_| self.folder()).collect::<Option<_>>()?;
                    self.expect(id::UNPACK_SIZE)?;
                    for f in &mut s.folders {
                        let outs = f.coders.iter().map(|c| c.outs).sum::<u64>();
                        f.sizes = (0..outs).map(|_| self.number()).collect::<Option<_>>()?;
                    }
                    loop {
                        match self.number()? {
                            id::END => break,
                            id::CRC => {
                                let crcs = self.digests(n)?;
                                s.folders.iter_mut().zip(crcs).for_each(|(f, c)| f.crc = c);
                            }
                            _ => return None,
                        }
                    }
                }
                id::SUBSTREAMS => self.substreams(&mut s)?,
                _ => return None,
            }
        }
    }

    fn substreams(&mut self, s: &mut Streams) -> Option<()> {
        let mut counts = vec![1usize; s.folders.len()];
        let mut t = self.number()?;
        if t == id::NUM_UNPACK_STREAM {
            for c in &mut counts {
                *c = self.count()?;
            }
            t = self.number()?;
        }
        let sized = t == id::SIZE;
        s.subs = Vec::with_capacity(s.folders.len());
        for (f, &n) in s.folders.iter().zip(&counts) {
            if n == 0 {
                s.subs.push(Vec::new());
                continue;
            }
            let total = f.size()?;
            let mut sizes = Vec::with_capacity(n);
            if sized {
                for _ in 1..n {
                    sizes.push(self.number()?);
                }
            } else if n > 1 {
                return None;
            }
            let sum = sizes.iter().try_fold(0u64, |a, &x| a.checked_add(x))?;
            sizes.push(total.checked_sub(sum)?);
            s.subs.push(sizes);
        }
        if sized {
            t = self.number()?;
        }
        // (the CRC-32 of a folder of one substream is the folder's, where given)
        let unknown = |f: &Folder, n: usize| if n == 1 && f.crc.is_some() { 0 } else { n };
        let mut crcs = Vec::new();
        loop {
            match t {
                id::END => break,
                id::CRC => {
                    let n = s.folders.iter().zip(&counts).map(|(f, &n)| unknown(f, n)).sum();
                    crcs = self.digests(n)?;
                }
                _ => return None,
            }
            t = self.number()?;
        }
        let mut crcs = crcs.into_iter();
        s.sub_crcs = Vec::new();
        for (f, &n) in s.folders.iter().zip(&counts) {
            if unknown(f, n) == 0 {
                s.sub_crcs.push(f.crc);
            } else {
                (0..n).for_each(|_| s.sub_crcs.push(crcs.next().flatten()));
            }
        }
        Some(())
    }

    fn files(&mut self) -> Option<Vec<File>> {
        let n = self.count()?;
        let mut empty = vec![false; n];
        let mut names: Vec<Vec<u8>> = vec![Vec::new(); n];
        loop {
            let t = self.number()?;
            if t == id::END {
                break;
            }
            let size = self.number()?;
            let mut r = Reader { d: self.bytes(size)?, at: 0 };
            match t {
                id::EMPTY_STREAM => empty = r.bits(n)?,
                id::NAME => {
                    if r.byte()? != 0 {
                        return None;
                    }
                    let units: Vec<u16> = r.d[1..].as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c)).collect();
                    let mut it = units.split(|&u| u == 0);
                    for name in &mut names {
                        *name = String::from_utf16_lossy(it.next()?).into_bytes();
                    }
                }
                _ => {}
            }
        }
        Some(names.into_iter().zip(empty).map(|(name, e)| File { name, stream: !e }).collect())
    }

    fn header(&mut self) -> Option<Header> {
        let mut h = Header::default();
        loop {
            match self.number()? {
                id::END => return Some(h),
                id::ARCHIVE_PROPERTIES => loop {
                    if self.number()? == id::END {
                        break;
                    }
                    let n = self.number()?;
                    self.bytes(n)?;
                },
                id::ADDITIONAL_STREAMS => drop(self.streams()?),
                id::MAIN_STREAMS => h.main = self.streams()?,
                id::FILES => h.files = self.files()?,
                _ => return None,
            }
        }
    }
}

/// The header bytes `d`: the header, or the streams of an encoded header. None where they do
/// not parse, or the files with data are not as many as the substreams.
pub fn parse(d: &[u8]) -> Option<Top> {
    let mut r = Reader { d, at: 0 };
    match r.number()? {
        id::HEADER => {
            let h = r.header()?;
            let subs: usize = h.main.subs.iter().map(Vec::len).sum();
            (h.files.iter().filter(|f| f.stream).count() == subs && h.main.sub_crcs.len() == subs)
                .then_some(Top::Plain(h))
        }
        id::ENCODED_HEADER => Some(Top::Encoded(r.streams()?)),
        _ => None,
    }
}
