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

/// The range decoder of the bits.
struct Rc<'a> {
    d: &'a [u8],
    pos: usize,
    range: u32,
    code: u32,
}

impl<'a> Rc<'a> {
    fn new(d: &'a [u8]) -> Option<Self> {
        if d.len() < 5 || d[0] != 0 {
            return None;
        }
        let code = u32::from_be_bytes(d[1..5].try_into().unwrap());
        (code != u32::MAX).then_some(Rc { d, pos: 5, range: u32::MAX, code })
    }

    /// The next bit (normalized before it, as the decoder does). None where the stream ends
    /// before it.
    fn bit(&mut self, p: &mut u16) -> Option<bool> {
        if self.range < 1 << 24 {
            self.range <<= 8;
            self.code = self.code << 8 | *self.d.get(self.pos)? as u32;
            self.pos += 1;
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
fn candidate(prev: u8, b: u8) -> bool {
    b & 0xfe == 0xe8 || prev == 0x0f && b & 0xf0 == 0x80
}

/// The probability of a candidate's bit.
#[inline]
fn context(prev: u8, b: u8) -> usize {
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
    let mut rc = Rc::new(rc).ok_or(Error("bad range coder"))?;
    let mut probs = [1024u16; 258];
    let mut s = Streams { main: Vec::with_capacity(data.len()), ..Default::default() };
    let (mut i, mut prev) = (0, 0u8);
    while i < data.len() {
        let b = data[i];
        s.main.push(b);
        i += 1;
        if !candidate(prev, b) || !rc.bit(&mut probs[context(prev, b)]).ok_or_else(corrupt)? {
            prev = b;
            continue;
        }
        let t = data.get(i..i + 4).ok_or_else(corrupt)?;
        let abs = u32::from_le_bytes(t.try_into().unwrap()).wrapping_add(i as u32 + 4);
        let out = if b == 0xe8 { &mut s.call } else { &mut s.jump };
        out.extend(abs.to_be_bytes());
        prev = t[3];
        i += 4;
    }
    Ok(s)
}

/// The data of `size` bytes that the streams give. Fails where they do not give that, or are not
/// used up (but for what `rc` holds after its last bit).
pub fn decode(s: &Streams, rc: &[u8], size: usize) -> Result<Vec<u8>> {
    let mut rc = Rc::new(rc).ok_or(Error("bad range coder"))?;
    let mut probs = [1024u16; 258];
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
        let rel = u32::from_be_bytes(t.try_into().unwrap()).wrapping_sub(out.len() as u32 + 4);
        out.extend(rel.to_le_bytes());
        prev = (rel >> 24) as u8;
    }
    if out.len() != size || m != s.main.len() || c != s.call.len() || j != s.jump.len() {
        return Err(corrupt());
    }
    Ok(out)
}
