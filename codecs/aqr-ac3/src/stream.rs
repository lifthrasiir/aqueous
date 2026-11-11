//! A byte stream of AC-3: syncframes (coded) and the bytes between them (kept as they are). The records are, in order of
//! the stream: a bit (adaptive), 1 for a syncframe, which follows as `ac3::encode` codes it; 0 for a run of other bytes,
//! its length (32 bits) and the bytes (flat).
use alloc::vec::Vec;

use aqr_walk::Error;
use aqr_walk::model::Model;
use aqr_walk::rc::Coder;

use crate::ac3::{self, Ctx};

/// The bits of the model's table (2^21 probabilities of 4 bytes).
pub const MODEL_BITS: u32 = 21;
/// The most bytes of a syncframe (a 640 kbps frame at 32 kHz).
pub const FRAME_MAX: usize = 3840;
/// Bytes of a run of other bytes decoded at once.
pub const RUN_PIECE: usize = 4096;

const KIND: u32 = 0xFFFF_0001;

fn kind_bit<C: Coder>(rc: &mut C, m: &mut Model, prev: u32, b: u32) -> u32 {
    rc.bit(m.slot(KIND, prev), b)
}

fn flat32<C: Coder>(rc: &mut C, v: u32) -> u32 {
    let mut r = 0;
    for i in (0..32).rev() {
        r = r << 1 | rc.flat(v >> i & 1);
    }
    r
}

/// Codes `data`: its syncframes (whose checksums are right and that parse) by the model, the rest as it is.
pub fn encode<C: Coder>(rc: &mut C, data: &[u8]) {
    let mut m = Model::new(MODEL_BITS);
    let mut ctx = Ctx::default();
    let (mut pos, mut raw, mut prev) = (0usize, 0usize, 0u32);
    let flush = |rc: &mut C, m: &mut Model, from: usize, to: usize, prev: &mut u32| {
        kind_bit(rc, m, *prev, 0);
        *prev = 0;
        flat32(rc, (to - from) as u32);
        for &b in &data[from..to] {
            for k in (0..8).rev() {
                rc.flat((b >> k & 1) as u32);
            }
        }
    };
    while pos < data.len() {
        if let Some(n) = ac3::frame_len(&data[pos..])
            && pos + n <= data.len()
            && n <= FRAME_MAX
            && ac3::check(&data[pos..pos + n])
        {
            if raw < pos {
                flush(rc, &mut m, raw, pos, &mut prev);
            }
            kind_bit(rc, &mut m, prev, 1);
            prev = 1;
            ac3::encode(rc, &mut m, &mut ctx, &data[pos..pos + n]).expect("a checked frame");
            pos += n;
            raw = pos;
        } else {
            pos += 1;
        }
    }
    if raw < data.len() {
        flush(rc, &mut m, raw, data.len(), &mut prev);
    }
}

/// The decoding state of a stream: its records one after the other.
pub struct Decoder {
    m: Model,
    ctx: Ctx,
    prev: u32,
    run: u32,
}

impl Default for Decoder {
    fn default() -> Self {
        Decoder { m: Model::new(MODEL_BITS), ctx: Ctx::default(), prev: 0, run: 0 }
    }
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// The next bytes of the stream into `out` (cleared first): a syncframe, or a piece of a run of other bytes.
    /// `out` should have the capacity of [`FRAME_MAX`] and [`RUN_PIECE`] bytes at least.
    pub fn next<C: Coder>(&mut self, rc: &mut C, out: Vec<u8>) -> Result<Vec<u8>, Error> {
        if self.run == 0 {
            let k = kind_bit(rc, &mut self.m, self.prev, 0);
            self.prev = k;
            if k == 1 {
                return ac3::decode(rc, &mut self.m, &mut self.ctx, out);
            }
            self.run = flat32(rc, 0);
            if self.run == 0 {
                return Err(Error::Parse);
            }
        }
        let mut out = out;
        out.clear();
        let n = (self.run as usize).min(RUN_PIECE);
        for _ in 0..n {
            let mut v = 0u8;
            for _ in 0..8 {
                v = v << 1 | rc.flat(0) as u8;
            }
            out.push(v);
        }
        self.run -= n as u32;
        Ok(out)
    }
}
