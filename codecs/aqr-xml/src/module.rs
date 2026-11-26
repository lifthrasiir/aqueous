//! The code of the module (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module: the text rebuilt from its skeleton and the
//! values of its containers.

use aqr_api::stack::{block, whole};
use aqr_common::varint;

use crate::{Result, corrupt};

/// A value's place in the skeleton.
pub const VALUE: u8 = 1;
/// A start tag follows, its attributes' values VALUE.
pub const OPEN: u8 = 2;
/// An end tag of the innermost element's name.
pub const CLOSE: u8 = 3;
/// An end tag as written follows.
pub const END: u8 = 4;
/// The special byte after it is not a mark.
pub const ESC: u8 = 5;

/// The most containers of an application.
pub const MAX_CONTAINERS: u64 = 1 << 16;
/// The most elements open at once.
pub const MAX_DEPTH: u64 = 1 << 10;

/// Whether `b` is a special byte (VALUE to ESC).
#[inline]
pub fn special(b: u8) -> bool {
    matches!(b, VALUE..=ESC)
}

/// Whether `b` is a space of XML.
#[inline]
pub fn space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n')
}

/// Whether `b` may be in a name.
#[inline]
pub fn name_byte(b: u8) -> bool {
    !(space(b) || special(b) || matches!(b, b'=' | b'/' | b'>' | b'<' | b'"' | b'\''))
}

/// The params of an application: the containers and the depth, and the names of the elements
/// open at the start, as strings of `names`.
pub struct Params<'a> {
    pub containers: usize,
    pub depth: usize,
    pub open: usize,
    pub names: &'a [u8],
}

impl<'a> Params<'a> {
    /// The params `p` read, the names checked.
    pub fn parse(p: &'a [u8]) -> Result<Params<'a>> {
        let mut at = 0;
        let containers = varint::get(p, &mut at).map_err(|_| corrupt())?;
        let depth = varint::get(p, &mut at).map_err(|_| corrupt())?;
        let open = varint::get(p, &mut at).map_err(|_| corrupt())?;
        if containers > MAX_CONTAINERS || depth > MAX_DEPTH || open > depth {
            return Err(corrupt());
        }
        let names = &p[at..];
        let mut rest = names;
        for _ in 0..open {
            string(&mut rest)?;
        }
        if !rest.is_empty() {
            return Err(corrupt());
        }
        Ok(Params { containers: containers as usize, depth: depth as usize, open: open as usize, names })
    }
}

/// A string of the params read from `s`.
fn string<'a>(s: &mut &'a [u8]) -> Result<&'a [u8]> {
    let mut at = 0;
    let n = varint::get(s, &mut at).map_err(|_| corrupt())?;
    let end = at.checked_add(usize::try_from(n).map_err(|_| corrupt())?).filter(|&e| e <= s.len());
    let end = end.ok_or_else(corrupt)?;
    let r = &s[at..end];
    *s = &s[end..];
    Ok(r)
}

/// The entries of the table for `containers` containers: a power of two, at least twice as many.
pub fn table_size(containers: usize) -> usize {
    (2 * containers).next_power_of_two().max(2)
}

// (the plan's sizes, of wasm32)
#[cfg(target_arch = "wasm32")]
const _: () = assert!(
    size_of::<Frame>() == FRAME_BYTES as usize
        && size_of::<Container>() == CONTAINER_BYTES as usize
        && size_of::<Entry>() == ENTRY_BYTES as usize
);
const FRAME_BYTES: u64 = 12;
const CONTAINER_BYTES: u64 = 12;
const ENTRY_BYTES: u64 = 24;

/// The memory plan of the module ([`aqr_api::stack`]): what it takes beyond its initial memory
/// at most, of params `params`, inputs of `inputs` bytes and a text of `len` bytes: the stack
/// of names, the containers, the table of the contexts and the text, all held to the end.
pub fn plan(params: &[u8], inputs: &[u64], len: u64) -> Option<u64> {
    let &[_, table, _, _] = inputs else { return None };
    let p = Params::parse(params).ok()?;
    if p.containers as u64 > table / 2 {
        return None;
    }
    let held = block(FRAME_BYTES * p.depth as u64)
        + block(CONTAINER_BYTES * p.containers as u64)
        + block(ENTRY_BYTES * table_size(p.containers) as u64)
        + block(len);
    Some(whole(params.len() as u64, &[], inputs) + held)
}

/// A name open, and the container of its text (`NONE` before the first value of it).
#[derive(Clone, Copy)]
struct Frame<'a> {
    name: &'a [u8],
    cont: u32,
}

/// A container: what is left of its part of `values`, and whether its lengths are in `lens`.
#[derive(Clone, Copy)]
struct Container<'a> {
    part: &'a [u8],
    lens: bool,
}

/// An entry of the table of contexts: the kind (0 text, 1 attribute), its two names and its
/// container (`NONE` where the entry is empty).
#[derive(Clone, Copy)]
struct Entry<'a> {
    a: &'a [u8],
    b: &'a [u8],
    cont: u32,
    kind: u8,
}

const NONE: u32 = u32::MAX;

fn hash(kind: u8, a: &[u8], b: &[u8]) -> usize {
    let mut h = 0x811c_9dc5u32 ^ kind as u32;
    for &x in a {
        h = (h ^ x as u32).wrapping_mul(0x0100_0193);
    }
    h = (h ^ 0x100).wrapping_mul(0x0100_0193);
    for &x in b {
        h = (h ^ x as u32).wrapping_mul(0x0100_0193);
    }
    (h ^ h >> 15) as usize
}

struct Decoder<'a> {
    skeleton: &'a [u8],
    lens: &'a [u8],
    stack: Vec<Frame<'a>>,
    conts: Vec<Container<'a>>,
    /// The containers made so far.
    made: usize,
    table: Vec<Entry<'a>>,
    out: Vec<u8>,
    len: usize,
}

/// A new vector of `n` elements of room, none else (and no more).
fn room<T>(n: usize) -> Result<Vec<T>> {
    let mut v = Vec::new();
    v.try_reserve_exact(n).map_err(|_| corrupt())?;
    Ok(v)
}

/// The text of `len` bytes from `params` and `inputs` (the streams `skeleton`, `table`, `lens`
/// and `values`).
pub fn decode<'a>(params: &'a [u8], inputs: &[&'a [u8]], len: u64, _threads: usize) -> Result<Vec<u8>> {
    let &[skeleton, table, lens, values] = inputs else { return Err(corrupt()) };
    let len = usize::try_from(len).map_err(|_| corrupt())?;
    let p = Params::parse(params)?;
    if p.containers > table.len() / 2 {
        return Err(corrupt());
    }
    let mut d = Decoder {
        skeleton,
        lens,
        stack: room(p.depth)?,
        conts: room(p.containers)?,
        made: 0,
        table: Vec::new(),
        out: Vec::new(),
        len,
    };
    d.table = room(table_size(p.containers))?;
    d.table.resize(table_size(p.containers), Entry { a: &[], b: &[], cont: NONE, kind: 0 });
    d.out = room(len)?;
    let (mut names, mut at, mut rest) = (p.names, 0, values);
    for _ in 0..p.open {
        d.stack.push(Frame { name: string(&mut names)?, cont: NONE });
    }
    for _ in 0..p.containers {
        let flags = *table.get(at).ok_or_else(corrupt)?;
        at += 1;
        let n = varint::get(table, &mut at).map_err(|_| corrupt())?;
        if flags > 1 || n > rest.len() as u64 {
            return Err(corrupt());
        }
        let (part, tail) = rest.split_at(n as usize);
        d.conts.push(Container { part, lens: flags & 1 != 0 });
        rest = tail;
    }
    if at != table.len() || !rest.is_empty() {
        return Err(corrupt());
    }
    d.run(p.depth)?;
    if d.out.len() != len || d.made != d.conts.len() || !d.lens.is_empty() || d.conts.iter().any(|c| !c.part.is_empty())
    {
        return Err(corrupt());
    }
    Ok(d.out)
}

impl<'a> Decoder<'a> {
    fn emit(&mut self, s: &[u8]) -> Result<()> {
        if s.len() > self.len - self.out.len() {
            return Err(corrupt());
        }
        self.out.extend_from_slice(s);
        Ok(())
    }

    /// The container of the context (`kind`, `a`, `b`), made if it is the first value taken in it.
    fn container(&mut self, kind: u8, a: &'a [u8], b: &'a [u8]) -> Result<u32> {
        let mask = self.table.len() - 1;
        let mut i = hash(kind, a, b) & mask;
        loop {
            let e = &mut self.table[i];
            if e.cont == NONE {
                if self.made == self.conts.len() {
                    return Err(corrupt());
                }
                *e = Entry { a, b, cont: self.made as u32, kind };
                self.made += 1;
                return Ok(e.cont);
            }
            if e.kind == kind && e.a == a && e.b == b {
                return Ok(e.cont);
            }
            i = (i + 1) & mask;
        }
    }

    /// The next value of container `c`, put out.
    fn value(&mut self, c: u32) -> Result<()> {
        let ct = &mut self.conts[c as usize];
        let part = ct.part;
        let (v, rest) = if ct.lens {
            let mut at = 0;
            let n = varint::get(self.lens, &mut at).map_err(|_| corrupt())?;
            self.lens = &self.lens[at..];
            if n > part.len() as u64 {
                return Err(corrupt());
            }
            part.split_at(n as usize)
        } else {
            let n = part.iter().position(|&b| b == b'\n').ok_or_else(corrupt)?;
            (&part[..n], &part[n + 1..])
        };
        ct.part = rest;
        self.emit(v)
    }

    /// The text of the skeleton, and the stack's bound `depth`.
    fn run(&mut self, depth: usize) -> Result<()> {
        let s = self.skeleton;
        let mut i = 0;
        while i < s.len() {
            let j = i + s[i..].iter().position(|&b| special(b)).unwrap_or(s.len() - i);
            self.emit(&s[i..j])?;
            if j == s.len() {
                break;
            }
            i = j + 1;
            match s[j] {
                VALUE => {
                    let top = self.stack.len().checked_sub(1).ok_or_else(corrupt)?;
                    let mut c = self.stack[top].cont;
                    if c == NONE {
                        let parent = top.checked_sub(1).map_or(&[][..], |p| self.stack[p].name);
                        c = self.container(0, parent, self.stack[top].name)?;
                        self.stack[top].cont = c;
                    }
                    self.value(c)?;
                }
                OPEN => i = self.open(i, depth)?,
                CLOSE => {
                    let f = self.stack.pop().ok_or_else(corrupt)?;
                    self.emit(b"</")?;
                    self.emit(f.name)?;
                    self.emit(b">")?;
                }
                END => {
                    let n = s[i..].iter().position(|&b| b == b'>').ok_or_else(corrupt)? + 1;
                    let tag = &s[i..i + n];
                    if !tag.starts_with(b"</") || tag.iter().any(|&b| special(b)) {
                        return Err(corrupt());
                    }
                    self.stack.pop().ok_or_else(corrupt)?;
                    self.emit(tag)?;
                    i += n;
                }
                _ => {
                    // ESC
                    let b = *s.get(i).filter(|&&b| special(b)).ok_or_else(corrupt)?;
                    self.emit(&[b])?;
                    i += 1;
                }
            }
        }
        Ok(())
    }

    /// The start tag at `i` of the skeleton put out; where it ends.
    fn open(&mut self, mut i: usize, depth: usize) -> Result<usize> {
        let s = self.skeleton;
        let from = i;
        let at = |i: usize| s.get(i).copied();
        let skip = |mut i: usize| {
            while at(i).is_some_and(space) {
                i += 1;
            }
            i
        };
        let name = |mut i: usize| {
            let start = i;
            while at(i).is_some_and(name_byte) {
                i += 1;
            }
            (i > start).then_some(i)
        };
        if at(i) != Some(b'<') {
            return Err(corrupt());
        }
        let n0 = i + 1;
        i = name(n0).ok_or_else(corrupt)?;
        let element = &s[n0..i];
        let mut pending = from;
        let push = loop {
            let j = skip(i);
            match at(j) {
                Some(b'>') => {
                    i = j + 1;
                    break true;
                }
                Some(b'/') if at(j + 1) == Some(b'>') => {
                    i = j + 2;
                    break false;
                }
                Some(_) if j > i => {
                    let end = name(j).ok_or_else(corrupt)?;
                    let attr = &s[j..end];
                    let k = skip(end);
                    if at(k) != Some(b'=') {
                        return Err(corrupt());
                    }
                    let k = skip(k + 1);
                    let q = at(k).filter(|&q| q == b'"' || q == b'\'').ok_or_else(corrupt)?;
                    if at(k + 1) != Some(VALUE) || at(k + 2) != Some(q) {
                        return Err(corrupt());
                    }
                    self.emit(&s[pending..k + 1])?;
                    let c = self.container(1, element, attr)?;
                    self.value(c)?;
                    pending = k + 2;
                    i = k + 3;
                }
                _ => return Err(corrupt()),
            }
        };
        self.emit(&s[pending..i])?;
        if push {
            if self.stack.len() >= depth {
                return Err(corrupt());
            }
            self.stack.push(Frame { name: element, cont: NONE });
        }
        Ok(i)
    }
}
