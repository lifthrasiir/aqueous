//! The module's streams taken in pieces: the inputs read in order ([`Pull`], [`Reader`]) and the
//! output put in order ([`Put`], [`Writer`]), so that a part module holds a piece of each and
//! nothing of their lengths (FORMAT.md, "The module").

use crate::{Result, corrupt};

/// Bytes of the pieces in which a stream is taken or put.
pub const PIECE: usize = 1 << 16;

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

/// A stream read a byte at a time out of a piece of it ([`PIECE`]).
pub struct Reader<'a> {
    src: &'a mut dyn Pull,
    buf: Vec<u8>,
    at: usize,
    end: usize,
}

impl<'a> Reader<'a> {
    pub fn new(src: &'a mut dyn Pull) -> Reader<'a> {
        Reader { src, buf: vec![0; PIECE], at: 0, end: 0 }
    }

    /// Takes the next piece, if any is left; whether it did.
    #[cold]
    fn refill(&mut self) -> bool {
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

    /// The next varint (at most ten bytes; bits beyond 64 are an error).
    #[inline]
    pub fn num(&mut self) -> Result<u64> {
        let b = self.byte().ok_or_else(corrupt)?;
        if b < 0x80 {
            return Ok(b as u64);
        }
        self.num_slow(b)
    }

    #[cold]
    fn num_slow(&mut self, first: u8) -> Result<u64> {
        let mut v = (first & 0x7f) as u64;
        for shift in (7..64).step_by(7) {
            let b = self.byte().ok_or_else(corrupt)?;
            if shift == 63 && b > 1 {
                return Err(corrupt());
            }
            v |= ((b & 0x7f) as u64) << shift;
            if b < 0x80 {
                return Ok(v);
            }
        }
        Err(corrupt())
    }

    /// Copies the bytes up to the next LF to `w`, and takes the LF; fails at the end of the
    /// stream before one.
    pub fn line_into(&mut self, w: &mut Writer) -> Result<()> {
        loop {
            if self.at == self.end && !self.refill() {
                return Err(corrupt());
            }
            let piece = &self.buf[self.at..self.end];
            match piece.iter().position(|&b| b == b'\n') {
                Some(i) => {
                    w.put(&piece[..i])?;
                    self.at += i + 1;
                    return Ok(());
                }
                None => {
                    w.put(piece)?;
                    self.at = self.end;
                }
            }
        }
    }
}

/// A stream put in order.
pub trait Put {
    /// Appends `d`.
    fn put(&mut self, d: &[u8]);
}

/// A vector as a [`Put`], natively.
impl Put for Vec<u8> {
    fn put(&mut self, d: &[u8]) {
        self.extend_from_slice(d);
    }
}

/// An output of `len` bytes in all, put a piece at a time ([`PIECE`]). Bytes past `len` are
/// dropped and tell ([`Writer::over`]).
pub struct Writer<'a> {
    sink: &'a mut dyn Put,
    buf: Vec<u8>,
    /// The bytes put to the sink.
    flushed: u64,
    len: u64,
    over: bool,
}

impl<'a> Writer<'a> {
    pub fn new(sink: &'a mut dyn Put, len: u64) -> Writer<'a> {
        Writer { sink, buf: Vec::with_capacity(PIECE), flushed: 0, len, over: false }
    }

    fn flush(&mut self) {
        if self.flushed + self.buf.len() as u64 > self.len {
            self.over = true;
        } else {
            self.sink.put(&self.buf);
            self.flushed += self.buf.len() as u64;
        }
        self.buf.clear();
    }

    /// The bytes that fit in the piece, after putting it if it is full: at least one.
    #[inline(always)]
    pub fn room(&mut self) -> usize {
        if self.buf.len() == PIECE {
            self.flush();
        }
        PIECE - self.buf.len()
    }

    /// Appends `b`; the caller has made `room` for it.
    #[inline(always)]
    pub fn push(&mut self, b: u8) {
        debug_assert!(self.buf.len() < PIECE);
        self.buf.push(b);
    }

    /// Appends `n` copies of `b`, within the `room`.
    #[inline]
    pub fn repeat(&mut self, b: u8, n: usize) {
        debug_assert!(self.buf.len() + n <= PIECE);
        self.buf.resize(self.buf.len() + n, b);
    }

    /// Appends `d`.
    pub fn put(&mut self, mut d: &[u8]) -> Result<()> {
        while !d.is_empty() {
            let k = d.len().min(self.room());
            self.buf.extend_from_slice(&d[..k]);
            d = &d[k..];
            if self.over {
                return Err(corrupt());
            }
        }
        Ok(())
    }

    /// Whether more than the length was put.
    pub fn over(&self) -> bool {
        self.over
    }

    /// Puts what is left, and tells whether exactly the length was put in all.
    pub fn finish(mut self) -> Result<()> {
        self.flush();
        match !self.over && self.flushed == self.len {
            true => Ok(()),
            false => Err(corrupt()),
        }
    }

    /// The bytes put so far, flushed or not.
    pub fn total(&self) -> u64 {
        self.flushed + self.buf.len() as u64
    }
}
