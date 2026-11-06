//! CodeView symbol records of the modules' streams (FORMAT.md §3): each record's fields by its
//! kind's layout to streams of their own, the offsets of a scope's parent and end derived from
//! how the scopes nest, item ids and type indices through caches, code offsets as deltas from
//! the previous of their field.

use aqr_common::varint;

use crate::{Result, corrupt};

/// The streams, and the inputs they are packed into.
pub const STREAM_NAMES: [&str; 23] = [
    "kind",
    "len",
    "scope",
    "item",
    "type",
    "code",
    "name",
    "ann",
    "tail",
    "raw",
    "annop",
    "annoff",
    "annlen",
    "annline",
    "annother",
    "lines",
    "c13",
    "c13line",
    "c13off",
    "c13flag",
    "c13inl",
    "c13inlline",
    "c13inlid",
];
const KIND: usize = 0;
const LEN: usize = 1;
const SCOPE: usize = 2;
const ITEM: usize = 3;
const TYPE: usize = 4;
const CODE: usize = 5;
const NAME: usize = 6;
const ANN: usize = 7;
const TAIL: usize = 8;
const RAW: usize = 9;
const ANNOP: usize = 10;
const ANNOFF: usize = 11;
const ANNLEN: usize = 12;
const ANNLINE: usize = 13;
const ANNOTHER: usize = 14;
const LINES: usize = 15;
const C13: usize = 16;
const C13LINE: usize = 17;
const C13OFF: usize = 18;
const C13FLAG: usize = 19;
const C13INL: usize = 20;
const C13INLLINE: usize = 21;
const C13INLID: usize = 22;
pub const NUM_STREAMS: usize = STREAM_NAMES.len();
pub const INPUTS: [(&str, &[usize]); 6] = [
    ("head", &[KIND, LEN, SCOPE]),
    ("fields", &[ITEM, TYPE, CODE, TAIL, RAW]),
    ("text", &[NAME, ANN]),
    ("ann", &[ANNOP, ANNOFF, ANNLEN, ANNLINE, ANNOTHER]),
    ("c13", &[C13, C13LINE, C13OFF, C13FLAG, C13INL, C13INLLINE, C13INLID]),
    ("lines", &[LINES]),
];

/// A field of a record's layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum F {
    /// The offset of the scope's parent (u32), derived.
    Parent,
    /// The offset of the scope's end (u32), derived.
    End,
    /// An item id (u32).
    Item,
    /// A type index (u32).
    Type,
    /// A number (u32) as the delta from the previous of its field.
    Code,
    /// A count (u32) of item ids that follow.
    Items,
    /// u32, u16, u8, as they are.
    U32,
    U16,
    U8,
    /// A NUL-terminated name.
    Name,
}

use F::*;

const PROC: &[F] = &[Parent, End, U32, Code, Code, Code, Type, Code, U16, U8, Name];
const PROC_ID: &[F] = &[Parent, End, U32, Code, Code, Code, Item, Code, U16, U8, Name];

/// The layout of the records of `kind` (their fields, the rest after them as it is), and whether
/// they open a scope (`Some(true)`) or close one (`Some(false)`).
fn layout(kind: u16) -> (&'static [F], Option<bool>) {
    match kind {
        0x114d => (&[Parent, End, Item], Some(true)),            // S_INLINESITE
        0x115d => (&[Parent, End, Item, U32], Some(true)),       // S_INLINESITE2
        0x1110 | 0x110f | 0x1155 | 0x1156 => (PROC, Some(true)), // S_GPROC32, ...
        0x1147 | 0x1146 => (PROC_ID, Some(true)),                // S_GPROC32_ID, ...
        0x1103 => (&[Parent, End, Code, Code, U16, Name], Some(true)), // S_BLOCK32
        0x1102 => (&[Parent, End, U32, Code, U16, U16, U8, Name], Some(true)), // S_THUNK32
        0x1104 => (&[Parent, End, Code, Code, U16, Name], Some(true)), // S_WITH32
        0x1132 => (&[Parent, End], Some(true)),                  // S_SEPCODE
        0x0006 | 0x114f | 0x114e => (&[], Some(false)),          // S_END, ...
        0x113e => (&[Type, U16, Name], None),                    // S_LOCAL
        0x1111 => (&[Code, Type, U16, Name], None),              // S_REGREL32
        0x110b => (&[Code, Type, Name], None),                   // S_BPREL32
        0x110c | 0x110d | 0x1112 | 0x1113 => (&[Type, Code, U16, Name], None), // S_LDATA32, ...
        0x1105 => (&[Code, U16, U8, Name], None),                // S_LABEL32
        0x1108 => (&[Type, Name], None),                         // S_UDT
        0x1168 => (&[Items], None),                              // S_INLINEES
        0x114c => (&[Item], None),                               // S_BUILDINFO
        0x1101 => (&[U32, Name], None),                          // S_OBJNAME
        _ => (&[], None),
    }
}

/// The stream of the rest of a record of `kind`.
fn rest_stream(kind: u16) -> usize {
    if matches!(kind, 0x114d | 0x115d) { ANN } else { TAIL }
}

const SLOTS: usize = 1024;
const CACHE: usize = 64;

/// The state both sides keep, of which values are predicted.
struct State {
    prev: Vec<u32>,
    /// The previous of the lines' fields: a block's offset and file, a line's number, an
    /// inlinee's file and line.
    c13: [u32; 5],
    /// Of each inlinee, the file and line of its last entry in inlinee lines.
    inlinees: Inlinees,
    item_prev: u32,
    item_cache: Vec<u32>,
    type_prev: u32,
    type_cache: Vec<u32>,
}

fn slot(kind: u16, field: usize) -> usize {
    (kind as usize * 31 + field) & (SLOTS - 1)
}

/// The symbol (at least 0) of `v` among `cache` and from `prev`; both updated.
fn cached(cache: &mut Vec<u32>, prev: &mut u32, v: u32) -> u64 {
    let sym = match cache.iter().position(|&c| c == v) {
        Some(k) => k as u64,
        None => CACHE as u64 + varint::zz(v as i64 - *prev as i64),
    };
    used(cache, prev, v);
    sym
}

fn uncached(cache: &mut Vec<u32>, prev: &mut u32, sym: u64) -> Option<u32> {
    let v = match sym {
        _ if sym < CACHE as u64 => *cache.get(sym as usize)?,
        _ => u32::try_from(*prev as i64 + varint::unzz(sym - CACHE as u64)).ok()?,
    };
    used(cache, prev, v);
    Some(v)
}

fn used(cache: &mut Vec<u32>, prev: &mut u32, v: u32) {
    *prev = v;
    match cache.iter().position(|&x| x == v) {
        Some(k) => cache[..=k].rotate_right(1),
        None => {
            if cache.len() == CACHE {
                cache.pop();
            }
            cache.insert(0, v);
        }
    }
}

impl State {
    /// The state at first, of `inlinees` entries of inlinee lines at most (more grow its table).
    fn new(inlinees: usize) -> State {
        State {
            prev: vec![0; SLOTS],
            c13: [0; 5],
            inlinees: Inlinees::new(inlinees),
            item_prev: 0,
            item_cache: Vec::with_capacity(CACHE),
            type_prev: 0,
            type_cache: Vec::with_capacity(CACHE),
        }
    }

    /// Bytes that [`State::new`] takes of [`aqr_api::stack`], in its blocks' order.
    fn plan(inlinees: usize) -> u64 {
        use aqr_api::stack::block;
        block(4 * SLOTS as u64) + block(Inlinees::bytes(inlinees)) + 2 * block(4 * CACHE as u64)
    }
}

/// A table of the inlinees' files and lines by their ids (open addressing, linear probing),
/// kept 2/3 full at most: room made for `n` of them, more growing it.
struct Inlinees {
    /// The id, file and line of each, [`VACANT`] the id of none.
    slots: Vec<[u32; 3]>,
    len: usize,
    /// Of the id [`VACANT`].
    vacant: Option<(u32, u32)>,
}

const VACANT: u32 = u32::MAX;

impl Inlinees {
    fn slots(n: usize) -> usize {
        n + n / 2 + 1
    }

    fn bytes(n: usize) -> u64 {
        12 * Inlinees::slots(n) as u64
    }

    fn new(n: usize) -> Inlinees {
        Inlinees { slots: vec![[VACANT, 0, 0]; Inlinees::slots(n)], len: 0, vacant: None }
    }

    /// The slot of `id` (not [`VACANT`]), or the vacant one where it would go.
    fn find(&self, id: u32) -> usize {
        let n = self.slots.len();
        let mut i = ((id.wrapping_mul(0x9e37_79b9) as u64 * n as u64) >> 32) as usize;
        while self.slots[i][0] != id && self.slots[i][0] != VACANT {
            i = if i + 1 == n { 0 } else { i + 1 };
        }
        i
    }

    fn get(&self, id: u32) -> Option<(u32, u32)> {
        if id == VACANT {
            return self.vacant;
        }
        let [k, file, line] = self.slots[self.find(id)];
        (k == id).then_some((file, line))
    }

    fn insert(&mut self, id: u32, file: u32, line: u32) {
        if id == VACANT {
            self.vacant = Some((file, line));
            return;
        }
        let mut i = self.find(id);
        if self.slots[i][0] == VACANT {
            if 3 * (self.len + 1) > 2 * self.slots.len() {
                let old = std::mem::replace(self, Inlinees::new(2 * self.len + 4));
                self.vacant = old.vacant;
                for s in old.slots.into_iter().filter(|s| s[0] != VACANT) {
                    let j = self.find(s[0]);
                    self.slots[j] = s;
                    self.len += 1;
                }
                i = self.find(id);
            }
            self.len += 1;
        }
        self.slots[i] = [id, file, line];
    }
}

fn u32le(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(*d.get(at..)?.first_chunk()?))
}

fn u16le(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(*d.get(at..)?.first_chunk()?))
}

/// The params and the streams of the modules' streams `mods` (each with what the DBI stream
/// tells of it), or `None` where they are not rebuilt as they are. A module's C13 lines are coded
/// by their subsections; its C11 lines and what follows (its global references) go to `lines`
/// as they are.
pub fn encode(mods: &[(&[u8], crate::dbi::Module)]) -> Option<(Vec<u8>, Vec<Vec<u8>>)> {
    let mut st = vec![Vec::new(); NUM_STREAMS];
    let mut s = State::new(0);
    let mut most = 0;
    for &(m, info) in mods {
        let (sym, rest) = m.split_at_checked(info.symbols)?;
        most = most.max(module(&mut st, &mut s, sym));
        // the C11 lines, the C13 lines (where they are whole within), the rest
        let c11 = info.c11.min(rest.len());
        let c13 = if info.c13 <= rest.len() - c11 { info.c13 } else { 0 };
        varint::put(&mut st[LEN], c11 as u64);
        st[LINES].extend_from_slice(&rest[..c11]);
        varint::put(&mut st[LEN], c13 as u64);
        if c13 > 0 {
            lines(&mut st, &mut s, &rest[c11..c11 + c13]);
        }
        varint::put(&mut st[LEN], (rest.len() - c11 - c13) as u64);
        st[LINES].extend_from_slice(&rest[c11 + c13..]);
    }
    let mut params = Vec::new();
    varint::put(&mut params, most as u64);
    let refs: Vec<&[u8]> = st.iter().map(|v| &v[..]).collect();
    let back = decode_sized(&params, &refs, mods.iter().map(|m| m.0.len()).sum()).ok()?;
    let mut at = 0;
    for &(m, _) in mods {
        if back.get(at..at + m.len())? != m {
            return None;
        }
        at += m.len();
    }
    (at == back.len()).then_some((params, st))
}

/// The scopes open at once at most, of params `params`, as many as the records of kinds `kinds`
/// (of 2 bytes each) at most.
fn most_open(params: &[u8], kinds: &[u8]) -> Result<usize> {
    let mut at = 0;
    let most = varint::get(params, &mut at).map_err(|_| corrupt())?;
    if at != params.len() || most > kinds.len() as u64 / 2 {
        return Err(corrupt());
    }
    Ok(most as usize)
}

const DEBUG_S_LINES: u32 = 0xf2;
const DEBUG_S_INLINEELINES: u32 = 0xf6;

/// The subsections of C13 lines `c`: `(kind, body)`, each padded to 4 bytes with zeros; `None`
/// where they are not so.
fn subsections(c: &[u8]) -> Option<Vec<(u32, &[u8])>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < c.len() {
        let (kind, len) = (u32le(c, at)?, u32le(c, at + 4)? as usize);
        let body = c.get(at + 8..at.checked_add(8)?.checked_add(len)?)?;
        let end = (at + 8 + len).next_multiple_of(4);
        if c.get(at + 8 + len..end)?.iter().any(|&b| b != 0) {
            return None;
        }
        out.push((kind, body));
        at = end;
    }
    Some(out)
}

/// Whether a lines subsection's body `b` is as [`lines`] codes it: its header, then blocks each
/// of its lines (and their columns where the header's flags tell), to its end.
fn lines_fit(b: &[u8]) -> bool {
    let Some(flags) = u16le(b, 6) else { return false };
    let per = if flags & 1 != 0 { 12 } else { 8 };
    let mut at = 12;
    while at < b.len() {
        let (Some(n), Some(size)) = (u32le(b, at + 4), u32le(b, at + 8)) else { return false };
        if (n as u64) * per + 12 != size as u64 || at + size as usize > b.len() {
            return false;
        }
        at += size as usize;
    }
    at == b.len() && b.len() >= 12
}

/// Whether an inlinee lines subsection's body `b` is as [`lines`] codes it: of signature 0, its
/// entries of 12 bytes.
fn inlinees_fit(b: &[u8]) -> bool {
    u32le(b, 0) == Some(0) && (b.len() - 4).is_multiple_of(12)
}

/// C13 lines `c`: a byte of `c13`, 1 where they are kept as they are (in `lines`), else 0 and
/// their subsections: each its kind and length (varints of `c13`), then, of lines and inlinee
/// lines as they fit, a 0 and their fields, else a 1 and the body as it is.
fn lines(st: &mut [Vec<u8>], s: &mut State, c: &[u8]) {
    let Some(subs) = subsections(c) else {
        st[C13].push(1);
        st[LINES].extend_from_slice(c);
        return;
    };
    st[C13].push(0);
    varint::put(&mut st[C13], subs.len() as u64);
    for (kind, b) in subs {
        varint::put(&mut st[C13], kind as u64);
        varint::put(&mut st[C13], b.len() as u64);
        let delta = |st: &mut [Vec<u8>], k: usize, prev: &mut u32, v: u32| {
            varint::put_signed(&mut st[k], v as i64 - *prev as i64);
            *prev = v;
        };
        match kind {
            DEBUG_S_LINES if lines_fit(b) => {
                st[C13].push(0);
                let flags = u16le(b, 6).unwrap();
                delta(st, C13, &mut s.c13[0], u32le(b, 0).unwrap());
                st[C13].extend_from_slice(&b[4..8]);
                varint::put(&mut st[C13], u32le(b, 8).unwrap() as u64);
                let mut at = 12;
                while at < b.len() {
                    let n = u32le(b, at + 4).unwrap() as usize;
                    delta(st, C13, &mut s.c13[1], u32le(b, at).unwrap());
                    varint::put(&mut st[C13], n as u64);
                    let mut off = 0;
                    for i in 0..n {
                        let (o, l) = (u32le(b, at + 12 + 8 * i).unwrap(), u32le(b, at + 16 + 8 * i).unwrap());
                        delta(st, C13OFF, &mut off, o);
                        delta(st, C13LINE, &mut s.c13[2], l & 0xff_ffff);
                        st[C13FLAG].push((l >> 24) as u8);
                    }
                    let cols = if flags & 1 != 0 { 4 * n } else { 0 };
                    st[LINES].extend_from_slice(&b[at + 12 + 8 * n..][..cols]);
                    at += 12 + 8 * n + cols;
                }
            }
            DEBUG_S_INLINEELINES if inlinees_fit(b) => {
                st[C13].push(0);
                for e in b[4..].as_chunks::<12>().0 {
                    let item = u32::from_le_bytes(e[..4].try_into().unwrap());
                    st[C13INLID].extend_from_slice(&item.to_le_bytes());
                    // the file and line from the inlinee's last entry, else the previous entry's
                    let (file, line) = (u32le(e, 4).unwrap(), u32le(e, 8).unwrap());
                    let (pf, pl) = s.inlinees.get(item).unwrap_or((s.c13[3], s.c13[4]));
                    varint::put_signed(&mut st[C13INL], file as i64 - pf as i64);
                    varint::put_signed(&mut st[C13INLLINE], line as i64 - pl as i64);
                    (s.c13[3], s.c13[4]) = (file, line);
                    s.inlinees.insert(item, file, line);
                }
            }
            _ => {
                st[C13].push(1);
                st[LINES].extend_from_slice(b);
            }
        }
    }
}

/// [`lines`] decoded: C13 lines of `size` bytes appended to `out`.
fn lines_decode(r: &mut [Reader], s: &mut State, out: &mut Vec<u8>, size: usize) -> Result<()> {
    let start = out.len();
    if r[C13].bytes(1)?[0] != 0 {
        out.extend_from_slice(r[LINES].bytes(size)?);
        return Ok(());
    }
    let n = r[C13].get()?;
    let u32_of = |v: u64| u32::try_from(v).map_err(|_| corrupt());
    let moved = |r: &mut Reader, prev: &mut u32| -> Result<u32> {
        *prev = u32::try_from(*prev as i64 + varint::unzz(r.get()?)).map_err(|_| corrupt())?;
        Ok(*prev)
    };
    for _ in 0..n {
        let kind = u32_of(r[C13].get()?)?;
        let len = usize::try_from(r[C13].get()?).map_err(|_| corrupt())?;
        if out.len() - start + 8 + len > size {
            return Err(corrupt());
        }
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&(len as u32).to_le_bytes());
        let body = out.len();
        match (kind, r[C13].bytes(1)?[0]) {
            (_, 1) => out.extend_from_slice(r[LINES].bytes(len)?),
            (DEBUG_S_LINES, 0) => {
                let off = moved(&mut r[C13], &mut s.c13[0])?;
                out.extend_from_slice(&off.to_le_bytes());
                let seg_flags: [u8; 4] = r[C13].bytes(4)?.try_into().unwrap();
                out.extend_from_slice(&seg_flags);
                let cols = seg_flags[2] & 1 != 0;
                out.extend_from_slice(&u32_of(r[C13].get()?)?.to_le_bytes());
                while out.len() - body < len {
                    let file = moved(&mut r[C13], &mut s.c13[1])?;
                    let n = r[C13].get()?;
                    let size = 12 + n * if cols { 12 } else { 8 };
                    if (out.len() - body) as u64 + size > len as u64 {
                        return Err(corrupt());
                    }
                    out.extend_from_slice(&file.to_le_bytes());
                    out.extend_from_slice(&(n as u32).to_le_bytes());
                    out.extend_from_slice(&(size as u32).to_le_bytes());
                    let mut off = 0;
                    for _ in 0..n {
                        let o = moved(&mut r[C13OFF], &mut off)?;
                        let l = moved(&mut r[C13LINE], &mut s.c13[2])?;
                        let flags = r[C13FLAG].bytes(1)?[0] as u32;
                        if l > 0xff_ffff {
                            return Err(corrupt());
                        }
                        out.extend_from_slice(&o.to_le_bytes());
                        out.extend_from_slice(&(l | flags << 24).to_le_bytes());
                    }
                    if cols {
                        out.extend_from_slice(r[LINES].bytes(4 * n as usize)?);
                    }
                }
            }
            (DEBUG_S_INLINEELINES, 0) => {
                if len < 4 || !(len - 4).is_multiple_of(12) {
                    return Err(corrupt());
                }
                out.extend_from_slice(&0u32.to_le_bytes());
                for _ in 0..(len - 4) / 12 {
                    let item = u32::from_le_bytes(r[C13INLID].bytes(4)?.try_into().unwrap());
                    let (mut file, mut line) = s.inlinees.get(item).unwrap_or((s.c13[3], s.c13[4]));
                    let file = moved(&mut r[C13INL], &mut file)?;
                    let line = moved(&mut r[C13INLLINE], &mut line)?;
                    (s.c13[3], s.c13[4]) = (file, line);
                    s.inlinees.insert(item, file, line);
                    out.extend_from_slice(&item.to_le_bytes());
                    out.extend_from_slice(&file.to_le_bytes());
                    out.extend_from_slice(&line.to_le_bytes());
                }
            }
            _ => return Err(corrupt()),
        }
        if out.len() - body != len {
            return Err(corrupt());
        }
        let pad = (4 - (out.len() - start) % 4) % 4;
        out.resize(out.len() + pad, 0);
    }
    if out.len() - start != size {
        return Err(corrupt());
    }
    Ok(())
}

/// Whether payload `d` holds `fields` (the record is coded by its layout, else kept).
fn fits(d: &[u8], fields: &[F]) -> bool {
    let mut at = 0;
    for &f in fields {
        let n = match f {
            Parent | End | Item | Type | Code | U32 => 4,
            U16 => 2,
            U8 => 1,
            Items => match u32le(d, at) {
                Some(n) if n as usize <= (d.len() - at - 4) / 4 => 4 + 4 * n as usize,
                _ => return false,
            },
            Name => match d.get(at..).and_then(|r| r.iter().position(|&c| c == 0)) {
                Some(n) => n + 1,
                None => return false,
            },
        };
        at += n;
        if at > d.len() {
            return false;
        }
    }
    true
}

/// The records of module substream `m`, `(offset, kind, payload range)`, as far as they hold,
/// and where they end.
fn records(m: &[u8]) -> (Vec<(usize, u16, usize, usize)>, usize) {
    let mut out = Vec::new();
    let mut q = 4;
    while let (Some(len), Some(kind)) = (u16le(m, q), u16le(m, q + 2)) {
        let end = q + 2 + len as usize;
        if len < 2 || end > m.len() {
            break;
        }
        out.push((q, kind, q + 4, end));
        q = end;
    }
    (out, q.min(m.len()))
}

/// Module substream `m` coded: the scopes open at once at most in it.
fn module(st: &mut [Vec<u8>], s: &mut State, m: &[u8]) -> usize {
    varint::put(&mut st[LEN], m.len() as u64);
    if m.len() < 4 {
        st[RAW].extend_from_slice(m);
        return 0;
    }
    st[TAIL].extend_from_slice(&m[..4]);
    let (recs, end) = records(m);
    // of each record opening a scope, the offset of the record closing it (none: `None`)
    let mut ends = vec![None; recs.len()];
    let mut stack = Vec::new();
    for (i, &(q, kind, _, _)) in recs.iter().enumerate() {
        match layout(kind).1 {
            Some(true) => stack.push(i),
            Some(false) => {
                if let Some(o) = stack.pop() {
                    ends[o] = Some(q as u32);
                }
            }
            None => {}
        }
    }
    let (mut stack, mut most): (Vec<u32>, usize) = (Vec::new(), 0);
    for (i, &(q, kind, p, e)) in recs.iter().enumerate() {
        st[KIND].extend_from_slice(&kind.to_le_bytes());
        let len = (e - q - 2) as u64;
        let (fields, scope) = layout(kind);
        if fits(&m[p..e], fields) {
            varint::put(&mut st[LEN], len << 1);
            let coded = record(st, s, m, kind, p, e, fields, &stack, ends[i]);
            debug_assert!(coded.is_some(), "a record that fits is coded");
        } else {
            // kept as it is
            varint::put(&mut st[LEN], len << 1 | 1);
            st[RAW].extend_from_slice(&m[p..e]);
        }
        match scope {
            Some(true) => {
                stack.push(q as u32);
                most = most.max(stack.len());
            }
            Some(false) => {
                stack.pop();
            }
            None => {}
        }
    }
    // the end of the module: a length of 1 (no record is of that), and what is left
    varint::put(&mut st[LEN], 1 << 1);
    varint::put(&mut st[LEN], (m.len() - end) as u64);
    st[RAW].extend_from_slice(&m[end..]);
    most
}

#[allow(clippy::too_many_arguments)]
fn record(
    st: &mut [Vec<u8>],
    s: &mut State,
    m: &[u8],
    kind: u16,
    p: usize,
    e: usize,
    fields: &[F],
    stack: &[u32],
    end: Option<u32>,
) -> Option<()> {
    let r = &m[..e];
    let mut at = p;
    for (k, &f) in fields.iter().enumerate() {
        match f {
            Parent => {
                let v = u32le(r, at)?;
                at += 4;
                let parent = stack.last().copied().unwrap_or(0);
                if v == parent {
                    st[SCOPE].push(0);
                } else {
                    varint::put(&mut st[SCOPE], v as u64 + 1);
                }
            }
            End => {
                let v = u32le(r, at)?;
                at += 4;
                if end == Some(v) {
                    st[SCOPE].push(0);
                } else {
                    varint::put(&mut st[SCOPE], v as u64 + 1);
                }
            }
            Item => {
                let v = u32le(r, at)?;
                at += 4;
                let sym = cached(&mut s.item_cache, &mut s.item_prev, v);
                varint::put(&mut st[ITEM], sym);
            }
            Items => {
                let n = u32le(r, at)?;
                at += 4;
                if n as usize > (e - at) / 4 {
                    return None;
                }
                varint::put(&mut st[CODE], n as u64);
                for _ in 0..n {
                    let v = u32le(r, at)?;
                    at += 4;
                    let sym = cached(&mut s.item_cache, &mut s.item_prev, v);
                    varint::put(&mut st[ITEM], sym);
                }
            }
            Type => {
                let v = u32le(r, at)?;
                at += 4;
                let sym = cached(&mut s.type_cache, &mut s.type_prev, v);
                varint::put(&mut st[TYPE], sym);
            }
            Code => {
                let v = u32le(r, at)?;
                at += 4;
                let sl = slot(kind, k);
                varint::put_signed(&mut st[CODE], v as i64 - s.prev[sl] as i64);
                s.prev[sl] = v;
            }
            U32 => {
                st[TAIL].extend_from_slice(r.get(at..at + 4)?);
                at += 4;
            }
            U16 => {
                st[TAIL].extend_from_slice(r.get(at..at + 2)?);
                at += 2;
            }
            U8 => {
                st[TAIL].push(*r.get(at)?);
                at += 1;
            }
            Name => {
                let n = r.get(at..)?.iter().position(|&c| c == 0)?;
                st[NAME].extend_from_slice(&r[at..=at + n]);
                at += n + 1;
            }
        }
    }
    // the rest, its length coded by the record's: an inline site's binary annotations by their
    // operations, else as it is
    if rest_stream(kind) == ANN {
        let mark: Vec<usize> = st.iter().map(Vec::len).collect();
        if annotations(st, &r[at..e]).is_some() {
            return Some(());
        }
        for (v, l) in st.iter_mut().zip(mark) {
            v.truncate(l);
        }
        st[ANNOP].push(ANN_RAW);
    }
    st[rest_stream(kind)].extend_from_slice(&r[at..e]);
    Some(())
}

/// The operation symbol of annotations kept as they are (no operation is of it).
const ANN_RAW: u8 = 0xff;

/// A compressed integer of binary annotations at `*at` of `d`: 1, 2 or 4 bytes (7, 14 or 29
/// bits).
fn cint(d: &[u8], at: &mut usize) -> Option<u32> {
    let b = *d.get(*at)? as u32;
    let (v, n) = match b {
        _ if b & 0x80 == 0 => (b, 1),
        _ if b & 0xc0 == 0x80 => ((b & 0x3f) << 8 | *d.get(*at + 1)? as u32, 2),
        _ if b & 0xe0 == 0xc0 => {
            let rest = d.get(*at + 1..*at + 4)?;
            ((b & 0x1f) << 24 | (rest[0] as u32) << 16 | (rest[1] as u32) << 8 | rest[2] as u32, 4)
        }
        _ => return None,
    };
    *at += n;
    Some(v)
}

fn put_cint(out: &mut Vec<u8>, v: u32) -> Option<()> {
    match v {
        _ if v < 0x80 => out.push(v as u8),
        _ if v < 0x4000 => out.extend_from_slice(&[0x80 | (v >> 8) as u8, v as u8]),
        _ if v < 0x2000_0000 => out.extend_from_slice(&(0xc000_0000 | v).to_be_bytes()),
        _ => return None,
    }
    Some(())
}

/// The stream of the operands of annotation operation `op` (1 to 13), and how many it has.
fn operands(op: u32) -> (usize, usize) {
    match op {
        3 => (ANNOFF, 1),
        12 => (ANNLEN, 2),
        1 | 2 | 4 => (ANNLEN, 1),
        6 | 7 | 11 => (ANNLINE, 1),
        _ => (ANNOTHER, 1),
    }
}

/// Binary annotations `d` by their operations (each in the fewest bytes), up to zeros to the end.
fn annotations(st: &mut [Vec<u8>], d: &[u8]) -> Option<()> {
    let mut at = 0;
    while at < d.len() {
        let start = at;
        let op = cint(d, &mut at)?;
        if op == 0 {
            if d[start..].iter().any(|&b| b != 0) {
                return None;
            }
            break;
        }
        if op > 13 || at - start != 1 {
            return None;
        }
        st[ANNOP].push(op as u8);
        let (k, n) = operands(op);
        for _ in 0..n {
            let s = at;
            let v = cint(d, &mut at)?;
            let mut back = Vec::new();
            put_cint(&mut back, v)?;
            if back[..] != d[s..at] {
                return None;
            }
            varint::put(&mut st[k], v as u64);
        }
    }
    st[ANNOP].push(0);
    Some(())
}

struct Reader<'a> {
    d: &'a [u8],
    at: usize,
}

/// Bytes of a [`Reader`] on WebAssembly.
const READER_BYTES: u64 = 12;
#[cfg(target_family = "wasm")]
const _: () = assert!(size_of::<Reader>() as u64 == READER_BYTES);

impl<'a> Reader<'a> {
    fn get(&mut self) -> Result<u64> {
        varint::get(self.d, &mut self.at).map_err(|_| corrupt())
    }

    fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let b = self.d.get(self.at..self.at.checked_add(n).ok_or_else(corrupt)?).ok_or_else(corrupt)?;
        self.at += n;
        Ok(b)
    }

    fn done(&self) -> bool {
        self.at == self.d.len()
    }
}

/// The symbol substreams, one after another, from `params` and the streams of
/// [`STREAM_NAMES`].
pub fn decode(params: &[u8], streams: &[&[u8]]) -> Result<Vec<u8>> {
    decode_sized(params, streams, 0)
}

/// [`decode`], the output's room made for `len` bytes (what the module is to give) at first.
pub fn decode_sized(params: &[u8], streams: &[&[u8]], len: usize) -> Result<Vec<u8>> {
    if streams.len() != NUM_STREAMS {
        return Err(corrupt());
    }
    let most = most_open(params, streams[KIND])?;
    let mut r = Vec::with_capacity(NUM_STREAMS);
    r.extend(streams.iter().map(|d| Reader { d, at: 0 }));
    let mut s = State::new(streams[C13INLID].len() / 4);
    let mut out = Vec::with_capacity(len);
    let mut open = Vec::with_capacity(most);
    while !r[LEN].done() {
        let size = usize::try_from(r[LEN].get()?).map_err(|_| corrupt())?;
        if size < 4 {
            out.extend_from_slice(r[RAW].bytes(size)?);
        } else {
            symbols(&mut r, &mut s, &mut out, size, &mut open)?;
        }
        // what follows the symbols: the C11 lines, the C13 lines, the rest
        let n = usize::try_from(r[LEN].get()?).map_err(|_| corrupt())?;
        out.extend_from_slice(r[LINES].bytes(n)?);
        let n = usize::try_from(r[LEN].get()?).map_err(|_| corrupt())?;
        if n > 0 {
            lines_decode(&mut r, &mut s, &mut out, n)?;
        }
        let n = usize::try_from(r[LEN].get()?).map_err(|_| corrupt())?;
        out.extend_from_slice(r[LINES].bytes(n)?);
    }
    if !r.iter().all(Reader::done) {
        return Err(corrupt());
    }
    Ok(out)
}

/// Bytes that [`decode_sized`] takes of [`aqr_api::stack`] at most, of params `params`, a
/// stream of kinds `kinds` and of inlinees' ids of `inlinee_ids` bytes and an output of `len`
/// bytes (`None` where the params are not so), in its blocks' order: the readers, the state
/// ([`State::plan`], of an entry of inlinee lines for each 4 bytes of ids), the output, the scopes
/// open.
pub fn decode_plan(params: &[u8], kinds: &[u8], inlinee_ids: usize, len: u64) -> Option<u64> {
    use aqr_api::stack::block;
    let most = most_open(params, kinds).ok()?;
    Some(
        block(READER_BYTES * NUM_STREAMS as u64)
            + State::plan(inlinee_ids / 4)
            + block(len)
            + block(OPEN_BYTES * most as u64),
    )
}

/// Bytes of a scope open: where it opened (in the module), and where its end goes (0 where none).
const OPEN_BYTES: u64 = 8;

/// A module's symbols of `size` bytes (4 at least), appended to `out`, the scopes open in
/// `stack` (of their capacity at most).
fn symbols(r: &mut [Reader], s: &mut State, out: &mut Vec<u8>, size: usize, stack: &mut Vec<(u32, u32)>) -> Result<()> {
    let start = out.len();
    {
        out.extend_from_slice(r[TAIL].bytes(4)?);
        stack.clear();
        loop {
            let v = r[LEN].get()?;
            if v == 2 {
                break;
            }
            let (len, raw) = (v >> 1, v & 1 == 1);
            let len = usize::try_from(len).ok().filter(|&l| (2..=0xffff).contains(&l)).ok_or_else(corrupt)?;
            let kind = u16::from_le_bytes(r[KIND].bytes(2)?.try_into().unwrap());
            let q = u32::try_from(out.len() - start).map_err(|_| corrupt())?;
            let rec_end = out.len() + 2 + len;
            if rec_end - start > size {
                return Err(corrupt());
            }
            out.extend_from_slice(&(len as u16).to_le_bytes());
            out.extend_from_slice(&kind.to_le_bytes());
            let (fields, scope) = layout(kind);
            let mut end_at = 0;
            if raw {
                out.extend_from_slice(r[RAW].bytes(len - 2)?);
            } else {
                for (k, &f) in fields.iter().enumerate() {
                    match f {
                        Parent => {
                            let v = match r[SCOPE].get()? {
                                0 => stack.last().map_or(0, |s| s.0),
                                v => u32::try_from(v - 1).map_err(|_| corrupt())?,
                            };
                            out.extend_from_slice(&v.to_le_bytes());
                        }
                        End => {
                            match r[SCOPE].get()? {
                                0 => {
                                    end_at = u32::try_from(out.len() - start).map_err(|_| corrupt())?;
                                    out.extend_from_slice(&[0; 4]);
                                }
                                v => out.extend_from_slice(&u32::try_from(v - 1).map_err(|_| corrupt())?.to_le_bytes()),
                            };
                        }
                        Item => {
                            let v =
                                uncached(&mut s.item_cache, &mut s.item_prev, r[ITEM].get()?).ok_or_else(corrupt)?;
                            out.extend_from_slice(&v.to_le_bytes());
                        }
                        Items => {
                            let n = r[CODE].get()?;
                            let n = u32::try_from(n).ok().filter(|&n| n as usize <= len / 4).ok_or_else(corrupt)?;
                            out.extend_from_slice(&n.to_le_bytes());
                            for _ in 0..n {
                                let v = uncached(&mut s.item_cache, &mut s.item_prev, r[ITEM].get()?)
                                    .ok_or_else(corrupt)?;
                                out.extend_from_slice(&v.to_le_bytes());
                            }
                        }
                        Type => {
                            let v =
                                uncached(&mut s.type_cache, &mut s.type_prev, r[TYPE].get()?).ok_or_else(corrupt)?;
                            out.extend_from_slice(&v.to_le_bytes());
                        }
                        Code => {
                            let sl = slot(kind, k);
                            let z = varint::unzz(r[CODE].get()?);
                            let v = u32::try_from(s.prev[sl] as i64 + z).map_err(|_| corrupt())?;
                            s.prev[sl] = v;
                            out.extend_from_slice(&v.to_le_bytes());
                        }
                        U32 => out.extend_from_slice(r[TAIL].bytes(4)?),
                        U16 => out.extend_from_slice(r[TAIL].bytes(2)?),
                        U8 => out.extend_from_slice(r[TAIL].bytes(1)?),
                        Name => {
                            let n = r[NAME].d[r[NAME].at..].iter().position(|&c| c == 0).ok_or_else(corrupt)?;
                            out.extend_from_slice(r[NAME].bytes(n + 1)?);
                        }
                    }
                }
                let rest = rec_end.checked_sub(out.len()).ok_or_else(corrupt)?;
                if rest_stream(kind) == ANN && r[ANNOP].d.get(r[ANNOP].at) != Some(&ANN_RAW) {
                    loop {
                        let op = r[ANNOP].bytes(1)?[0] as u32;
                        if op == 0 {
                            break;
                        }
                        if op > 13 {
                            return Err(corrupt());
                        }
                        out.push(op as u8);
                        let (k, n) = operands(op);
                        for _ in 0..n {
                            let v = u32::try_from(r[k].get()?).map_err(|_| corrupt())?;
                            put_cint(out, v).ok_or_else(corrupt)?;
                        }
                        if out.len() > rec_end {
                            return Err(corrupt());
                        }
                    }
                    out.resize(rec_end, 0);
                } else {
                    if rest_stream(kind) == ANN {
                        r[ANNOP].bytes(1)?;
                    }
                    out.extend_from_slice(r[rest_stream(kind)].bytes(rest)?);
                }
            }
            match scope {
                Some(true) => {
                    if stack.len() == stack.capacity() {
                        return Err(corrupt());
                    }
                    stack.push((q, end_at));
                }
                Some(false) => {
                    if let Some((_, at)) = stack.pop().filter(|s| s.1 > 0) {
                        let at = start + at as usize;
                        out[at..at + 4].copy_from_slice(&q.to_le_bytes());
                    }
                }
                None => {}
            }
            if out.len() != rec_end {
                return Err(corrupt());
            }
        }
        // a scope's end to derive that no record closed
        if stack.iter().any(|s| s.1 > 0) {
            return Err(corrupt());
        }
        let n = usize::try_from(r[LEN].get()?).map_err(|_| corrupt())?;
        out.extend_from_slice(r[RAW].bytes(n)?);
        if out.len() - start != size {
            return Err(corrupt());
        }
    }
    Ok(())
}
