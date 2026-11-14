//! An input of a module read through a buffer, with a limit for the bytes of one chunk of a coded stream.
use crate::Error;
use crate::rc::Source;
use alloc::vec;
use alloc::vec::Vec;
use aqr_api::stream::{Pull, Put};

pub struct Reader<'a> {
    src: &'a mut dyn Pull,
    buf: Vec<u8>,
    pos: usize,
    len: usize,
    /// Bytes that may still be read before the limit ([`Reader::limit`]).
    left: u64,
}

impl<'a> Reader<'a> {
    pub fn new(src: &'a mut dyn Pull, buffer: usize) -> Self {
        Reader { src, buf: vec![0; buffer], pos: 0, len: 0, left: u64::MAX }
    }

    fn fill(&mut self) -> bool {
        if self.pos == self.len {
            self.len = self.src.pull(&mut self.buf);
            self.pos = 0;
        }
        self.len != 0
    }

    /// The next byte, or None at the end of the input or the limit.
    // (not an iterator: the limit and the refills are its own)
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<u8> {
        if self.left == 0 || !self.fill() {
            return None;
        }
        self.pos += 1;
        self.left -= 1;
        Some(self.buf[self.pos - 1])
    }

    /// A LEB128 number of at most ten bytes (as `aqr_common::varint::get` reads it), or None at the end (before its
    /// first byte); `Err` if it is cut or too long.
    pub fn varint(&mut self) -> Result<Option<u64>, Error> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let Some(b) = self.next() else {
                return if shift == 0 { Ok(None) } else { Err(Error::Parse) };
            };
            if shift == 63 && b > 1 {
                return Err(Error::Parse);
            }
            v |= ((b & 0x7f) as u64) << shift;
            if b < 0x80 {
                return Ok(Some(v));
            }
        }
        Err(Error::Parse)
    }

    /// `n` bytes put to `out`; false if the input or the limit ends first or the output refuses them.
    pub fn copy(&mut self, mut n: u64, out: &mut dyn Put) -> bool {
        while n > 0 {
            if self.left == 0 || !self.fill() {
                return false;
            }
            let k = (self.len - self.pos).min(n.min(self.left).min(usize::MAX as u64) as usize);
            if out.put(&self.buf[self.pos..self.pos + k]).is_none() {
                return false;
            }
            self.pos += k;
            self.left -= k as u64;
            n -= k as u64;
        }
        true
    }

    /// Reads no more than `n` bytes from here on (until the limit is set again).
    pub fn limit(&mut self, n: u64) {
        self.left = n;
    }

    /// The bytes left up to the limit read and dropped.
    pub fn skip_rest(&mut self) {
        while self.left > 0 && self.fill() {
            let k = (self.len - self.pos).min(self.left.min(usize::MAX as u64) as usize);
            self.pos += k;
            self.left -= k as u64;
        }
        self.left = u64::MAX;
    }
}

impl Source for Reader<'_> {
    fn byte(&mut self) -> u8 {
        self.next().unwrap_or(0)
    }
}

impl<S: Source + ?Sized> Source for &mut S {
    fn byte(&mut self) -> u8 {
        (**self).byte()
    }
}

impl<S: Source + ?Sized> Source for &core::cell::RefCell<S> {
    fn byte(&mut self) -> u8 {
        self.borrow_mut().byte()
    }
}
