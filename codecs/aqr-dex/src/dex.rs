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
    pub const CODE_HEAD: usize = 33;
    pub const CODE_LEN: usize = 34;
    pub const CODE_TRIES: usize = 35;
    pub const CODE_DEBUG: usize = 36;
    pub const CODE_TRY: usize = 37;
    pub const INS_OP: usize = 38;
    pub const INS_ZERO: usize = 39;
    pub const INS_REG4: usize = 40;
    pub const INS_REG8: usize = 41;
    pub const INS_REG16: usize = 42;
    pub const INS_ARGN: usize = 43;
    pub const INS_ARGS: usize = 44;
    pub const INS_LIT: usize = 45;
    pub const INS_BRANCH: usize = 46;
    pub const INS_PAYLOAD: usize = 47;
    pub const IDX_HIT: usize = 48;
    pub const IDX_STRING: usize = 49;
    pub const IDX_TYPE: usize = 50;
    pub const IDX_FIELD: usize = 51;
    pub const IDX_METHOD: usize = 52;
    pub const IDX_OTHER: usize = 53;
    pub const HIT_STRING: usize = 54;
    pub const HIT_TYPE: usize = 55;
    pub const HIT_FIELD: usize = 56;
    pub const HIT_METHOD: usize = 57;
    pub const N: usize = 58;
}

/// The names of the streams, in order.
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
    "code-head",
    "code-len",
    "code-tries",
    "code-debug",
    "code-try",
    "ins-op",
    "ins-zero",
    "ins-reg4",
    "ins-reg8",
    "ins-reg16",
    "ins-argn",
    "ins-args",
    "ins-lit",
    "ins-branch",
    "ins-payload",
    "idx-hit",
    "idx-string",
    "idx-type",
    "idx-field",
    "idx-method",
    "idx-other",
    "hit-string",
    "hit-type",
    "hit-field",
    "hit-method",
];

/// The module's inputs: each holds the streams of [`s`] listed, one after another, after the
/// lengths of all but the last (FORMAT.md §2). Streams of a kind go together, which costs
/// little: on the 21 files of `README.md`, 31.32 MB under zstd -9 rather than 31.05 MB with
/// each stream an input of its own.
pub const INPUTS: [&[usize]; 14] = [
    &[s::REST, s::HEAD, s::CODE, s::TYPE_LIST, s::RAW],
    &[s::DEBUG],
    &[s::ANNOTATION, s::ANNOTATION_SETS, s::ARRAY],
    &[s::STR_SUFFIX],
    &[
        s::STR_PREFIX,
        s::TYPE,
        s::PROTO_RET,
        s::PROTO_PARAMS,
        s::PROTO_SHORTY,
        s::FIELD_CLASS,
        s::FIELD_NAME,
        s::FIELD_TYPE,
        s::METHOD_CLASS,
        s::METHOD_NAME,
        s::METHOD_PROTO,
    ],
    &[
        s::CDEF_CLASS,
        s::CDEF_FLAGS,
        s::CDEF_SUPER,
        s::CDEF_IFACES,
        s::CDEF_SRC,
        s::CDEF_OFFS,
        s::CDATA_SIZES,
        s::CDATA_FDIFF,
        s::CDATA_FFLAGS,
        s::CDATA_MDIFF,
        s::CDATA_MFLAGS,
        s::CDATA_CODE,
    ],
    &[s::CODE_HEAD, s::CODE_LEN, s::CODE_TRIES, s::CODE_DEBUG, s::CODE_TRY, s::INS_ZERO, s::INS_ARGN],
    &[s::INS_OP],
    &[s::INS_REG4],
    &[s::INS_REG8, s::INS_REG16],
    &[s::INS_ARGS],
    &[s::INS_LIT, s::INS_BRANCH, s::INS_PAYLOAD],
    &[s::IDX_HIT, s::HIT_STRING, s::HIT_TYPE, s::HIT_FIELD, s::HIT_METHOD],
    &[s::IDX_STRING, s::IDX_TYPE, s::IDX_FIELD, s::IDX_METHOD, s::IDX_OTHER],
];

/// The names of the inputs (their classes are `dex/NAME`).
pub const INPUT_NAMES: [&str; INPUTS.len()] = [
    "sections",
    "debug",
    "annotation",
    "strings",
    "ids",
    "classes",
    "code",
    "op",
    "reg4",
    "reg8",
    "args",
    "lit",
    "hits",
    "misses",
];

/// The module's inputs from the streams `st`.
pub fn pack(st: &St) -> Vec<Vec<u8>> {
    INPUTS
        .iter()
        .map(|group| {
            let mut v = Vec::new();
            for &l in &group[..group.len() - 1] {
                varint::put(&mut v, st[l].len() as u64);
            }
            for &l in *group {
                v.extend_from_slice(&st[l]);
            }
            v
        })
        .collect()
}

/// The streams from the module's inputs.
pub fn unpack<'a>(inputs: &[&'a [u8]]) -> Result<Vec<&'a [u8]>> {
    if inputs.len() != INPUTS.len() {
        return Err(corrupt());
    }
    let mut st: Vec<&[u8]> = vec![&[]; s::N];
    for (group, &d) in INPUTS.iter().zip(inputs) {
        let mut r = R::new(d);
        let lens: Vec<u64> = (1..group.len()).map(|_| r.var()).collect::<Result<_>>()?;
        for (&l, &n) in group.iter().zip(&lens) {
            st[l] = r.bytes(usize::try_from(n).map_err(|_| corrupt())?)?;
        }
        st[group[group.len() - 1]] = r.rest();
    }
    Ok(st)
}

/// The streams of [`s`] as they are read, from the module's inputs.
pub struct Raw<'a>(pub Vec<R<'a>>);

impl<'a> std::ops::Index<usize> for Raw<'a> {
    type Output = R<'a>;
    fn index(&self, l: usize) -> &R<'a> {
        &self.0[l]
    }
}

impl std::ops::IndexMut<usize> for Raw<'_> {
    fn index_mut(&mut self, l: usize) -> &mut Self::Output {
        &mut self.0[l]
    }
}

/// The streams of [`s`] as they are written, into the module's inputs.
pub struct St(pub Vec<Vec<u8>>);

impl Default for St {
    fn default() -> Self {
        St(vec![Vec::new(); s::N])
    }
}

impl St {
    /// `other` after what each input has.
    pub fn append(&mut self, other: St) {
        for (a, b) in self.0.iter_mut().zip(other.0) {
            a.extend_from_slice(&b);
        }
    }
}

impl std::ops::Index<usize> for St {
    type Output = Vec<u8>;
    fn index(&self, l: usize) -> &Vec<u8> {
        &self.0[l]
    }
}

impl std::ops::IndexMut<usize> for St {
    fn index_mut(&mut self, l: usize) -> &mut Vec<u8> {
        &mut self.0[l]
    }
}

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
            | ty::CODE
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
    map.as_chunks::<12>()
        .0
        .iter()
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
        let q = end(p, 16 + 2 * insns, d.len())?;
        p = if tries > 0 { q + handlers(&d[q..], insns, tries)? } else { q };
    }
    Ok(v)
}

/// Each method's words of parameters (two for `J` and `D`, one for any other type) by the
/// method ids `methods`, the prototypes `protos` and the type ids `types` in `b`, the type
/// descriptors by `desc` (one word where it gives none); 0 where a method's prototype or its
/// parameters are not there. At most 255.
pub fn param_words<'a>(
    b: &[u8],
    methods: Option<Item>,
    protos: Option<Item>,
    types: Option<Item>,
    desc: impl Fn(u32) -> Option<&'a [u8]>,
) -> Vec<u8> {
    let (Some(m), Some(p), Some(t)) = (methods, protos, types) else { return Vec::new() };
    let word = |ty: u16| -> Option<u64> {
        if ty as u32 >= t.count {
            return None;
        }
        let d = desc(le32(b, t.off as usize + 4 * ty as usize).ok()?);
        Some(if matches!(d.and_then(|d| d.first()), Some(b'J' | b'D')) { 2 } else { 1 })
    };
    let words = |i: usize| -> Option<u8> {
        let proto = le16(b, m.off as usize + 8 * i + 2).ok()? as u32;
        if proto >= p.count {
            return None;
        }
        let params = le32(b, p.off as usize + 12 * proto as usize + 8).ok()? as usize;
        if params == 0 {
            return Some(0);
        }
        let n = le32(b, params).ok()? as usize;
        let mut w = 0;
        for k in 0..n {
            w += word(le16(b, params + 4 + 2 * k).ok()?)?;
            if w > 255 {
                return Some(255);
            }
        }
        Some(w as u8)
    };
    let n = param_words_len(b.len(), m.count);
    let mut out = Vec::with_capacity(n);
    out.extend((0..n).map(|i| words(i).unwrap_or(0)));
    out
}

/// The words of parameters [`param_words`] gives, of a file of `len` bytes and `methods`
/// method ids.
fn param_words_len(len: usize, methods: u32) -> usize {
    (methods as usize).min(len / 8)
}

/// The offsets of the `count` debug info items from `off` in `d`.
pub fn debug_items(d: &[u8], off: usize, count: u32) -> Result<Vec<u32>> {
    let mut p = off;
    let mut v = Vec::with_capacity((count as usize).min(d.len()));
    for _ in 0..count {
        v.push(u32::try_from(p).map_err(|_| corrupt())?);
        uleb(d, &mut p)?;
        for _ in 0..uleb(d, &mut p)? {
            uleb(d, &mut p)?;
        }
        loop {
            let op = *d.get(p).ok_or_else(corrupt)?;
            p += 1;
            let args = match op {
                0 => break,
                1 | 5 | 6 | 9 => 1,
                2 => {
                    sleb(d, &mut p)?;
                    0
                }
                3 => 3,
                4 => 4,
                _ => 0,
            };
            for _ in 0..args {
                uleb(d, &mut p)?;
            }
        }
    }
    Ok(v)
}

/// How code items refer to debug info items (FORMAT.md §4.7): the next not used yet, or one
/// used before, by its place among the most recent.
pub struct Debug {
    pub offs: Vec<u32>,
    next: usize,
    /// The most recent, as many as `len` tells, the most recent first.
    recent: [usize; DEBUG_RECENT],
    len: usize,
}

/// The debug info items [`Debug`] keeps as the most recent.
pub const DEBUG_RECENT: usize = 128;

impl Debug {
    pub fn new(offs: Vec<u32>) -> Self {
        Debug { offs, next: 0, recent: [0; DEBUG_RECENT], len: 0 }
    }

    fn recent(&self) -> &[usize] {
        &self.recent[..self.len]
    }

    fn used(&mut self, j: usize, at: Option<usize>) {
        let end = match at {
            Some(i) => i,
            None => {
                self.len = (self.len + 1).min(DEBUG_RECENT);
                self.len - 1
            }
        };
        self.recent.copy_within(..end, 1);
        self.recent[0] = j;
        if j == self.next {
            self.next += 1;
        }
    }

    /// The value of `debug_info_off` `off`, `None` where it is no item's.
    pub fn encode(&mut self, off: u32) -> Option<u64> {
        if off == 0 {
            return Some(0);
        }
        let j = self.offs.binary_search(&off).ok()?;
        let at = self.recent().iter().position(|&r| r == j);
        let v = match at {
            _ if j == self.next => 1,
            Some(i) => 2 + i as u64,
            None => 2 + self.len as u64 + j as u64,
        };
        self.used(j, at);
        Some(v)
    }

    pub fn decode(&mut self, v: u64) -> Result<u32> {
        let n = self.len as u64;
        let (j, at) = match v {
            0 => return Ok(0),
            1 => (self.next, self.recent().iter().position(|&r| r == self.next)),
            _ if v - 2 < n => (self.recent[(v - 2) as usize], Some((v - 2) as usize)),
            _ => {
                let j = usize::try_from(v - 2 - n).map_err(|_| corrupt())?;
                (j, None)
            }
        };
        let off = *self.offs.get(j).ok_or_else(corrupt)?;
        self.used(j, at);
        Ok(off)
    }
}

/// The bytes of the tries and the handlers of a code item of `insns` units and `tries` tries,
/// at the start of `d` (from the end of its instructions).
pub fn handlers(d: &[u8], insns: u64, tries: u64) -> Result<usize> {
    let mut q = end(0, (insns & 1) * 2 + 8 * tries, d.len())?;
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
    Ok(q)
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

    /// What is left of the stream, not read.
    pub fn rest(&self) -> &'a [u8] {
        &self.d[self.at..]
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
    /// `n` bytes at `at` taken: where they are.
    fn mark(&mut self, at: usize, n: usize) -> Result<std::ops::Range<usize>> {
        let end = at.checked_add(n).filter(|&e| e <= self.b.len()).ok_or_else(corrupt)?;
        self.taken.push((at, end));
        Ok(at..end)
    }

    fn take(&mut self, at: usize, n: usize) -> Result<&mut [u8]> {
        let r = self.mark(at, n)?;
        Ok(&mut self.b[r])
    }

    fn put(&mut self, at: usize, src: &[u8]) -> Result<()> {
        self.take(at, src.len())?.copy_from_slice(src);
        Ok(())
    }

    /// Everything not taken, from `r`.
    fn fill(&mut self, r: &mut R) -> Result<()> {
        self.taken.sort_unstable();
        let mut at = 0;
        for &(s, e) in self.taken.iter().chain([&(self.b.len(), self.b.len())]) {
            if s < at {
                return Err(corrupt());
            }
            self.b[at..s].copy_from_slice(r.bytes(s - at)?);
            at = e;
        }
        Ok(())
    }
}

/// Writes bytes into the file from a place on, within it.
pub struct W<'a> {
    b: &'a mut [u8],
    pub at: usize,
}

impl W<'_> {
    pub fn put(&mut self, src: &[u8]) -> Result<()> {
        let end = self.at.checked_add(src.len()).filter(|&e| e <= self.b.len()).ok_or_else(corrupt)?;
        self.b[self.at..end].copy_from_slice(src);
        self.at = end;
        Ok(())
    }

    fn uleb(&mut self, v: u32) -> Result<()> {
        let (mut b, mut n, mut v) = ([0u8; 5], 0, v);
        loop {
            let c = (v & 0x7f) as u8;
            v >>= 7;
            b[n] = c | if v != 0 { 0x80 } else { 0 };
            n += 1;
            if v == 0 {
                break;
            }
        }
        self.put(&b[..n])
    }
}

impl<'a> Raw<'a> {
    /// The streams from the module's inputs, as [`unpack`] makes them.
    fn of(inputs: &[&'a [u8]]) -> Result<Raw<'a>> {
        if inputs.len() != INPUTS.len() {
            return Err(corrupt());
        }
        let mut st = Vec::with_capacity(s::N);
        st.resize_with(s::N, || R::new(&[]));
        for (group, &d) in INPUTS.iter().zip(inputs) {
            let mut r = R::new(d);
            let mut lens = [0usize; MOST_GROUP];
            for n in &mut lens[..group.len() - 1] {
                *n = usize::try_from(r.var()?).map_err(|_| corrupt())?;
            }
            for (&l, &n) in group.iter().zip(&lens) {
                st[l] = R::new(r.bytes(n)?);
            }
            st[group[group.len() - 1]] = R::new(r.rest());
        }
        Ok(Raw(st))
    }
}

/// The streams of an input at most.
const MOST_GROUP: usize = {
    let (mut most, mut k) = (0, 0);
    while k < INPUTS.len() {
        if INPUTS[k].len() > most {
            most = INPUTS[k].len();
        }
        k += 1;
    }
    most
};

/// What the head stream tells first: the header, the flags, the map's entries and how each
/// item's section is rebuilt.
struct Head<'a> {
    header: &'a [u8],
    flags: u8,
    count: u32,
    map: &'a [u8],
    modes: &'a [u8],
}

impl<'a> Head<'a> {
    fn read(head: &mut R<'a>) -> Result<Head<'a>> {
        let header = head.bytes(HEADER)?;
        let flags = head.u8()?;
        if flags > 3 {
            return Err(corrupt());
        }
        let count = le32(head.bytes(4)?, 0)?;
        let map = head.bytes((count as usize).checked_mul(12).filter(|&n| n <= head.left()).ok_or_else(corrupt)?)?;
        let modes = head.bytes(count as usize)?;
        Ok(Head { header, flags, count, map, modes })
    }
}

/// The file of `len` bytes from the module's inputs ([`INPUTS`], FORMAT.md §3).
pub fn decode(inputs: &[&[u8]], len: usize) -> Result<Vec<u8>> {
    let mut o = Out { b: vec![0; len], taken: Vec::new() };
    let mut raw = Raw::of(inputs)?;
    let h = Head::read(&mut raw[s::HEAD])?;
    let items = items(h.map);
    // (the header, the map and each section put, at most)
    o.taken.reserve_exact(TAKES + items.len());
    o.put(0, h.header)?;
    let map_off = le32(h.header, 0x34)? as usize;
    o.put(map_off, &h.count.to_le_bytes())?;
    o.put(map_off.checked_add(4).ok_or_else(corrupt)?, h.map)?;

    // The sections as they are, and which item of each type is taken.
    let mut taken = Types(Vec::with_capacity(items.len()));
    let mut kept = Types(Vec::with_capacity(items.len()));
    for (i, (it, &m)) in items.iter().zip(h.modes).enumerate() {
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
    let lists = match kept.get(&ty::TYPE_LIST) {
        Some(it) => type_lists(&o.b, it.off as usize, it.count)?,
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
            let ids = o.mark(off, 4 * n)?;
            let mut w = W { b: &mut o.b, at: data.off as usize };
            strings = decode_strings(&mut raw, &mut w, n, ids.start)?;
            let end = w.at;
            o.mark(data.off as usize, end - data.off as usize)?;
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
        let shorties = StringIndex::new(&strings, &o.b);
        let at = o.mark(off, 12 * n)?.start;
        let mut prev = 0;
        for k in 0..n {
            let ret = moved(prev, raw[s::PROTO_RET].var()?, u32::MAX)?;
            prev = ret;
            let params = match raw[s::PROTO_PARAMS].var()? {
                0 => 0,
                k => *lists.get(k as usize - 1).ok_or_else(corrupt)?,
            };
            let shorty = match raw[s::PROTO_SHORTY].var()? {
                0 => {
                    let b = &o.b;
                    let desc = |t: u32| -> Result<&[u8]> {
                        if t as usize >= types_n {
                            return Err(corrupt());
                        }
                        strings.get(b, le32(b, types_off + 4 * t as usize)?).ok_or_else(corrupt)
                    };
                    shorties.shorty(&strings, b, ret, params, desc)?
                }
                k => u32::try_from(k - 1).map_err(|_| corrupt())?,
            };
            for (j, v) in [shorty, ret, params].into_iter().enumerate() {
                o.b[at + 12 * k + 4 * j..][..4].copy_from_slice(&v.to_le_bytes());
            }
        }
    }

    // Fields and methods.
    for (t, class, name, other) in [
        (ty::FIELD_ID, s::FIELD_CLASS, s::FIELD_NAME, s::FIELD_TYPE),
        (ty::METHOD_ID, s::METHOD_CLASS, s::METHOD_NAME, s::METHOD_PROTO),
    ] {
        if let Some((off, n)) = table(t)? {
            let e = o.take(off, 8 * n)?;
            let (mut pc, mut pn) = (0, 0);
            for e in e.as_chunks_mut::<8>().0 {
                let c = moved(pc, raw[class].var()?, u16::MAX as u32)?;
                let z = raw[name].var()?;
                let nm = if c == pc { moved(pn, z, u32::MAX)? } else { u32::try_from(z).map_err(|_| corrupt())? };
                e[..2].copy_from_slice(&(c as u16).to_le_bytes());
                e[2..4].copy_from_slice(raw[other].bytes(2)?);
                e[4..].copy_from_slice(&nm.to_le_bytes());
                (pc, pn) = (c, nm);
            }
        }
    }

    // Code items.
    let code = match (kept.get(&ty::CODE), taken.get(&ty::CODE)) {
        (Some(it), _) => code_items(&o.b, it.off as usize, it.count)?,
        (None, Some(it)) => {
            let debug = match kept.get(&ty::DEBUG_INFO) {
                Some(it) => debug_items(&o.b, it.off as usize, it.count)?,
                None => Vec::new(),
            };
            let item = |t: u16| kept.get(&t).or(taken.get(&t)).copied();
            let words =
                param_words(&o.b, item(ty::METHOD_ID), item(ty::PROTO_ID), item(ty::TYPE_ID), |i| strings.get(&o.b, i));
            let mut w = W { b: &mut o.b, at: it.off as usize };
            let offs = decode_code(&mut raw, &mut w, it.count, Debug::new(debug), &words)?;
            let end = w.at;
            o.mark(it.off as usize, end - it.off as usize)?;
            offs
        }
        (None, None) => Vec::new(),
    };

    // Classes and their data.
    if let Some((off, n)) = table(ty::CLASS_DEF)? {
        let data = taken.get(&ty::CLASS_DATA).copied();
        let (code_n, mut code) = (code.len(), Code::new(code));
        let mut window = Vec::with_capacity(code_n);
        // (the data of the classes one after another, of room for its section, and of each its
        // rank in the section, its class and where it is)
        let (mut cd, mut ranks) = match data {
            Some(d) => (Vec::with_capacity(class_data_room(&items, d, len)), Vec::with_capacity(d.count as usize)),
            None => (Vec::new(), Vec::new()),
        };
        let e = o.mark(off, 32 * n)?.start;
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
                        let rank = moved(ranks.len() as u32, z - 1, u32::MAX)?;
                        let start = cd.len();
                        class_data(&mut raw, &mut cd, &mut code, &mut window)?;
                        ranks.push((rank, i as u32, start as u32, cd.len() as u32));
                        // (set below, once the data is laid out)
                        0
                    }
                    _ => {
                        prev[1 + k] = moved(prev[1 + k], z - 1, u32::MAX)?;
                        prev[1 + k]
                    }
                };
            }
            for (j, v) in [prev[0], flags, sup, ifaces, src, offs[0], offs[1], offs[2]].into_iter().enumerate() {
                o.b[e + 32 * i + 4 * j..][..4].copy_from_slice(&v.to_le_bytes());
            }
        }
        if let Some(d) = data {
            if ranks.len() != d.count as usize {
                return Err(corrupt());
            }
            ranks.sort_unstable_by_key(|c| c.0);
            let all = o.mark(d.off as usize, cd.len())?;
            let mut at = all.start;
            for (r, &(rank, i, start, end)) in ranks.iter().enumerate() {
                if rank as usize != r {
                    return Err(corrupt());
                }
                let i = i as usize;
                o.b[e + 32 * i + 24..e + 32 * i + 28].copy_from_slice(&(at as u32).to_le_bytes());
                let b = &cd[start as usize..end as usize];
                o.b[at..at + b.len()].copy_from_slice(b);
                at += b.len();
            }
        }
    } else if taken.contains_key(&ty::CLASS_DATA) {
        return Err(corrupt());
    }
    o.fill(&mut raw[s::REST])?;
    for r in &raw.0 {
        r.done()?;
    }
    if h.flags & 2 != 0 {
        let sig = Sha1::digest(o.b.get(32..).ok_or_else(corrupt)?);
        o.b[12..32].copy_from_slice(&sig);
    }
    if h.flags & 1 != 0 {
        let sum = aqr_common::adler32::hash(o.b.get(12..).ok_or_else(corrupt)?);
        o.b[8..12].copy_from_slice(&sum.to_le_bytes());
    }
    Ok(o.b)
}

/// The ranges [`decode`] takes at most besides the sections kept: the header, the map's count
/// and entries, and the sections it takes.
const TAKES: usize = 3 + 10;

/// The bytes the class data of item `d` may take: to the next section, else the end.
fn class_data_room(items: &[Item], d: Item, len: usize) -> usize {
    let next = items.iter().filter(|e| e.off > d.off).map(|e| e.off as usize).min().unwrap_or(len);
    next.saturating_sub(d.off as usize)
}

/// Bytes that [`decode`] takes of [`aqr_api::stack`] at most, of the first input `sections`
/// and a file of `len` bytes (`None` where its head does not parse), in its blocks' order: the
/// file, the streams, the ranges taken, the map's items, the items taken and kept, the type
/// lists, the strings; then the larger of the prototypes' index of the strings and of the code
/// (the debug info items, the parameters' words, the code items) with the classes (the code
/// items used and a class's window of them, the classes' data and their ranks).
pub fn decode_plan(sections: &[u8], len: u64) -> Option<u64> {
    use aqr_api::stack::block;
    let mut r = R::new(sections);
    let mut lens = [0usize; MOST_GROUP];
    for n in &mut lens[..INPUTS[0].len() - 1] {
        *n = usize::try_from(r.var().ok()?).ok()?;
    }
    let head = INPUTS[0].iter().position(|&l| l == s::HEAD)?;
    for &n in &lens[..head] {
        r.bytes(n).ok()?;
    }
    let mut hr = R::new(r.bytes(lens[head]).ok()?);
    let h = Head::read(&mut hr).ok()?;
    let items = items(h.map);
    let n = items.len() as u64;
    let (mut taken, mut kept) = (Types(Vec::new()), Types(Vec::new()));
    for (it, &m) in items.iter().zip(h.modes) {
        match m {
            mode::RAW => _ = kept.insert(it.ty, *it),
            mode::TAKEN => _ = taken.insert(it.ty, *it),
            _ => {}
        }
    }
    let size = usize::try_from(len).ok()?;
    let count = |t: &Types, ty: u16| t.get(&ty).map_or(0, |it| it.count as u64);
    let lists = kept.get(&ty::TYPE_LIST).map_or(0, |it| block(4 * (it.count as u64).min(len / 4)));
    let strings = count(&taken, ty::STRING_ID);
    let held = block(len)
        + block(R_BYTES * s::N as u64)
        + block(8 * (TAKES as u64 + n))
        + 3 * block(12 * n)
        + lists
        + if taken.get(&ty::STRING_DATA).is_some() { block(8 * strings) } else { 0 };
    let protos = if taken.get(&ty::PROTO_ID).is_some() { block(4 * StringIndex::slots(strings) as u64) } else { 0 };
    let code = match (kept.get(&ty::CODE), taken.get(&ty::CODE)) {
        (Some(it), _) => block(4 * (it.count as u64).min(len / 16)),
        (None, Some(it)) => {
            let debug = kept.get(&ty::DEBUG_INFO).map_or(0, |d| block(4 * (d.count as u64).min(len)));
            let item = |t: u16| kept.get(&t).or(taken.get(&t)).copied();
            let words = match (item(ty::METHOD_ID), item(ty::PROTO_ID), item(ty::TYPE_ID)) {
                (Some(m), Some(_), Some(_)) => block(param_words_len(size, m.count) as u64),
                _ => 0,
            };
            debug + words + block(4 * it.count as u64)
        }
        (None, None) => 0,
    };
    let code_n = match (kept.get(&ty::CODE), taken.get(&ty::CODE)) {
        (Some(it), _) => (it.count as u64).min(len / 16),
        (None, Some(it)) => it.count as u64,
        (None, None) => 0,
    };
    let classes = match taken.get(&ty::CLASS_DEF) {
        Some(_) => {
            let data = taken
                .get(&ty::CLASS_DATA)
                .map_or(0, |&d| block(class_data_room(&items, d, size) as u64) + block(RANK_BYTES * d.count as u64));
            block(code_n) + block(4 * code_n) + data
        }
        None => 0,
    };
    Some(held + protos.max(code + classes))
}

/// Bytes of an [`R`] on WebAssembly.
const R_BYTES: u64 = 12;
#[cfg(target_family = "wasm")]
const _: () = assert!(size_of::<R>() as u64 == R_BYTES);
/// Bytes of a class's rank, class and where its data is.
const RANK_BYTES: u64 = 16;

/// The `count` code items from `w`'s place on (FORMAT.md §4.7), and their offsets.
fn decode_code(raw: &mut Raw, w: &mut W, count: u32, mut debug: Debug, words: &[u8]) -> Result<Vec<u32>> {
    let mut offs = Vec::with_capacity(count as usize);
    let mut caches = crate::code::Caches::default();
    for _ in 0..count {
        while !w.at.is_multiple_of(4) {
            w.put(&[0])?;
        }
        offs.push(u32::try_from(w.at).map_err(|_| corrupt())?);
        let start = w.at;
        for _ in 0..2 {
            let v = u16::try_from(raw[s::CODE_HEAD].var()?).map_err(|_| corrupt())?;
            w.put(&v.to_le_bytes())?;
        }
        // (the outs, set below)
        w.put(&[0, 0])?;
        let tries = u16::try_from(raw[s::CODE_TRIES].var()?).map_err(|_| corrupt())?;
        w.put(&tries.to_le_bytes())?;
        let debug = debug.decode(raw[s::CODE_DEBUG].var()?)?;
        w.put(&debug.to_le_bytes())?;
        let n = raw[s::CODE_LEN].var32()?;
        w.put(&n.to_le_bytes())?;
        let outs = crate::code::decode(n as usize, &mut caches, words, raw, w)?;
        let outs = moved(outs.min(0xffff) as u32, raw[s::CODE_HEAD].var()?, 0xffff)?;
        w.b[start + 4..start + 6].copy_from_slice(&(outs as u16).to_le_bytes());
        if tries > 0 {
            let k = handlers(raw[s::CODE_TRY].rest(), n as u64, tries as u64)?;
            w.put(raw[s::CODE_TRY].bytes(k)?)?;
        }
    }
    Ok(offs)
}

/// The strings as they are rebuilt in the file: where the bytes of each are and how many.
#[derive(Default)]
struct Strings {
    spans: Vec<(u32, u32)>,
}

impl Strings {
    fn get<'a>(&self, b: &'a [u8], i: u32) -> Option<&'a [u8]> {
        let &(at, n) = self.spans.get(i as usize)?;
        b.get(at as usize..(at + n) as usize)
    }
}

/// The `n` strings' items written by `w`, their ids (the items' offsets) at `ids` in the file.
fn decode_strings(raw: &mut Raw, w: &mut W, n: usize, ids: usize) -> Result<Strings> {
    let mut st = Strings { spans: Vec::with_capacity(n) };
    let mut prev = (0usize, 0usize);
    for i in 0..n {
        let k = usize::try_from(raw[s::STR_PREFIX].var()?).map_err(|_| corrupt())?;
        let suffix = raw[s::STR_SUFFIX].until0()?;
        if k > prev.1 {
            return Err(corrupt());
        }
        let item = u32::try_from(w.at).map_err(|_| corrupt())?;
        w.b.get_mut(ids + 4 * i..ids + 4 * i + 4).ok_or_else(corrupt)?.copy_from_slice(&item.to_le_bytes());
        w.uleb(utf16_len(&w.b[prev.0..prev.0 + k]) + utf16_len(suffix))?;
        let at = w.at;
        let len = k + suffix.len();
        let end = at.checked_add(len + 1).filter(|&e| e <= w.b.len()).ok_or_else(corrupt)?;
        w.b.copy_within(prev.0..prev.0 + k, at);
        w.b[at + k..at + len].copy_from_slice(suffix);
        w.b[at + len] = 0;
        w.at = end;
        st.spans.push((u32::try_from(at).map_err(|_| corrupt())?, len as u32));
        prev = (at, len);
    }
    Ok(st)
}

/// The index of each string (the first of equal ones) by its bytes: a table of open addressing
/// (linear probing) of the strings' indices (1 on, 0 for none), 2/3 full at most.
struct StringIndex(Vec<u32>);

impl StringIndex {
    fn slots(n: u64) -> usize {
        (n + n / 2 + 1) as usize
    }

    fn hash(chars: impl Iterator<Item = u8>) -> u64 {
        let mut h = Fnv::default();
        for c in chars {
            h.write(&[c]);
        }
        h.finish()
    }

    fn slot(&self, h: u64) -> usize {
        (((h >> 32) * self.0.len() as u64) >> 32) as usize
    }

    fn new(strings: &Strings, b: &[u8]) -> StringIndex {
        let n = strings.spans.len();
        let mut t = StringIndex(vec![0; StringIndex::slots(n as u64)]);
        for i in 0..n as u32 {
            let s = strings.get(b, i).unwrap();
            let mut j = t.slot(StringIndex::hash(s.iter().copied()));
            loop {
                match t.0[j] {
                    0 => {
                        t.0[j] = i + 1;
                        break;
                    }
                    k if strings.get(b, k - 1) == Some(s) => break,
                    _ => j = if j + 1 == t.0.len() { 0 } else { j + 1 },
                }
            }
        }
        t
    }

    /// The string of the shorty of the prototype of return type `ret` and parameters at
    /// `params` in `b` (0 for none), the descriptors of types given by `desc`.
    fn shorty<'a>(
        &self,
        strings: &Strings,
        b: &[u8],
        ret: u32,
        params: u32,
        desc: impl Fn(u32) -> Result<&'a [u8]>,
    ) -> Result<u32> {
        let p = params as usize;
        let n = if params == 0 { 0 } else { le32(b, p)? as usize };
        let ch = |k: usize| -> Result<u8> {
            let t = if k == 0 { ret } else { le16(b, p + 4 + 2 * (k - 1))? as u32 };
            shorty_char(desc(t)?).ok_or_else(corrupt)
        };
        // (each letter checked as it is taken)
        for k in 0..=n {
            ch(k)?;
        }
        let chars = || (0..=n).map(|k| ch(k).unwrap());
        let mut j = self.slot(StringIndex::hash(chars()));
        loop {
            match self.0[j] {
                0 => return Err(corrupt()),
                k => {
                    let s = strings.get(b, k - 1).ok_or_else(corrupt)?;
                    if s.len() == n + 1 && s.iter().copied().eq(chars()) {
                        return Ok(k - 1);
                    }
                }
            }
            j = if j + 1 == self.0.len() { 0 } else { j + 1 };
        }
    }
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
        let mut w = Vec::new();
        self.window_into(methods, &mut w);
        w
    }

    /// [`Code::window`] into `w`.
    pub fn window_into(&self, methods: usize, w: &mut Vec<usize>) {
        let end = self.offs.len().min(self.cursor.saturating_add(methods));
        w.clear();
        w.extend((self.cursor..end).filter(|&j| !self.used[j]));
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

/// A class's data from the streams, appended to `cd`; the offsets of its code by `code`, its
/// window in `window` (of room for all the code items).
fn class_data(raw: &mut Raw, cd: &mut Vec<u8>, code: &mut Code, window: &mut Vec<usize>) -> Result<()> {
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
    code.window_into(methods, window);
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
