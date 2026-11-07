//! The module's streams taken in pieces: the inputs read in order ([`Pull`], [`Reader`]) and the
//! output put in order ([`Put`], [`Writer`]), so that a part module holds a piece of each and
//! nothing of their lengths (FORMAT.md, "The module"). The reading and the sink are
//! [`aqr_api::stream`]'s.

use crate::{Result, corrupt};
pub use aqr_api::stream::{Pull, Put, Reader, Slice};

/// Bytes of the pieces in which a stream is taken or put.
pub const PIECE: usize = 1 << 16;

/// Lines taken out of a [`Reader`].
pub trait Lines {
    /// Copies the bytes up to the next LF to `w`, and takes the LF; fails at the end of the
    /// stream before one.
    fn line_into(&mut self, w: &mut Writer) -> Result<()>;
}

impl Lines for Reader<'_> {
    fn line_into(&mut self, w: &mut Writer) -> Result<()> {
        let mut put = Ok(());
        let found = self.until(b'\n', |piece| {
            put = w.put(piece);
            put.is_ok()
        });
        put.and(found.then_some(()).ok_or_else(corrupt))
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
            // (a sink with no room is the output of another length)
            match self.sink.put(&self.buf) {
                Some(()) => self.flushed += self.buf.len() as u64,
                None => self.over = true,
            }
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
