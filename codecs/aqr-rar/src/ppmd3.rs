//! PPMd blocks of RAR 2.9 streams over `ppmd` (the model and RAR's range coder): a model
//! started or restarted, symbols decoded and encoded.

use crate::{Error, Result};

/// A model (placeholder until `ppmd` is in).
pub struct Ppmd;

/// Starts `m` afresh: of `order`, `mem` bytes.
pub fn start(_m: &mut Option<Ppmd>, _order: u32, _mem: u32) -> Result<()> {
    Err(Error("PPMd not yet"))
}

pub struct Reader;

impl Reader {
    pub fn new(_d: &[u8]) -> Result<Reader> {
        Err(Error("PPMd not yet"))
    }
    /// The bytes read, an error where past the data.
    pub fn used(&self) -> Result<usize> {
        Err(Error("PPMd not yet"))
    }
}

pub fn decode(_m: &mut Ppmd, _r: &mut Reader) -> Result<u8> {
    Err(Error("PPMd not yet"))
}

#[derive(Default)]
pub struct Writer;

impl Writer {
    pub fn new() -> Writer {
        Writer
    }
    pub fn finish(self) -> Vec<u8> {
        Vec::new()
    }
}

pub fn encode(_m: &mut Ppmd, _w: &mut Writer, _s: u8) {}
