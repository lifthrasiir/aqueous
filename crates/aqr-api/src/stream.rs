//! The streams of a part module taken in pieces: the inputs read in order ([`Pull`], [`Reader`])
//! and the output put in order ([`Put`]), through the windows of the memories ([`In`],
//! [`Written`]), so that a part module holds a piece of each and nothing of their lengths.
//! Natively, a byte slice is a [`Pull`] ([`Slice`]) and a vector a [`Put`].

use alloc::vec;
use alloc::vec::Vec;

use crate::{Input, Memory, Output};

/// A stream of bytes taken in order.
pub trait Pull {
    /// Copies the next bytes into the front of `buf` (at least one, as many as it has room for
    /// at most) and tells how many; 0 where the stream has ended.
    fn pull(&mut self, buf: &mut [u8]) -> usize;
}

/// A byte slice as a [`Pull`], natively.
pub struct Slice<'a>(pub &'a [u8]);

impl Pull for Slice<'_> {
    fn pull(&mut self, buf: &mut [u8]) -> usize {
        let n = self.0.len().min(buf.len());
        buf[..n].copy_from_slice(&self.0[..n]);
        self.0 = &self.0[n..];
        n
    }
}

/// A stream put in order.
pub trait Put {
    /// Appends `d`. None where there is no room for it (the output is of another length).
    fn put(&mut self, d: &[u8]) -> Option<()>;
}

/// A vector as a [`Put`], natively.
impl Put for Vec<u8> {
    fn put(&mut self, d: &[u8]) -> Option<()> {
        self.extend_from_slice(d);
        Some(())
    }
}

/// A varint that is not one (more than ten bytes, bits beyond 64) or ends with the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Corrupt;

/// A stream read a byte at a time out of a piece of it of a given size.
pub struct Reader<'a> {
    src: &'a mut dyn Pull,
    buf: Vec<u8>,
    at: usize,
    end: usize,
    /// The bytes of the pieces before this one.
    before: u64,
}

impl<'a> Reader<'a> {
    pub fn new(src: &'a mut dyn Pull, size: usize) -> Reader<'a> {
        Reader { src, buf: vec![0; size], at: 0, end: 0, before: 0 }
    }

    /// The bytes read (or dropped) so far.
    pub fn taken(&self) -> u64 {
        self.before + self.at as u64
    }

    /// Takes the next piece, if any is left; whether it did.
    #[cold]
    fn refill(&mut self) -> bool {
        self.before += self.end as u64;
        self.at = 0;
        self.end = self.src.pull(&mut self.buf);
        self.end > 0
    }

    /// The next byte, or `None` at the end of the stream.
    #[inline(always)]
    pub fn byte(&mut self) -> Option<u8> {
        if self.at == self.end && !self.refill() {
            return None;
        }
        let b = self.buf[self.at];
        self.at += 1;
        Some(b)
    }

    /// Whether every byte was read.
    pub fn done(&mut self) -> bool {
        self.at == self.end && !self.refill()
    }

    /// Drops the next `n` bytes; None where the stream ends before.
    pub fn skip(&mut self, mut n: u64) -> Option<()> {
        while n > 0 {
            if self.at == self.end && !self.refill() {
                return None;
            }
            let k = ((self.end - self.at) as u64).min(n);
            self.at += k as usize;
            n -= k;
        }
        Some(())
    }

    /// Hands `f` the next `n` bytes in pieces; None where the stream ends before them, or where
    /// `f` is None for a piece (it stops there).
    pub fn each(&mut self, mut n: u64, mut f: impl FnMut(&[u8]) -> Option<()>) -> Option<()> {
        while n > 0 {
            if self.at == self.end && !self.refill() {
                return None;
            }
            let k = ((self.end - self.at) as u64).min(n) as usize;
            f(&self.buf[self.at..self.at + k])?;
            self.at += k;
            n -= k as u64;
        }
        Some(())
    }

    /// Fills `out` with the next bytes; None where the stream ends before.
    pub fn exact(&mut self, out: &mut [u8]) -> Option<()> {
        let mut at = 0;
        self.each(out.len() as u64, |b| {
            out[at..at + b.len()].copy_from_slice(b);
            at += b.len();
            Some(())
        })
    }

    /// The next varint (at most ten bytes; bits beyond 64 are an error).
    #[inline]
    pub fn num(&mut self) -> Result<u64, Corrupt> {
        let b = self.byte().ok_or(Corrupt)?;
        if b < 0x80 {
            return Ok(b as u64);
        }
        self.num_slow(b)
    }

    #[cold]
    fn num_slow(&mut self, first: u8) -> Result<u64, Corrupt> {
        let mut v = (first & 0x7f) as u64;
        for shift in (7..64).step_by(7) {
            let b = self.byte().ok_or(Corrupt)?;
            if shift == 63 && b > 1 {
                return Err(Corrupt);
            }
            v |= ((b & 0x7f) as u64) << shift;
            if b < 0x80 {
                return Ok(v);
            }
        }
        Err(Corrupt)
    }

    /// Hands `each` the bytes up to the next `stop` (in pieces, and not the `stop`), and takes
    /// the `stop`; false where `each` is false for a piece (it stops there), or the stream ends
    /// before a `stop`.
    pub fn until(&mut self, stop: u8, mut each: impl FnMut(&[u8]) -> bool) -> bool {
        loop {
            if self.at == self.end && !self.refill() {
                return false;
            }
            let piece = &self.buf[self.at..self.end];
            match piece.iter().position(|&b| b == stop) {
                Some(i) => {
                    let ok = each(&piece[..i]);
                    self.at += i + 1;
                    return ok;
                }
                None => {
                    if !each(piece) {
                        return false;
                    }
                    self.at = self.end;
                }
            }
        }
    }
}

/// The `K`-th input of a part module read in order, through its window, `len` bytes in all.
pub struct In<const K: u32> {
    at: u64,
    len: u64,
}

impl<const K: u32> In<K> {
    pub fn new(len: u64) -> Self {
        In { at: 0, len }
    }
}

impl<const K: u32> Pull for In<K>
where
    Input<K>: Memory,
{
    fn pull(&mut self, buf: &mut [u8]) -> usize {
        // (an empty input has no pages)
        if self.at == self.len {
            return 0;
        }
        let w = Input::<K>::size();
        let k = (self.len - self.at).min(buf.len() as u64).min(w - self.at % w) as usize;
        Input::<K>::request(self.at + k as u64);
        Input::<K>::read(self.at % w, &mut buf[..k]);
        Input::<K>::release(self.at + k as u64);
        self.at += k as u64;
        k
    }
}

/// The output of a part module, `size` bytes in all, written through its window.
pub struct Written {
    at: u64,
    size: u64,
}

impl Written {
    pub fn new(size: u64) -> Self {
        Written { at: 0, size }
    }

    /// The bytes written so far.
    pub fn total(&self) -> u64 {
        self.at
    }
}

impl Put for Written {
    fn put(&mut self, mut d: &[u8]) -> Option<()> {
        if d.len() as u64 > self.size - self.at {
            return None;
        }
        let w = Output::<0>::size();
        while !d.is_empty() {
            let k = (d.len() as u64).min(w - self.at % w) as usize;
            Output::<0>::write(self.at % w, &d[..k]);
            self.at += k as u64;
            Output::<0>::commit(self.at);
            d = &d[k..];
        }
        Some(())
    }
}
