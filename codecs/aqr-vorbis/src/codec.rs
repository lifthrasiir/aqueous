//! A stream's packets, each after its length (16 bits, little-endian, as FSB5 frames them), as
//! a record of their values coded under the [`Model`], and back (FORMAT.md §4).

use aqr_common::arith::{ArithDecoder, ArithEncoder};

use crate::bits::Writer;
use crate::cm::{Coder, Counter, p16};
use crate::model::Model;
use crate::packet::{self, End, Field, Source};
use crate::setup::Setup;
use crate::{Error, Result};

/// The record's decisions other than the model's: whether a packet follows, whether it is cut
/// short, its values' count where it is, and its tail.
struct Frame {
    more: Counter,
    short: Counter,
    count: [Counter; 64],
    tail: [Counter; 2],
}

impl Frame {
    fn new() -> Frame {
        Frame {
            more: Counter::at(65000, 4),
            short: Counter::at(64, 4),
            count: [Counter::default(); 64],
            tail: [Counter::at(64, 4); 2],
        }
    }

    fn bit(c: &mut dyn Coder, k: &mut Counter, bit: bool) -> bool {
        let b = c.code(p16(k.p12()), bit);
        k.update(b);
        b
    }

    /// A number of up to 32 bits: its bit length in unary (counters by position), then its bits
    /// below the leading one.
    fn number(&mut self, c: &mut dyn Coder, v: u32) -> u32 {
        let len = 32 - v.leading_zeros();
        let mut n = 0;
        while n < 32 && Frame::bit(c, &mut self.count[n as usize], n < len) {
            n += 1;
        }
        if n == 0 {
            return 0;
        }
        let mut out = 1;
        for i in (0..n - 1).rev() {
            out = out << 1 | Frame::bit(c, &mut self.count[32 + i.min(31) as usize], v >> i & 1 == 1) as u32;
        }
        out
    }
}

/// Gives the walk a packet's values, coding each.
struct Encode<'a, 'm> {
    model: &'m mut Model<'a>,
    c: &'m mut ArithEncoder,
    values: std::slice::Iter<'m, u32>,
}

impl Source for Encode<'_, '_> {
    fn bits(&mut self, f: Field, n: u32) -> Option<u32> {
        let &v = self.values.next()?;
        Some(self.model.bits(self.c, f, n, v))
    }

    fn entry(&mut self, f: Field, book: usize) -> Option<u32> {
        let &e = self.values.next()?;
        Some(self.model.entry(self.c, f, book, e))
    }
}

/// The record of the packets at the start of `d` (each after its length; up to the end of `d`
/// or a length of 0, which is not taken): the bytes taken and the record, or None where a
/// packet does not parse under `setup`.
pub fn encode(setup: &Setup, d: &[u8]) -> Option<(usize, Vec<u8>)> {
    let mut model = Model::new(setup);
    let mut frame = Frame::new();
    let mut c = ArithEncoder::new(Vec::new());
    let mut at = 0;
    while at + 2 <= d.len() {
        let n = u16::from_le_bytes([d[at], d[at + 1]]) as usize;
        if n == 0 {
            break;
        }
        let p = d.get(at + 2..at + 2 + n)?;
        let parsed = packet::parse(setup, p).ok()?;
        Frame::bit(&mut c, &mut frame.more, true);
        Frame::bit(&mut c, &mut frame.short, parsed.short);
        if parsed.short {
            frame.number(&mut c, parsed.values.len() as u32);
        }
        let mut e = Encode { model: &mut model, c: &mut c, values: parsed.values.iter() };
        let end = packet::walk(setup, &mut e).ok()?;
        debug_assert_eq!(end == End::Short, parsed.short);
        model.end_packet();
        // the tail: where the packet is whole, its bytes past the least, then all its bits
        if !parsed.short {
            let least = (parsed.tail.len() % 8) as u32;
            frame.number(&mut c, (parsed.tail.len() as u32 - least) / 8);
        } else {
            frame.number(&mut c, parsed.tail.len() as u32);
        }
        for &b in &parsed.tail {
            Frame::bit(&mut c, &mut frame.tail[parsed.short as usize], b == 1);
        }
        at += 2 + n;
    }
    Frame::bit(&mut c, &mut frame.more, false);
    Some((at, c.finish()))
}

/// Takes a packet's values from the model, writing their bits; up to `left` values where the
/// packet is cut short.
struct Decode<'a, 'm, 'r> {
    setup: &'a Setup,
    model: &'m mut Model<'a>,
    c: &'m mut ArithDecoder<'r>,
    w: Writer,
    left: Option<u32>,
    bad: bool,
}

impl Decode<'_, '_, '_> {
    fn take(&mut self) -> bool {
        match &mut self.left {
            Some(0) => false,
            Some(n) => {
                *n -= 1;
                true
            }
            None => true,
        }
    }
}

impl Source for Decode<'_, '_, '_> {
    fn bits(&mut self, f: Field, n: u32) -> Option<u32> {
        if !self.take() {
            return None;
        }
        let v = self.model.bits(self.c, f, n, 0);
        if n < 32 && v >> n != 0 {
            self.bad = true;
            return None;
        }
        self.w.bits(v, n);
        Some(v)
    }

    fn entry(&mut self, f: Field, book: usize) -> Option<u32> {
        if !self.take() {
            return None;
        }
        let e = self.model.entry(self.c, f, book, 0);
        let b = &self.setup.codebooks[book];
        if !b.used(e) {
            self.bad = true;
            return None;
        }
        b.write(e, &mut self.w);
        Some(e)
    }
}

/// The packets of `record`, each after its length, `len` bytes in all.
pub fn decode(setup: &Setup, record: &[u8], len: usize) -> Result<Vec<u8>> {
    let corrupt = || Error("corrupt record");
    let mut model = Model::new(setup);
    let mut frame = Frame::new();
    let mut c = ArithDecoder::new(record);
    let mut out = Vec::with_capacity(len);
    while Frame::bit(&mut c, &mut frame.more, false) {
        let short = Frame::bit(&mut c, &mut frame.short, false);
        let left = if short { Some(frame.number(&mut c, 0)) } else { None };
        let mut d = Decode { setup, model: &mut model, c: &mut c, w: Writer::new(), left, bad: false };
        let end = packet::walk(setup, &mut d)?;
        let (bad, rest, mut w) = (d.bad, d.left, d.w);
        if bad || (end == End::Short) != short || rest.is_some_and(|n| n != 0) {
            return Err(corrupt());
        }
        model.end_packet();
        let tail = if short {
            frame.number(&mut c, 0) as usize
        } else {
            (8 - w.pos() % 8) % 8 + 8 * frame.number(&mut c, 0) as usize
        };
        // (the packet must fit in what is left of the output)
        if out.len() + 2 + (w.pos() + tail).div_ceil(8) > len {
            return Err(corrupt());
        }
        for _ in 0..tail {
            let b = Frame::bit(&mut c, &mut frame.tail[short as usize], false);
            w.bit(b as u32);
        }
        if !w.pos().is_multiple_of(8)
            || out.len() + 2 + w.pos() / 8 > len
            || w.pos() / 8 > u16::MAX as usize
            || w.pos() == 0
        {
            return Err(corrupt());
        }
        let p = w.finish();
        out.extend_from_slice(&(p.len() as u16).to_le_bytes());
        out.extend_from_slice(&p);
        if c.overrun() {
            return Err(corrupt());
        }
    }
    if out.len() != len {
        return Err(corrupt());
    }
    Ok(out)
}
