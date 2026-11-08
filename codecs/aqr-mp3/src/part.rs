//! The module as a part module (FORMAT.md, "Part module"): the same decoding as
//! [`crate::codec::decode_parts`], with the ten inputs read front to back through windows and the
//! output put in order, so that what it holds does not follow the lengths of either.
//!
//! - The file structure (SEGS, HDR) is read as the frames come, one at a time. A frame waits in a
//!   queue ([`Out`]) from its side information being complete until its payload is final in the
//!   reservoir's window, which is no later than its successors' main data reaching back for it:
//!   [`RING`] frames at most.
//! - The gaps are copied ahead of the frames: the bits of the reservoir before the next frame's
//!   payload less [`crate::codec::MAX_MDB`] bytes are gap whatever it does ([`crate::codec::gap_before`]), so
//!   that the window does not hold a long run of frames whose payloads are all gap.
//! - The bit strings (TAIL, RAWGR, GAPS, the raw bits) are slides ([`Slide`]) that hold, before
//!   each copy or frame, at least the most that it reads; the rANS words are a window of the
//!   decoder itself ([`rans::Decoder::streaming`]) that holds at least the symbols of a frame
//!   ahead (a symbol takes one word at most); the side information's range decoder and the other
//!   byte streams are read byte by byte.

use std::collections::VecDeque;

use aqr_api::stream::{Pull, Put, Reader};
use aqr_raw::audio::slide::{Slide, Words};

use crate::bits::{BitReader, BitWriter, FastWriter};
use crate::codec::{
    BitIn, DISCARD_MIN, Dec, Error, InlineSide, N_BYTE_STREAMS, Pass, RUN_FRAMES, SIDE_ROOM, WINDOW, copy_gap,
    gap_before,
};
use crate::frame::{Header, SideInfo, crc16};
use crate::huff::BitBuf;
use crate::model::{MAX_GRANULE_RAW_BITS, MAX_GRANULE_SYMS};
use crate::module::SLOTS;
use crate::rans::{self, Tables};
use crate::rc::{self, Bounded};
use crate::side::SideModel;

/// Bytes of a byte stream read at a time, and of the pieces of bit strings copied at a time.
pub(crate) const PIECE: usize = 4096;

/// Frames waiting for their payloads at most: a frame's main data reaches back [`MAX_MDB`] bytes
/// at most, and the frames whose payloads are in them are 4 bytes or more each (the longest run
/// of frames in 511 bytes is 170 of MPEG-2.5's, 8 kbps stereo); the others are a corrupt file.
pub(crate) const RING: usize = 1024;

/// Bytes of the output staged before they are put.
pub(crate) const STAGE: usize = 16 << 10;

/// Symbols a frame decodes at most (its granules', and their scalefactors'), and the words of
/// the rANS stream that the decoder holds ahead of a frame (and the states of a new run).
pub(crate) const FRAME_SYMS: usize = 4 * (MAX_GRANULE_SYMS + 64);
pub(crate) const WORDS_AHEAD: usize = FRAME_SYMS + 2 * 8;
/// Words that the decoder holds at most.
pub(crate) const WORDS_WINDOW: usize = 2 * WORDS_AHEAD;

/// Bytes of the raw bits that a frame reads at most, and the 8 that the decoder loads ahead.
pub(crate) const RAW_AHEAD: usize = 4 * MAX_GRANULE_RAW_BITS.div_ceil(8) + 16;

/// A bit string read through a slide ([`BitIn`]), `len` bytes long.
struct SlideBits<'a> {
    s: Slide<'a>,
    len: u64,
}

impl<'a> SlideBits<'a> {
    fn new(src: &'a mut dyn Pull, len: u64) -> Self {
        SlideBits { s: Slide::new(src, PIECE + 2), len }
    }
}

impl BitIn for SlideBits<'_> {
    fn copy_to(&mut self, w: &mut FastWriter, mut n: usize) -> Result<(), Error> {
        while n > 0 {
            let k = n.min(8 * PIECE);
            self.s.need(k.div_ceil(8) + 1);
            // (the string ends before: no bits past its end are copied)
            if (self.s.data().len() * 8) < k + self.s.bit() {
                return Err(Error("bit string exhausted"));
            }
            let mut r = BitReader::new(self.s.padded(), self.s.bit());
            w.copy(&mut r, k);
            self.s.advance_bits(r.pos);
            n -= k;
        }
        debug_assert!(self.s.taken() <= self.len);
        Ok(())
    }
}

/// A frame as the main pass completes it, and the junk before it.
struct Rec {
    h: Header,
    si: SideInfo,
    /// Offset and length of its payload in the reservoir.
    pay: usize,
    len: usize,
    /// Bytes of junk before it.
    junk: u64,
}

/// The size of [`Rec`] on WebAssembly (checked there).
pub(crate) const REC: u64 = 120;
#[cfg(target_family = "wasm")]
const _: () = assert!(size_of::<Rec>() == REC as usize);

/// The next frame of the file structure.
struct Next {
    h: Header,
    pay: usize,
    len: usize,
    junk: u64,
}

/// The file structure read as the frames come ([`crate::codec::parse_frames`] one frame at a time).
struct Walk<'a> {
    segs: Reader<'a>,
    hdr: Reader<'a>,
    /// The bytes of junk read and not yet given to a frame; at the end, the junk after the last.
    junk: u64,
    /// Frames left in the current run and its free format length.
    left: u64,
    free: usize,
    prev_h: u32,
    /// The length of the reservoir so far.
    r_len: usize,
    ended: bool,
    /// The length of the file.
    size: u64,
}

impl Walk<'_> {
    fn next(&mut self) -> Result<Option<Next>, Error> {
        while self.left == 0 {
            if self.ended {
                return Ok(None);
            }
            let j = self.segs.num().map_err(|_| Error("segments"))?;
            self.junk = self.junk.saturating_add(j);
            if self.segs.done() {
                self.ended = true;
                return Ok(None);
            }
            self.left = self.segs.num().map_err(|_| Error("segments"))?;
            let free = self.segs.num().map_err(|_| Error("segments"))?;
            if free > 1 << 16 {
                return Err(Error("free format length"));
            }
            self.free = free as usize;
        }
        self.left -= 1;
        let mut x = [0; 4];
        self.hdr.exact(&mut x).ok_or(Error("headers"))?;
        let h = Header(u32::from_be_bytes(x) ^ self.prev_h);
        self.prev_h = h.0;
        if !h.valid_any() || h.free_format() != (self.free > 0) {
            return Err(Error("bad header"));
        }
        let flen = h.frame_len_free(self.free);
        if flen < h.prefix_len() || (self.r_len + flen) as u64 > self.size {
            return Err(Error("frame length"));
        }
        let (pay, len) = (self.r_len, flen - h.prefix_len());
        self.r_len += len;
        Ok(Some(Next { h, pay, len, junk: std::mem::take(&mut self.junk) }))
    }
}

/// The output staged, and put to `put` in pieces; `written` counts what was staged.
struct Staged<'a> {
    stage: Vec<u8>,
    put: &'a mut dyn Put,
    written: u64,
}

impl Staged<'_> {
    fn flush(&mut self) -> Result<(), Error> {
        self.put.put(&self.stage).ok_or(Error("length"))?;
        self.stage.clear();
        Ok(())
    }

    fn emit(&mut self, d: &[u8]) -> Result<(), Error> {
        self.written += d.len() as u64;
        if d.len() >= STAGE {
            self.flush()?;
            return self.put.put(d).ok_or(Error("length"));
        }
        if self.stage.len() + d.len() > STAGE {
            self.flush()?;
        }
        self.stage.extend_from_slice(d);
        Ok(())
    }
}

/// The output file, written as the main pass goes ([`crate::codec::Assembler`]'s): each frame
/// once its side information is complete and its payload in the reservoir is final.
struct Out<'a> {
    ring: VecDeque<Rec>,
    junk_s: Reader<'a>,
    crc: Reader<'a>,
    o: Staged<'a>,
}

impl Out<'_> {
    /// Writes the frames whose payloads lie within `r` (the reservoir from byte `base`).
    fn write(&mut self, base: usize, r: &[u8]) -> Result<(), Error> {
        while let Some(f) = self.ring.front() {
            if f.pay + f.len > base + r.len() {
                break;
            }
            let Some(f) = self.ring.pop_front() else { break };
            self.junk(f.junk)?;
            let hb = f.h.0.to_be_bytes();
            self.o.emit(&hb)?;
            let mut sw = BitWriter::with_capacity(SIDE_ROOM);
            f.si.write(f.h, &mut sw);
            let side = sw.finish();
            if f.h.protected() {
                if self.crc.byte().ok_or(Error("crc"))? != 0 {
                    self.o.emit(&crc16(&hb, &side).to_be_bytes())?;
                } else {
                    let mut c = [0; 2];
                    self.crc.exact(&mut c).ok_or(Error("crc"))?;
                    self.o.emit(&c)?;
                }
            }
            self.o.emit(&side)?;
            self.o.emit(&r[f.pay - base..f.pay + f.len - base])?;
        }
        Ok(())
    }

    fn junk(&mut self, n: u64) -> Result<(), Error> {
        let o = &mut self.o;
        self.junk_s.each(n, |p| o.emit(p).ok()).ok_or(Error("junk"))
    }

    /// After the main pass (the whole reservoir in `w`): the frames left, and the junk after the
    /// last.
    fn finish(&mut self, trailing: u64, w: &FastWriter) -> Result<(), Error> {
        let (base, r) = w.done();
        self.write(base, r)?;
        if !self.ring.is_empty() {
            return Err(Error("length"));
        }
        self.junk(trailing)?;
        self.o.flush()
    }

    /// The reservoir handed over as it is written: the frames now final are written, and what
    /// no frame to come needs is dropped from `w`.
    fn ready(&mut self, w: &mut FastWriter) -> Result<(), Error> {
        let (base, r) = w.done();
        self.write(base, r)?;
        let keep = self.ring.front().map_or(base + r.len(), |f| f.pay.min(base + r.len()));
        if keep - base >= DISCARD_MIN {
            w.discard(keep);
        }
        Ok(())
    }
}

/// The blocks that [`run`] takes, after the tables, in the order it takes them ([`crate::plan`]):
/// `rec`, `state` and `side_model` are the sizes of a [`Rec`] and of the models on WebAssembly.
#[cfg(feature = "encode")]
pub(crate) fn plan(m: &mut aqr_api::stack::Sim, rec: u64, state: u64, side_model: u64) {
    use aqr_api::stack::block;
    let slide = |ahead: usize| Slide::plan(ahead, block);
    // the main pass's state
    m.take(state);
    // the side information's range decoder, then its model
    m.take(PIECE as u64);
    m.take(side_model);
    // TAIL, RAWGR and GAPS, and the raw bits
    (0..3).for_each(|_| m.take(slide(PIECE + 2)));
    m.take(slide(RAW_AHEAD));
    // the rANS decoder's window
    m.take(4 * (WORDS_WINDOW + rans::SRC_PAD) as u64);
    // SEGS and HDR, JUNK and CRC
    (0..4).for_each(|_| m.take(PIECE as u64));
    // the output staged, the frames waiting, the reservoir's window
    m.take(STAGE as u64);
    m.take(RING as u64 * rec);
    m.take(WINDOW as u64 + 16);
    // (a frame's side information written)
    m.peak(block(SIDE_ROOM as u64));
}

/// The module on its ten inputs (`inputs`: each with its length), the file of `size` bytes put to
/// `out`. The same output and errors as [`crate::codec::decode_parts`], with the checks of its
/// limits (a frame whose payload waits for more than [`RING`] frames; a bit string ending before
/// its copy does).
pub(crate) fn run(
    t: &Tables,
    inputs: [(&mut dyn Pull, u64); SLOTS],
    out: &mut dyn Put,
    size: u64,
) -> Result<(), Error> {
    let [
        (segs, _),
        (junk, _),
        (hdr, _),
        (crc, _),
        (tail, n_tail),
        (raw_gr, n_raw_gr),
        (gaps, n_gaps),
        (side, n_side),
        (words, n_words),
        (raw, n_raw),
    ] = inputs;
    let mut pass = Pass::new();
    let mut side = InlineSide { rc: rc::StreamDecoder::new(side, n_side, PIECE), m: SideModel::new() };
    let mut bits = [SlideBits::new(tail, n_tail), SlideBits::new(raw_gr, n_raw_gr), SlideBits::new(gaps, n_gaps)];
    let mut raw = Slide::new(raw, RAW_AHEAD);
    let mut rans = rans::Decoder::streaming(t, (n_words / 4) as usize, WORDS_WINDOW);
    let mut fill = rans::SlideState::default();
    let mut words = Words::new(words);
    let mut walk = Walk {
        segs: Reader::new(segs, PIECE),
        hdr: Reader::new(hdr, PIECE),
        junk: 0,
        left: 0,
        free: 0,
        prev_h: 0,
        r_len: 0,
        ended: false,
        size,
    };
    let mut o = Out {
        junk_s: Reader::new(junk, PIECE),
        crc: Reader::new(crc, PIECE),
        o: Staged { stage: Vec::with_capacity(STAGE), put: out, written: 0 },
        ring: VecDeque::with_capacity(RING),
    };
    let mut w = FastWriter::with_capacity(WINDOW);
    let mut fi = 0usize;
    while let Some(nx) = walk.next()? {
        // (the gap that no frame to come reaches back into, and what it has finished)
        let before = gap_before(nx.pay);
        if before > w.len() {
            let [_, _, gaps] = &mut bits;
            let n = before - w.len();
            copy_gap(&mut w, gaps, n, &mut |w| o.ready(w))?;
        }
        o.ready(&mut w)?;
        rans.slide(&mut fill, WORDS_AHEAD, |b| words.fill(b));
        if fi > 0 && fi.is_multiple_of(RUN_FRAMES) {
            rans.restart();
        }
        raw.need(RAW_AHEAD);
        let mut dc = Dec { rans, raw: BitBuf::new(&BitReader::new(raw.padded(), raw.bit())) };
        let mut si = SideInfo::default();
        let r = pass.frame((nx.h, nx.pay, nx.len), &mut si, &mut side, &mut dc, &mut bits, &mut w, &mut |w| o.ready(w));
        let pos = dc.raw.pos();
        rans = dc.rans;
        r?;
        // (the raw bits read are the stream's)
        if raw.taken() * 8 + pos as u64 > n_raw * 8 {
            return Err(Error("raw bits exhausted"));
        }
        raw.advance_bits(pos);
        if o.ring.len() >= RING {
            return Err(Error("frames pending"));
        }
        o.ring.push_back(Rec { h: nx.h, si, pay: nx.pay, len: nx.len, junk: nx.junk });
        fi += 1;
    }
    if !side.rc.in_bounds() {
        return Err(Error("side stream exhausted"));
    }
    let [_, _, gaps] = &mut bits;
    pass.finish(walk.r_len, gaps, &mut w, &mut |w| o.ready(w))?;
    o.ready(&mut w)?;
    o.finish(walk.junk, &w)?;
    if o.o.written != size {
        return Err(Error("length"));
    }
    if !rans.streamed_in_bounds(&fill) {
        return Err(Error("rans stream exhausted"));
    }
    Ok(())
}

const _: () = assert!(N_BYTE_STREAMS + 2 == SLOTS);
