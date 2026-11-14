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

/// A record of a stream: its bytes `start..end`, and whether it is a syncframe (else a run of other bytes, of
/// [`RUN_MAX`] bytes at most: its length is coded in 32 bits).
#[derive(Clone, Copy, Debug)]
pub struct Record {
    pub start: usize,
    pub end: usize,
    pub frame: bool,
}

/// The most bytes of a record of a run of other bytes.
const RUN_MAX: usize = u32::MAX as usize;

/// Appends the run `start..end` of other bytes as records, cut where it is longer than [`RUN_MAX`].
fn push_run(out: &mut Vec<Record>, start: usize, end: usize) {
    let mut at = start;
    while at < end {
        let to = at + (end - at).min(RUN_MAX);
        out.push(Record { start: at, end: to, frame: false });
        at = to;
    }
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
                push_run(&mut out, raw, pos);
            }
            out.push(Record { start: pos, end: pos + n, frame: true });
            pos += n;
            raw = pos;
        } else {
            pos += 1;
        }
    }
    if raw < data.len() {
        push_run(&mut out, raw, data.len());
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
            rc.flat_bits((r.end - r.start) as u32, 32);
            for &b in &data[r.start..r.end] {
                rc.flat_bits(b as u32, 8);
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
            self.run = rc.flat_bits(0, 32);
            if self.run == 0 {
                return Err(Error::Parse);
            }
        }
        let mut out = out;
        out.clear();
        let n = (self.run as usize).min(RUN_PIECE);
        for _ in 0..n {
            out.push(rc.flat_bits(0, 8) as u8);
        }
        self.run -= n as u32;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn long_runs_are_cut() {
        let mut out = Vec::new();
        push_run(&mut out, 7, 7 + RUN_MAX + 5);
        let cuts: Vec<_> = out.iter().map(|r| (r.start, r.end)).collect();
        assert_eq!(cuts, [(7, 7 + RUN_MAX), (7 + RUN_MAX, 7 + RUN_MAX + 5)]);
        let mut out = Vec::new();
        push_run(&mut out, 0, RUN_MAX);
        assert_eq!(out.len(), 1);
        push_run(&mut out, 0, 3 * RUN_MAX + 1);
        assert!(out.iter().all(|r| r.end - r.start <= RUN_MAX && !r.frame));
        assert_eq!(out.iter().map(|r| r.end - r.start).sum::<usize>(), 4 * RUN_MAX + 1);
    }
}
