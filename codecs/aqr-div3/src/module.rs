//! The code of the module `div3-recons` (see `formula`, and `FORMAT.md`), which needs no aqr-formula,
//! so that aqr-modules builds it into its Wasm module.
use alloc::vec;
use alloc::vec::Vec;

use aqr_api::stream::{In, Pull, Put, Slice, Written};

use crate::{Codec, Decoder, Error, Result, Source, corrupt};

/// Pages of the windows of `div3-recons` as a part module: of each input and of the output.
pub const WINDOW: u32 = 4;
/// The most bytes of a frame the module takes.
pub const FRAME_MAX: usize = 4 << 20;
/// Bytes of each buffer reading an input.
const BUF: usize = 16 << 10;

/// Bytes the model takes (`Model::new(22)`: 4-byte probabilities).
const MODEL_BYTES: u64 = 4 << 22;

/// The memory plan of `div3-recons` as a part module ([`aqr_api::stack`]): what [`decode_part`] takes beyond
/// its initial memory (`initial` bytes) at most, whatever the inputs: the model, the frame, and four buffers.
pub fn plan(initial: u64) -> u64 {
    let mut m = aqr_api::stack::Sim::new(initial);
    m.take(MODEL_BYTES);
    m.take(FRAME_MAX as u64 + 8);
    m.take(BUF as u64);
    m.take(BUF as u64);
    m.take(BUF as u64);
    m.take(BUF as u64);
    m.plan()
}

/// Bytes read from an input through a buffer.
struct Reader<'a> {
    src: &'a mut dyn Pull,
    buf: Vec<u8>,
    pos: usize,
    len: usize,
}

impl<'a> Reader<'a> {
    fn new(src: &'a mut dyn Pull) -> Self {
        Reader { src, buf: vec![0; BUF], pos: 0, len: 0 }
    }
    fn fill(&mut self) -> bool {
        if self.pos == self.len {
            self.len = self.src.pull(&mut self.buf);
            self.pos = 0;
        }
        self.len != 0
    }
    fn next(&mut self) -> Option<u8> {
        if !self.fill() {
            return None;
        }
        self.pos += 1;
        Some(self.buf[self.pos - 1])
    }
    /// A LEB128 number, or None at the end of the input.
    fn varint(&mut self) -> Result<Option<u64>> {
        let (mut v, mut shift) = (0u64, 0);
        loop {
            let Some(b) = self.next() else {
                return if shift == 0 { Ok(None) } else { Err(corrupt()) };
            };
            if shift > 56 {
                return Err(corrupt());
            }
            v |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return Ok(Some(v));
            }
            shift += 7;
        }
    }
    /// `n` bytes put to `out`.
    fn copy(&mut self, mut n: u64, out: &mut dyn Put) -> Result<()> {
        while n > 0 {
            if !self.fill() {
                return Err(Error("input of another length"));
            }
            let k = (self.len - self.pos).min(n.min(usize::MAX as u64) as usize);
            out.put(&self.buf[self.pos..self.pos + k]).ok_or(Error("output of another length"))?;
            self.pos += k;
            n -= k as u64;
        }
        Ok(())
    }
}

impl Source for Reader<'_> {
    fn byte(&mut self) -> u8 {
        self.next().unwrap_or(0)
    }
}

/// The file of `size` bytes from the coded frames, the skeleton and the layout (see `FORMAT.md`), put to `out`.
pub fn run(
    coded: &mut dyn Pull,
    skeleton: &mut dyn Pull,
    layout: &mut dyn Pull,
    audio: &mut dyn Pull,
    out: &mut dyn Put,
    width: usize,
    height: usize,
    size: u64,
) -> Result<()> {
    // (in the order of the plan)
    let mut codec = Codec::new(width, height);
    let mut frame: Vec<u8> = Vec::with_capacity(FRAME_MAX + 8);
    let mut dec = Decoder::new(Reader::new(coded));
    let mut skel = Reader::new(skeleton);
    let mut lay = Reader::new(layout);
    let mut aud = Reader::new(audio);
    let mut written = 0u64;
    while let Some(gap) = lay.varint()? {
        let tag = lay.varint()?.ok_or_else(corrupt)?;
        let n = tag >> 1;
        if written + gap + n > size {
            return Err(corrupt());
        }
        skel.copy(gap, out)?;
        if tag & 1 == 0 {
            if n as usize > FRAME_MAX {
                return Err(corrupt());
            }
            frame = codec.decode(&mut dec, n as usize, frame).map_err(|_| corrupt())?;
            out.put(&frame).ok_or(Error("output of another length"))?;
        } else {
            aud.copy(n, out)?;
        }
        written += gap + n;
    }
    skel.copy(size - written, out)?;
    if skel.next().is_some() || aud.next().is_some() {
        return Err(Error("input of another length"));
    }
    Ok(())
}

fn dims(params: &[u8]) -> Result<(usize, usize)> {
    let &[a, b, c, d] = params else { return Err(corrupt()) };
    Ok((u16::from_le_bytes([a, b]) as usize, u16::from_le_bytes([c, d]) as usize))
}

/// The module `div3-recons` on whole inputs, natively.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, _threads: usize) -> Result<Vec<u8>> {
    let &[coded, skeleton, layout, audio] = inputs else { return Err(corrupt()) };
    let (w, h) = dims(params)?;
    let mut out = Vec::with_capacity(len.min(1 << 32) as usize);
    run(&mut Slice(coded), &mut Slice(skeleton), &mut Slice(layout), &mut Slice(audio), &mut out, w, h, len)?;
    if out.len() as u64 != len {
        return Err(Error("output of another length"));
    }
    Ok(out)
}

/// The module `div3-recons` as a part module (its entry point, of `aqr_api::entry!`): the inputs taken and the
/// output put in order through windows ([`WINDOW`]).
pub fn decode_part(params_len: u64, in0_len: u64, in1_len: u64, in2_len: u64, in3_len: u64, size: u64) {
    aqr_api::finish(part(params_len, in0_len, in1_len, in2_len, in3_len, size));
}

fn part(params_len: u64, in0_len: u64, in1_len: u64, in2_len: u64, in3_len: u64, size: u64) -> Result<()> {
    use aqr_api::Memory;
    let mut p = [0; 4];
    if params_len != 4 {
        return Err(corrupt());
    }
    aqr_api::Params::read(0, &mut p);
    let (w, h) = dims(&p)?;
    let mut out = Written::new(size);
    run(&mut In::<0>::new(in0_len), &mut In::<1>::new(in1_len), &mut In::<2>::new(in2_len), &mut In::<3>::new(in3_len), &mut out, w, h, size)?;
    match out.total() == size {
        true => Ok(()),
        false => Err(Error("output of another length")),
    }
}
