//! The code of the module (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module. It is a part module: its inputs are taken and its
//! output put in order through windows, in pieces, and nothing of their lengths is held.

use crate::fasta::{self, Shape, Streams};
use crate::stream::{PIECE, Pull, Reader, Slice, Writer};
use crate::{Result, bases, corrupt};
use aqr_api::stream::{In, Written};
use aqr_common::varint;

/// Pages of the windows of the module as a part module ([`decode_part`]): of each input and of
/// the output.
pub const WINDOW: u32 = 16;

/// The memory plan of the module as a part module ([`aqr_api::stack`]): what [`decode_part`]
/// takes beyond its initial memory (`initial` bytes) at most, whatever the lengths: a piece for
/// each of the five inputs and the output, and the bases' model.
pub fn plan(initial: u64) -> u64 {
    let mut m = aqr_api::stack::Sim::new(initial);
    for _ in 0..6 {
        m.take(PIECE as u64);
    }
    m.take(bases::MODEL_BYTES);
    m.plan()
}

/// The shape of the file from the module's parameters (the line width, and a byte of whether it
/// ends with a line's end and whether its first record has no header).
fn shape(params: &[u8]) -> Result<Shape> {
    let mut at = 0;
    let width = varint::get(params, &mut at).map_err(|_| corrupt())?;
    let &[nl] = &params[at..] else { return Err(corrupt()) };
    if nl > 3 {
        return Err(corrupt());
    }
    Ok(Shape { width, final_newline: nl & 1 == 1, headless: nl & 2 != 0 })
}

/// The module on the five streams, put to `w`.
fn run(shape: Shape, inputs: [&mut dyn Pull; 5], w: &mut Writer) -> Result<()> {
    let [headers, layout, case, other, coded] = inputs;
    let mut streams = Streams {
        headers: Reader::new(headers, PIECE),
        layout: Reader::new(layout, PIECE),
        case: Reader::new(case, PIECE),
        other: Reader::new(other, PIECE),
    };
    let mut bases = bases::Decoder::new(Reader::new(coded, PIECE))?;
    fasta::build(shape, &mut streams, &mut bases, w)?;
    bases.finish()
}

/// The module: the file of `len` bytes from `params` and `inputs` (the streams of
/// `STREAM_NAMES`), whole.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, _threads: usize) -> Result<Vec<u8>> {
    let &[headers, layout, case, other, coded] = inputs else { return Err(corrupt()) };
    let shape = shape(params)?;
    let (mut h, mut l, mut c, mut o, mut b) = (Slice(headers), Slice(layout), Slice(case), Slice(other), Slice(coded));
    let mut out = Vec::with_capacity(len.min(1 << 30) as usize);
    let mut w = Writer::new(&mut out, len);
    run(shape, [&mut h, &mut l, &mut c, &mut o, &mut b], &mut w)?;
    w.finish()?;
    Ok(out)
}

/// The module as a part module (its entry point, of `aqr_api::entry!`): what [`decode`] gives,
/// its inputs taken and its output put in order through windows ([`WINDOW`]), in pieces:
/// nothing is held, whatever the lengths.
pub fn decode_part(params_len: u64, lens: [u64; 5], size: u64) {
    aqr_api::finish(part(params_len, lens, size));
}

fn part(params_len: u64, lens: [u64; 5], size: u64) -> Result<()> {
    use aqr_api::{Memory, Params};
    // (a varint and a byte at most)
    let mut params = [0; 11];
    let params = params.get_mut(..params_len as usize).ok_or_else(corrupt)?;
    Params::read(0, params);
    let shape = shape(params)?;
    let (mut i0, mut i1, mut i2, mut i3, mut i4) = (
        In::<0>::new(lens[0]),
        In::<1>::new(lens[1]),
        In::<2>::new(lens[2]),
        In::<3>::new(lens[3]),
        In::<4>::new(lens[4]),
    );
    let mut out = Written::new(size);
    let mut w = Writer::new(&mut out, size);
    run(shape, [&mut i0, &mut i1, &mut i2, &mut i3, &mut i4], &mut w)?;
    w.finish()
}
