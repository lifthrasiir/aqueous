//! The layout of a DEX file as the module sees it (FORMAT.md), and its decoding: the file from
//! its streams.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

use aqr_common::varint;
use sha1::{Digest, Sha1};

use crate::{Result, corrupt};

/// The bytes of the header.
pub const HEADER: usize = 0x70;

/// The types of map items (`type_code`s of the DEX format).
pub mod ty {
    pub const HEADER: u16 = 0x0000;
    pub const STRING_ID: u16 = 0x0001;
    pub const TYPE_ID: u16 = 0x0002;
    pub const PROTO_ID: u16 = 0x0003;
    pub const FIELD_ID: u16 = 0x0004;
    pub const METHOD_ID: u16 = 0x0005;
    pub const CLASS_DEF: u16 = 0x0006;
    pub const MAP_LIST: u16 = 0x1000;
    pub const TYPE_LIST: u16 = 0x1001;
    pub const ANNOTATION_SET_REF_LIST: u16 = 0x1002;
    pub const ANNOTATION_SET: u16 = 0x1003;
    pub const CLASS_DATA: u16 = 0x2000;
    pub const CODE: u16 = 0x2001;
    pub const STRING_DATA: u16 = 0x2002;
    pub const DEBUG_INFO: u16 = 0x2003;
    pub const ANNOTATION: u16 = 0x2004;
    pub const ENCODED_ARRAY: u16 = 0x2005;
    pub const ANNOTATIONS_DIRECTORY: u16 = 0x2006;
}

/// The streams, by their index among the inputs.
pub mod s {
    pub const REST: usize = 0;
    pub const HEAD: usize = 1;
    pub const CODE: usize = 2;
    pub const DEBUG: usize = 3;
    pub const TYPE_LIST: usize = 4;
    pub const ANNOTATION: usize = 5;
    pub const ANNOTATION_SETS: usize = 6;
    pub const ARRAY: usize = 7;
    pub const RAW: usize = 8;
    pub const STR_PREFIX: usize = 9;
    pub const STR_SUFFIX: usize = 10;
    pub const TYPE: usize = 11;
    pub const PROTO_RET: usize = 12;
    pub const PROTO_PARAMS: usize = 13;
    pub const PROTO_SHORTY: usize = 14;
    pub const FIELD_CLASS: usize = 15;
    pub const FIELD_NAME: usize = 16;
    pub const FIELD_TYPE: usize = 17;
    pub const METHOD_CLASS: usize = 18;
    pub const METHOD_NAME: usize = 19;
    pub const METHOD_PROTO: usize = 20;
    pub const CDEF_CLASS: usize = 21;
    pub const CDEF_FLAGS: usize = 22;
    pub const CDEF_SUPER: usize = 23;
    pub const CDEF_IFACES: usize = 24;
    pub const CDEF_SRC: usize = 25;
    pub const CDEF_OFFS: usize = 26;
    pub const CDATA_SIZES: usize = 27;
    pub const CDATA_FDIFF: usize = 28;
    pub const CDATA_FFLAGS: usize = 29;
    pub const CDATA_MDIFF: usize = 30;
    pub const CDATA_MFLAGS: usize = 31;
    pub const CDATA_CODE: usize = 32;
    pub const N: usize = 33;
}

/// The names of the streams, in order (their classes are `dex/NAME`).
pub const STREAM_NAMES: [&str; s::N] = [
    "rest",
    "head",
    "code",
    "debug",
    "type-list",
    "annotation",
    "annotation-sets",
    "array",
    "raw",
    "str-prefix",
    "str-suffix",
    "type",
    "proto-ret",
    "proto-params",
    "proto-shorty",
    "field-class",
    "field-name",
    "field-type",
    "method-class",
    "method-name",
    "method-proto",
    "cdef-class",
    "cdef-flags",
    "cdef-super",
    "cdef-ifaces",
    "cdef-src",
    "cdef-offs",
    "cdata-sizes",
    "cdata-fdiff",
    "cdata-fflags",
    "cdata-mdiff",
    "cdata-mflags",
    "cdata-code",
];

/// How a map item's section is rebuilt.
pub mod mode {
    /// Not at all: its bytes are among the rest.
    pub const REST: u8 = 0;
    /// As it is, from the stream of its type ([`super::raw_stream`]).
    pub const RAW: u8 = 1;
    /// From the streams of its type (FORMAT.md §4).
    pub const TAKEN: u8 = 2;
}

/// The stream of a section kept as it is.
pub fn raw_stream(t: u16) -> usize {
    match t {
        ty::CODE => s::CODE,
        ty::DEBUG_INFO => s::DEBUG,
        ty::TYPE_LIST => s::TYPE_LIST,
        ty::ANNOTATION => s::ANNOTATION,
        ty::ANNOTATION_SET | ty::ANNOTATION_SET_REF_LIST | ty::ANNOTATIONS_DIRECTORY => s::ANNOTATION_SETS,
        ty::ENCODED_ARRAY => s::ARRAY,
        _ => s::RAW,
    }
}

/// Whether a section of type `t` may be taken ([`mode::TAKEN`]).
pub fn takes(t: u16) -> bool {
    matches!(
        t,
        ty::STRING_ID
            | ty::STRING_DATA
            | ty::TYPE_ID
            | ty::PROTO_ID
            | ty::FIELD_ID
            | ty::METHOD_ID
            | ty::CLASS_DEF
            | ty::CLASS_DATA
    )
}

/// The bytes of an entry of the tables of fixed size.
pub fn entry_size(t: u16) -> usize {
    match t {
        ty::STRING_ID | ty::TYPE_ID => 4,
        ty::PROTO_ID => 12,
        ty::FIELD_ID | ty::METHOD_ID => 8,
        ty::CLASS_DEF => 32,
        _ => 0,
    }
}

/// An item of the map list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Item {
    pub ty: u16,
    pub count: u32,
    pub off: u32,
}

pub fn le16(d: &[u8], at: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(*d.get(at..).and_then(|d| d.first_chunk()).ok_or_else(corrupt)?))
}

pub fn le32(d: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(*d.get(at..).and_then(|d| d.first_chunk()).ok_or_else(corrupt)?))
}

/// The items of the map list's entries `map` (12 bytes each, after its count).
pub fn items(map: &[u8]) -> Vec<Item> {
    map.as_chunks::<12>().0.iter()
        .map(|e| Item {
            ty: u16::from_le_bytes([e[0], e[1]]),
            count: u32::from_le_bytes([e[4], e[5], e[6], e[7]]),
            off: u32::from_le_bytes([e[8], e[9], e[10], e[11]]),
        })
        .collect()
}

/// Where the section of item `i` ends as kept as it is: at the least offset of the items above
/// its own, else at `len`.
pub fn raw_end(items: &[Item], i: usize, len: usize) -> usize {
    let off = items[i].off;
    items.iter().filter(|e| e.off > off).map(|e| e.off as usize).min().unwrap_or(len)
}

pub fn align4(p: usize) -> usize {
    (p + 3) & !3
}

/// An unsigned LEB128 value of the DEX format (at most 5 bytes, below 2^32) at `*at`.
pub fn uleb(d: &[u8], at: &mut usize) -> Result<u32> {
    let mut v = 0u64;
    for i in 0..5 {
        let b = *d.get(*at).ok_or_else(corrupt)?;
        *at += 1;
        v |= ((b & 0x7f) as u64) << (7 * i);
        if b < 0x80 {
            return u32::try_from(v).map_err(|_| corrupt());
        }
    }
    Err(corrupt())
}

/// A signed LEB128 value (at most 5 bytes) at `*at`.
pub fn sleb(d: &[u8], at: &mut usize) -> Result<i32> {
    let mut v = 0i64;
    for i in 0..5 {
        let b = *d.get(*at).ok_or_else(corrupt)?;
        *at += 1;
        v |= ((b & 0x7f) as i64) << (7 * i);
        if b < 0x80 {
            let bits = 7 * (i + 1);
            if bits < 64 && b & 0x40 != 0 {
                v -= 1 << bits;
            }
            return i32::try_from(v).map_err(|_| corrupt());
        }
    }
    Err(corrupt())
}

/// The UTF-16 code units of MUTF-8 bytes `s`: its bytes that do not continue a sequence.
pub fn utf16_len(s: &[u8]) -> u32 {
    s.iter().filter(|&&b| b & 0xc0 != 0x80).count() as u32
}

/// The letter of a type descriptor in a shorty: its first, `L` for arrays.
pub fn shorty_char(desc: &[u8]) -> Option<u8> {
    match desc.first()? {
        b'[' => Some(b'L'),
        &c => Some(c),
    }
}

/// `p + n`, which must not be past `len`.
fn end(p: usize, n: u64, len: usize) -> Result<usize> {
    (p as u64).checked_add(n).filter(|&e| e <= len as u64).map(|e| e as usize).ok_or_else(corrupt)
}

/// The offsets of the `count` type lists from `off` in `d`.
pub fn type_lists(d: &[u8], off: usize, count: u32) -> Result<Vec<u32>> {
    let mut p = off;
    let mut v = Vec::with_capacity((count as usize).min(d.len() / 4));
    for _ in 0..count {
        p = align4(p);
        let n = le32(d, p)? as u64;
        v.push(u32::try_from(p).map_err(|_| corrupt())?);
        p = end(p, 4 + 2 * n, d.len())?;
    }
    Ok(v)
}

/// The offsets of the `count` code items from `off` in `d`.
pub fn code_items(d: &[u8], off: usize, count: u32) -> Result<Vec<u32>> {
    let mut p = off;
    let mut v = Vec::with_capacity((count as usize).min(d.len() / 16));
    for _ in 0..count {
        p = align4(p);
        v.push(u32::try_from(p).map_err(|_| corrupt())?);
        let tries = le16(d, p + 6)? as u64;
        let insns = le32(d, p + 12)? as u64;
        let mut q = end(p, 16 + 2 * insns, d.len())?;
        if tries > 0 {
            q = end(q, (insns & 1) * 2 + 8 * tries, d.len())?;
            let n = uleb(d, &mut q)?;
            for _ in 0..n {
                let size = sleb(d, &mut q)?;
                for _ in 0..size.unsigned_abs() {
                    uleb(d, &mut q)?;
                    uleb(d, &mut q)?;
                }
                if size <= 0 {
                    uleb(d, &mut q)?;
                }
            }
        }
        if q > d.len() {
            return Err(corrupt());
        }
        p = q;
    }
    Ok(v)
}

/// FNV-1a, for maps that must be the same in Wasm (no random keys).
#[derive(Default)]
pub struct Fnv(u64);

impl Hasher for Fnv {
    fn write(&mut self, b: &[u8]) {
        let mut h = if self.0 == 0 { 0xcbf2_9ce4_8422_2325 } else { self.0 };
        for &x in b {
            h = (h ^ x as u64).wrapping_mul(0x100_0000_01b3);
        }
        self.0 = h;
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

/// The index of each string of `strings` (the first of equal ones).
pub fn string_index<'a>(
    strings: impl ExactSizeIterator<Item = &'a [u8]>,
) -> HashMap<&'a [u8], u32, BuildHasherDefault<Fnv>> {
    let mut m = HashMap::with_capacity_and_hasher(strings.len(), Default::default());
    for (i, s) in strings.enumerate() {
        m.entry(s).or_insert(i as u32);
    }
    m
}

/// A stream as it is read.
pub struct R<'a> {
    d: &'a [u8],
    at: usize,
}

impl<'a> R<'a> {
    pub fn new(d: &'a [u8]) -> Self {
        R { d, at: 0 }
    }

    pub fn var(&mut self) -> Result<u64> {
        varint::get(self.d, &mut self.at).map_err(|_| corrupt())
    }

    pub fn var32(&mut self) -> Result<u32> {
        u32::try_from(self.var()?).map_err(|_| corrupt())
    }

    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let b = self.d.get(self.at..).and_then(|d| d.get(..n)).ok_or_else(corrupt)?;
        self.at += n;
        Ok(b)
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }

    /// The bytes up to the next zero byte, past which it goes.
    pub fn until0(&mut self) -> Result<&'a [u8]> {
        let rest = &self.d[self.at..];
        let n = rest.iter().position(|&b| b == 0).ok_or_else(corrupt)?;
        self.at += n + 1;
        Ok(&rest[..n])
    }

    pub fn left(&self) -> usize {
        self.d.len() - self.at
    }

    pub fn done(&self) -> Result<()> {
        if self.at == self.d.len() { Ok(()) } else { Err(corrupt()) }
    }
}

/// `prev` moved by the zigzag value `z`, which must land in `0..=max`.
pub fn moved(prev: u32, z: u64, max: u32) -> Result<u32> {
    let v = prev as i64 + varint::unzz(z);
    if (0..=max as i64).contains(&v) { Ok(v as u32) } else { Err(corrupt()) }
}

pub fn put_uleb(out: &mut Vec<u8>, v: u32) {
    varint::put(out, v as u64)
}

/// The first item of each type.
#[derive(Default)]
struct Types(Vec<Item>);

impl Types {
    /// Adds `it` unless an item of type `t` is there: whether it was added.
    fn insert(&mut self, t: u16, it: Item) -> bool {
        let new = self.get(&t).is_none();
        if new {
            self.0.push(it);
        }
        new
    }

    fn get(&self, t: &u16) -> Option<&Item> {
        self.0.iter().find(|it| it.ty == *t)
    }

    fn contains_key(&self, t: &u16) -> bool {
        self.get(t).is_some()
    }
}

/// The file as it is written: the ranges taken, the rest after them.
struct Out {
    b: Vec<u8>,
    taken: Vec<(usize, usize)>,
}

impl Out {
    fn take(&mut self, at: usize, n: usize) -> Result<&mut [u8]> {
        let end = at.checked_add(n).filter(|&e| e <= self.b.len()).ok_or_else(corrupt)?;
        self.taken.push((at, end));
        Ok(&mut self.b[at..end])
    }

    fn put(&mut self, at: usize, src: &[u8]) -> Result<()> {
        self.take(at, src.len())?.copy_from_slice(src);
        Ok(())
    }

    /// Everything not taken, from `rest`.
    fn fill(&mut self, rest: &[u8]) -> Result<()> {
        self.taken.sort_unstable();
        let mut r = R::new(rest);
        let mut at = 0;
        for &(s, e) in self.taken.iter().chain([&(self.b.len(), self.b.len())]) {
            if s < at {
                return Err(corrupt());
            }
            self.b[at..s].copy_from_slice(r.bytes(s - at)?);
            at = e;
        }
        r.done()
    }
}

/// The file of `len` bytes from the streams of [`STREAM_NAMES`] (FORMAT.md §3).
pub fn decode(inputs: &[&[u8]; s::N], len: usize) -> Result<Vec<u8>> {
    let mut o = Out { b: vec![0; len], taken: Vec::new() };
    let mut head = R::new(inputs[s::HEAD]);
    let header = head.bytes(HEADER)?;
    let flags = head.u8()?;
    if flags > 3 {
        return Err(corrupt());
    }
    o.put(0, header)?;
    let map_off = le32(header, 0x34)? as usize;
    let count = le32(head.bytes(4)?, 0)?;
    let map_bytes = head.bytes((count as usize).checked_mul(12).filter(|&n| n <= head.left()).ok_or_else(corrupt)?)?;
    o.put(map_off, &count.to_le_bytes())?;
    o.put(map_off.checked_add(4).ok_or_else(corrupt)?, map_bytes)?;
    let items = items(map_bytes);
    let modes = head.bytes(items.len())?;
    head.done()?;

    // The sections as they are, and which item of each type is taken.
    let mut raw: [R; s::N] = std::array::from_fn(|i| R::new(inputs[i]));
    let mut taken = Types::default();
    let mut kept = Types::default();
    for (i, (it, &m)) in items.iter().zip(modes).enumerate() {
        match m {
            mode::REST => {}
            mode::RAW => {
                let (start, end) = (it.off as usize, raw_end(&items, i, len));
                let src = raw[raw_stream(it.ty)].bytes(end.checked_sub(start).ok_or_else(corrupt)?)?;
                o.put(start, src)?;
                kept.insert(it.ty, *it);
            }
            mode::TAKEN if takes(it.ty) && taken.insert(it.ty, *it) => {}
            _ => return Err(corrupt()),
        }
    }
    for r in &raw[s::CODE..=s::RAW] {
        r.done()?;
    }
    let lists = match kept.get(&ty::TYPE_LIST) {
        Some(it) => type_lists(&o.b, it.off as usize, it.count)?,
        None => Vec::new(),
    };
    let code = match kept.get(&ty::CODE) {
        Some(it) => code_items(&o.b, it.off as usize, it.count)?,
        None => Vec::new(),
    };
    let table = |t: u16| -> Result<Option<(usize, usize)>> {
        let Some(it) = taken.get(&t) else { return Ok(None) };
        let n = it.count as usize;
        n.checked_mul(entry_size(t)).filter(|&b| b <= len).ok_or_else(corrupt)?;
        Ok(Some((it.off as usize, n)))
    };

    // Strings.
    let mut strings = Strings::default();
    match (table(ty::STRING_ID)?, taken.get(&ty::STRING_DATA)) {
        (Some((off, n)), Some(data)) if data.count as usize == n => {
            strings = decode_strings(&mut raw, n, data.off)?;
            let ids = o.take(off, 4 * n)?;
            for (id, &at) in ids.as_chunks_mut::<4>().0.iter_mut().zip(&strings.items) {
                id.copy_from_slice(&(data.off + at).to_le_bytes());
            }
            o.put(data.off as usize, &strings.data)?;
        }
        (None, None) => {}
        _ => return Err(corrupt()),
    }

    // Type ids.
    if let Some((off, n)) = table(ty::TYPE_ID)? {
        let t = o.take(off, 4 * n)?;
        let mut prev = 0;
        for e in t.as_chunks_mut::<4>().0 {
            prev = moved(prev, raw[s::TYPE].var()?, u32::MAX)?;
            e.copy_from_slice(&prev.to_le_bytes());
        }
    }

    // Prototypes.
    if let Some((off, n)) = table(ty::PROTO_ID)? {
        let types = kept.get(&ty::TYPE_ID).or(taken.get(&ty::TYPE_ID)).ok_or_else(corrupt)?;
        let (types_off, types_n) = (types.off as usize, types.count as usize);
        let shorties = strings.index();
        let mut e = Vec::with_capacity(12 * n);
        let mut prev = 0;
        for _ in 0..n {
            let ret = moved(prev, raw[s::PROTO_RET].var()?, u32::MAX)?;
            prev = ret;
            let params = match raw[s::PROTO_PARAMS].var()? {
                0 => 0,
                k => *lists.get(k as usize - 1).ok_or_else(corrupt)?,
            };
            let shorty = match raw[s::PROTO_SHORTY].var()? {
                0 => {
                    let desc = |t: u32| -> Result<&[u8]> {
                        if t as usize >= types_n {
                            return Err(corrupt());
                        }
                        strings.get(le32(&o.b, types_off + 4 * t as usize)?).ok_or_else(corrupt)
                    };
                    let shorty = shorty(&o.b, ret, params, desc)?;
                    *shorties.get(shorty.as_slice()).ok_or_else(corrupt)?
                }
                k => u32::try_from(k - 1).map_err(|_| corrupt())?,
            };
            for v in [shorty, ret, params] {
                e.extend_from_slice(&v.to_le_bytes());
            }
        }
        o.put(off, &e)?;
    }

    // Fields and methods.
    for (t, class, name, other) in [
        (ty::FIELD_ID, s::FIELD_CLASS, s::FIELD_NAME, s::FIELD_TYPE),
        (ty::METHOD_ID, s::METHOD_CLASS, s::METHOD_NAME, s::METHOD_PROTO),
    ] {
        if let Some((off, n)) = table(t)? {
            let mut e = Vec::with_capacity(8 * n);
            let (mut pc, mut pn) = (0, 0);
            for _ in 0..n {
                let c = moved(pc, raw[class].var()?, u16::MAX as u32)?;
                let z = raw[name].var()?;
                let nm = if c == pc { moved(pn, z, u32::MAX)? } else { u32::try_from(z).map_err(|_| corrupt())? };
                e.extend_from_slice(&(c as u16).to_le_bytes());
                e.extend_from_slice(raw[other].bytes(2)?);
                e.extend_from_slice(&nm.to_le_bytes());
                (pc, pn) = (c, nm);
            }
            o.put(off, &e)?;
        }
    }

    // Classes and their data.
    if let Some((off, n)) = table(ty::CLASS_DEF)? {
        let data = taken.get(&ty::CLASS_DATA).copied();
        let mut code = Code::new(code);
        // (the data of each class with its rank in the section and its class)
        let mut cd: Vec<(u32, usize, Vec<u8>)> = Vec::new();
        let mut e = Vec::with_capacity(32 * n);
        let mut prev = [0u32; 4];
        for i in 0..n {
            prev[0] = moved(prev[0], raw[s::CDEF_CLASS].var()?, u32::MAX)?;
            let flags = raw[s::CDEF_FLAGS].var32()?;
            let sup = raw[s::CDEF_SUPER].var32()?.wrapping_sub(1);
            let ifaces = match raw[s::CDEF_IFACES].var()? {
                0 => 0,
                k => *lists.get(k as usize - 1).ok_or_else(corrupt)?,
            };
            let src = raw[s::CDEF_SRC].var32()?.wrapping_sub(1);
            let mut offs = [0u32; 3];
            for (k, v) in offs.iter_mut().enumerate() {
                let z = raw[s::CDEF_OFFS].var()?;
                *v = match (k, data) {
                    _ if z == 0 => 0,
                    (1, Some(_)) => {
                        let rank = moved(cd.len() as u32, z - 1, u32::MAX)?;
                        let mut b = Vec::new();
                        class_data(&mut raw, &mut b, &mut code)?;
                        cd.push((rank, i, b));
                        // (set below, once the data is laid out)
                        0
                    }
                    _ => {
                        prev[1 + k] = moved(prev[1 + k], z - 1, u32::MAX)?;
                        prev[1 + k]
                    }
                };
            }
            for v in [prev[0], flags, sup, ifaces, src, offs[0], offs[1], offs[2]] {
                e.extend_from_slice(&v.to_le_bytes());
            }
        }
        if let Some(d) = data {
            if cd.len() != d.count as usize {
                return Err(corrupt());
            }
            cd.sort_unstable_by_key(|c| c.0);
            let mut all = Vec::new();
            for (r, (rank, i, b)) in cd.into_iter().enumerate() {
                if rank as usize != r {
                    return Err(corrupt());
                }
                let at = d.off.checked_add(u32::try_from(all.len()).map_err(|_| corrupt())?).ok_or_else(corrupt)?;
                e[32 * i + 24..32 * i + 28].copy_from_slice(&at.to_le_bytes());
                all.extend_from_slice(&b);
            }
            o.put(d.off as usize, &all)?;
        }
        o.put(off, &e)?;
    } else if taken.contains_key(&ty::CLASS_DATA) {
        return Err(corrupt());
    }
    for r in &raw[s::STR_PREFIX..] {
        r.done()?;
    }

    o.fill(inputs[s::REST])?;
    if flags & 2 != 0 {
        let sig = Sha1::digest(o.b.get(32..).ok_or_else(corrupt)?);
        o.b[12..32].copy_from_slice(&sig);
    }
    if flags & 1 != 0 {
        let sum = aqr_common::adler32::hash(o.b.get(12..).ok_or_else(corrupt)?);
        o.b[8..12].copy_from_slice(&sum.to_le_bytes());
    }
    Ok(o.b)
}

/// The string data as it is rebuilt.
#[derive(Default)]
struct Strings {
    /// The items, one after the other.
    data: Vec<u8>,
    /// Where each item starts in `data`.
    items: Vec<u32>,
    /// Where the bytes of each string are in `data` and how many.
    spans: Vec<(u32, u32)>,
}

impl Strings {
    fn get(&self, i: u32) -> Option<&[u8]> {
        let &(at, n) = self.spans.get(i as usize)?;
        Some(&self.data[at as usize..(at + n) as usize])
    }

    fn index(&self) -> HashMap<&[u8], u32, BuildHasherDefault<Fnv>> {
        string_index((0..self.spans.len() as u32).map(|i| self.get(i).unwrap()))
    }
}

fn decode_strings(raw: &mut [R; s::N], n: usize, base: u32) -> Result<Strings> {
    let mut st = Strings::default();
    let mut prev = (0, 0);
    let mut s = Vec::new();
    for _ in 0..n {
        let k = raw[s::STR_PREFIX].var()?;
        let suffix = raw[s::STR_SUFFIX].until0()?;
        if k > prev.1 as u64 {
            return Err(corrupt());
        }
        s.clear();
        s.extend_from_slice(&st.data[prev.0 as usize..][..k as usize]);
        s.extend_from_slice(suffix);
        let item = u32::try_from(st.data.len()).ok().filter(|&i| base.checked_add(i).is_some()).ok_or_else(corrupt)?;
        put_uleb(&mut st.data, utf16_len(&s));
        let at = u32::try_from(st.data.len()).map_err(|_| corrupt())?;
        st.items.push(item);
        st.spans.push((at, s.len() as u32));
        st.data.extend_from_slice(&s);
        st.data.push(0);
        prev = (at, s.len() as u32);
    }
    Ok(st)
}

/// The shorty of the prototype of return type `ret` and parameters at `params` in `d` (0 for
/// none), the descriptors of types given by `desc`.
pub fn shorty<'a>(d: &[u8], ret: u32, params: u32, desc: impl Fn(u32) -> Result<&'a [u8]>) -> Result<Vec<u8>> {
    let mut v = vec![shorty_char(desc(ret)?).ok_or_else(corrupt)?];
    if params != 0 {
        let p = params as usize;
        let n = le32(d, p)? as usize;
        for i in 0..n {
            v.push(shorty_char(desc(le16(d, p + 4 + 2 * i)? as u32)?).ok_or_else(corrupt)?);
        }
    }
    Ok(v)
}

/// The code items as the classes' data refers to them (FORMAT.md §4.7): which are used, and the
/// first not used.
pub struct Code {
    pub offs: Vec<u32>,
    used: Vec<bool>,
    cursor: usize,
}

impl Code {
    pub fn new(offs: Vec<u32>) -> Self {
        let used = vec![false; offs.len()];
        Code { offs, used, cursor: 0 }
    }

    /// The window of a class of `methods` methods: the items not used from the first not used,
    /// as many as its methods at most.
    pub fn window(&self, methods: usize) -> Vec<usize> {
        let end = self.offs.len().min(self.cursor.saturating_add(methods));
        (self.cursor..end).filter(|&j| !self.used[j]).collect()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn use_(&mut self, j: usize) {
        self.used[j] = true;
    }

    /// After a class: the cursor past the items used.
    pub fn advance(&mut self) {
        while self.cursor < self.used.len() && self.used[self.cursor] {
            self.cursor += 1;
        }
    }
}

/// A class's data from the streams, appended to `cd`; the offsets of its code by `code`.
fn class_data(raw: &mut [R; s::N], cd: &mut Vec<u8>, code: &mut Code) -> Result<()> {
    let mut sizes = [0u32; 4];
    for v in &mut sizes {
        *v = raw[s::CDATA_SIZES].var32()?;
        put_uleb(cd, *v);
    }
    for _ in 0..sizes[0] as u64 + sizes[1] as u64 {
        put_uleb(cd, raw[s::CDATA_FDIFF].var32()?);
        put_uleb(cd, raw[s::CDATA_FFLAGS].var32()?);
    }
    let methods = sizes[2] as usize + sizes[3] as usize;
    let mut window = code.window(methods);
    for _ in 0..methods {
        put_uleb(cd, raw[s::CDATA_MDIFF].var32()?);
        put_uleb(cd, raw[s::CDATA_MFLAGS].var32()?);
        let v = raw[s::CDATA_CODE].var()? as usize;
        let off = if v == 0 {
            0
        } else {
            let j = if v <= window.len() {
                window.remove(v - 1)
            } else {
                let j = code.cursor() as i64 + varint::unzz((v - window.len() - 1) as u64);
                usize::try_from(j).ok().filter(|&j| j < code.offs.len()).ok_or_else(corrupt)?
            };
            code.use_(j);
            code.offs[j]
        };
        put_uleb(cd, off);
    }
    code.advance();
    Ok(())
}
