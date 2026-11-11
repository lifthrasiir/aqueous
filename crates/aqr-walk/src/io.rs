//! The two directions of the syntax walk. `Enc` reads elements from the original bits and codes them;
//! `Dec` decodes them and writes the bits; `Check` only reads (a dry run to see that a frame parses).
use crate::bits::{BitReader, BitWriter};
use crate::Error;
use crate::model::Model;
use crate::rc::Coder;

pub type Vlc = [(u32, u8, u16)];
/// The symbol of an escape code.
pub const ESC: u16 = 0xFFFF;

pub trait Io {
    fn total_bits(&self) -> usize;
    /// Bits consumed (compressing) or produced (decompressing) so far.
    fn done(&self) -> usize;
    /// A symbol of the VLC `tab`; `ctx` selects the statistics.
    fn vlc(&mut self, tab: &'static Vlc, ctx: u32) -> Result<u16, Error>;
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

#[inline]
fn find(tab: &Vlc, peek: u32) -> Option<(u32, u8, u16)> {
    let i = tab.partition_point(|e| e.0 <= peek);
    if i == 0 {
        return None;
    }
    let e = tab[i - 1];
    if e.1 == 0 || (peek ^ e.0) >> (32 - e.1 as u32) != 0 { None } else { Some(e) }
}

#[inline]
fn exact(tab: &Vlc, la: u32, len: u8) -> Option<u16> {
    let i = tab.partition_point(|e| e.0 < la);
    match tab.get(i) {
        Some(e) if e.0 == la && e.1 == len => Some(e.2),
        _ => None,
    }
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
    fn vlc(&mut self, tab: &'static Vlc, ctx: u32) -> Result<u16, Error> {
        let e = find(tab, self.r.peek32()).ok_or(Error::Parse)?;
        if self.r.left() < e.1 as usize {
            return Err(Error::Parse);
        }
        for k in 0..e.1 as u32 {
            let node = 1u32 << k | if k == 0 { 0 } else { e.0 >> (32 - k) };
            let bit = e.0 >> (31 - k) & 1;
            self.rc.bit(self.m.slot(ctx, node), bit);
        }
        self.r.skip(e.1 as usize);
        Ok(e.2)
    }
    fn raw(&mut self, n: u32) -> Result<u32, Error> {
        let v = self.r.get(n).ok_or(Error::Parse)?;
        for k in (0..n).rev() {
            self.rc.flat(v >> k & 1);
        }
        Ok(v)
    }
    fn flag(&mut self, ctx: u32) -> Result<u32, Error> {
        let v = self.r.get(1).ok_or(Error::Parse)?;
        self.rc.bit(self.m.slot(ctx, 0x8000_0000), v);
        Ok(v)
    }
    fn field(&mut self, n: u32, ctx: u32) -> Result<u32, Error> {
        let v = self.r.get(n).ok_or(Error::Parse)?;
        let k = n.min(8);
        for i in 0..k {
            let node = 1u32 << i | v >> (n - i);
            self.rc.bit(self.m.slot(ctx, node), v >> (n - 1 - i) & 1);
        }
        for i in (0..n - k).rev() {
            self.rc.flat(v >> i & 1);
        }
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
    fn vlc(&mut self, tab: &'static Vlc, ctx: u32) -> Result<u16, Error> {
        let mut prefix = 0u32;
        for k in 0..32u32 {
            let node = 1u32 << k | prefix;
            let bit = self.rc.bit(self.m.slot(ctx, node), 0);
            prefix = prefix << 1 | bit;
            let la = prefix << (31 - k);
            if let Some(s) = exact(tab, la, k as u8 + 1) {
                self.w.put(prefix, k + 1);
                return Ok(s);
            }
        }
        Err(Error::Parse)
    }
    fn raw(&mut self, n: u32) -> Result<u32, Error> {
        let mut v = 0;
        for _ in 0..n {
            v = v << 1 | self.rc.flat(0);
        }
        self.w.put(v, n);
        Ok(v)
    }
    fn flag(&mut self, ctx: u32) -> Result<u32, Error> {
        let v = self.rc.bit(self.m.slot(ctx, 0x8000_0000), 0);
        self.w.put(v, 1);
        Ok(v)
    }
    fn field(&mut self, n: u32, ctx: u32) -> Result<u32, Error> {
        let k = n.min(8);
        let mut v = 0u32;
        for i in 0..k {
            let node = 1u32 << i | v;
            v = v << 1 | self.rc.bit(self.m.slot(ctx, node), 0);
        }
        for _ in 0..n - k {
            v = v << 1 | self.rc.flat(0);
        }
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
    fn vlc(&mut self, tab: &'static Vlc, _ctx: u32) -> Result<u16, Error> {
        let e = find(tab, self.r.peek32()).ok_or(Error::Parse)?;
        if self.r.left() < e.1 as usize {
            return Err(Error::Parse);
        }
        self.r.skip(e.1 as usize);
        Ok(e.2)
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
