//! PPMd blocks of RAR 2.9 streams over `ppmd` (the model and RAR's range coder): a model
//! started or restarted, symbols decoded and encoded.

use crate::ppmd::{Model, RangeDecoder, RangeEncoder};
use crate::{Error, Result, corrupt};

pub type Ppmd = Model;

/// Starts `m` afresh: of `order`, `mem` bytes (its memory kept where of that size).
pub fn start(m: &mut Option<Ppmd>, order: u32, mem: u32) -> Result<()> {
    if m.as_ref().map(Model::size) != Some(mem) {
        *m = None;
        *m = Some(Model::new(mem).ok_or(Error("a PPMd model of no memory to have"))?);
    }
    m.as_mut().unwrap().init(order);
    Ok(())
}

/// A range decoder over a block's bytes.
pub struct Reader<'a>(RangeDecoder<'a>);

impl<'a> Reader<'a> {
    pub fn new(d: &'a [u8]) -> Result<Reader<'a>> {
        Ok(Reader(RangeDecoder::new(d)))
    }

    /// The bytes read, an error where past the data.
    pub fn used(&self) -> Result<usize> {
        if self.0.overrun() { Err(corrupt()) } else { Ok(self.0.consumed()) }
    }
}

pub fn decode(m: &mut Ppmd, r: &mut Reader) -> Result<u8> {
    if r.0.overrun() {
        return Err(corrupt());
    }
    m.decode(&mut r.0).ok_or_else(corrupt)
}

/// A range encoder of a block's bytes.
#[derive(Default)]
pub struct Writer(RangeEncoder);

impl Writer {
    pub fn new() -> Writer {
        Writer(RangeEncoder::new())
    }

    /// The bytes, flushed.
    pub fn finish(self) -> Vec<u8> {
        self.0.flush()
    }
}

pub fn encode(m: &mut Ppmd, w: &mut Writer, s: u8) {
    m.encode(&mut w.0, s);
}
