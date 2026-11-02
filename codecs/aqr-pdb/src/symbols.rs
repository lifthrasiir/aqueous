//! CodeView symbol records of the modules' streams (FORMAT.md §3): each record's fields by its
//! kind's layout to streams of their own, the offsets of a scope's parent and end derived from
//! how the scopes nest, item ids and type indices through caches, code offsets as deltas from
//! the previous of their field.

use aqr_common::varint;

use crate::{Result, corrupt};

/// The streams, and the inputs they are packed into.
pub const STREAM_NAMES: [&str; 15] = [
    "kind", "len", "scope", "item", "type", "code", "name", "ann", "tail", "raw", "annop", "annoff", "annlen",
    "annline", "annother",
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
pub const NUM_STREAMS: usize = STREAM_NAMES.len();
pub const INPUTS: [(&str, &[usize]); 4] = [
    ("head", &[KIND, LEN, SCOPE]),
    ("fields", &[ITEM, TYPE, CODE, TAIL, RAW]),
    ("text", &[NAME, ANN]),
    ("ann", &[ANNOP, ANNOFF, ANNLEN, ANNLINE, ANNOTHER]),
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
    fn new() -> State {
        State { prev: vec![0; SLOTS], item_prev: 0, item_cache: Vec::new(), type_prev: 0, type_cache: Vec::new() }
    }
}

fn u32le(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(*d.get(at..)?.first_chunk()?))
}

fn u16le(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(*d.get(at..)?.first_chunk()?))
}

/// The streams of the symbol substreams `subs` (each a module's, from its signature), or `None`
/// where they are not rebuilt as they are.
pub fn encode(subs: &[&[u8]]) -> Option<Vec<Vec<u8>>> {
    let mut st = vec![Vec::new(); NUM_STREAMS];
    let mut s = State::new();
    for &m in subs {
        module(&mut st, &mut s, m);
    }
    let refs: Vec<&[u8]> = st.iter().map(|v| &v[..]).collect();
    let back = decode(&refs).ok()?;
    let mut at = 0;
    for &m in subs {
        if back.get(at..at + m.len())? != m {
            return None;
        }
        at += m.len();
    }
    (at == back.len()).then_some(st)
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

fn module(st: &mut [Vec<u8>], s: &mut State, m: &[u8]) {
    varint::put(&mut st[LEN], m.len() as u64);
    if m.len() < 4 {
        st[RAW].extend_from_slice(m);
        return;
    }
    st[TAIL].extend_from_slice(&m[..4]);
    let (recs, end) = records(m);
    // the end of each scope opened, by where it opens
    let mut ends = std::collections::HashMap::new();
    let mut stack = Vec::new();
    for &(q, kind, _, _) in &recs {
        match layout(kind).1 {
            Some(true) => stack.push(q),
            Some(false) => {
                if let Some(o) = stack.pop() {
                    ends.insert(o, q as u32);
                }
            }
            None => {}
        }
    }
    let mut stack: Vec<u32> = Vec::new();
    for &(q, kind, p, e) in &recs {
        st[KIND].extend_from_slice(&kind.to_le_bytes());
        let mark = st.iter().map(Vec::len).collect::<Vec<_>>();
        let saved = (s.prev.clone(), s.item_prev, s.item_cache.clone(), s.type_prev, s.type_cache.clone());
        let len = (e - q - 2) as u64;
        varint::put(&mut st[LEN], len << 1);
        let (fields, scope) = layout(kind);
        if record(st, s, m, q, kind, p, e, fields, &stack, &ends).is_none() {
            // kept as it is
            for (v, l) in st.iter_mut().zip(mark) {
                v.truncate(l);
            }
            (s.prev, s.item_prev, s.item_cache, s.type_prev, s.type_cache) = saved;
            varint::put(&mut st[LEN], len << 1 | 1);
            st[RAW].extend_from_slice(&m[p..e]);
        }
        match scope {
            Some(true) => stack.push(q as u32),
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
}

#[allow(clippy::too_many_arguments)]
fn record(
    st: &mut [Vec<u8>],
    s: &mut State,
    m: &[u8],
    q: usize,
    kind: u16,
    p: usize,
    e: usize,
    fields: &[F],
    stack: &[u32],
    ends: &std::collections::HashMap<usize, u32>,
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
                if ends.get(&q) == Some(&v) {
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

/// The symbol substreams, one after another, from the streams of [`STREAM_NAMES`].
pub fn decode(streams: &[&[u8]]) -> Result<Vec<u8>> {
    if streams.len() != NUM_STREAMS {
        return Err(corrupt());
    }
    let mut r: Vec<Reader> = streams.iter().map(|d| Reader { d, at: 0 }).collect();
    let mut s = State::new();
    let mut out = Vec::new();
    while !r[LEN].done() {
        let size = usize::try_from(r[LEN].get()?).map_err(|_| corrupt())?;
        let start = out.len();
        if size < 4 {
            out.extend_from_slice(r[RAW].bytes(size)?);
            continue;
        }
        out.extend_from_slice(r[TAIL].bytes(4)?);
        // the scopes open: where each opened (in the module), and where its end goes
        let mut stack: Vec<(u32, Option<usize>)> = Vec::new();
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
            let mut end_at = None;
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
                                    end_at = Some(out.len());
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
                            put_cint(&mut out, v).ok_or_else(corrupt)?;
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
                Some(true) => stack.push((q, end_at)),
                Some(false) => {
                    if let Some((_, Some(at))) = stack.pop() {
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
        if stack.iter().any(|s| s.1.is_some()) {
            return Err(corrupt());
        }
        let n = usize::try_from(r[LEN].get()?).map_err(|_| corrupt())?;
        out.extend_from_slice(r[RAW].bytes(n)?);
        if out.len() - start != size {
            return Err(corrupt());
        }
    }
    if !r.iter().all(Reader::done) {
        return Err(corrupt());
    }
    Ok(out)
}
