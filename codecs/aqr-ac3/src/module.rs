//! The code of the module `ac3-recons` (see `formula`), which needs no aqr-formula, so that aqr-modules builds it into
//! its Wasm module: an AC-3 byte stream from its coded form (`stream`).
use alloc::vec;
use alloc::vec::Vec;

use aqr_api::stream::{In, Pull, Put, Slice, Written};
use aqr_walk::rc::{Decoder, Source};

use crate::stream::{self, FRAME_MAX, RUN_PIECE};
use crate::{ModuleError as Error, Result, corrupt};

/// Pages of the windows of `ac3-recons` as a part module: of the input and of the output.
pub const WINDOW: u32 = 4;
/// Bytes of the buffer reading the input.
const BUF: usize = 16 << 10;
/// The capacity of the buffer of the bytes of a record.
const RECORD: usize = (if FRAME_MAX > RUN_PIECE { FRAME_MAX } else { RUN_PIECE }) + 16;
/// Bytes the model takes (`Model::new(MODEL_BITS)`: 4-byte probabilities).
const MODEL_BYTES: u64 = 4 << stream::MODEL_BITS;

/// The memory plan of `ac3-recons` as a part module ([`aqr_api::stack`]): what [`decode_part`] takes beyond its
/// initial memory (`initial` bytes) at most, whatever the input: the model, the record's buffer, the input's buffer, and
/// the state of a frame's block while it is walked.
pub fn plan(initial: u64) -> u64 {
    let mut m = aqr_api::stack::Sim::new(initial);
    m.take(MODEL_BYTES);
    m.take(RECORD as u64);
    m.take(BUF as u64);
    m.peak(crate::BLK_BYTES as u64);
    m.plan()
}

/// Bytes read from the input through a buffer.
struct Reader<'a> {
    src: &'a mut dyn Pull,
    buf: Vec<u8>,
    pos: usize,
    len: usize,
}

impl Source for Reader<'_> {
    fn byte(&mut self) -> u8 {
        if self.pos == self.len {
            self.len = self.src.pull(&mut self.buf);
            self.pos = 0;
            if self.len == 0 {
                return 0;
            }
        }
        self.pos += 1;
        self.buf[self.pos - 1]
    }
}

/// The stream of `size` bytes from its coded form, put to `out`.
pub fn run(coded: &mut dyn Pull, out: &mut dyn Put, size: u64) -> Result<()> {
    // (in the order of the plan)
    let mut sd = stream::Decoder::new();
    let mut buf: Vec<u8> = Vec::with_capacity(RECORD);
    let mut rc = Decoder::new(Reader { src: coded, buf: vec![0; BUF], pos: 0, len: 0 });
    let mut produced = 0u64;
    while produced < size {
        buf = sd.next(&mut rc, buf).map_err(|_| corrupt())?;
        produced += buf.len() as u64;
        if produced > size {
            return Err(corrupt());
        }
        out.put(&buf).ok_or(Error("output of another length"))?;
    }
    Ok(())
}

/// The module `ac3-recons` on a whole input, natively.
pub fn decode(_params: &[u8], inputs: &[&[u8]], len: u64, _threads: usize) -> Result<Vec<u8>> {
    let &[coded] = inputs else { return Err(corrupt()) };
    let mut out = Vec::with_capacity(len.min(1 << 32) as usize);
    run(&mut Slice(coded), &mut out, len)?;
    if out.len() as u64 != len {
        return Err(Error("output of another length"));
    }
    Ok(out)
}

/// The module `ac3-recons` as a part module (its entry point, of `aqr_api::entry!`).
pub fn decode_part(params_len: u64, in0_len: u64, size: u64) {
    aqr_api::finish(part(params_len, in0_len, size));
}

fn part(params_len: u64, in0_len: u64, size: u64) -> Result<()> {
    if params_len != 0 {
        return Err(corrupt());
    }
    let mut out = Written::new(size);
    run(&mut In::<0>::new(in0_len), &mut out, size)?;
    match out.total() == size {
        true => Ok(()),
        false => Err(Error("output of another length")),
    }
}
