//! Streams read in pieces by the decoders of a part module: a sliding buffer that holds, before
//! each frame, at least the most that a frame can read (or the rest of the stream), so that the
//! readers of slices (`Rd`, [`aqr_common::msb::BitReader`], the rANS decoder's words) work on it as
//! they do on the whole stream, and give the same.

use aqr_api::stream::Pull;
use aqr_common::msb::PAD;

/// A stream held in a window of its bytes, which moves forward by whole frames.
pub struct Slide<'a> {
    src: &'a mut dyn Pull,
    /// `cap` bytes of room, and [`PAD`] zeros after `end`.
    buf: Vec<u8>,
    cap: usize,
    at: usize,
    end: usize,
    /// Bits of the byte at `at` already read (bit readers).
    bit: usize,
    /// The source has ended.
    eof: bool,
    /// The bytes before `buf[0]`.
    before: u64,
}

impl<'a> Slide<'a> {
    /// Bytes of the block of a slide that holds `ahead` bytes ahead at least (as `block` rounds it).
    pub fn plan(ahead: usize, block: impl Fn(u64) -> u64) -> u64 {
        block((Self::cap(ahead) + PAD) as u64)
    }

    /// The room of a window that holds `ahead` bytes ahead at least: so many more are taken at a
    /// time (the bytes behind the position are copied once for as many taken).
    fn cap(ahead: usize) -> usize {
        2 * ahead.max(1)
    }

    /// A window over `src` that holds `ahead` bytes ahead of the position at least ([`Slide::need`]).
    pub fn new(src: &'a mut dyn Pull, ahead: usize) -> Slide<'a> {
        let cap = Self::cap(ahead);
        Slide { src, buf: vec![0; cap + PAD], cap, at: 0, end: 0, bit: 0, eof: false, before: 0 }
    }

    /// Makes sure that `min` bytes (at most the room less what is not read yet) are ahead of the
    /// position, or the stream ends before them: bytes behind it are dropped as need be.
    #[inline]
    pub fn need(&mut self, min: usize) {
        if self.end - self.at < min && !self.eof {
            self.fill(min);
        }
    }

    #[cold]
    fn fill(&mut self, min: usize) {
        debug_assert!(min < self.cap, "a frame reads more than the window holds");
        self.buf.copy_within(self.at..self.end, 0);
        self.before += self.at as u64;
        self.end -= self.at;
        self.at = 0;
        while self.end < self.cap && !self.eof {
            match self.src.pull(&mut self.buf[self.end..self.cap]) {
                0 => self.eof = true,
                n => self.end += n,
            }
        }
        self.buf[self.end..self.end + PAD].fill(0);
    }

    /// The bytes held from the position on.
    #[inline]
    pub fn data(&self) -> &[u8] {
        &self.buf[self.at..self.end]
    }

    /// [`Slide::data`] and [`PAD`] zeros after it, for a bit reader (starting at [`Slide::bit`]).
    #[inline]
    pub fn padded(&self) -> &[u8] {
        &self.buf[self.at..self.end + PAD]
    }

    /// Bits of the first byte already read.
    #[inline]
    pub fn bit(&self) -> usize {
        self.bit
    }

    /// Moves past `n` bytes of [`Slide::data`].
    #[inline]
    pub fn advance(&mut self, n: usize) {
        debug_assert!(n <= self.end - self.at);
        self.at += n;
    }

    /// Moves to bit `pos` of [`Slide::padded`] (a bit reader's position, which starts at
    /// [`Slide::bit`]).
    #[inline]
    pub fn advance_bits(&mut self, pos: usize) {
        self.at = (self.at + pos / 8).min(self.end);
        self.bit = pos % 8;
    }

    /// The bytes before the position, in all.
    #[inline]
    pub fn taken(&self) -> u64 {
        self.before + self.at as u64
    }

    /// Whether every byte was read (after a [`Slide::need`] of one).
    #[inline]
    pub fn at_end(&self) -> bool {
        self.eof && self.at == self.end
    }
}

/// The words of the rANS stream (u32 LE) from a stream of bytes.
pub struct Words<'a> {
    src: &'a mut dyn Pull,
    carry: [u8; 4],
    n: usize,
}

impl<'a> Words<'a> {
    pub fn new(src: &'a mut dyn Pull) -> Words<'a> {
        Words { src, carry: [0; 4], n: 0 }
    }

    /// Words put in a prefix of `out`, as many as there are room for or until the stream ends (0:
    /// it ended).
    pub fn fill(&mut self, out: &mut [u32]) -> usize {
        let mut tmp = [0u8; 512];
        let mut w = 0;
        while w < out.len() {
            tmp[..self.n].copy_from_slice(&self.carry[..self.n]);
            let want = ((out.len() - w) * 4 - self.n).min(tmp.len() - self.n);
            let got = self.src.pull(&mut tmp[self.n..self.n + want]);
            if got == 0 {
                break;
            }
            let total = self.n + got;
            let words = total / 4;
            for (o, c) in out[w..w + words].iter_mut().zip(tmp.as_chunks::<4>().0) {
                *o = u32::from_le_bytes(*c);
            }
            w += words;
            self.n = total % 4;
            self.carry[..self.n].copy_from_slice(&tmp[words * 4..total]);
        }
        w
    }
}
