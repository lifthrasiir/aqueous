//! GIF's variable-length LZW (`FORMAT.md` §3): the codes of a stream and the pixels they give,
//! and codes written as a stream again.

use crate::{Error, Result};

/// Codes at most: 12 bits.
pub const CODES: usize = 4096;

/// A stream's codes and the pixels they give.
pub struct Decoded {
    pub pixels: Vec<u8>,
    /// Every code read, clear and end codes included, up to the end code or the last valid one.
    pub codes: Vec<u16>,
    /// Bits of the stream up to the end of the last code.
    pub end_bit: usize,
}

/// The dictionary of a decoder: each code's prefix code, last and first pixel, and length.
pub struct Dict {
    pub min: u8,
    pub prefix: Vec<u16>,
    pub last: Vec<u8>,
    pub first: Vec<u8>,
    pub len: Vec<u16>,
    /// The next code to define, and the width of the next code read.
    pub next: usize,
    pub width: u32,
}

impl Dict {
    pub fn new(min: u8) -> Self {
        let roots = 1 << min;
        let mut d = Dict {
            min,
            prefix: vec![0; CODES],
            last: vec![0; CODES],
            first: vec![0; CODES],
            len: vec![0; CODES],
            next: 0,
            width: 0,
        };
        for c in 0..roots {
            d.last[c] = c as u8;
            d.first[c] = c as u8;
            d.len[c] = 1;
        }
        d.clear();
        d
    }

    pub fn clear_code(&self) -> usize {
        1 << self.min
    }

    pub fn clear(&mut self) {
        self.next = self.clear_code() + 2;
        self.width = self.min as u32 + 1;
    }

    /// Defines the next code as `prefix` and `last` (where there is room), and widens the codes
    /// once it reaches the width's bound.
    #[inline]
    pub fn add(&mut self, prefix: u16, last: u8) {
        let n = self.next;
        if n < CODES {
            self.prefix[n] = prefix;
            self.last[n] = last;
            self.first[n] = self.first[prefix as usize];
            self.len[n] = self.len[prefix as usize] + 1;
            self.next = n + 1;
            if self.next == 1 << self.width && self.width < 12 {
                self.width += 1;
            }
        }
    }

    /// Appends the pixels of `code`, a defined one, to `out`.
    #[inline]
    pub fn put(&self, code: u16, out: &mut Vec<u8>) {
        let n = self.len[code as usize] as usize;
        let at = out.len();
        out.resize(at + n, 0);
        let mut c = code as usize;
        for i in (at..at + n).rev() {
            out[i] = self.last[c];
            c = self.prefix[c] as usize;
        }
    }
}

/// Reads bits from the least significant first.
pub struct BitReader<'a> {
    d: &'a [u8],
    pub pos: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(d: &'a [u8]) -> Self {
        BitReader { d, pos: 0 }
    }

    /// `n` bits (at most 25), or None past the end.
    #[inline]
    pub fn read(&mut self, n: u32) -> Option<u32> {
        if self.pos + n as usize > self.d.len() * 8 {
            return None;
        }
        let at = self.pos / 8;
        let mut x = 0u32;
        for (i, &b) in self.d[at..(at + 4).min(self.d.len())].iter().enumerate() {
            x |= (b as u32) << (8 * i);
        }
        self.pos += n as usize;
        Some(x >> ((self.pos - n as usize) % 8) & ((1 << n) - 1))
    }
}

/// Writes bits from the least significant first.
#[derive(Default)]
pub struct BitWriter {
    pub out: Vec<u8>,
    acc: u32,
    n: u32,
}

impl BitWriter {
    #[inline]
    pub fn put(&mut self, v: u32, n: u32) {
        self.acc |= v << self.n;
        self.n += n;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    /// Bits written.
    pub fn bits(&self) -> usize {
        self.out.len() * 8 + self.n as usize
    }

    /// Fills the last byte with `pad`, the bits above those written.
    pub fn finish(mut self, pad: u8) -> Vec<u8> {
        if self.n > 0 {
            self.out.push(self.acc as u8 | pad << self.n);
        }
        self.out
    }
}

/// Decodes the stream `d` of minimum code size `min` (2 to 8), giving at most `limit` pixels.
/// It ends at an end code, at the end of `d`, or before a code that is not defined.
pub fn decode(d: &[u8], min: u8, limit: usize) -> Result<Decoded> {
    if !(2..=8).contains(&min) {
        return Err(Error("minimum code size beyond 2 to 8"));
    }
    let mut dict = Dict::new(min);
    let clear = dict.clear_code() as u16;
    let mut r = BitReader::new(d);
    let mut out = Decoded { pixels: Vec::with_capacity(d.len() * 3), codes: vec![], end_bit: 0 };
    let mut prev: Option<u16> = None;
    while let Some(c) = r.read(dict.width) {
        let c = c as u16;
        if c == clear {
            dict.clear();
            prev = None;
        } else if c == clear + 1 {
            out.codes.push(c);
            out.end_bit = r.pos;
            break;
        } else if let Some(p) = prev {
            if (c as usize) < dict.next {
                let f = dict.first[c as usize];
                dict.add(p, f);
            } else if c as usize == dict.next {
                let f = dict.first[p as usize];
                dict.add(p, f);
            } else {
                break;
            }
            prev = Some(c);
        } else if c < clear {
            prev = Some(c);
        } else {
            break;
        }
        if c != clear {
            if out.pixels.len() + dict.len[c as usize] as usize > limit {
                return Err(Error("pixels beyond the limit"));
            }
            dict.put(c, &mut out.pixels);
        }
        out.codes.push(c);
        out.end_bit = r.pos;
    }
    Ok(out)
}
