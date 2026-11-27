//! The files the installer carries, recompressed as Aqueous would (aqr-exe's filters), though
//! without Aqueous: the code sections of a PE image of x86 or x86-64 go through the x86
//! disfilter (`exe-recons/x86`), its base relocations through `exe-recons/pe-reloc`. Their
//! streams of a kind, of every file, are one raw LZMA2 frame (solid), as is the rest of the
//! files that are images, and that of the others.
//!
//! ```text
//! payload  = "AQRWINS3" u8:count file* frame[FRAMES]
//! file     = u8:n name[n] (UTF-8) le64:size u8:count region* le32:rest u8:frame
//! region   = u8:kind le64:offset le64:len params le32:length*
//!            kind 1, x86: params le64:origin u8:sentinel u8:is64; 14 lengths (x86::STREAM_NAMES)
//!            kind 2, base relocations: params u8:align; 2 lengths (reloc::STREAM_NAMES)
//! frame    = le32:raw le32:n lzma2[n]
//! ```
//!
//! The frames are of the x86 streams (in their order), the two of the base relocations, the
//! rest of the images and the rest of the others ([`FRAMES`]); each is the pieces of its kind
//! one after another, in the order of the files and of their regions, which take theirs from
//! it by the lengths they tell. The regions are in the order of their offsets, apart, within
//! the file; its `rest` (of `rest` bytes, from frame `frame`: [`REST`] or [`OTHER`]) is the
//! file without them. An LZMA2 frame's dictionary is [`dict`] of its length.

use aqr_exe::arch::x86;
use aqr_exe::reloc;

pub const MAGIC: &[u8; 8] = b"AQRWINS3";

pub const X86: u8 = 1;
pub const RELOC: u8 = 2;

/// The frame of the base relocations' first stream (the x86 streams are those before it).
pub const RELOC_FRAME: usize = x86::NUM_STREAMS;
/// The frame of the rest of the files that are images.
pub const REST: u8 = (RELOC_FRAME + 2) as u8;
/// The frame of the rest of the others (all of them).
pub const OTHER: u8 = REST + 1;
/// The number of frames.
pub const FRAMES: usize = OTHER as usize + 1;

/// The dictionary of a frame of `raw` bytes: as large as the frame (a power of two).
pub fn dict(raw: usize) -> u32 {
    raw.max(1).next_power_of_two().clamp(1 << 12, 1 << 30) as u32
}

/// A file of the payload.
pub struct File {
    pub name: String,
    pub data: Vec<u8>,
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if self.0.len() < n {
            return Err("the payload is cut short".into());
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<usize, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()) as usize)
    }

    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn usize(&mut self) -> Result<usize, String> {
        usize::try_from(self.u64()?).map_err(|_| "too large".into())
    }
}

/// The `raw` bytes of raw LZMA2 frame `z`.
fn unlzma2(z: &[u8], raw: usize) -> Result<Vec<u8>, String> {
    use liblzma::stream::{Action, Filters, LzmaOptions, Status, Stream};
    let e = |e: liblzma::stream::Error| format!("LZMA2: {e}");
    let mut opts = LzmaOptions::new_preset(0).map_err(e)?;
    opts.dict_size(dict(raw));
    let mut f = Filters::new();
    f.lzma2(&opts);
    let mut s = Stream::new_raw_decoder(&f).map_err(e)?;
    // (a byte more, so that a frame of more is told)
    let mut out = Vec::with_capacity(raw + 1);
    loop {
        let (at, had) = (s.total_in() as usize, out.len());
        match s.process_vec(&z[at..], &mut out, Action::Finish).map_err(e)? {
            Status::StreamEnd if out.len() == raw => return Ok(out),
            Status::StreamEnd => return Err("a frame of another length".into()),
            _ if out.len() > raw || (s.total_in() as usize == at && out.len() == had) => {
                return Err("a frame of another length".into());
            }
            _ => {}
        }
    }
}

/// What a region tells before its frames' pieces.
enum Head {
    X86 { origin: u64, sentinel: u8, is64: bool },
    Reloc { align: u8 },
}

struct Region {
    off: usize,
    len: usize,
    head: Head,
    lengths: Vec<usize>,
}

/// The frames, and where each is read to.
struct Pieces<'a>(&'a [Vec<u8>], &'a mut [usize; FRAMES]);

impl<'a> Pieces<'a> {
    /// The next `n` bytes of frame `frame`.
    fn take(&mut self, frame: usize, n: usize) -> Result<&'a [u8], String> {
        let at = self.1[frame];
        let p = self.0[frame].get(at..at.saturating_add(n)).ok_or("a frame cut short")?;
        self.1[frame] += n;
        Ok(p)
    }
}

/// The files of `payload`.
pub fn unpack(payload: &[u8]) -> Result<Vec<File>, String> {
    let mut r = Reader(payload);
    if r.take(8)? != MAGIC {
        return Err("not a payload of this installer".into());
    }
    let count = r.u8()?;
    let mut heads = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let n = r.u8()? as usize;
        let name = String::from_utf8(r.take(n)?.to_vec()).map_err(|_| "a name not of UTF-8")?;
        let size = r.usize()?;
        let mut regions = Vec::new();
        for _ in 0..r.u8()? {
            let (kind, off, len) = (r.u8()?, r.usize()?, r.usize()?);
            let (head, n) = match kind {
                X86 => (Head::X86 { origin: r.u64()?, sentinel: r.u8()?, is64: r.u8()? != 0 }, x86::NUM_STREAMS),
                RELOC => (Head::Reloc { align: r.u8()? }, 2),
                _ => return Err(format!("a region of kind {kind}")),
            };
            let lengths = (0..n).map(|_| r.u32()).collect::<Result<_, _>>()?;
            regions.push(Region { off, len, head, lengths });
        }
        let (rest, frame) = (r.u32()?, r.u8()?);
        if frame != REST && frame != OTHER {
            return Err(format!("{name}: its rest of frame {frame}"));
        }
        heads.push((name, size, regions, rest, frame as usize));
    }
    let frames: Vec<(usize, &[u8])> = (0..FRAMES)
        .map(|_| {
            let raw = r.u32()?;
            let n = r.u32()?;
            Ok((raw, r.take(n)?))
        })
        .collect::<Result<_, String>>()?;
    // (each frame on a thread of its own)
    let frames: Vec<Vec<u8>> = std::thread::scope(|s| {
        let threads: Vec<_> = frames.iter().map(|&(raw, z)| s.spawn(move || unlzma2(z, raw))).collect();
        threads.into_iter().map(|t| t.join().unwrap()).collect::<Result<_, _>>()
    })?;
    let mut at = [0; FRAMES];
    let mut piece = Pieces(&frames, &mut at);
    let mut piece = |frame: usize, n: usize| piece.take(frame, n);
    let mut files = Vec::with_capacity(heads.len());
    for (name, size, regions, rest, frame) in heads {
        let mut decoded = Vec::with_capacity(regions.len());
        for g in &regions {
            let data = match g.head {
                Head::X86 { origin, sentinel, is64 } => {
                    let mut streams = [&[][..]; x86::NUM_STREAMS];
                    for (i, (s, &n)) in streams.iter_mut().zip(&g.lengths).enumerate() {
                        *s = piece(i, n)?;
                    }
                    let st = x86::DecodedStreams::new(origin, sentinel, streams);
                    x86::decode_with(&st, is64, &x86::Opcodes::BUILTIN, g.len)
                }
                Head::Reloc { align } => {
                    let rvas = piece(RELOC_FRAME, g.lengths[0])?;
                    let types = piece(RELOC_FRAME + 1, g.lengths[1])?;
                    reloc::decode(rvas, types, align, g.len)
                }
            };
            match data {
                Some(d) if d.len() == g.len => decoded.push((g.off, d)),
                _ => return Err(format!("{name}: a region at {:#x} not decoded", g.off)),
            }
        }
        let rest = piece(frame, rest)?;
        let mut data = Vec::with_capacity(size);
        let mut at = 0;
        for (off, d) in decoded {
            let Some(gap) = off.checked_sub(data.len()).filter(|&g| at + g <= rest.len()) else {
                return Err(format!("{name}: regions out of order"));
            };
            data.extend_from_slice(&rest[at..at + gap]);
            at += gap;
            data.extend_from_slice(&d);
        }
        data.extend_from_slice(&rest[at..]);
        if data.len() != size {
            return Err(format!("{name}: of {} bytes, not {size}", data.len()));
        }
        files.push(File { name, data });
    }
    if at.iter().zip(&frames).any(|(&a, f)| a != f.len()) {
        return Err("a frame of more than its pieces".into());
    }
    Ok(files)
}
