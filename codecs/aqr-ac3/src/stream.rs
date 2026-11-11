//! A byte stream of AC-3: syncframes (coded) and the bytes between them (kept as they are), in chunks that are coded each on
//! its own (a fresh model), so that they can be coded and decoded at once. The records of a chunk are, in order: a bit
//! (adaptive), 1 for a syncframe, which follows as `ac3::encode` codes it; 0 for a run of other bytes, its length (32 bits)
//! and the bytes (flat).
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
/// The bytes of a chunk, about: it ends with the record that passes them.
pub const CHUNK: usize = 16 << 20;

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

/// A record of a stream: its bytes `start..end`, and whether it is a syncframe (else a run of other bytes).
#[derive(Clone, Copy, Debug)]
pub struct Record {
    pub start: usize,
    pub end: usize,
    pub frame: bool,
}

/// The records of `data`: the syncframes whose checksums are right and that parse, and the runs between them.
pub fn records(data: &[u8]) -> Vec<Record> {
    let mut out = Vec::new();
    let (mut pos, mut raw) = (0usize, 0usize);
    while pos < data.len() {
        if let Some(n) = ac3::frame_len(&data[pos..])
            && pos + n <= data.len()
            && n <= FRAME_MAX
            && ac3::check(&data[pos..pos + n])
        {
            if raw < pos {
                out.push(Record { start: raw, end: pos, frame: false });
            }
            out.push(Record { start: pos, end: pos + n, frame: true });
            pos += n;
            raw = pos;
        } else {
            pos += 1;
        }
    }
    if raw < data.len() {
        out.push(Record { start: raw, end: data.len(), frame: false });
    }
    out
}

/// The chunks: ranges of records of about [`CHUNK`] bytes.
pub fn chunks(recs: &[Record]) -> Vec<core::ops::Range<usize>> {
    let (mut out, mut from, mut bytes) = (Vec::new(), 0usize, 0usize);
    for (i, r) in recs.iter().enumerate() {
        bytes += r.end - r.start;
        if bytes >= CHUNK {
            out.push(from..i + 1);
            from = i + 1;
            bytes = 0;
        }
    }
    if from < recs.len() {
        out.push(from..recs.len());
    }
    out
}

/// Codes the records `recs` of `data` as one chunk.
pub fn encode_chunk<C: Coder>(rc: &mut C, data: &[u8], recs: &[Record]) {
    let mut m = Model::new(MODEL_BITS);
    let mut ctx = Ctx::default();
    let mut prev = 0u32;
    for r in recs {
        if r.frame {
            kind_bit(rc, &mut m, prev, 1);
            prev = 1;
            ac3::encode(rc, &mut m, &mut ctx, &data[r.start..r.end]).expect("a checked frame");
        } else {
            kind_bit(rc, &mut m, prev, 0);
            prev = 0;
            flat32(rc, (r.end - r.start) as u32);
            for &b in &data[r.start..r.end] {
                for k in (0..8).rev() {
                    rc.flat((b >> k & 1) as u32);
                }
            }
        }
    }
}

/// The decoding state of a chunk: its records one after the other.
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

    /// The next bytes of the chunk into `out` (cleared first): a syncframe, or a piece of a run of other bytes.
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
