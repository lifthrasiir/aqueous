//! The two directions of the syntax walk. `Enc` reads elements from the original bits and codes them;
//! `Dec` decodes them and writes the bits; `Check` only reads (a dry run to see that a frame parses).
//!
//! A symbol of a VLC is coded as the bits of its code, each under the probability of its node of the code's tree and the
//! context: the nodes of four bits share a line of the model's table, so that a code takes a miss of the cache for every
//! four of its bits.
use crate::Error;
use crate::bits::{BitReader, BitWriter};
use crate::model::Model;
use crate::rc::Coder;
use alloc::vec::Vec;

/// The symbol of an escape code.
pub const ESC: u16 = 0xFFFF;

/// A VLC: its codes, `(code left-aligned to 32 bits, length, symbol)`, and the tree of them to walk bit by bit.
pub struct Vlc {
    /// By node and bit: the next node (positive), or the symbol (`!symbol`, negative), or 0 for none.
    trie: Vec<[i32; 2]>,
}

impl Vlc {
    pub fn new(codes: &'static [(u32, u8, u16)]) -> Vlc {
        let mut trie: Vec<[i32; 2]> = alloc::vec![[0, 0]];
        for &(code, len, sym) in codes {
            let mut node = 0usize;
            for k in 0..len as u32 {
                let bit = (code >> (31 - k) & 1) as usize;
                if k + 1 == len as u32 {
                    trie[node][bit] = !(sym as i32);
                } else {
                    if trie[node][bit] <= 0 {
                        trie.push([0, 0]);
                        trie[node][bit] = trie.len() as i32 - 1;
                    }
                    node = trie[node][bit] as usize;
                }
            }
        }
        Vlc { trie }
    }
}

pub trait Io {
    fn total_bits(&self) -> usize;
    /// Bits consumed (compressing) or produced (decompressing) so far.
    fn done(&self) -> usize;
    /// A symbol of the VLC `tab`; `ctx` selects the statistics.
    fn vlc(&mut self, tab: &Vlc, ctx: u32) -> Result<u16, Error>;
    /// `n` (at most 32) bits as they are, coded flat.
    fn raw(&mut self, n: u32) -> Result<u32, Error>;
    /// One bit, adaptive under `ctx`.
    fn flag(&mut self, ctx: u32) -> Result<u32, Error>;
    /// `n` (at most 16) bits, the top eight coded adaptively under `ctx` as a binary tree, the others flat.
    fn field(&mut self, n: u32, ctx: u32) -> Result<u32, Error>;
    /// `n` (at most 32) bits that must be `v`: not coded.
    fn expect(&mut self, v: u32, n: u32) -> Result<(), Error>;
    /// `n` bits that follow from the others (a checksum): not coded; written as zeros when decompressing, to be patched.
    fn derived(&mut self, n: u32) -> Result<(), Error>;
}

/// Walks the tree of `tab` over the bits of `peek` (left-aligned): the symbol and the length of its code.
#[inline]
fn walk(tab: &Vlc, peek: u32) -> Option<(u16, u32)> {
    let mut node = 0usize;
    for k in 0..32u32 {
        let next = tab.trie[node][(peek >> (31 - k) & 1) as usize];
        if next < 0 {
            return Some((!next as u16, k + 1));
        }
        if next == 0 {
            return None;
        }
        node = next as usize;
    }
    None
}

pub struct Enc<'a, 'm, C: Coder> {
    pub r: BitReader<'a>,
    pub rc: &'m mut C,
    pub m: &'m mut Model,
}

impl<C: Coder> Io for Enc<'_, '_, C> {
    fn total_bits(&self) -> usize {
        self.r.total()
    }
    fn done(&self) -> usize {
        self.r.pos()
    }
    fn vlc(&mut self, tab: &Vlc, ctx: u32) -> Result<u16, Error> {
        let peek = self.r.peek32();
        let (sym, len) = walk(tab, peek).ok_or(Error::Parse)?;
        if self.r.left() < len as usize {
            return Err(Error::Parse);
        }
        let (mut base, mut gn) = (0usize, 1u32);
        for k in 0..len {
            if k & 3 == 0 {
                base = self.m.line(ctx, k >> 2, if k == 0 { 0 } else { peek >> (32 - k) });
                gn = 1;
            }
            let bit = peek >> (31 - k) & 1;
            self.rc.bit(self.m.at(base, gn), bit);
            gn = gn << 1 | bit;
        }
        self.r.skip(len as usize);
        Ok(sym)
    }
    fn raw(&mut self, n: u32) -> Result<u32, Error> {
        let v = self.r.get(n).ok_or(Error::Parse)?;
        self.rc.flat_bits(v, n);
        Ok(v)
    }
    fn flag(&mut self, ctx: u32) -> Result<u32, Error> {
        let v = self.r.get(1).ok_or(Error::Parse)?;
        let base = self.m.line(ctx, 0, 0);
        self.rc.bit(self.m.at(base, 1), v);
        Ok(v)
    }
    fn field(&mut self, n: u32, ctx: u32) -> Result<u32, Error> {
        let v = self.r.get(n).ok_or(Error::Parse)?;
        let k = n.min(8);
        let (mut base, mut gn) = (0usize, 1u32);
        for i in 0..k {
            if i & 3 == 0 {
                base = self.m.line(ctx, i >> 2, if i == 0 { 0 } else { v >> (n - i) });
                gn = 1;
            }
            let bit = v >> (n - 1 - i) & 1;
            self.rc.bit(self.m.at(base, gn), bit);
            gn = gn << 1 | bit;
        }
        self.rc.flat_bits(v, n - k);
        Ok(v)
    }
    fn expect(&mut self, v: u32, n: u32) -> Result<(), Error> {
        if self.r.get(n).ok_or(Error::Parse)? == v { Ok(()) } else { Err(Error::Parse) }
    }
    fn derived(&mut self, n: u32) -> Result<(), Error> {
        self.r.get(n).map(|_| ()).ok_or(Error::Parse)
    }
}

pub struct Dec<'m, C: Coder> {
    pub w: BitWriter,
    pub total: usize,
    pub rc: &'m mut C,
    pub m: &'m mut Model,
}

impl<C: Coder> Io for Dec<'_, C> {
    fn total_bits(&self) -> usize {
        self.total
    }
    fn done(&self) -> usize {
        self.w.bits()
    }
    fn vlc(&mut self, tab: &Vlc, ctx: u32) -> Result<u16, Error> {
        let (mut node, mut prefix) = (0usize, 0u32);
        let (mut base, mut gn) = (0usize, 1u32);
        for k in 0..32u32 {
            if k & 3 == 0 {
                base = self.m.line(ctx, k >> 2, prefix);
                gn = 1;
            }
            let bit = self.rc.bit(self.m.at(base, gn), 0);
            gn = gn << 1 | bit;
            prefix = prefix << 1 | bit;
            let next = tab.trie[node][bit as usize];
            if next < 0 {
                self.w.put(prefix, k + 1);
                return Ok(!next as u16);
            }
            if next == 0 {
                return Err(Error::Parse);
            }
            node = next as usize;
        }
        Err(Error::Parse)
    }
    fn raw(&mut self, n: u32) -> Result<u32, Error> {
        let v = self.rc.flat_bits(0, n);
        self.w.put(v, n);
        Ok(v)
    }
    fn flag(&mut self, ctx: u32) -> Result<u32, Error> {
        let base = self.m.line(ctx, 0, 0);
        let v = self.rc.bit(self.m.at(base, 1), 0);
        self.w.put(v, 1);
        Ok(v)
    }
    fn field(&mut self, n: u32, ctx: u32) -> Result<u32, Error> {
        let k = n.min(8);
        let mut v = 0u32;
        let (mut base, mut gn) = (0usize, 1u32);
        for i in 0..k {
            if i & 3 == 0 {
                base = self.m.line(ctx, i >> 2, v);
                gn = 1;
            }
            let bit = self.rc.bit(self.m.at(base, gn), 0);
            gn = gn << 1 | bit;
            v = v << 1 | bit;
        }
        v = v << (n - k) | self.rc.flat_bits(0, n - k);
        self.w.put(v, n);
        Ok(v)
    }
    fn expect(&mut self, v: u32, n: u32) -> Result<(), Error> {
        self.w.put(v, n);
        Ok(())
    }
    fn derived(&mut self, n: u32) -> Result<(), Error> {
        self.w.put(0, n);
        Ok(())
    }
}

pub struct Check<'a> {
    pub r: BitReader<'a>,
}

impl Io for Check<'_> {
    fn total_bits(&self) -> usize {
        self.r.total()
    }
    fn done(&self) -> usize {
        self.r.pos()
    }
    fn vlc(&mut self, tab: &Vlc, _ctx: u32) -> Result<u16, Error> {
        let (sym, len) = walk(tab, self.r.peek32()).ok_or(Error::Parse)?;
        if self.r.left() < len as usize {
            return Err(Error::Parse);
        }
        self.r.skip(len as usize);
        Ok(sym)
    }
    fn raw(&mut self, n: u32) -> Result<u32, Error> {
        self.r.get(n).ok_or(Error::Parse)
    }
    fn flag(&mut self, _ctx: u32) -> Result<u32, Error> {
        self.r.get(1).ok_or(Error::Parse)
    }
    fn field(&mut self, n: u32, _ctx: u32) -> Result<u32, Error> {
        self.r.get(n).ok_or(Error::Parse)
    }
    fn expect(&mut self, v: u32, n: u32) -> Result<(), Error> {
        if self.r.get(n).ok_or(Error::Parse)? == v { Ok(()) } else { Err(Error::Parse) }
    }
    fn derived(&mut self, n: u32) -> Result<(), Error> {
        self.r.get(n).map(|_| ()).ok_or(Error::Parse)
    }
}
