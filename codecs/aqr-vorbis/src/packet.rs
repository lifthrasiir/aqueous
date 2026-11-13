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

/// The room a walk works in, reserved once for a setup: nothing is allocated per packet.
#[derive(Debug, Clone)]
pub struct Scratch {
    /// A residue's classes, a row of `parts + per_word` for each vector.
    classes: Vec<u8>,
    /// Per vector of a submap, whether it is not decoded.
    skip: Vec<bool>,
    /// Per channel, whether its floor is unused.
    unused: Vec<bool>,
}

impl Scratch {
    /// The sizes of the vectors for `s`: the channels, the channels, and [`Scratch::class_bytes`].
    pub fn sizes(s: &Setup) -> [usize; 3] {
        [s.ident.channels, s.ident.channels, Scratch::class_bytes(s)]
    }

    pub fn new(s: &Setup) -> Scratch {
        let [a, b, c] = Scratch::sizes(s);
        Scratch { unused: Vec::with_capacity(a), skip: Vec::with_capacity(b), classes: Vec::with_capacity(c) }
    }

    /// The most bytes of classes a residue of a mode's mapping decodes at once.
    pub fn class_bytes(s: &Setup) -> usize {
        let mut most = 0;
        residue_shapes(s, |_, parts, rows, dims| most = most.max((parts + dims) * rows));
        most
    }
}

/// `f` of every residue a mode's mapping decodes, as (submap, partitions, rows of vectors, the
/// classbook's dimensions): the residues that decode something.
pub fn residue_shapes(s: &Setup, mut f: impl FnMut(usize, usize, usize, usize)) {
    for mode in &s.modes {
        let map = &s.mappings[mode.mapping as usize];
        for (submap, &(_, res)) in map.submaps().iter().enumerate() {
            let vectors = map.mux().iter().filter(|&&m| m as usize == submap).count();
            let half = s.ident.blocksizes[mode.blockflag as usize] / 2;
            if let Some((parts, rows)) = residue_shape(s, res as usize, vectors, half) {
                f(submap, parts, rows, s.codebooks[s.residues[res as usize].classbook as usize].dims);
            }
        }
    }
}

/// The partitions of a residue `res` of `vectors` vectors of `half` values each, and the rows
/// of vectors it decodes (one where the vectors are interleaved): None where it decodes
/// nothing.
fn residue_shape(s: &Setup, res: usize, vectors: usize, half: usize) -> Option<(usize, usize)> {
    let r = &s.residues[res];
    let (rows, size) = if r.kind == 2 { (1, half * vectors) } else { (vectors, half) };
    let begin = (r.begin as usize).min(size);
    let end = (r.end as usize).min(size);
    let parts = end.saturating_sub(begin) / r.partition as usize;
    (parts > 0).then_some((parts, rows))
}

/// Runs the decode of an audio packet of `setup` against `src`.
pub fn walk(setup: &Setup, src: &mut dyn Source) -> Result<End> {
    walk_in(setup, &mut Scratch::new(setup), src)
}

/// [`walk`] in `scratch` (of `setup`).
pub fn walk_in(setup: &Setup, scratch: &mut Scratch, src: &mut dyn Source) -> Result<End> {
    match walk_or_short(setup, scratch, src) {
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

fn walk_or_short(s: &Setup, scratch: &mut Scratch, src: &mut dyn Source) -> std::result::Result<(), Stop> {
    let bits = |src: &mut dyn Source, f, n| src.bits(f, n).ok_or(Stop::Short);
    let entry = |src: &mut dyn Source, f, book: usize| src.entry(f, book).ok_or(Stop::Short);
    if !s.ident.stripped && bits(src, Field::Type, 1)? != 0 {
        return Err(Error("not an audio packet").into());
    }
    let mode = bits(src, Field::Mode, ilog(s.modes.len() as u32 - 1))? as usize;
    let mode = *s.modes.get(mode).ok_or(Error("packet mode"))?;
    if mode.blockflag && !s.ident.stripped {
        bits(src, Field::Window, 2)?;
    }
    let n = s.ident.blocksizes[mode.blockflag as usize];
    let map = &s.mappings[mode.mapping as usize];
    let channels = s.ident.channels;
    scratch.unused.clear();
    scratch.unused.resize(channels, false);
    for ch in 0..channels {
        let floor = &s.floors[map.submaps()[map.mux()[ch] as usize].0 as usize];
        if bits(src, Field::Nonzero { ch }, 1)? == 0 {
            scratch.unused[ch] = true;
            continue;
        }
        let rb = floor.range_bits();
        bits(src, Field::Y01 { ch, i: 0 }, rb)?;
        bits(src, Field::Y01 { ch, i: 1 }, rb)?;
        let mut i = 2;
        for (part, &c) in floor.partitions().iter().enumerate() {
            let class = &floor.classes()[c as usize];
            let mut cval = if class.subclasses > 0 {
                entry(src, Field::Master { ch, part }, class.masterbook as usize)?
            } else {
                0
            };
            let csub = (1 << class.subclasses) - 1;
            for _ in 0..class.dims {
                if let Some(book) = class.subbooks[(cval & csub) as usize] {
                    entry(src, Field::Y { ch, i }, book as usize)?;
                }
                cval >>= class.subclasses;
                i += 1;
            }
        }
    }
    let unused = &mut scratch.unused;
    for &(m, a) in map.coupling() {
        let (m, a) = (m as usize, a as usize);
        if !unused[m] || !unused[a] {
            unused[m] = false;
            unused[a] = false;
        }
    }
    for (submap, &(_, res)) in map.submaps().iter().enumerate() {
        scratch.skip.clear();
        let unused = &scratch.unused;
        scratch.skip.extend((0..channels).filter(|&ch| map.mux()[ch] as usize == submap).map(|ch| unused[ch]));
        residue(s, res as usize, submap, scratch, n / 2, src)?;
    }
    Ok(())
}

/// The decode of residue `res` (§8.6.2) of the vectors of a submap (`scratch.skip` those not
/// decoded), each of `half` values.
fn residue(
    s: &Setup,
    res: usize,
    submap: usize,
    scratch: &mut Scratch,
    half: usize,
    src: &mut dyn Source,
) -> std::result::Result<(), Stop> {
    let r = &s.residues[res];
    // type 2 decodes the vectors interleaved as one, unless none is decoded
    let skip = &mut scratch.skip;
    if r.kind == 2 && skip.iter().all(|&k| k) {
        return Ok(());
    }
    let Some((parts, rows)) = residue_shape(s, res, skip.len(), half) else { return Ok(()) };
    if r.kind == 2 {
        skip.clear();
        skip.push(false);
    }
    let classbook = &s.codebooks[r.classbook as usize];
    let per_word = classbook.dims;
    let row = parts + per_word;
    // each vector's partitions' classes, one row per vector
    let classes = &mut scratch.classes;
    classes.clear();
    classes.resize(row * rows, 0);
    for pass in 0..8 {
        let mut part = 0;
        while part < parts {
            if pass == 0 {
                for v in (0..skip.len()).filter(|&v| !skip[v]) {
                    let e = src.entry(Field::Class { submap, v, part }, r.classbook as usize).ok_or(Stop::Short)?;
                    let mut t = e as usize;
                    for i in (0..per_word).rev() {
                        classes[v * row + part + i] = (t % r.classifications as usize) as u8;
                        t /= r.classifications as usize;
                    }
                }
            }
            for _ in 0..per_word {
                if part >= parts {
                    break;
                }
                for v in (0..skip.len()).filter(|&v| !skip[v]) {
                    let class = classes[v * row + part] as usize;
                    let Some(book) = r.book(class, pass) else { continue };
                    let dims = s.codebooks[book].dims;
                    let partition = r.partition as usize;
                    let reads = if r.kind == 0 { partition / dims } else { partition.div_ceil(dims) };
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
    Ok(Packet { values: p.values, tail: p.r.tail_bits(), short: end == End::Short })
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
