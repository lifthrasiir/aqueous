//! The code of the module `mszip-recons` (see `formula`, and `FORMAT.md`), which needs no
//! aqr-formula, so that aqr-modules builds it into its Wasm module.

use aqr_common::varint;

use crate::mszip::{self, BitR};
use crate::{Error, Result, corrupt};

/// A block of the output, after the bytes kept before it: an MSZIP block of so many DEFLATE
/// blocks of the joined streams, or so many bytes of the raw input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Block {
    Mszip { kept: usize, blocks: usize },
    Raw { kept: usize, len: usize },
}

/// The index of `blocks`: for each, the bytes kept before it, then its DEFLATE blocks times two
/// or its bytes times two plus one, as varints.
pub fn index(blocks: &[Block]) -> Vec<u8> {
    let mut out = Vec::new();
    for b in blocks {
        let (kept, v) = match *b {
            Block::Mszip { kept, blocks } => (kept, 2 * blocks as u64),
            Block::Raw { kept, len } => (kept, 2 * len as u64 + 1),
        };
        varint::put(&mut out, kept as u64);
        varint::put(&mut out, v);
    }
    out
}

/// The blocks of `index` (reserved for them all: two varints each).
fn blocks(index: &[u8]) -> Result<Vec<Block>> {
    let mut at = 0;
    let mut out = Vec::with_capacity(if index.is_empty() { 0 } else { aqr_common::streams::streams(index) });
    while at < index.len() {
        let mut get = || varint::get(index, &mut at).ok().and_then(|n| usize::try_from(n).ok()).ok_or_else(corrupt);
        let (kept, v) = (get()?, get()?);
        out.push(match v & 1 {
            0 => Block::Mszip { kept, blocks: v / 2 },
            _ => Block::Raw { kept, len: v / 2 },
        });
    }
    Ok(out)
}

/// The first `n` bytes of `from`, taken from it.
fn take<'a>(from: &mut &'a [u8], n: usize) -> Result<&'a [u8]> {
    let (a, b) = from.split_at_checked(n).ok_or_else(corrupt)?;
    *from = b;
    Ok(a)
}

/// Pages of the windows of the module as a part module ([`decode_part`]): of each input and of
/// the output.
pub const WINDOW: u32 = 4;

/// Bytes of the joined stream that [`decode_part`] holds at most: a block of the output is cut
/// from the bytes held, and a block of a CAB file (`cbData` of 16 bits) spans at most 65533 bytes
/// of the stream, and 78639 with the padding of stored DEFLATE blocks (one byte in five at most).
const JOINED: usize = 160 << 10;

/// Bytes at least of the joined stream that are held before a block is cut (but at its end).
const SPAN: usize = 96 << 10;

/// Bytes of the block that is cut: those of the stream that it spans, and a byte of padding for
/// each stored DEFLATE block (five bytes at least) and for the block, with its signature.
const CUT: usize = JOINED + JOINED / 5 + 8;

/// Bytes of the pieces in which the other inputs are taken and the output put.
const PIECE: usize = 4096;

/// The memory plan of the module as a part module ([`aqr_api::stack`]): what [`decode_part`]
/// takes beyond its initial memory at most, whatever the lengths: the joined stream held, the
/// block cut from it, and on top of them what cutting DEFLATE blocks takes
/// ([`mszip::cut_plan`]).
pub fn plan() -> u64 {
    use aqr_api::stack::block;
    block(JOINED as u64) + block(CUT as u64) + mszip::cut_plan()
}

/// The `K`-th input of a part module taken in order through its window, in pieces.
struct In<const K: u32> {
    at: u64,
    len: u64,
    buf: [u8; PIECE],
    /// The bytes of `buf` not taken yet are `lo..hi`.
    lo: usize,
    hi: usize,
}

impl<const K: u32> In<K>
where
    aqr_api::Input<K>: aqr_api::Memory,
{
    fn new(len: u64) -> In<K> {
        In { at: 0, len, buf: [0; PIECE], lo: 0, hi: 0 }
    }

    /// Whether every byte has been taken.
    fn done(&self) -> bool {
        self.lo == self.hi && self.at == self.len
    }

    /// The bytes not taken, from the piece read and the input after it.
    fn left(&self) -> u64 {
        (self.len - self.at) + (self.hi - self.lo) as u64
    }

    /// Reads the next piece, where none is left.
    fn fill(&mut self) {
        use aqr_api::{Input, Memory};
        if self.lo < self.hi || self.at == self.len {
            return;
        }
        let w = Input::<K>::size();
        let n = (self.len - self.at).min(PIECE as u64).min(w - self.at % w);
        Input::<K>::request(self.at + n);
        Input::<K>::read(self.at % w, &mut self.buf[..n as usize]);
        Input::<K>::release(self.at + n);
        (self.lo, self.hi) = (0, n as usize);
        self.at += n;
    }

    /// The next byte.
    fn byte(&mut self) -> Result<u8> {
        self.fill();
        let b = *self.buf[..self.hi].get(self.lo).ok_or_else(corrupt)?;
        self.lo += 1;
        Ok(b)
    }

    /// A varint (see `aqr_common::varint`).
    fn varint(&mut self) -> Result<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.byte()?;
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

    /// The next `n` bytes, given to `f` in pieces.
    fn each(&mut self, mut n: u64, f: &mut dyn FnMut(&[u8]) -> Result<()>) -> Result<()> {
        while n > 0 {
            self.fill();
            let k = ((self.hi - self.lo) as u64).min(n) as usize;
            if k == 0 {
                return Err(corrupt());
            }
            f(&self.buf[self.lo..self.lo + k])?;
            self.lo += k;
            n -= k as u64;
        }
        Ok(())
    }
}

/// The output of a part module put in order through its window, `size` bytes in all.
struct Out {
    at: u64,
    size: u64,
    buf: [u8; PIECE],
    n: usize,
}

impl Out {
    fn new(size: u64) -> Out {
        Out { at: 0, size, buf: [0; PIECE], n: 0 }
    }

    fn put(&mut self, d: &[u8]) -> Result<()> {
        if d.len() >= PIECE {
            self.flush()?;
            return write(&mut self.at, self.size, d);
        }
        if self.n + d.len() > PIECE {
            self.flush()?;
        }
        self.buf[self.n..self.n + d.len()].copy_from_slice(d);
        self.n += d.len();
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        write(&mut self.at, self.size, &self.buf[..self.n])?;
        self.n = 0;
        Ok(())
    }

    fn finish(mut self) -> Result<()> {
        self.flush()?;
        match self.at == self.size {
            true => Ok(()),
            false => Err(Error("output of another length")),
        }
    }
}

/// Writes `d` at `*at` of the output through its window of `size` bytes; fails past `size`.
fn write(at: &mut u64, size: u64, mut d: &[u8]) -> Result<()> {
    use aqr_api::{Memory, Output};
    if d.len() as u64 > size - *at {
        return Err(Error("output of another length"));
    }
    while !d.is_empty() {
        let w = Output::<0>::size();
        let k = (d.len() as u64).min(w - *at % w) as usize;
        Output::<0>::write(*at % w, &d[..k]);
        *at += k as u64;
        Output::<0>::commit(*at);
        d = &d[k..];
    }
    Ok(())
}

/// The joined streams (input 0) as they are cut: [`SPAN`] bytes at least are held, from the byte
/// that has the next bit, until the input ends.
struct Joined {
    buf: Vec<u8>,
    /// The offset in the input of the first byte held, the next of the input to read, and its
    /// length.
    base: u64,
    at: u64,
    len: u64,
    /// The bit of the stream that is next, within `buf`.
    bit: usize,
}

impl Joined {
    fn new(len: u64) -> Joined {
        Joined { buf: Vec::with_capacity(JOINED), base: 0, at: 0, len, bit: 0 }
    }

    /// Reads on, dropping the bytes before the next bit, where fewer than [`SPAN`] bytes are held.
    fn fill(&mut self) {
        use aqr_api::{Input, Memory};
        if self.buf.len() - self.bit / 8 >= SPAN || self.at == self.len {
            return;
        }
        let drop = self.bit / 8;
        self.buf.drain(..drop);
        self.base += drop as u64;
        self.bit -= 8 * drop;
        let w = Input::<0>::size();
        while self.buf.len() < JOINED && self.at < self.len {
            let n = (self.len - self.at).min((JOINED - self.buf.len()) as u64).min(w - self.at % w) as usize;
            Input::<0>::request(self.at + n as u64);
            let old = self.buf.len();
            self.buf.resize(old + n, 0);
            Input::<0>::read(self.at % w, &mut self.buf[old..]);
            Input::<0>::release(self.at + n as u64);
            self.at += n as u64;
        }
    }

    /// Whether the stream is used up: its last bit was the last of the input.
    fn done(&self) -> bool {
        self.at == self.len && self.next_bit() == 8 * self.len
    }

    /// The offset in the input, in bits, of the next bit of the stream.
    fn next_bit(&self) -> u64 {
        8 * self.base + self.bit as u64
    }
}

/// The module as a part module (its entry point, of `aqr_api::entry!`): what [`decode`] gives,
/// its inputs taken and its output put in order through windows ([`WINDOW`]) in pieces, the
/// joined stream held `SPAN` bytes at a time: nothing depends on the lengths. A block of the
/// output (a CAB file's `CFDATA`) whose DEFLATE blocks span more of the stream than that is
/// refused.
pub fn decode_part(params_len: u64, in0_len: u64, in1_len: u64, in2_len: u64, in3_len: u64, size: u64) {
    aqr_api::finish(part(params_len, in0_len, in1_len, in2_len, in3_len, size));
}

fn part(params_len: u64, in0_len: u64, in1_len: u64, in2_len: u64, in3_len: u64, size: u64) -> Result<()> {
    if params_len != 0 {
        return Err(corrupt());
    }
    let mut joined = Joined::new(in0_len);
    let mut cut = Vec::with_capacity(CUT);
    let (mut raw, mut kept, mut index) = (In::<1>::new(in1_len), In::<2>::new(in2_len), In::<3>::new(in3_len));
    let mut out = Out::new(size);
    while !index.done() {
        let (k, v) = (index.varint()?, index.varint()?);
        kept.each(k, &mut |d| out.put(d))?;
        if v & 1 == 1 {
            raw.each(v / 2, &mut |d| out.put(d))?;
            continue;
        }
        joined.fill();
        let mut r = BitR::at(&joined.buf, joined.bit);
        cut.clear();
        mszip::cut_onto(&mut r, usize::try_from(v / 2).map_err(|_| corrupt())?, &mut cut)?;
        joined.bit = r.pos();
        out.put(&cut)?;
    }
    kept.each(kept.left(), &mut |d| out.put(d))?;
    if !joined.done() || !raw.done() {
        return Err(Error("output of another length"));
    }
    out.finish()
}

/// The module: of the inputs (the joined streams, the raw bytes, the bytes kept, the index), each
/// block of the index after the bytes kept before it, then the bytes kept after the last, `len`
/// bytes in all (see `FORMAT.md`).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let &[joined, raw, kept, index] = inputs else { return Err(Error("four inputs wanted")) };
    if !params.is_empty() {
        return Err(corrupt());
    }
    let len = usize::try_from(len).map_err(|_| corrupt())?;
    let mut out = Vec::with_capacity(len);
    let (mut r, mut raw, mut kept) = (BitR::new(joined), raw, kept);
    for b in blocks(index)? {
        match b {
            Block::Mszip { kept: k, blocks } => {
                out.extend_from_slice(take(&mut kept, k)?);
                mszip::cut_onto(&mut r, blocks, &mut out)?;
            }
            Block::Raw { kept: k, len } => {
                out.extend_from_slice(take(&mut kept, k)?);
                out.extend_from_slice(take(&mut raw, len)?);
            }
        }
        if out.len() > len {
            return Err(Error("output of another length"));
        }
    }
    out.extend_from_slice(kept);
    if r.pos() != 8 * joined.len() || !raw.is_empty() || out.len() != len {
        return Err(Error("output of another length"));
    }
    Ok(out)
}
