//! Audio packets (Vorbis I §4.3.1): the fields and codebook entries a packet codes, in the order
//! a decoder reads them, as far as the bitstream goes (no synthesis).
//!
//! [`walk`] runs the decode of one packet against a [`Source`] of values: one reading the packet
//! ([`parse`]) tells what it codes; one given the values ([`fn@write`]) writes the packet back. A
//! packet is its values, then its tail: the bits after the last value read whole, which are the
//! padding of the last byte, or where the packet ends inside a field (end-of-packet, §4.3.1),
//! that field's bits present.

use crate::bits::{Reader, Writer, ilog};
use crate::setup::Setup;
use crate::{Error, Result};

/// What a value is, for a [`Source`]: where in the packet it is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// The packet type bit (0 for audio).
    Type,
    Mode,
    /// The previous and next window flags of a long block.
    Window,
    /// Whether channel `ch`'s floor is used.
    Nonzero {
        ch: usize,
    },
    /// `Y[0]` or `Y[1]` of channel `ch`'s floor.
    Y01 {
        ch: usize,
        i: usize,
    },
    /// The master book's entry of floor partition `part` (choosing its subclass books).
    Master {
        ch: usize,
        part: usize,
    },
    /// `Y[i]` of channel `ch`'s floor, coded by a subclass book.
    Y {
        ch: usize,
        i: usize,
    },
    /// A classword of residue vector `v` (of submap `submap`), for the partitions from `part`.
    Class {
        submap: usize,
        v: usize,
        part: usize,
    },
    /// A VQ entry of residue vector `v`, partition `part`, pass `pass`, its `k`-th in the
    /// partition.
    Vq {
        submap: usize,
        v: usize,
        part: usize,
        pass: usize,
        class: usize,
        k: usize,
    },
}

/// Where [`walk`] gets values. A None ends the walk (end-of-packet). An entry must be one of the
/// book's used entries.
pub trait Source {
    /// An `n`-bit field.
    fn bits(&mut self, f: Field, n: u32) -> Option<u32>;
    /// An entry of codebook `book`.
    fn entry(&mut self, f: Field, book: usize) -> Option<u32>;
    /// The `reads` VQ entries of book `book` of a partition, in order (`f` a [`Field::Vq`] of `k`
    /// 0): [`Source::entry`] of each, unless a source does them at once. Whether all were given.
    fn vq(&mut self, f: Field, book: usize, reads: usize) -> bool {
        let Field::Vq { submap, v, part, pass, class, .. } = f else { return false };
        (0..reads).all(|k| self.entry(Field::Vq { submap, v, part, pass, class, k }, book).is_some())
    }
}

/// How a walk ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    /// The decode read all it reads.
    Whole,
    /// A value was missing (end-of-packet).
    Short,
}

/// Runs the decode of an audio packet of `setup` against `src`.
pub fn walk(setup: &Setup, src: &mut dyn Source) -> Result<End> {
    match walk_or_short(setup, src) {
        Ok(()) => Ok(End::Whole),
        Err(Stop::Short) => Ok(End::Short),
        Err(Stop::Bad(e)) => Err(e),
    }
}

enum Stop {
    Short,
    Bad(Error),
}

impl From<Error> for Stop {
    fn from(e: Error) -> Stop {
        Stop::Bad(e)
    }
}

fn walk_or_short(s: &Setup, src: &mut dyn Source) -> std::result::Result<(), Stop> {
    let bits = |src: &mut dyn Source, f, n| src.bits(f, n).ok_or(Stop::Short);
    let entry = |src: &mut dyn Source, f, book: usize| src.entry(f, book).ok_or(Stop::Short);
    if bits(src, Field::Type, 1)? != 0 {
        return Err(Error("not an audio packet").into());
    }
    let mode = bits(src, Field::Mode, ilog(s.modes.len() as u32 - 1))? as usize;
    let mode = *s.modes.get(mode).ok_or(Error("packet mode"))?;
    if mode.blockflag {
        bits(src, Field::Window, 2)?;
    }
    let n = s.ident.blocksizes[mode.blockflag as usize];
    let map = &s.mappings[mode.mapping];
    let channels = s.ident.channels;
    let mut unused = vec![false; channels];
    for (ch, unused) in unused.iter_mut().enumerate() {
        let floor = &s.floors[map.submaps[map.mux[ch]].0];
        if bits(src, Field::Nonzero { ch }, 1)? == 0 {
            *unused = true;
            continue;
        }
        let rb = floor.range_bits();
        bits(src, Field::Y01 { ch, i: 0 }, rb)?;
        bits(src, Field::Y01 { ch, i: 1 }, rb)?;
        let mut i = 2;
        for (part, &c) in floor.partitions.iter().enumerate() {
            let class = &floor.classes[c];
            let mut cval =
                if class.subclasses > 0 { entry(src, Field::Master { ch, part }, class.masterbook)? } else { 0 };
            let csub = (1 << class.subclasses) - 1;
            for _ in 0..class.dims {
                if let Some(book) = class.subbooks[(cval & csub) as usize] {
                    entry(src, Field::Y { ch, i }, book)?;
                }
                cval >>= class.subclasses;
                i += 1;
            }
        }
    }
    for &(m, a) in &map.coupling {
        if !unused[m] || !unused[a] {
            unused[m] = false;
            unused[a] = false;
        }
    }
    for (submap, &(_, res)) in map.submaps.iter().enumerate() {
        let skip: Vec<bool> = (0..channels).filter(|&ch| map.mux[ch] == submap).map(|ch| unused[ch]).collect();
        residue(s, res, submap, &skip, n / 2, src)?;
    }
    Ok(())
}

/// The decode of residue `res` (§8.6.2) of the vectors of a submap, `skip` those not decoded,
/// each of `half` values.
fn residue(
    s: &Setup,
    res: usize,
    submap: usize,
    skip: &[bool],
    half: usize,
    src: &mut dyn Source,
) -> std::result::Result<(), Stop> {
    let r = &s.residues[res];
    // type 2 decodes the vectors interleaved as one, unless none is decoded
    let (skip, size) = if r.kind == 2 {
        if skip.iter().all(|&k| k) {
            return Ok(());
        }
        (vec![false], half * skip.len())
    } else {
        (skip.to_vec(), half)
    };
    let begin = r.begin.min(size);
    let end = r.end.min(size);
    let parts = (end - begin) / r.partition;
    if parts == 0 {
        return Ok(());
    }
    let classbook = &s.codebooks[r.classbook];
    let per_word = classbook.dims;
    // each vector's partitions' classes, one row per vector
    let row = parts + per_word;
    let mut classes = vec![0u8; row * skip.len()];
    for pass in 0..8 {
        let mut part = 0;
        while part < parts {
            if pass == 0 {
                for v in (0..skip.len()).filter(|&v| !skip[v]) {
                    let e = src.entry(Field::Class { submap, v, part }, r.classbook).ok_or(Stop::Short)?;
                    let mut t = e as usize;
                    for i in (0..per_word).rev() {
                        classes[v * row + part + i] = (t % r.classifications) as u8;
                        t /= r.classifications;
                    }
                }
            }
            for _ in 0..per_word {
                if part >= parts {
                    break;
                }
                for v in (0..skip.len()).filter(|&v| !skip[v]) {
                    let class = classes[v * row + part] as usize;
                    let Some(book) = r.books[class][pass] else { continue };
                    let dims = s.codebooks[book].dims;
                    let reads = if r.kind == 0 { r.partition / dims } else { r.partition.div_ceil(dims) };
                    if !src.vq(Field::Vq { submap, v, part, pass, class, k: 0 }, book, reads) {
                        return Err(Stop::Short);
                    }
                }
                part += 1;
            }
        }
    }
    Ok(())
}

/// A packet as its values and tail.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Packet {
    /// The values read, in order: a field's bits, or a codebook's entry.
    pub values: Vec<u32>,
    /// The tail's bits (each 0 or 1), from where the last value ended to the end of the packet.
    pub tail: Vec<u8>,
    /// Whether the packet ended inside a field (the tail then that field's bits present).
    pub short: bool,
}

struct Parse<'a> {
    setup: &'a Setup,
    r: Reader<'a>,
    values: Vec<u32>,
}

impl Source for Parse<'_> {
    fn bits(&mut self, _: Field, n: u32) -> Option<u32> {
        let v = self.r.bits(n)?;
        self.values.push(v);
        Some(v)
    }

    fn entry(&mut self, _: Field, book: usize) -> Option<u32> {
        let e = self.setup.codebooks[book].read(&mut self.r)?;
        self.values.push(e);
        Some(e)
    }
}

/// The values and tail of audio packet `d`. Fails where it is not an audio packet of `setup`,
/// or codes a path no codeword has.
pub fn parse(setup: &Setup, d: &[u8]) -> Result<Packet> {
    let mut p = Parse { setup, r: Reader::new(d), values: Vec::new() };
    let end = walk(setup, &mut p)?;
    let mut r = p.r;
    let tail = (r.pos()..r.len()).map(|_| r.bit().expect("within the packet") as u8).collect();
    Ok(Packet { values: p.values, tail, short: end == End::Short })
}

struct Write<'a> {
    setup: &'a Setup,
    w: Writer,
    values: std::slice::Iter<'a, u32>,
    /// A value that the packet cannot code (the walk was stopped there).
    bad: bool,
}

impl Source for Write<'_> {
    fn bits(&mut self, _: Field, n: u32) -> Option<u32> {
        let &v = self.values.next()?;
        if n < 32 && v >> n != 0 {
            self.bad = true;
            return None;
        }
        self.w.bits(v, n);
        Some(v)
    }

    fn entry(&mut self, _: Field, book: usize) -> Option<u32> {
        let &e = self.values.next()?;
        if !self.setup.codebooks[book].used(e) {
            self.bad = true;
            return None;
        }
        self.setup.codebooks[book].write(e, &mut self.w);
        Some(e)
    }
}

/// The packet of `p`'s values and tail. Fails where they are not those of a packet of `setup`:
/// values left over, an entry without a codeword, a field's value too wide, a tail that does not
/// end on a byte.
pub fn write(setup: &Setup, p: &Packet) -> Result<Vec<u8>> {
    let mut w = Write { setup, w: Writer::new(), values: p.values.iter(), bad: false };
    let end = walk(setup, &mut w)?;
    if w.bad || w.values.len() != 0 || (end == End::Short) != p.short {
        return Err(Error("packet values"));
    }
    let mut out = w.w;
    p.tail.iter().for_each(|&b| out.bit(b as u32));
    if !out.pos().is_multiple_of(8) {
        return Err(Error("packet tail"));
    }
    Ok(out.finish())
}
