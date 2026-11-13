//! Wwise's stripped setup header, and the Vorbis setup header (Vorbis I §4.2.4) it stands for.
//!
//! The stripped header is a Vorbis setup header without its packet type and `vorbis`, with each
//! codebook given as a 10-bit id, no time-domain list, and with some fields left out: a floor's
//! type (always 1), a residue's type in 2 bits (not 16), a mapping's type (always 0), a mode's
//! window and transform types (always 0), and the framing flag. The rest is as Vorbis has it.

use aqr_vorbis::bits::{Reader, Writer, ilog};

/// A stripped setup header read.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Stripped {
    /// The codebooks' ids, in order.
    pub ids: Vec<u16>,
    /// The Vorbis setup header's bits after its codebooks: the time-domain list to the framing
    /// flag, one bit a byte.
    pub rest: Vec<u8>,
    /// The residues' `end`s.
    pub ends: Vec<u32>,
    /// The modes' block flags.
    pub blockflags: Vec<bool>,
    /// The stripped header's bits read, of the bits it has.
    pub used: (usize, usize),
}

/// The walk of a setup's lists after its codebooks, either way: reading a stripped header and
/// writing the Vorbis one's bits (`strip` false), or reading those and writing the stripped
/// header's (`strip` true).
struct Copy<'a> {
    r: Reader<'a>,
    out: Vec<u8>,
    strip: bool,
}

impl Copy<'_> {
    fn put(&mut self, v: u32, n: u32) {
        self.out.extend((0..n).map(|i| (v >> i & 1) as u8));
    }

    /// Reads `n` bits and writes them.
    fn copy(&mut self, n: u32) -> Option<u32> {
        let v = self.r.bits(n)?;
        self.put(v, n);
        Some(v)
    }

    /// A field of `n` bits, always `v`, that the stripped header leaves out.
    fn fixed(&mut self, v: u32, n: u32) -> Option<()> {
        if self.strip {
            (self.r.bits(n)? == v).then_some(())
        } else {
            self.put(v, n);
            Some(())
        }
    }

    /// A field of `short` bits in the stripped header and `long` in Vorbis's.
    fn widen(&mut self, short: u32, long: u32) -> Option<u32> {
        if self.strip {
            let v = self.r.bits(long)?;
            if v >> short != 0 {
                return None;
            }
            self.put(v, short);
            Some(v)
        } else {
            let v = self.r.bits(short)?;
            self.put(v, long);
            Some(v)
        }
    }
}

/// The lists from the time-domain one to the framing flag, either way ([`Copy`]): the residues'
/// ends and the modes' block flags.
fn lists(c: &mut Copy, channels: usize) -> Option<(Vec<u32>, Vec<bool>)> {
    // The time-domain placeholder.
    c.fixed(0, 6)?;
    c.fixed(0, 16)?;
    let floors = c.copy(6)? + 1;
    for _ in 0..floors {
        c.fixed(1, 16)?;
        let partitions = c.copy(5)?;
        let classes: Vec<u32> = (0..partitions).map(|_| c.copy(4)).collect::<Option<_>>()?;
        let nclasses = classes.iter().max().map_or(0, |m| m + 1);
        let mut dims = Vec::new();
        for _ in 0..nclasses {
            dims.push(c.copy(3)? + 1);
            let sub = c.copy(2)?;
            if sub > 0 {
                c.copy(8)?;
            }
            for _ in 0..1 << sub {
                c.copy(8)?;
            }
        }
        c.copy(2)?;
        let rangebits = c.copy(4)?;
        for &cl in &classes {
            for _ in 0..dims[cl as usize] {
                c.copy(rangebits)?;
            }
        }
    }
    let residues = c.copy(6)? + 1;
    let mut ends = Vec::new();
    for _ in 0..residues {
        c.widen(2, 16)?;
        c.copy(24)?;
        ends.push(c.copy(24)?);
        c.copy(24)?;
        let classifications = c.copy(6)? + 1;
        c.copy(8)?;
        let mut cascade = Vec::new();
        for _ in 0..classifications {
            let low = c.copy(3)?;
            let high = if c.copy(1)? == 1 { c.copy(5)? } else { 0 };
            cascade.push(high << 3 | low);
        }
        for cs in cascade {
            for b in 0..8 {
                if cs >> b & 1 == 1 {
                    c.copy(8)?;
                }
            }
        }
    }
    let mappings = c.copy(6)? + 1;
    for _ in 0..mappings {
        c.fixed(0, 16)?;
        let submaps = if c.copy(1)? == 1 { c.copy(4)? + 1 } else { 1 };
        if c.copy(1)? == 1 {
            let steps = c.copy(8)? + 1;
            let w = ilog(channels as u32 - 1);
            for _ in 0..steps {
                c.copy(w)?;
                c.copy(w)?;
            }
        }
        if c.copy(2)? != 0 {
            return None;
        }
        if submaps > 1 {
            for _ in 0..channels {
                c.copy(4)?;
            }
        }
        for _ in 0..submaps {
            c.copy(8)?;
            c.copy(8)?;
            c.copy(8)?;
        }
    }
    let modes = c.copy(6)? + 1;
    let mut blockflags = Vec::new();
    for _ in 0..modes {
        blockflags.push(c.copy(1)? == 1);
        c.fixed(0, 16)?;
        c.fixed(0, 16)?;
        c.copy(8)?;
    }
    c.fixed(1, 1)?;
    Some((ends, blockflags))
}

/// Bits (one a byte) as bytes, the last padded with zero bits.
fn pack(bits: &[u8]) -> Vec<u8> {
    let mut w = Writer::new();
    bits.iter().for_each(|&b| w.bit(b as u32));
    w.finish()
}

/// Reads stripped setup header `d` of a stream of `channels`.
pub fn parse(d: &[u8], channels: usize) -> Option<Stripped> {
    let mut c = Copy { r: Reader::new(d), out: Vec::new(), strip: false };
    let n = c.r.bits(8)? as usize + 1;
    let ids = (0..n).map(|_| c.r.bits(10).map(|v| v as u16)).collect::<Option<Vec<_>>>()?;
    let (ends, blockflags) = lists(&mut c, channels)?;
    let used = (c.r.pos(), c.r.len());
    Some(Stripped { ids, rest: c.out, ends, blockflags, used })
}

/// The stripped setup header of ids `ids` and of the lists of Vorbis setup header bits `rest`
/// (its bits after its codebooks, one a byte), of a stream of `channels`: [`parse`]'s inverse
/// (its last byte padded with zero bits).
pub fn strip(ids: &[u16], rest: &[u8], channels: usize) -> Option<Vec<u8>> {
    if !(1..=256).contains(&ids.len()) || ids.iter().any(|&i| i >= 1 << 10) {
        return None;
    }
    let rest = pack(rest);
    let mut c = Copy { r: Reader::new(&rest), out: Vec::new(), strip: true };
    c.put(ids.len() as u32 - 1, 8);
    ids.iter().for_each(|&i| c.put(i as u32, 10));
    lists(&mut c, channels)?;
    Some(pack(&c.out))
}

impl Stripped {
    /// The Vorbis setup header with codebooks `books` (each its bits and their number), one for
    /// each id.
    pub fn assemble(&self, books: &[(&[u8], usize)]) -> Vec<u8> {
        let mut w = Writer::new();
        w.bits(5, 8);
        for &b in b"vorbis" {
            w.bits(b as u32, 8);
        }
        w.bits(books.len() as u32 - 1, 8);
        for &(bytes, bits) in books {
            for i in 0..bits {
                w.bit((bytes[i / 8] >> (i % 8)) as u32 & 1);
            }
        }
        for &b in &self.rest {
            w.bit(b as u32);
        }
        w.finish()
    }

    /// The bits of a mode number in an audio packet.
    pub fn mode_bits(&self) -> u32 {
        ilog(self.blockflags.len() as u32 - 1)
    }
}

/// Audio packet `d` of a stream with modified packets as Vorbis codes it: the packet type and,
/// for a long block, the window flags (`prev` and `next` long) put back. Also gives the bits put
/// in.
pub fn rebuild(s: &Stripped, d: &[u8], prev: bool, next: bool) -> Option<(Vec<u8>, usize)> {
    let mut r = Reader::new(d);
    let mut w = Writer::new();
    w.bit(0);
    let n = s.mode_bits();
    let mode = r.bits(n)?;
    w.bits(mode, n);
    let mut added = 1;
    if *s.blockflags.get(mode as usize)? {
        w.bit(prev as u32);
        w.bit(next as u32);
        added += 2;
    }
    while let Some(b) = r.bit() {
        w.bit(b);
    }
    Some((w.finish(), added))
}

/// The mode of audio packet `d` with modified packets.
pub fn mode(s: &Stripped, d: &[u8]) -> Option<usize> {
    Reader::new(d).bits(s.mode_bits()).map(|m| m as usize)
}
