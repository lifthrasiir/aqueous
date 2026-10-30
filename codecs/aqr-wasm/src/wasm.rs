//! A WebAssembly module split into streams and joined back (FORMAT.md, "Streams"): the
//! instructions of its code by field, its data segments' bytes and its custom sections apart.
//! LEB128s and bytes are copied as they are, but for the local indices, coded by their ranks
//! among those used last ([`Mtf`]), so that the module comes back as it was whatever its
//! encoding. Both directions walk the code by the same functions, through [`Io`].

use crate::{Result, corrupt};
use aqr_common::varint;

/// The streams, in order.
pub const STREAMS: usize = 14;

/// The names of the streams, in order.
pub const STREAM_NAMES: [&str; STREAMS] = [
    "skeleton",
    "sections",
    "custom",
    "locals",
    "op",
    "local",
    "label",
    "brtable",
    "blocktype",
    "offset",
    "i64",
    "bytes",
    "misc",
    "data",
];

/// The sections' ids, sizes and counts, and how each function is kept.
const SKELETON: usize = 0;
/// The sections kept as they are but custom ones, and the functions kept as they are.
const SECTIONS: usize = 1;
/// The custom sections.
pub const CUSTOM: usize = 2;
/// The functions' declarations of locals.
const LOCALS: usize = 3;
/// The opcodes, with the memory arguments' alignments, the first bytes of their offsets, the
/// constants of `i32.const` and the functions of `call`.
const OP: usize = 4;
/// The local indices' codes.
const LOCAL: usize = 5;
/// The labels of `br` and `br_if`.
const LABEL: usize = 6;
/// The labels of `br_table`, after their count.
const BRTABLE: usize = 7;
const BLOCKTYPE: usize = 8;
/// The memory arguments' offsets but their first bytes.
const OFFSET: usize = 9;
/// The constants of `i64.const`.
const I64: usize = 10;
/// The constants of `f32.const`, `f64.const` and `v128.const`, the lanes of `i8x16.shuffle`
/// and the lane indices.
const BYTES: usize = 11;
/// The other immediates, and the data segments but their bytes.
const MISC: usize = 12;
/// The data segments' bytes.
pub const DATA: usize = 13;

/// The header of a module of the binary format's version 1.
pub const HEADER: [u8; 8] = *b"\0asm\x01\0\0\0";

const CUSTOM_SECTION: u8 = 0;
const CODE_SECTION: u8 = 10;
const DATA_SECTION: u8 = 11;

/// The longest LEB128s of 32-bit and 64-bit values.
const L32: usize = 5;
const L64: usize = 10;

/// What a walk of the code copies, a field at a time: the module into streams ([`Split`]) or
/// streams into the module ([`Join`]).
trait Io {
    /// Copies a byte of stream `s`.
    fn byte(&mut self, s: usize) -> Option<u8>;
    /// Copies `n` bytes of stream `s`.
    fn bytes(&mut self, s: usize, n: u64) -> Option<()>;
    /// Copies a LEB128 of stream `s`, of at most `max` bytes; its value (its first 64 bits).
    fn leb(&mut self, s: usize, max: usize) -> Option<u64>;
    /// Copies a local index, in the shortest form in the module, as its code of `mtf` in
    /// stream [`LOCAL`].
    fn local(&mut self, mtf: &mut Mtf) -> Option<()>;

    /// [`Io::leb`] with its first byte in stream `head` and the others in stream `tail`.
    fn leb2(&mut self, head: usize, tail: usize, max: usize) -> Option<u64> {
        let b = self.byte(head)?;
        if b < 0x80 {
            return Some(b as u64);
        }
        Some((b & 0x7f) as u64 | self.leb(tail, max - 1)?.wrapping_shl(7))
    }
}

/// The local indices last used, the last first: a local index is coded as its rank there, or
/// if it is not there, as the list's length plus the zigzag of its difference from the index
/// after the greatest used so far. It then goes to the front.
struct Mtf {
    list: Vec<u32>,
    next: u64,
}

impl Mtf {
    /// The longest the list grows, beyond which the last is dropped.
    const LEN: usize = 256;

    fn new() -> Mtf {
        Mtf { list: Vec::with_capacity(Mtf::LEN + 1), next: 0 }
    }

    /// The code of index `v`.
    fn code(&mut self, v: u32) -> u64 {
        let c = match self.list.iter().position(|&x| x == v) {
            Some(r) => r as u64,
            None => self.list.len() as u64 + varint::zz(v as i64 - self.next as i64),
        };
        self.used(v);
        c
    }

    /// The index of code `c`.
    fn index(&mut self, c: u64) -> Option<u32> {
        let v = match c.checked_sub(self.list.len() as u64) {
            None => self.list[c as usize],
            Some(d) => u32::try_from((self.next as i64).checked_add(varint::unzz(d))?).ok()?,
        };
        self.used(v);
        Some(v)
    }

    fn used(&mut self, v: u32) {
        if let Some(r) = self.list.iter().position(|&x| x == v) {
            self.list.remove(r);
        }
        self.list.insert(0, v);
        self.list.truncate(Mtf::LEN);
        self.next = self.next.max(v as u64 + 1);
    }
}

/// A memory argument: its alignment and flags, its memory if bit 6 of them tells, its offset.
fn memarg(io: &mut impl Io) -> Option<()> {
    if io.leb(OP, L32)? & 0x40 != 0 {
        io.leb(MISC, L32)?;
    }
    io.leb2(OP, OFFSET, L64).map(drop)
}

/// The instructions of an expression up to its `end`, of the locals of `mtf`.
fn expr(io: &mut impl Io, mtf: &mut Mtf) -> Option<()> {
    let mut depth = 0u64;
    loop {
        match io.byte(OP)? {
            0x02..=0x04 => {
                io.leb(BLOCKTYPE, L32)?;
                depth += 1;
            }
            0x0b if depth == 0 => return Some(()),
            0x0b => depth -= 1,
            0x0c | 0x0d => _ = io.leb(LABEL, L32)?,
            0x0e => {
                let n = io.leb(BRTABLE, L32)?;
                for _ in 0..=n {
                    io.leb(BRTABLE, L32)?;
                }
            }
            0x10 | 0x12 => _ = io.leb(OP, L32)?,
            0x11 | 0x13 => {
                io.leb(MISC, L32)?;
                io.leb(MISC, L32)?;
            }
            0x1c => {
                let n = io.leb(MISC, L32)?;
                io.bytes(MISC, n)?;
            }
            0x20..=0x22 => io.local(mtf)?,
            0x23..=0x26 | 0x3f | 0x40 | 0xd0 | 0xd2 => _ = io.leb(MISC, L32)?,
            0x28..=0x3e => memarg(io)?,
            0x41 => _ = io.leb(OP, L32)?,
            0x42 => _ = io.leb(I64, L64)?,
            0x43 => io.bytes(BYTES, 4)?,
            0x44 => io.bytes(BYTES, 8)?,
            0x00 | 0x01 | 0x05 | 0x0f | 0x1a | 0x1b | 0x45..=0xc4 | 0xd1 => {}
            0xfc => match io.leb(OP, L32)? {
                0..=7 => {}
                8 | 10 | 12 | 14 => {
                    io.leb(MISC, L32)?;
                    io.leb(MISC, L32)?;
                }
                9 | 11 | 13 | 15..=17 => _ = io.leb(MISC, L32)?,
                _ => return None,
            },
            0xfd => match io.leb(OP, L32)? {
                0..=11 | 92 | 93 => memarg(io)?,
                12 | 13 => io.bytes(BYTES, 16)?,
                21..=34 => _ = io.byte(BYTES)?,
                84..=91 => {
                    memarg(io)?;
                    io.byte(BYTES)?;
                }
                14..=20 | 35..=83 | 94..=255 => {}
                _ => return None,
            },
            _ => return None,
        }
    }
}

/// A function body after its size: its declarations of locals, then its instructions.
fn body(io: &mut impl Io) -> Option<()> {
    let n = io.leb(LOCALS, L32)?;
    for _ in 0..n {
        io.leb(LOCALS, L32)?;
        io.byte(LOCALS)?;
    }
    expr(io, &mut Mtf::new())
}

/// A data segment: its kind, its memory and its offset, its length, then its bytes.
fn segment(io: &mut impl Io) -> Option<()> {
    let kind = io.leb(MISC, L32)?;
    if kind == 2 {
        io.leb(MISC, L32)?;
    }
    if kind != 1 {
        expr(io, &mut Mtf::new())?;
    }
    let n = io.leb(MISC, L32)?;
    io.bytes(DATA, n)
}

/// The module read into streams.
struct Split<'a> {
    d: &'a [u8],
    at: usize,
    out: [Vec<u8>; STREAMS],
}

impl Io for Split<'_> {
    fn byte(&mut self, s: usize) -> Option<u8> {
        let b = *self.d.get(self.at)?;
        self.at += 1;
        self.out[s].push(b);
        Some(b)
    }

    fn bytes(&mut self, s: usize, n: u64) -> Option<()> {
        let b = self.d[self.at..].get(..usize::try_from(n).ok()?)?;
        self.at += b.len();
        self.out[s].extend_from_slice(b);
        Some(())
    }

    fn leb(&mut self, s: usize, max: usize) -> Option<u64> {
        let (v, n) = leb(&self.d[self.at..], max)?;
        self.raw(s, n);
        Some(v)
    }

    fn local(&mut self, mtf: &mut Mtf) -> Option<()> {
        let (v, n) = leb(&self.d[self.at..], L32)?;
        let v = u32::try_from(v).ok().filter(|&v| varint::len(v as u64) == n)?;
        self.at += n;
        varint::put(&mut self.out[LOCAL], mtf.code(v));
        Some(())
    }
}

impl Split<'_> {
    /// The lengths of the streams, to go back to where a part could not be split.
    fn mark(&self) -> [usize; STREAMS] {
        std::array::from_fn(|i| self.out[i].len())
    }

    fn reset(&mut self, at: usize, mark: &[usize; STREAMS]) {
        self.at = at;
        for (o, &n) in self.out.iter_mut().zip(mark) {
            o.truncate(n);
        }
    }

    /// Copies the next `n` bytes to stream `s`.
    fn raw(&mut self, s: usize, n: usize) {
        self.out[s].extend_from_slice(&self.d[self.at..][..n]);
        self.at += n;
    }

    /// The items of a code or data section ending at `end`, after its size.
    fn items(&mut self, id: u8, end: usize) -> Option<()> {
        let count = self.leb(SKELETON, L32)?;
        for _ in 0..count {
            if id == DATA_SECTION {
                segment(self)?;
                continue;
            }
            let (size, n) = leb(&self.d[self.at..end], L32)?;
            let stop = (self.at + n).checked_add(usize::try_from(size).ok()?).filter(|&e| e <= end)?;
            let (start, mark) = (self.at, self.mark());
            if n == varint::len(size) {
                self.out[SKELETON].push(0);
                self.at += n;
                if body(self).is_some() && self.at == stop {
                    continue;
                }
                self.reset(start, &mark);
            }
            self.out[SKELETON].push(1);
            self.raw(SECTIONS, stop - start);
        }
        (self.at == end).then_some(())
    }
}

/// The value and the length of the LEB128 of at most `max` bytes at the start of `d`.
fn leb(d: &[u8], max: usize) -> Option<(u64, usize)> {
    let mut v = 0u64;
    for (i, &b) in d.iter().take(max).enumerate() {
        v |= ((b & 0x7f) as u64).wrapping_shl(7 * i as u32);
        if b < 0x80 {
            return Some((v, i + 1));
        }
    }
    None
}

/// The streams of module `wasm`, or `None` where it is not a module of version 1 of sections
/// that end where their sizes tell. The code and data sections are split where their sizes
/// are written in the shortest form, and of the code section, each function so written that
/// the walk of its instructions ends with it and of its local indices so written; the other
/// functions and sections go as they are.
pub fn split(wasm: &[u8]) -> Option<[Vec<u8>; STREAMS]> {
    if !wasm.starts_with(&HEADER) {
        return None;
    }
    let mut s = Split { d: wasm, at: 0, out: Default::default() };
    s.raw(SKELETON, HEADER.len());
    while s.at < wasm.len() {
        let id = wasm[s.at];
        let (size, n) = leb(&wasm[s.at + 1..], L32)?;
        let end = (s.at + 1 + n).checked_add(usize::try_from(size).ok()?).filter(|&e| e <= wasm.len())?;
        let items = id == CODE_SECTION || id == DATA_SECTION;
        let (start, mark) = (s.at, s.mark());
        if items && n == varint::len(size) {
            s.raw(SKELETON, 1);
            s.out[SKELETON].push(0);
            s.at += n;
            if s.items(id, end).is_some() {
                continue;
            }
            s.reset(start, &mark);
        }
        s.raw(SKELETON, 1);
        if items {
            s.out[SKELETON].push(1);
        }
        s.raw(SKELETON, n);
        s.raw(if id == CUSTOM_SECTION { CUSTOM } else { SECTIONS }, end - s.at);
    }
    Some(s.out)
}

/// The streams read into the module.
struct Join<'a> {
    ins: [&'a [u8]; STREAMS],
    out: Vec<u8>,
}

impl Io for Join<'_> {
    fn byte(&mut self, s: usize) -> Option<u8> {
        let b = self.take(s)?;
        self.out.push(b);
        Some(b)
    }

    fn bytes(&mut self, s: usize, n: u64) -> Option<()> {
        let n = usize::try_from(n).ok().filter(|&n| n <= self.ins[s].len())?;
        let (b, rest) = self.ins[s].split_at(n);
        self.ins[s] = rest;
        self.out.extend_from_slice(b);
        Some(())
    }

    fn leb(&mut self, s: usize, max: usize) -> Option<u64> {
        let (v, n) = leb(self.ins[s], max)?;
        self.bytes(s, n as u64).map(|_| v)
    }

    fn local(&mut self, mtf: &mut Mtf) -> Option<()> {
        let (c, n) = leb(self.ins[LOCAL], L64)?;
        self.ins[LOCAL] = &self.ins[LOCAL][n..];
        varint::put(&mut self.out, mtf.index(c)? as u64);
        Some(())
    }
}

impl Join<'_> {
    /// A byte of stream `s`, not copied.
    fn take(&mut self, s: usize) -> Option<u8> {
        let (&b, rest) = self.ins[s].split_first()?;
        self.ins[s] = rest;
        Some(b)
    }

    /// Runs `f`, then puts the LEB128 of the length of what it wrote before it (in the room
    /// of the longest left before it, then moved back).
    fn sized(&mut self, f: impl FnOnce(&mut Self) -> Option<()>) -> Option<()> {
        let start = self.out.len();
        self.out.extend_from_slice(&[0; L32]);
        f(self)?;
        let n = self.out.len() - start - L32;
        let mut size = Vec::with_capacity(L32);
        varint::put(&mut size, u32::try_from(n).ok()? as u64);
        self.out.copy_within(start + L32.., start + size.len());
        self.out[start..start + size.len()].copy_from_slice(&size);
        self.out.truncate(start + size.len() + n);
        Some(())
    }

    /// A section of the module.
    fn section(&mut self) -> Option<()> {
        let id = self.byte(SKELETON)?;
        if id == CODE_SECTION || id == DATA_SECTION {
            match self.take(SKELETON)? {
                0 => return self.sized(|j| j.items(id)),
                1 => {}
                _ => return None,
            }
        }
        let size = self.leb(SKELETON, L32)?;
        self.bytes(if id == CUSTOM_SECTION { CUSTOM } else { SECTIONS }, size)
    }

    /// The items of a code or data section split.
    fn items(&mut self, id: u8) -> Option<()> {
        let count = self.leb(SKELETON, L32)?;
        for _ in 0..count {
            if id == DATA_SECTION {
                segment(self)?;
                continue;
            }
            match self.take(SKELETON)? {
                0 => self.sized(body)?,
                1 => {
                    let size = self.leb(SECTIONS, L32)?;
                    self.bytes(SECTIONS, size)?
                }
                _ => return None,
            }
        }
        Some(())
    }
}

/// The module of streams `ins` ([`split`]), of `len` bytes.
pub fn join(ins: [&[u8]; STREAMS], len: u64) -> Result<Vec<u8>> {
    let cap = usize::try_from(len).map_err(|_| corrupt())?;
    // (and the room of the sizes of a section and a function made)
    let mut j = Join { ins, out: Vec::with_capacity(cap.min(1 << 30) + 2 * L32) };
    j.bytes(SKELETON, HEADER.len() as u64).filter(|_| j.out == HEADER).ok_or_else(corrupt)?;
    while !j.ins[SKELETON].is_empty() {
        j.section().ok_or_else(corrupt)?;
        if j.out.len() > cap {
            return Err(corrupt());
        }
    }
    if j.out.len() != cap || j.ins.iter().any(|i| !i.is_empty()) {
        return Err(corrupt());
    }
    Ok(j.out)
}
