//! PPMd blocks of RAR 2.9 streams over `ppmd` (the model and RAR's range coder): a model
//! started or restarted, symbols decoded and encoded.

use crate::ppmd::{Model, RangeDecoder, RangeEncoder};
use crate::{Error, Result, corrupt};

pub type Ppmd = Model;

/// A model holding `cap` bytes of memory, for streams whose blocks use no more ([`start`] then
/// takes none from the allocator).
pub fn with_capacity(cap: u32) -> Result<Ppmd> {
    Model::new(cap).ok_or(Error("a PPMd model of no memory to have"))
}

/// Starts `m` afresh: of `order`, `mem` bytes (its memory kept where it holds that much).
pub fn start(m: &mut Option<Ppmd>, order: u32, mem: u32) -> Result<()> {
    if !m.as_mut().is_some_and(|model| model.set_size(mem)) {
        *m = None;
        *m = Some(with_capacity(mem)?);
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

    /// Writing onto the end of `out`.
    pub fn with_output(out: Vec<u8>) -> Writer {
        Writer(RangeEncoder::with_output(out))
    }

    /// The bytes, flushed.
    pub fn finish(self) -> Vec<u8> {
        self.0.flush()
    }
}

pub fn encode(m: &mut Ppmd, w: &mut Writer, s: u8) {
    m.encode(&mut w.0, s);
}
