//! The type records of the TPI and IPI streams (FORMAT.md §4): each record's fields by its
//! kind's layout to streams of their own, type indices as the distance back from the record's own,
//! numbers in one canonical form, names apart, padding derived; and the hash stream the headers
//! point at (the hash of each record, the offsets of its index) derived from the records.

use aqr_api::stream::{Pull, Put, Reader};
use aqr_common::varint;

use crate::{Result, corrupt};

/// The streams, each an input of the module `pdb-types`.
pub const STREAM_NAMES: [&str; 13] =
    ["kind", "sub", "ti", "fti", "num", "attr", "name", "raw", "hdr", "tif", "tim", "attrf", "tia"];
const KIND: usize = 0;
const SUB: usize = 1;
const TI: usize = 2;
const FTI: usize = 3;
const NUM: usize = 4;
const ATTR: usize = 5;
const NAME: usize = 6;
const RAW: usize = 7;
const HDR: usize = 8;
const TIF: usize = 9;
const TIM: usize = 10;
const ATTRF: usize = 11;
const TIA: usize = 12;
pub const NUM_INPUTS: usize = STREAM_NAMES.len();
/// The outputs: for the TPI stream and then the IPI stream, the stream, its hash values and the
/// offsets of its index.
pub const NUM_OUTPUTS: usize = 6;

/// A stream is absent, its records alone are coded (its header kept), or its hash stream is
/// derived too.
pub const ABSENT: u8 = 0;
pub const RECORDS: u8 = 1;
pub const FULL: u8 = 2;

/// Bytes of the header of a TPI or IPI stream.
const HEADER: usize = 56;
/// The most bytes of a record, with its length.
const MAX_RECORD: usize = 2 + 65535;
/// Bytes between two entries of the index.
const INDEX_STEP: u64 = 8192;

/// The kinds of the records that have a layout, in the order of their codes in `kind`; the code
/// 255 is a record kept as it is.
const KINDS: [u16; 21] = [
    0x000a, 0x1001, 0x1002, 0x1008, 0x1009, 0x1201, 0x1203, 0x1205, 0x1206, 0x1503, 0x1504, 0x1505, 0x1506, 0x1507,
    0x1601, 0x1602, 0x1603, 0x1604, 0x1605, 0x1606, 0x1607,
];
/// The kinds of the members of a field list, with the codes 1 and up in `sub` (0 ends a list).
const SUBS: [u16; 12] =
    [0x1400, 0x1401, 0x1402, 0x1404, 0x1409, 0x1502, 0x150d, 0x150e, 0x150f, 0x1510, 0x1511, 0x1512];
const KEPT: u8 = 255;
/// A member of a method list in `sub`.
const ITEM: u8 = 255;

/// What an index counts.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Space {
    Type,
    Id,
}

/// How an index of the stream's own space is coded.
#[derive(Clone, Copy)]
enum How {
    /// The distance back from the record's own.
    Back,
    /// As it is.
    Abs,
    /// The distance from the last of this kind.
    Prev,
}

/// The streams that the indices and the attributes of records of `kind` are coded to.
fn classes(kind: u16) -> (usize, usize) {
    match kind {
        0x1203 => (TIF, ATTRF),
        0x1009 => (TIM, ATTR),
        _ => (TI, ATTR),
    }
}

/// The fields of the record being read or written, in the order of its layout. The encoder
/// takes them from a record, the decoder puts them in one.
trait Io {
    /// An index of `space`, coded as `how`.
    fn ti(&mut self, space: Space, how: How) -> Result<()>;
    fn u8(&mut self) -> Result<u8>;
    fn u16(&mut self) -> Result<u16>;
    fn u32(&mut self) -> Result<u32>;
    /// A count of `bytes` (2 or 4).
    fn count(&mut self, bytes: usize) -> Result<u32>;
    /// A numeric leaf.
    fn numeric(&mut self) -> Result<()>;
    /// A name, with its NUL.
    fn name(&mut self) -> Result<()>;
    /// `n` bytes as they are.
    fn bytes(&mut self, n: usize) -> Result<()>;
    /// `n` bytes as they are, where there are as many before the padding.
    fn tail(&mut self, n: usize) -> Result<()>;
    /// The padding to 4 bytes.
    fn pad(&mut self) -> Result<()>;
    /// The kind of the next member of a field list (`leaf`), or whether a method list has
    /// another (then 0); `None` at its end.
    fn sub(&mut self, leaf: bool) -> Result<Option<u16>>;
}

/// The fields of a record of `kind`.
fn walk(io: &mut dyn Io, kind: u16) -> Result<()> {
    use How::{Abs, Back, Prev};
    use Space::{Id, Type};
    match kind {
        0x000a => {
            // a shape of virtual tables: a count, and a nibble each
            let n = io.u16()?;
            io.bytes((n as usize).div_ceil(2))?;
        }
        0x1001 => {
            io.ti(Type, Back)?;
            io.u16()?;
        }
        0x1002 => {
            io.ti(Type, Back)?;
            let attr = io.u32()?;
            // (a pointer to a member: its class and form)
            if matches!((attr >> 5) & 7, 2 | 3) {
                io.ti(Type, Back)?;
                io.u16()?;
            }
        }
        0x1008 => {
            io.ti(Type, Back)?;
            io.u8()?;
            io.u8()?;
            io.u16()?;
            io.ti(Type, Back)?;
        }
        0x1009 => {
            io.ti(Type, Back)?;
            io.ti(Type, Prev)?;
            io.ti(Type, Back)?;
            io.u8()?;
            io.u8()?;
            io.u16()?;
            io.ti(Type, Abs)?;
            io.u32()?;
        }
        0x1201 => {
            let n = io.count(4)?;
            for _ in 0..n {
                io.ti(Type, Back)?;
            }
        }
        0x1203 => {
            while let Some(leaf) = io.sub(true)? {
                match leaf {
                    0x1400 => {
                        io.u16()?;
                        io.ti(Type, Back)?;
                        io.numeric()?;
                    }
                    0x1401 | 0x1402 => {
                        io.u16()?;
                        io.ti(Type, Back)?;
                        io.ti(Type, Back)?;
                        io.numeric()?;
                        io.numeric()?;
                    }
                    0x1404 | 0x1409 => {
                        io.u16()?;
                        io.ti(Type, Back)?;
                    }
                    0x1502 => {
                        io.u16()?;
                        io.numeric()?;
                        io.name()?;
                    }
                    0x150d => {
                        io.u16()?;
                        io.ti(Type, Back)?;
                        io.numeric()?;
                        io.name()?;
                    }
                    0x150e => {
                        io.u16()?;
                        io.ti(Type, Abs)?;
                        io.name()?;
                    }
                    0x1510 | 0x1512 => {
                        io.u16()?;
                        io.ti(Type, Back)?;
                        io.name()?;
                    }
                    0x150f => {
                        io.count(2)?;
                        io.ti(Type, Back)?;
                        io.name()?;
                    }
                    0x1511 => {
                        let attr = io.u16()?;
                        io.ti(Type, Back)?;
                        // (a method that introduces a virtual function has its offset)
                        if matches!((attr >> 2) & 7, 4 | 6) {
                            io.u32()?;
                        }
                        io.name()?;
                    }
                    _ => return Err(corrupt()),
                }
                io.pad()?;
            }
        }
        0x1205 => {
            io.ti(Type, Back)?;
            io.u8()?;
            io.u8()?;
        }
        0x1206 => {
            while io.sub(false)?.is_some() {
                let attr = io.u16()?;
                io.u16()?;
                io.ti(Type, Back)?;
                if matches!((attr >> 2) & 7, 4 | 6) {
                    io.u32()?;
                }
            }
        }
        0x1503 => {
            io.ti(Type, Back)?;
            io.ti(Type, Back)?;
            io.numeric()?;
            io.name()?;
        }
        0x1504 | 0x1505 => {
            io.count(2)?;
            let prop = io.u16()?;
            io.ti(Type, Back)?;
            io.ti(Type, Back)?;
            io.ti(Type, Back)?;
            io.numeric()?;
            io.name()?;
            if prop & 0x200 != 0 {
                io.name()?;
            }
        }
        0x1506 => {
            io.count(2)?;
            let prop = io.u16()?;
            io.ti(Type, Back)?;
            io.numeric()?;
            io.name()?;
            if prop & 0x200 != 0 {
                io.name()?;
            }
        }
        0x1507 => {
            io.count(2)?;
            let prop = io.u16()?;
            io.ti(Type, Back)?;
            io.ti(Type, Back)?;
            io.name()?;
            if prop & 0x200 != 0 {
                io.name()?;
            }
        }
        0x1601 => {
            io.ti(Id, Back)?;
            io.ti(Type, Back)?;
            io.name()?;
            io.tail(8)?;
        }
        0x1602 => {
            io.ti(Type, Back)?;
            io.ti(Type, Back)?;
            io.name()?;
            io.tail(8)?;
        }
        0x1603 => {
            let n = io.count(2)?;
            for _ in 0..n {
                io.ti(Id, Back)?;
            }
        }
        0x1604 => {
            let n = io.count(4)?;
            for _ in 0..n {
                io.ti(Id, Back)?;
            }
        }
        0x1605 => {
            io.ti(Id, Back)?;
            io.name()?;
        }
        0x1606 => {
            io.ti(Type, Back)?;
            io.ti(Id, Back)?;
            io.u32()?;
        }
        0x1607 => {
            io.ti(Type, Back)?;
            io.ti(Id, Back)?;
            io.u32()?;
            io.u16()?;
            // (no padding)
            return Ok(());
        }
        _ => return Err(corrupt()),
    }
    io.pad()
}

/// The bytes after the code of a numeric leaf of `code` that `rest` starts with; `None` for a
/// code that is not one.
fn leaf_size(code: u16, rest: &[u8]) -> Option<usize> {
    Some(match code {
        0..=0x7fff => 0,
        0x8000 => 1,
        0x8001 | 0x8002 => 2,
        0x8003..=0x8005 => 4,
        0x8006 | 0x8009 | 0x800a | 0x800c => 8,
        0x8007 => 10,
        0x8008 | 0x800d => 16,
        0x800b => 6,
        0x800e => 20,
        0x800f => 32,
        0x8010 => 2 + u16::from_le_bytes(*rest.first_chunk()?) as usize,
        _ => return None,
    })
}

/// The canonical code of whole number `v` and the bytes after it (0, or 1, 2, 4, 8).
fn canonical(v: i64) -> (u16, usize) {
    match v {
        0..=0x7fff => (v as u16, 0),
        0x8000..=0xffff => (0x8002, 2),
        0x1_0000..=0xffff_ffff => (0x8004, 4),
        i64::MIN..=-0x8000_0001 => (0x8009, 8),
        -0x8000_0000..=-0x8001 => (0x8003, 4),
        -0x8000..=-0x81 => (0x8001, 2),
        -0x80..=-1 => (0x8000, 1),
        _ => (0x800a, 8),
    }
}

/// The value of an integer leaf of `code` whose bytes are `b`.
fn leaf_value(code: u16, b: &[u8]) -> Option<i64> {
    Some(match code {
        0..=0x7fff => code as i64,
        0x8000 => b[0] as i8 as i64,
        0x8001 => i16::from_le_bytes(*b.first_chunk()?) as i64,
        0x8002 => u16::from_le_bytes(*b.first_chunk()?) as i64,
        0x8003 => i32::from_le_bytes(*b.first_chunk()?) as i64,
        0x8004 => u32::from_le_bytes(*b.first_chunk()?) as i64,
        0x8009 => i64::from_le_bytes(*b.first_chunk()?),
        0x800a => i64::try_from(u64::from_le_bytes(*b.first_chunk()?)).ok()?,
        _ => return None,
    })
}

/// A number that is not in its canonical form, or not a whole one.
const ESCAPE: u64 = u64::MAX;

/// The code of index `r` of a record of index `me` (`begin` the first): the distance back less
/// 1 doubled for a record before it; else odd, the index itself for those below `begin` (the
/// simple types), or `begin` and the distance forward.
fn own_code(r: u32, me: u32, begin: u32) -> u64 {
    if r >= begin && r < me {
        2 * (me - 1 - r) as u64
    } else if r < begin {
        2 * r as u64 + 1
    } else {
        2 * (begin as u64 + (r - me) as u64) + 1
    }
}

fn own_index(v: u64, me: u32, begin: u32) -> Option<u32> {
    if v & 1 == 0 {
        let back = u32::try_from(v >> 1).ok()?;
        let r = me.checked_sub(1)?.checked_sub(back)?;
        (r >= begin).then_some(r)
    } else {
        let u = u32::try_from(v >> 1).ok()?;
        match u < begin {
            true => Some(u),
            false => me.checked_add(u - begin),
        }
    }
}

/// The bytes that pad to 4 after `pos` bytes of a record, as the compilers write them.
fn padding(pos: usize) -> ([u8; 3], usize) {
    let n = pos.wrapping_neg() & 3;
    let mut p = [0; 3];
    for (k, b) in p[..n].iter_mut().enumerate() {
        *b = 0xf0 | (n - k) as u8;
    }
    (p, n)
}

// ---------------------------------------------------------------------------------------------
// The hash of a record

/// A record's hash input.
enum Input<'a> {
    /// The record's bytes, with the length.
    Record,
    /// A name, by the hash of strings.
    Name(&'a [u8]),
    /// The bytes of an index, by the same.
    Bytes(&'a [u8]),
}

fn cstr(d: &[u8], at: usize) -> Option<&[u8]> {
    let s = d.get(at..)?;
    Some(&s[..s.iter().position(|&b| b == 0)?])
}

/// The properties, name and unique name of a record ([`tag_names`]).
type TagNames<'a> = (u16, &'a [u8], Option<&'a [u8]>);

/// The names of a record of a class, a structure, a union or an enum `rec` (with its length):
/// its properties, name and unique name.
fn tag_names(rec: &[u8]) -> Option<TagNames<'_>> {
    let kind = u16::from_le_bytes(*rec.get(2..)?.first_chunk()?);
    let prop = u16::from_le_bytes(*rec.get(6..)?.first_chunk()?);
    let mut at = match kind {
        0x1504 | 0x1505 => 20,
        0x1506 => 12,
        0x1507 => 16,
        _ => return None,
    };
    if kind != 0x1507 {
        let code = u16::from_le_bytes(*rec.get(at..)?.first_chunk()?);
        at += 2 + leaf_size(code, rec.get(at + 2..)?)?;
    }
    let name = cstr(rec, at)?;
    let unique = match prop & 0x200 != 0 {
        true => Some(cstr(rec, at + name.len() + 1)?),
        false => None,
    };
    Some((prop, name, unique))
}

/// What the hash of record `rec` (with its length) is taken of: the record, except for the
/// records of types defined with a name (the name, the unique name for a nested one), and the
/// links of a type to its line (the type's index).
fn hash_input(rec: &[u8]) -> Input<'_> {
    let kind = rec.get(2..4).map_or(0, |k| u16::from_le_bytes([k[0], k[1]]));
    match kind {
        0x1606 | 0x1607 => rec.get(4..8).map_or(Input::Record, Input::Bytes),
        0x1504..=0x1507 => match tag_names(rec) {
            // (a forward declaration, or a type with no name of its own, is by its record)
            Some((prop, name, unique)) if prop & 0x80 == 0 && !anonymous(name) => {
                Input::Name(if prop & 0x100 != 0 { unique.unwrap_or(name) } else { name })
            }
            _ => Input::Record,
        },
        _ => Input::Record,
    }
}

/// Whether `name` is the name given to a type with none.
fn anonymous(name: &[u8]) -> bool {
    let last = name.rsplit(|&b| b == b':').next().unwrap_or(name);
    last == b"<unnamed-tag>" || last == b"__unnamed"
}

/// The hash of a string by the old hash of the PDB format.
fn hash_string(s: &[u8]) -> u32 {
    let mut r = 0u32;
    let (words, mut rest) = s.as_chunks::<4>();
    for w in words {
        r ^= u32::from_le_bytes(*w);
    }
    if rest.len() & 2 != 0 {
        r ^= u16::from_le_bytes([rest[0], rest[1]]) as u32;
        rest = &rest[2..];
    }
    if rest.len() & 1 != 0 {
        r ^= rest[0] as u32;
    }
    r |= 0x2020_2020;
    r ^= r >> 11;
    r ^ (r >> 16)
}

/// The CRC-32 of the table `t` over `d` with no initial value and no final one (the register
/// starts at 0 and is the result).
fn crc(t: &[u32; 256], d: &[u8]) -> u32 {
    let mut c = 0u32;
    for &b in d {
        c = t[((c ^ b as u32) & 0xff) as usize] ^ (c >> 8);
    }
    c
}

fn crc_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    for (b, e) in t.iter_mut().enumerate() {
        let mut c = b as u32;
        for _ in 0..8 {
            c = (c >> 1) ^ (0xedb8_8320 & (c & 1).wrapping_neg());
        }
        *e = c;
    }
    t
}

/// The hash value of record `rec` (with its length) among `buckets`.
fn hash(t: &[u32; 256], rec: &[u8], buckets: u32) -> u32 {
    (match hash_input(rec) {
        Input::Record => crc(t, rec),
        Input::Name(s) | Input::Bytes(s) => hash_string(s),
    }) % buckets
}

/// The offsets of the index of a stream: a record whose bytes `[off, off + len)` start the next
/// part of 8192 bytes of the records (rule 0), or run over its end (rule 1), and the first.
struct Index {
    rule: u8,
    next: u64,
    off: u64,
    first: bool,
}

impl Index {
    fn new(rule: u8) -> Index {
        Index { rule, next: 0, off: 0, first: true }
    }

    /// The entry (its offset) of a record of `len` bytes, if it has one.
    fn entry(&mut self, len: usize) -> Option<u32> {
        let off = self.off;
        self.off += len as u64;
        let has = match (self.first, self.rule) {
            (true, _) => true,
            (_, 0) => off >= self.next,
            _ => self.off / INDEX_STEP > off / INDEX_STEP,
        };
        self.first = false;
        if has {
            self.next = (off / INDEX_STEP + 1) * INDEX_STEP;
        }
        has.then_some(off as u32)
    }
}

/// The header of a stream of `count` records of `bytes` bytes with `entries` entries of the index.
struct Header {
    version: u32,
    begin: u32,
    hash: u16,
    aux: u16,
    key: u32,
    buckets: u32,
}

impl Header {
    /// `lld`: the offset of the (empty) adjusters is the index's, not the end of it.
    fn bytes(&self, count: u32, bytes: u32, entries: u32, lld: bool) -> [u8; HEADER] {
        let mut h = [0u8; HEADER];
        let f = [
            self.version,
            HEADER as u32,
            self.begin,
            self.begin.wrapping_add(count),
            bytes,
            self.hash as u32 | (self.aux as u32) << 16,
            self.key,
            self.buckets,
            0,
            4 * count,
            4 * count,
            8 * entries,
            4 * count + if lld { 0 } else { 8 * entries },
            0,
        ];
        for (k, v) in f.into_iter().enumerate() {
            // (the fifth field has the hash stream's index in 2 bytes, then the aux's)
            h[4 * k..4 * k + 4].copy_from_slice(&v.to_le_bytes());
        }
        h
    }
}

// ---------------------------------------------------------------------------------------------
// The parameters

/// What the parameters say: how each of the two streams is coded, and the sizes of the outputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// The mode of each stream, and its rule: the index's (bit 0) and the header's (bit 1).
    pub modes: [(u8, u8); 2],
    pub sizes: [u64; NUM_OUTPUTS],
}

impl Config {
    pub fn put(&self, out: &mut Vec<u8>) {
        for &(mode, rule) in &self.modes {
            out.push(mode | rule << 2);
        }
        for &s in &self.sizes {
            varint::put(out, s);
        }
    }

    pub fn get(p: &[u8]) -> Result<Config> {
        let &[a, b] = p.first_chunk().ok_or_else(corrupt)?;
        let mut at = 2;
        let mut modes = [(0, 0); 2];
        for (m, v) in modes.iter_mut().zip([a, b]) {
            *m = (v & 3, v >> 2);
            if v >> 2 > 3 || v & 3 > FULL {
                return Err(corrupt());
            }
        }
        let mut sizes = [0; NUM_OUTPUTS];
        for s in &mut sizes {
            *s = varint::get(p, &mut at).map_err(|_| corrupt())?;
        }
        if at != p.len() {
            return Err(corrupt());
        }
        let c = Config { modes, sizes };
        // (an absent stream has no outputs; one that is not full has no hash outputs)
        for k in 0..2 {
            let (mode, _) = modes[k];
            let s = &sizes[3 * k..3 * k + 3];
            let ok = match mode {
                ABSENT => s == [0, 0, 0],
                RECORDS => s[0] >= HEADER as u64 && s[1] == 0 && s[2] == 0,
                _ => s[0] >= HEADER as u64 && s[0] - (HEADER as u64) < 1 << 32 && s[1] % 4 == 0 && s[2] % 8 == 0,
            };
            if !ok {
                return Err(corrupt());
            }
        }
        Ok(c)
    }
}

// ---------------------------------------------------------------------------------------------
// Encoding

/// The streams being made.
pub type Streams = Vec<Vec<u8>>;

#[derive(Debug)]
struct Bad;

impl From<Bad> for crate::Error {
    fn from(_: Bad) -> crate::Error {
        corrupt()
    }
}

/// The reader of a record of the stream of `own` space and index `me`.
struct Enc<'a> {
    rec: &'a [u8],
    pos: usize,
    me: u32,
    begin: u32,
    own: Space,
    /// The last foreign index, and the last of the indices coded as the distance from it.
    fprev: [u32; 2],
    tis: usize,
    attrs: usize,
    st: &'a mut Streams,
}

impl Enc<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        let b = self.rec.get(self.pos..self.pos + n).ok_or_else(corrupt)?;
        self.pos += n;
        Ok(b)
    }

    fn le(&mut self, n: usize) -> Result<u32> {
        let mut v = 0u32;
        for (k, &b) in self.take(n)?.iter().enumerate() {
            v |= (b as u32) << (8 * k);
        }
        Ok(v)
    }
}

impl Io for Enc<'_> {
    fn ti(&mut self, space: Space, how: How) -> Result<()> {
        let r = self.le(4)?;
        if space == self.own {
            let c = match how {
                How::Back => own_code(r, self.me, self.begin),
                How::Abs => r as u64,
                How::Prev => {
                    let c = varint::zz(r as i64 - self.fprev[1] as i64);
                    self.fprev[1] = r;
                    c
                }
            };
            let at = if matches!(how, How::Abs) { TIA } else { self.tis };
            varint::put(&mut self.st[at], c);
        } else {
            varint::put(&mut self.st[FTI], varint::zz(r as i64 - self.fprev[0] as i64));
            self.fprev[0] = r;
        }
        Ok(())
    }

    fn u8(&mut self) -> Result<u8> {
        let v = self.le(1)?;
        varint::put(&mut self.st[self.attrs], v as u64);
        Ok(v as u8)
    }

    fn u16(&mut self) -> Result<u16> {
        let v = self.le(2)?;
        varint::put(&mut self.st[self.attrs], v as u64);
        Ok(v as u16)
    }

    fn u32(&mut self) -> Result<u32> {
        let v = self.le(4)?;
        varint::put(&mut self.st[self.attrs], v as u64);
        Ok(v)
    }

    fn count(&mut self, bytes: usize) -> Result<u32> {
        let v = self.le(bytes)?;
        varint::put(&mut self.st[NUM], v as u64);
        Ok(v)
    }

    fn numeric(&mut self) -> Result<()> {
        let start = self.pos;
        let code = self.le(2)? as u16;
        let n = leaf_size(code, self.rec.get(self.pos..).unwrap_or(&[])).ok_or_else(corrupt)?;
        let body = self.take(n)?;
        let canon = leaf_value(code, body).filter(|v| v.unsigned_abs() < 1 << 62 && canonical(*v) == (code, n));
        match canon {
            Some(v) => varint::put(&mut self.st[NUM], varint::zz(v)),
            None => {
                varint::put(&mut self.st[NUM], ESCAPE);
                let leaf = &self.rec[start..self.pos];
                varint::put(&mut self.st[RAW], leaf.len() as u64);
                self.st[RAW].extend_from_slice(leaf);
            }
        }
        Ok(())
    }

    fn name(&mut self) -> Result<()> {
        let s = cstr(self.rec, self.pos).ok_or_else(corrupt)?;
        self.st[NAME].extend_from_slice(s);
        self.st[NAME].push(0);
        self.pos += s.len() + 1;
        Ok(())
    }

    fn bytes(&mut self, n: usize) -> Result<()> {
        let b = self.rec.get(self.pos..self.pos + n).ok_or_else(corrupt)?;
        self.st[RAW].extend_from_slice(b);
        self.pos += n;
        Ok(())
    }

    fn tail(&mut self, n: usize) -> Result<()> {
        let has = self.rec.len() - self.pos >= n;
        varint::put(&mut self.st[self.attrs], has as u64);
        match has {
            true => self.bytes(n),
            false => Ok(()),
        }
    }

    fn pad(&mut self) -> Result<()> {
        let (p, n) = padding(self.pos);
        match self.rec.get(self.pos..self.pos + n) == Some(&p[..n]) {
            true => {
                self.pos += n;
                Ok(())
            }
            false => Err(corrupt()),
        }
    }

    fn sub(&mut self, leaf: bool) -> Result<Option<u16>> {
        if self.pos == self.rec.len() {
            self.st[SUB].push(0);
            return Ok(None);
        }
        if !leaf {
            self.st[SUB].push(ITEM);
            return Ok(Some(0));
        }
        let k = self.le(2)? as u16;
        let code = SUBS.iter().position(|&s| s == k).ok_or_else(corrupt)?;
        self.st[SUB].push(code as u8 + 1);
        Ok(Some(k))
    }
}

/// Record `rec` (with its length) of index `me` coded to `st`.
fn encode_record(st: &mut Streams, own: Space, me: u32, begin: u32, fprev: &mut [u32; 2], rec: &[u8]) {
    let kind = u16::from_le_bytes([rec[2], rec[3]]);
    let marks: [usize; NUM_INPUTS] = std::array::from_fn(|i| st[i].len());
    if let Some(code) = KINDS.iter().position(|&k| k == kind) {
        st[KIND].push(code as u8);
        let (tis, attrs) = classes(kind);
        let mut e = Enc { rec, pos: 4, me, begin, own, fprev: *fprev, tis, attrs, st };
        let ok = walk(&mut e, kind).is_ok() && e.pos == rec.len();
        let after = e.fprev;
        if ok {
            *fprev = after;
            return;
        }
        for (s, m) in st.iter_mut().zip(marks) {
            s.truncate(m);
        }
    }
    st[KIND].push(KEPT);
    varint::put(&mut st[RAW], kind as u64);
    varint::put(&mut st[RAW], rec.len() as u64 - 2);
    st[RAW].extend_from_slice(&rec[4..]);
}

fn u32le(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(*d.get(at..)?.first_chunk()?))
}

fn u16le(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(*d.get(at..)?.first_chunk()?))
}

/// The records of the stream `d` after its header: where each is.
fn records(d: &[u8]) -> Option<Vec<std::ops::Range<usize>>> {
    let mut out = Vec::new();
    let mut at = HEADER;
    while at < d.len() {
        let l = u16le(d, at)? as usize;
        if l < 2 || at + 2 + l > d.len() {
            return None;
        }
        out.push(at..at + 2 + l);
        at += 2 + l;
    }
    Some(out)
}

/// How a stream is coded: its mode, its index's rule and its outputs' sizes.
pub type Coded = (u8, u8, [u64; 3]);

/// Stream `d` of space `own` coded to `st` as `FULL` where its hash stream `hash` (with its index
/// `index` in the file) is derived from it, else as `RECORDS`; `None` (and `st` as it was) where
/// not even its records are coded.
pub fn encode_stream(st: &mut Streams, own: Space, d: &[u8], hash: Option<(&[u8], u16)>) -> Option<Coded> {
    let marks: [usize; NUM_INPUTS] = std::array::from_fn(|i| st[i].len());
    let coded = encode_records(st, own, d, hash);
    if coded.is_none() {
        for (s, m) in st.iter_mut().zip(marks) {
            s.truncate(m);
        }
    }
    coded
}

fn encode_records(st: &mut Streams, own: Space, d: &[u8], hash: Option<(&[u8], u16)>) -> Option<Coded> {
    let recs = records(d)?;
    let (version, hsize, begin) = (u32le(d, 0)?, u32le(d, 4)?, u32le(d, 8)?);
    if version != 20040203 || hsize != HEADER as u32 || recs.is_empty() || begin == 0 {
        return None;
    }
    let count = u32::try_from(recs.len()).ok()?;
    begin.checked_add(count)?;
    let kept = st[HDR].len();
    st[HDR].extend_from_slice(&d[..HEADER]);
    let mut fprev = [0; 2];
    for (k, r) in recs.iter().enumerate() {
        encode_record(st, own, begin + k as u32, begin, &mut fprev, &d[r.clone()]);
    }
    // the hash stream, if it follows from the records (else nothing changes)
    if let Some((h, index)) = hash
        && let Some(coded) = encode_hash(st, d, &recs, h, index, kept)
    {
        return Some(coded);
    }
    Some((RECORDS, 0, [d.len() as u64, 0, 0]))
}

/// The header of `d`, the exceptions to the hashes and the rule of the index put to `hdr`, in
/// place of the header kept at `kept`.
fn encode_hash(
    st: &mut Streams,
    d: &[u8],
    recs: &[std::ops::Range<usize>],
    h: &[u8],
    index: u16,
    kept: usize,
) -> Option<Coded> {
    let count = recs.len();
    let (hash_idx, aux) = (u16le(d, 20)?, u16le(d, 22)?);
    let (key, buckets) = (u32le(d, 24)?, u32le(d, 28)?);
    if hash_idx != index || buckets == 0 || h.len() < 4 * count {
        return None;
    }
    let hv = &h[..4 * count];
    let idx = &h[4 * count..];
    // the index by the rule that gives it
    let entries = |rule: u8| {
        let mut i = Index::new(rule);
        let mut out = Vec::new();
        let begin = u32le(d, 8)?;
        for (k, r) in recs.iter().enumerate() {
            if let Some(off) = i.entry(r.len()) {
                out.extend_from_slice(&(begin + k as u32).to_le_bytes());
                out.extend_from_slice(&off.to_le_bytes());
            }
        }
        Some(out)
    };
    let rule = (0..2).find(|&r| entries(r).as_deref() == Some(idx))?;
    let e = (idx.len() / 8) as u32;
    let header = Header { version: 20040203, begin: u32le(d, 8)?, hash: hash_idx, aux, key, buckets };
    // (the header, as the linkers write it)
    let lld = (0..2).find(|&l| header.bytes(count as u32, (d.len() - HEADER) as u32, e, l == 1)[..] == d[..HEADER])?;
    // the hashes that are not as derived
    let t = crc_table();
    let mut exceptions = Vec::new();
    let mut last = 0;
    let mut n = 0u64;
    for (k, r) in recs.iter().enumerate() {
        let want = u32::from_le_bytes(hv[4 * k..4 * k + 4].try_into().unwrap());
        if hash(&t, &d[r.clone()], buckets) != want {
            varint::put(&mut exceptions, (k - last) as u64);
            varint::put(&mut exceptions, want as u64);
            last = k;
            n += 1;
        }
    }
    st[HDR].truncate(kept);
    let hdr = &mut st[HDR];
    for v in [20040203u64, header.begin as u64, hash_idx as u64, aux as u64, key as u64, buckets as u64, n] {
        varint::put(hdr, v);
    }
    hdr.extend_from_slice(&exceptions);
    Some((FULL, rule | lld << 1, [d.len() as u64, 4 * count as u64, idx.len() as u64]))
}

/// For each kind of the records of stream `d`: how many, their bytes, how many are kept as they
/// are, and the bytes each stream takes of them (for the check tool).
#[doc(hidden)]
pub fn by_kind(d: &[u8], own: Space) -> Vec<(u16, usize, usize, usize, [usize; NUM_INPUTS])> {
    let mut out: Vec<(u16, usize, usize, usize, [usize; NUM_INPUTS])> = Vec::new();
    let Some(recs) = records(d) else { return out };
    let begin = u32le(d, 8).unwrap_or(0x1000);
    let mut st: Streams = vec![Vec::new(); NUM_INPUTS];
    let mut fprev = [0; 2];
    for (k, r) in recs.iter().enumerate() {
        st.iter_mut().for_each(|s| s.clear());
        encode_record(&mut st, own, begin + k as u32, begin, &mut fprev, &d[r.clone()]);
        let kind = u16::from_le_bytes([d[r.start + 2], d[r.start + 3]]);
        let at = match out.iter().position(|e| e.0 == kind) {
            Some(at) => at,
            None => {
                out.push((kind, 0, 0, 0, [0; NUM_INPUTS]));
                out.len() - 1
            }
        };
        let e = &mut out[at];
        e.1 += 1;
        e.2 += r.len();
        e.3 += (st[KIND][0] == KEPT) as usize;
        for (t, s) in e.4.iter_mut().zip(&st) {
            *t += s.len();
        }
    }
    out
}

/// What [`encode`] makes.
#[derive(Debug)]
pub struct Encoded {
    /// The module's params.
    pub params: Vec<u8>,
    /// The inputs of the module, one for each of [`STREAM_NAMES`].
    pub streams: Streams,
    /// The sizes of the module's outputs.
    pub sizes: [u64; NUM_OUTPUTS],
    /// Where the module's outputs go among the streams one after another: for each stream coded,
    /// its bytes, and the outputs (1 or 2) that make them.
    pub places: Vec<Place>,
}

/// Bytes of the streams one after another that outputs `outputs` of the module are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    pub bytes: std::ops::Range<usize>,
    /// The outputs, in order.
    pub outputs: Vec<usize>,
}

/// The TPI and IPI streams among `streams` (the streams of a file one after another, each where
/// `spans` tells) coded, or `None` where neither is.
pub fn encode(streams: &[u8], spans: &[Option<std::ops::Range<usize>>]) -> Option<Encoded> {
    let attempt = |hashes: bool| {
        let mut st: Streams = vec![Vec::new(); NUM_INPUTS];
        let mut config = Config { modes: [(ABSENT, 0); 2], sizes: [0; NUM_OUTPUTS] };
        let mut places = Vec::new();
        let mut taken: Vec<std::ops::Range<usize>> = Vec::new();
        for (k, (index, own)) in [(2usize, Space::Type), (4, Space::Id)].into_iter().enumerate() {
            let Some(r) = spans.get(index).cloned().flatten() else { continue };
            let d = &streams[r.clone()];
            // the hash stream, if the header names one that is another stream of the file
            let hash = u16le(d, 20)
                .filter(|&h| hashes && h != u16::MAX)
                .and_then(|h| Some((spans.get(h as usize)?.clone()?, h)))
                .filter(|(hr, _)| !hr.is_empty() && hr != &r && !taken.contains(hr));
            let hash_bytes = hash.as_ref().map(|(hr, h)| (&streams[hr.clone()], *h));
            let Some((mode, rule, sizes)) = encode_stream(&mut st, own, d, hash_bytes) else { continue };
            config.modes[k] = (mode, rule);
            config.sizes[3 * k..3 * k + 3].copy_from_slice(&sizes);
            places.push(Place { bytes: r.clone(), outputs: vec![3 * k] });
            taken.push(r);
            if mode == FULL {
                let hr = hash.unwrap().0;
                places.push(Place { bytes: hr.clone(), outputs: vec![3 * k + 1, 3 * k + 2] });
                taken.push(hr);
            }
        }
        if config.modes == [(ABSENT, 0); 2] {
            return None;
        }
        let mut params = Vec::new();
        config.put(&mut params);
        let inputs: Vec<&[u8]> = st.iter().map(|s| &s[..]).collect();
        let back = crate::module::types(&params, &inputs, config.sizes.iter().sum()).ok()?;
        // the outputs are the bytes they stand for
        for p in &places {
            let start: u64 = config.sizes[..p.outputs[0]].iter().sum();
            let size: u64 = p.outputs.iter().map(|&o| config.sizes[o]).sum();
            if back.get(start as usize..(start + size) as usize)? != &streams[p.bytes.clone()] {
                return None;
            }
        }
        Some(Encoded { params, streams: st, sizes: config.sizes, places })
    };
    attempt(true).or_else(|| attempt(false))
}

// ---------------------------------------------------------------------------------------------
// Decoding

/// The bytes of a piece of an input, and of the buffer of an output.
pub const PIECE: usize = 8192;

/// An output put in pieces, of `size` bytes in all.
struct Out<'a> {
    sink: &'a mut dyn Put,
    buf: Vec<u8>,
    sent: u64,
    size: u64,
}

impl<'a> Out<'a> {
    fn new(sink: &'a mut dyn Put, size: u64) -> Out<'a> {
        Out { sink, buf: Vec::with_capacity(PIECE), sent: 0, size }
    }

    fn total(&self) -> u64 {
        self.sent + self.buf.len() as u64
    }

    fn flush(&mut self) -> Result<()> {
        self.sink.put(&self.buf).ok_or_else(corrupt)?;
        self.sent += self.buf.len() as u64;
        self.buf.clear();
        Ok(())
    }

    fn put(&mut self, d: &[u8]) -> Result<()> {
        if d.len() as u64 > self.size - self.total() {
            return Err(corrupt());
        }
        if self.buf.len() + d.len() > PIECE {
            self.flush()?;
        }
        match d.len() > PIECE {
            true => {
                self.sink.put(d).ok_or_else(corrupt)?;
                self.sent += d.len() as u64;
            }
            false => self.buf.extend_from_slice(d),
        }
        Ok(())
    }

    /// Puts what is left; fails where the output is not `size` bytes.
    fn finish(mut self) -> Result<()> {
        self.flush()?;
        match self.sent == self.size {
            true => Ok(()),
            false => Err(corrupt()),
        }
    }
}

/// The writer of a record into `rec` from the streams.
struct Dec<'a> {
    rd: [Reader<'a>; NUM_INPUTS],
    rec: Vec<u8>,
    me: u32,
    begin: u32,
    own: Space,
    fprev: [u32; 2],
    tis: usize,
    attrs: usize,
}

impl Dec<'_> {
    fn push(&mut self, b: &[u8]) -> Result<()> {
        if self.rec.len() + b.len() > MAX_RECORD {
            return Err(corrupt());
        }
        self.rec.extend_from_slice(b);
        Ok(())
    }

    fn num(&mut self, s: usize) -> Result<u64> {
        self.rd[s].num().map_err(|_| corrupt())
    }

    /// The next record into `rec`.
    fn record(&mut self) -> Result<()> {
        self.rec.clear();
        self.rec.extend_from_slice(&[0, 0]);
        let code = self.rd[KIND].byte().ok_or_else(corrupt)?;
        if code == KEPT {
            let kind = u16::try_from(self.num(RAW)?).map_err(|_| corrupt())?;
            let n = usize::try_from(self.num(RAW)?).map_err(|_| corrupt())?;
            if !(2..=MAX_RECORD - 2).contains(&n) {
                return Err(corrupt());
            }
            self.rec.extend_from_slice(&kind.to_le_bytes());
            self.copy(RAW, n - 2)?;
        } else {
            let kind = *KINDS.get(code as usize).ok_or_else(corrupt)?;
            (self.tis, self.attrs) = classes(kind);
            self.rec.extend_from_slice(&kind.to_le_bytes());
            walk(self, kind)?;
        }
        let l = u16::try_from(self.rec.len() - 2).map_err(|_| corrupt())?;
        self.rec[..2].copy_from_slice(&l.to_le_bytes());
        Ok(())
    }

    /// `n` bytes of stream `s` appended to the record.
    fn copy(&mut self, s: usize, n: usize) -> Result<()> {
        if self.rec.len() + n > MAX_RECORD {
            return Err(corrupt());
        }
        let at = self.rec.len();
        self.rec.resize(at + n, 0);
        self.rd[s].exact(&mut self.rec[at..]).ok_or_else(corrupt)
    }
}

impl Io for Dec<'_> {
    fn ti(&mut self, space: Space, how: How) -> Result<()> {
        let r = if space == self.own {
            let v = self.num(if matches!(how, How::Abs) { TIA } else { self.tis })?;
            match how {
                How::Back => own_index(v, self.me, self.begin).ok_or_else(corrupt)?,
                How::Abs => u32::try_from(v).map_err(|_| corrupt())?,
                How::Prev => {
                    let r =
                        u32::try_from((self.fprev[1] as i64).wrapping_add(varint::unzz(v))).map_err(|_| corrupt())?;
                    self.fprev[1] = r;
                    r
                }
            }
        } else {
            let v = self.num(FTI)?;
            let r = u32::try_from((self.fprev[0] as i64).wrapping_add(varint::unzz(v))).map_err(|_| corrupt())?;
            self.fprev[0] = r;
            r
        };
        self.push(&r.to_le_bytes())
    }

    fn u8(&mut self) -> Result<u8> {
        let v = u8::try_from(self.num(self.attrs)?).map_err(|_| corrupt())?;
        self.push(&[v])?;
        Ok(v)
    }

    fn u16(&mut self) -> Result<u16> {
        let v = u16::try_from(self.num(self.attrs)?).map_err(|_| corrupt())?;
        self.push(&v.to_le_bytes())?;
        Ok(v)
    }

    fn u32(&mut self) -> Result<u32> {
        let v = u32::try_from(self.num(self.attrs)?).map_err(|_| corrupt())?;
        self.push(&v.to_le_bytes())?;
        Ok(v)
    }

    fn count(&mut self, bytes: usize) -> Result<u32> {
        let v = u32::try_from(self.num(NUM)?).map_err(|_| corrupt())?;
        if bytes == 2 && v > 0xffff {
            return Err(corrupt());
        }
        self.push(&v.to_le_bytes()[..bytes])?;
        Ok(v)
    }

    fn numeric(&mut self) -> Result<()> {
        let z = self.num(NUM)?;
        if z == ESCAPE {
            let n = usize::try_from(self.num(RAW)?).map_err(|_| corrupt())?;
            return self.copy(RAW, n);
        }
        let v = varint::unzz(z);
        let (code, n) = canonical(v);
        if n == 0 {
            return self.push(&code.to_le_bytes());
        }
        self.push(&code.to_le_bytes())?;
        self.push(&v.to_le_bytes()[..n])
    }

    fn name(&mut self) -> Result<()> {
        let mut over = false;
        let rec = &mut self.rec;
        let found = self.rd[NAME].until(0, |piece| {
            over = rec.len() + piece.len() + 1 > MAX_RECORD;
            if !over {
                rec.extend_from_slice(piece);
            }
            !over
        });
        if !found {
            return Err(corrupt());
        }
        self.push(&[0])
    }

    fn bytes(&mut self, n: usize) -> Result<()> {
        self.copy(RAW, n)
    }

    fn tail(&mut self, n: usize) -> Result<()> {
        match self.num(self.attrs)? {
            0 => Ok(()),
            1 => self.copy(RAW, n),
            _ => Err(corrupt()),
        }
    }

    fn pad(&mut self) -> Result<()> {
        let (p, n) = padding(self.rec.len());
        self.push(&p[..n])
    }

    fn sub(&mut self, leaf: bool) -> Result<Option<u16>> {
        match self.rd[SUB].byte().ok_or_else(corrupt)? {
            0 => Ok(None),
            ITEM if !leaf => Ok(Some(0)),
            c if leaf => {
                let k = *SUBS.get(c as usize - 1).ok_or_else(corrupt)?;
                self.push(&k.to_le_bytes())?;
                Ok(Some(k))
            }
            _ => Err(corrupt()),
        }
    }
}

/// The outputs of a stream.
struct Outs<'a, 'b> {
    stream: &'b mut Out<'a>,
    hashes: &'b mut Out<'a>,
    index: &'b mut Out<'a>,
}

/// A stream of `own` space in `mode` (its index's rule `rule`) put to `o`.
fn decode_stream(d: &mut Dec, o: Outs, own: Space, mode: u8, rule: u8, table: &[u32; 256]) -> Result<()> {
    let size = o.stream.size;
    let mut fixed = [0u8; HEADER];
    let (mut left, mut buckets) = (0u64, 1u32);
    if mode == RECORDS {
        d.rd[HDR].exact(&mut fixed).ok_or_else(corrupt)?;
        d.begin = u32le(&fixed, 8).ok_or_else(corrupt)?;
    } else {
        let mut v = [0u64; 7];
        for x in &mut v {
            *x = d.num(HDR)?;
        }
        let w = |k: usize| u32::try_from(v[k]).map_err(|_| corrupt());
        let (hash, aux) = (u16::try_from(v[2]).map_err(|_| corrupt())?, u16::try_from(v[3]).map_err(|_| corrupt())?);
        let h = Header { version: w(0)?, begin: w(1)?, hash, aux, key: w(4)?, buckets: w(5)? };
        (d.begin, buckets, left) = (h.begin, h.buckets, v[6]);
        if buckets == 0 {
            return Err(corrupt());
        }
        let count = o.hashes.size / 4;
        fixed = h.bytes(count as u32, (size - HEADER as u64) as u32, (o.index.size / 8) as u32, rule & 2 != 0);
    }
    o.stream.put(&fixed)?;
    d.own = own;
    d.fprev = [0; 2];
    let mut index = Index::new(rule & 1);
    // (the next exception to the hashes: its record's distance from the last)
    let mut next = u64::MAX;
    if left > 0 {
        next = d.num(HDR)?;
    }
    let count = o.hashes.size / 4;
    let mut n = 0u64;
    while match mode {
        FULL => n < count,
        _ => o.stream.total() < size,
    } {
        d.me = d.begin.checked_add(n as u32).ok_or_else(corrupt)?;
        d.record()?;
        if mode == FULL {
            let mut h = hash(table, &d.rec, buckets);
            if n == next {
                h = u32::try_from(d.num(HDR)?).map_err(|_| corrupt())?;
                left -= 1;
                next = if left > 0 { n.checked_add(d.num(HDR)?).ok_or_else(corrupt)? } else { u64::MAX };
            }
            o.hashes.put(&h.to_le_bytes())?;
            if let Some(off) = index.entry(d.rec.len()) {
                o.index.put(&d.me.to_le_bytes())?;
                o.index.put(&off.to_le_bytes())?;
            }
        }
        o.stream.put(&d.rec)?;
        n += 1;
    }
    match left {
        0 => Ok(()),
        _ => Err(corrupt()),
    }
}

/// The memory plan of [`run`] ([`aqr_api::stack`]): what it takes beyond `initial` bytes at
/// most, whatever the lengths: a piece for each input, a record and a piece for each output.
pub fn plan(initial: u64) -> u64 {
    let mut m = aqr_api::stack::Sim::new(initial);
    for _ in 0..NUM_INPUTS {
        m.take(PIECE as u64);
    }
    m.take(MAX_RECORD as u64);
    for _ in 0..NUM_OUTPUTS {
        m.take(PIECE as u64);
    }
    m.plan()
}

/// The module: the outputs (`config` has their sizes) put from the inputs.
pub fn run(config: &Config, ins: [&mut dyn Pull; NUM_INPUTS], outs: [&mut dyn Put; NUM_OUTPUTS]) -> Result<()> {
    // (in the order of the plan)
    let rd = ins.map(|i| Reader::new(i, PIECE));
    let rec = Vec::with_capacity(MAX_RECORD);
    let mut d = Dec { rd, rec, me: 0, begin: 0, own: Space::Type, fprev: [0; 2], tis: TI, attrs: ATTR };
    let [s0, s1, s2, s3, s4, s5] = config.sizes;
    let [o0, o1, o2, o3, o4, o5] = outs;
    let mut outs =
        [Out::new(o0, s0), Out::new(o1, s1), Out::new(o2, s2), Out::new(o3, s3), Out::new(o4, s4), Out::new(o5, s5)];
    let table = crc_table();
    for (k, own) in [Space::Type, Space::Id].into_iter().enumerate() {
        let (mode, rule) = config.modes[k];
        if mode == ABSENT {
            continue;
        }
        let [a, b, c] = &mut outs[3 * k..3 * k + 3] else { unreachable!() };
        decode_stream(&mut d, Outs { stream: a, hashes: b, index: c }, own, mode, rule, &table)?;
    }
    for o in outs {
        o.finish()?;
    }
    if !d.rd.iter_mut().all(|r| r.done()) {
        return Err(corrupt());
    }
    Ok(())
}
