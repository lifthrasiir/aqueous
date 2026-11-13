//! BCJ2 (7z's): the 32-bit targets of x86 calls (E8), jumps (E9) and conditional jumps (0F 8x)
//! taken out of the code (the *main* stream), made absolute, into a stream of calls and one of
//! jumps (big-endian), where a bit of a range coder (the fourth stream) says so: its
//! probability is by the opcode, and for E8 by the byte before it. As the LZMA SDK's decoder
//! (`Bcj2.c`, public domain) reads the streams, a candidate at the end of the data still has its
//! bit, and the byte after a target made relative again follows the target's last byte.
//!
//! The encoder's choice of the bits is not reproduced: [`encode`] reads them from the range
//! coder's stream, as the decoder does, so that the other three streams are rebuilt from the
//! data and that stream (kept as it is).

use crate::{Error, Result, corrupt};

/// The bytes of the range coder's stream, in order.
pub trait RcBytes {
    /// The next byte, None at the end of the stream.
    fn next(&mut self) -> Option<u8>;
}

/// A stream held in memory.
struct Slice<'a> {
    d: &'a [u8],
    pos: usize,
}

impl RcBytes for Slice<'_> {
    fn next(&mut self) -> Option<u8> {
        let b = *self.d.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }
}

/// The range decoder of the bits.
pub struct Rc<B> {
    bytes: B,
    range: u32,
    code: u32,
}

impl<B: RcBytes> Rc<B> {
    /// The decoder of a stream, which starts with a byte 0 and `code` (not `2^32 - 1`). None where
    /// it is not.
    pub fn new(mut bytes: B) -> Option<Self> {
        if bytes.next()? != 0 {
            return None;
        }
        let mut code = 0u32;
        for _ in 0..4 {
            code = code << 8 | bytes.next()? as u32;
        }
        (code != u32::MAX).then_some(Rc { bytes, range: u32::MAX, code })
    }

    /// The next bit (normalized before it, as the decoder does). None where the stream ends
    /// before it.
    pub fn bit(&mut self, p: &mut u16) -> Option<bool> {
        if self.range < 1 << 24 {
            self.range <<= 8;
            self.code = self.code << 8 | self.bytes.next()? as u32;
        }
        let bound = (self.range >> 11) * *p as u32;
        Some(if self.code < bound {
            self.range = bound;
            *p += (2048 - *p) >> 5;
            false
        } else {
            self.range -= bound;
            self.code -= bound;
            *p -= *p >> 5;
            true
        })
    }
}

/// Whether `b` after `prev` is an opcode whose target may be taken out.
#[inline]
pub(crate) fn candidate(prev: u8, b: u8) -> bool {
    b & 0xfe == 0xe8 || prev == 0x0f && b & 0xf0 == 0x80
}

/// The probabilities of the bits, each by its [`context`], at first one half (of 2048).
#[inline(always)]
pub(crate) fn fresh_probs() -> [u16; 2 + 256] {
    [1024; 2 + 256]
}

/// The probability of a candidate's bit.
#[inline]
pub(crate) fn context(prev: u8, b: u8) -> usize {
    match b {
        0xe8 => 2 + prev as usize,
        0xe9 => 1,
        _ => 0,
    }
}

/// The main, call and jump streams.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Streams {
    pub main: Vec<u8>,
    pub call: Vec<u8>,
    pub jump: Vec<u8>,
}

/// The streams of `data` with the bits that `rc` codes. Fails where `rc` ends before a bit, or a
/// bit takes out a target that is not there.
pub fn encode(data: &[u8], rc: &[u8]) -> Result<Streams> {
    let mut s = Streams { main: Vec::with_capacity(data.len()), ..Default::default() };
    walk(data, rc, |b| s.main.push(b), |call, t| if call { &mut s.call } else { &mut s.jump }.extend(t))?;
    Ok(s)
}

/// [`encode`]'s streams one after another (the main, call and jump streams), in a vector of
/// exactly `data.len()` bytes, which they take: their lengths found first, then each written
/// where it goes.
pub fn encode_joined(data: &[u8], rc: &[u8]) -> Result<Vec<u8>> {
    let (mut calls, mut jumps) = (0usize, 0usize);
    walk(data, rc, |_| {}, |call, _| *if call { &mut calls } else { &mut jumps } += 4)?;
    let mut out = vec![0; data.len()];
    let (main_s, rest) = out.split_at_mut(data.len() - calls - jumps);
    let (call_s, jump_s) = rest.split_at_mut(calls);
    let (mut m, mut c, mut j) = (0, 0, 0);
    walk(
        data,
        rc,
        |b| {
            main_s[m] = b;
            m += 1;
        },
        |call, t| {
            let (s, at) = if call { (&mut *call_s, &mut c) } else { (&mut *jump_s, &mut j) };
            s[*at..*at + 4].copy_from_slice(&t);
            *at += 4;
        },
    )?;
    Ok(out)
}

/// The decisions of [`encode`] on `data` with the bits that `rc` codes: each byte of the main
/// stream to `main`, each target taken out to `target` (whether a call's, and its bytes).
fn walk(data: &[u8], rc: &[u8], mut main: impl FnMut(u8), mut target: impl FnMut(bool, [u8; 4])) -> Result<()> {
    let mut rc = Rc::new(Slice { d: rc, pos: 0 }).ok_or(Error("bad range coder"))?;
    let mut probs = fresh_probs();
    let (mut i, mut prev) = (0, 0u8);
    while i < data.len() {
        let b = data[i];
        main(b);
        i += 1;
        if !candidate(prev, b) || !rc.bit(&mut probs[context(prev, b)]).ok_or_else(corrupt)? {
            prev = b;
            continue;
        }
        let t = data.get(i..i + 4).ok_or_else(corrupt)?;
        let abs = u32::from_le_bytes(t.try_into().unwrap()).wrapping_add((i as u32).wrapping_add(4));
        target(b == 0xe8, abs.to_be_bytes());
        prev = t[3];
        i += 4;
    }
    Ok(())
}

/// The data of `size` bytes that the streams give. Fails where they do not give that, or are not
/// used up (but for what `rc` holds after its last bit).
pub fn decode(s: &Streams, rc: &[u8], size: usize) -> Result<Vec<u8>> {
    let mut rc = Rc::new(Slice { d: rc, pos: 0 }).ok_or(Error("bad range coder"))?;
    let mut probs = fresh_probs();
    let mut out = Vec::with_capacity(size);
    let (mut m, mut c, mut j) = (0, 0, 0);
    let mut prev = 0u8;
    while out.len() < size {
        let Some(&b) = s.main.get(m) else { break };
        m += 1;
        out.push(b);
        if !candidate(prev, b) || !rc.bit(&mut probs[context(prev, b)]).ok_or_else(corrupt)? {
            prev = b;
            continue;
        }
        let (src, at) = if b == 0xe8 { (&s.call, &mut c) } else { (&s.jump, &mut j) };
        let t = src.get(*at..*at + 4).ok_or_else(corrupt)?;
        *at += 4;
        if out.len() + 4 > size {
            return Err(corrupt());
        }
        let rel = u32::from_be_bytes(t.try_into().unwrap()).wrapping_sub((out.len() as u32).wrapping_add(4));
        out.extend(rel.to_le_bytes());
        prev = (rel >> 24) as u8;
    }
    if out.len() != size || m != s.main.len() || c != s.call.len() || j != s.jump.len() {
        return Err(corrupt());
    }
    Ok(out)
}
