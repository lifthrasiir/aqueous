//! PyTorch's files of `torch.save` (FORMAT.md §2), of the ZIP format (since PyTorch 1.6): a ZIP
//! archive of stored entries, `PREFIX/data.pkl` first, the pickle of the object saved, whose
//! tensors' storages are persistent ids `('storage', torch.XStorage, KEY, LOCATION, NUMEL)`, and
//! each storage the entry `PREFIX/data/KEY`, its NUMEL elements little-endian (unless the entry
//! `PREFIX/byteorder` says otherwise). Only the persistent ids are read of the pickle, run by a
//! machine of the opcodes of protocols 2 to 5 that keeps the strings, integers, globals and tuples
//! (to read the ids, and the memo they are got from again) and nothing else.

use std::collections::HashMap;
use std::rc::Rc;

use crate::layout::Float;
use crate::tensors::{self, Container, Data, Tensors};

/// Whether `d` starts as a file of `torch.save`: a ZIP entry stored, `…/data.pkl`.
pub fn starts(d: &[u8]) -> bool {
    let Some(h) = d.get(..30) else { return false };
    let name_len = u16::from_le_bytes([h[26], h[27]]) as usize;
    h[..4] == *b"PK\x03\x04"
        && u16::from_le_bytes([h[8], h[9]]) == 0
        && d.get(30..30 + name_len).is_some_and(|n| n == b"data.pkl" || n.ends_with(b"/data.pkl"))
}

/// An entry of the archive: its name, and where its data is (stored).
struct Entry<'a> {
    name: &'a [u8],
    start: usize,
    len: usize,
}

fn u16_at(d: &[u8], at: usize) -> Option<u64> {
    Some(u16::from_le_bytes(d.get(at..at + 2)?.try_into().unwrap()) as u64)
}

fn u32_at(d: &[u8], at: usize) -> Option<u64> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().unwrap()) as u64)
}

fn u64_at(d: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(d.get(at..at + 8)?.try_into().unwrap()))
}

/// The entries of ZIP archive `d` by its central directory (ZIP64's too), those stored only (a
/// compressed one of the same sizes taken for stored: not a storage of `torch.save`'s).
fn entries(d: &[u8]) -> Option<Vec<Entry<'_>>> {
    // (the end of central directory record, its comment at most 64 KiB)
    let from = d.len().saturating_sub(22 + 0xffff);
    let eocd = (from..d.len().checked_sub(22)? + 1).rev().find(|&p| d[p..p + 4] == *b"PK\x05\x06")?;
    let (mut count, mut cd) = (u16_at(d, eocd + 10)?, u32_at(d, eocd + 16)?);
    if count == 0xffff || cd == 0xffff_ffff {
        // (ZIP64: its locator just before, telling where its end record is)
        let loc = eocd.checked_sub(20)?;
        if d[loc..loc + 4] != *b"PK\x06\x07" {
            return None;
        }
        let rec = usize::try_from(u64_at(d, loc + 8)?).ok().filter(|&r| r <= d.len())?;
        if d.get(rec..rec + 4)? != b"PK\x06\x06" {
            return None;
        }
        (count, cd) = (u64_at(d, rec + 32)?, u64_at(d, rec + 48)?);
    }
    let mut at = usize::try_from(cd).ok().filter(|&a| a <= d.len())?;
    // (an entry of the central directory is at least 46 bytes)
    if count > (d.len().saturating_sub(at) / 46) as u64 {
        return None;
    }
    let mut out = Vec::with_capacity(count as usize);
    for _ in 0..count {
        if d.get(at..at + 4)? != b"PK\x01\x02" {
            return None;
        }
        let method = u16_at(d, at + 10)?;
        let (mut csize, mut size) = (u32_at(d, at + 20)?, u32_at(d, at + 24)?);
        let (nl, xl, cl) = (u16_at(d, at + 28)? as usize, u16_at(d, at + 30)? as usize, u16_at(d, at + 32)? as usize);
        let mut local = u32_at(d, at + 42)?;
        let name = d.get(at + 46..at + 46 + nl)?;
        // (ZIP64's sizes and place, those of the fields full, in their order)
        let mut x = at + 46 + nl;
        let x_end = x + xl;
        while x + 4 <= x_end {
            let (id, len) = (u16_at(d, x)?, u16_at(d, x + 2)? as usize);
            if id == 1 {
                let mut f = x + 4;
                for v in [&mut size, &mut csize, &mut local] {
                    if *v == 0xffff_ffff && f + 8 <= x + 4 + len {
                        *v = u64_at(d, f)?;
                        f += 8;
                    }
                }
            }
            x += 4 + len;
        }
        at = x_end + cl;
        let Some(local) = usize::try_from(local).ok().filter(|&l| l <= d.len()) else { continue };
        if method != 0 || csize != size || d.get(local..local + 4) != Some(b"PK\x03\x04") {
            continue;
        }
        let start = local + 30 + u16_at(d, local + 26)? as usize + u16_at(d, local + 28)? as usize;
        let len = usize::try_from(size).ok()?;
        if start.checked_add(len)? > d.len() {
            continue;
        }
        out.push(Entry { name, start, len });
    }
    Some(out)
}

/// A value of the pickle, as far as it is kept.
#[derive(Clone, Debug)]
enum V<'a> {
    Str(&'a [u8]),
    Int(i64),
    Global(&'a [u8], &'a [u8]),
    Tuple(Rc<[V<'a>]>),
    Other,
}

/// Tuples nested deeper than this are not kept (a persistent id's are two deep at most): dropping
/// a pickle's of any depth would recurse as deep.
const TUPLE_DEPTH: usize = 4;

impl<'a> V<'a> {
    fn depth(&self) -> usize {
        match self {
            V::Tuple(t) => 1 + t.iter().map(V::depth).max().unwrap_or(0),
            _ => 0,
        }
    }

    /// The tuple of `t`, or `Other` where it nests too deep.
    fn tuple(t: Rc<[V<'a>]>) -> V<'a> {
        if t.iter().any(|v| v.depth() >= TUPLE_DEPTH) { V::Other } else { V::Tuple(t) }
    }
}

/// A storage that a pickle names: its key, its type's name (`FloatStorage`, say) and its elements.
type Storage<'a> = (&'a [u8], &'a [u8], u64);

/// The storages that pickle `p` names. None where it has an opcode not known, or does not hold.
fn storages(p: &[u8]) -> Option<Vec<Storage<'_>>> {
    let mut stack: Vec<V> = Vec::new();
    let mut marks: Vec<usize> = Vec::new();
    let mut memo: HashMap<u64, V> = HashMap::new();
    let mut out = Vec::new();
    let mut at = 0usize;
    let take = |at: &mut usize, n: usize| -> Option<&[u8]> {
        let s = p.get(*at..at.checked_add(n)?)?;
        *at += n;
        Some(s)
    };
    let uint = |b: &[u8]| b.iter().rev().fold(0u64, |a, &x| a << 8 | x as u64);
    let line = |at: &mut usize| -> Option<&[u8]> {
        let n = p.get(*at..)?.iter().position(|&b| b == b'\n')?;
        let s = &p[*at..*at + n];
        *at += n + 1;
        Some(s)
    };
    // (the values from the last mark on, the mark taken off)
    let to_mark = |stack: &mut Vec<V<'_>>, marks: &mut Vec<usize>| -> Option<usize> {
        let m = marks.pop()?;
        (m <= stack.len()).then_some(m)
    };
    loop {
        let op = *take(&mut at, 1)?.first()?;
        match op {
            // PROTO, FRAME
            0x80 => _ = take(&mut at, 1)?,
            0x95 => _ = take(&mut at, 8)?,
            // STOP
            b'.' => return Some(out),
            // MARK
            b'(' => marks.push(stack.len()),
            // EMPTY_DICT, EMPTY_LIST, EMPTY_SET, NONE, NEWTRUE, NEWFALSE
            b'}' | b']' | 0x8f | b'N' | 0x88 | 0x89 => stack.push(V::Other),
            // EMPTY_TUPLE
            b')' => stack.push(V::Tuple(Rc::from([]))),
            // BINPUT, LONG_BINPUT, MEMOIZE
            b'q' | b'r' | 0x94 => {
                let i = match op {
                    b'q' => uint(take(&mut at, 1)?),
                    b'r' => uint(take(&mut at, 4)?),
                    _ => memo.len() as u64,
                };
                memo.insert(i, stack.last()?.clone());
            }
            // BINGET, LONG_BINGET
            b'h' | b'j' => {
                let i = uint(take(&mut at, if op == b'h' { 1 } else { 4 })?);
                stack.push(memo.get(&i)?.clone());
            }
            // TUPLE, TUPLE1, TUPLE2, TUPLE3
            b't' => {
                let m = to_mark(&mut stack, &mut marks)?;
                let t: Rc<[V]> = stack.drain(m..).collect();
                stack.push(V::tuple(t));
            }
            0x85..=0x87 => {
                let n = (op - 0x84) as usize;
                let m = stack.len().checked_sub(n)?;
                let t: Rc<[V]> = stack.drain(m..).collect();
                stack.push(V::tuple(t));
            }
            // BINUNICODE, SHORT_BINUNICODE, BINUNICODE8, and the bytes' and strings' alike
            b'X' | 0x8c | 0x8d | b'T' | b'U' | b'B' | b'C' | 0x8e | 0x96 => {
                let w = match op {
                    0x8c | b'U' | b'C' => 1,
                    0x8d | 0x8e | 0x96 => 8,
                    _ => 4,
                };
                let n = usize::try_from(uint(take(&mut at, w)?)).ok()?;
                let s = take(&mut at, n)?;
                stack.push(if matches!(op, b'X' | 0x8c | 0x8d) { V::Str(s) } else { V::Other });
            }
            // BININT1, BININT2, BININT, LONG1, LONG4
            b'K' => stack.push(V::Int(take(&mut at, 1)?[0] as i64)),
            b'M' => stack.push(V::Int(uint(take(&mut at, 2)?) as i64)),
            b'J' => stack.push(V::Int(i32::from_le_bytes(take(&mut at, 4)?.try_into().unwrap()) as i64)),
            0x8a | 0x8b => {
                let n = usize::try_from(uint(take(&mut at, if op == 0x8a { 1 } else { 4 })?)).ok()?;
                let b = take(&mut at, n)?;
                // (of at most 8 bytes, as an i64; longer ones not kept)
                stack.push(match n {
                    0 => V::Int(0),
                    1..=8 => {
                        let v = uint(b);
                        let sh = 64 - 8 * n as u32;
                        V::Int(((v << sh) as i64) >> sh)
                    }
                    _ => V::Other,
                });
            }
            // BINFLOAT
            b'G' => {
                take(&mut at, 8)?;
                stack.push(V::Other);
            }
            // GLOBAL, STACK_GLOBAL
            b'c' => {
                let (m, n) = (line(&mut at)?, line(&mut at)?);
                stack.push(V::Global(m, n));
            }
            0x93 => {
                let n = stack.pop()?;
                let m = stack.pop()?;
                stack.push(match (m, n) {
                    (V::Str(m), V::Str(n)) => V::Global(m, n),
                    _ => V::Other,
                });
            }
            // REDUCE, NEWOBJ, NEWOBJ_EX, SETITEM: two or three taken, one put
            b'R' | 0x81 => {
                stack.truncate(stack.len().checked_sub(2)?);
                stack.push(V::Other);
            }
            0x92 => {
                stack.truncate(stack.len().checked_sub(3)?);
                stack.push(V::Other);
            }
            b's' => stack.truncate(stack.len().checked_sub(2)?),
            // BUILD, APPEND, POP
            b'b' | b'a' | b'0' => _ = stack.pop()?,
            // SETITEMS, APPENDS, ADDITEMS, POP_MARK
            b'u' | b'e' | 0x90 | b'1' => {
                let m = to_mark(&mut stack, &mut marks)?;
                stack.truncate(m);
            }
            // DICT, LIST, FROZENSET
            b'd' | b'l' | 0x91 => {
                let m = to_mark(&mut stack, &mut marks)?;
                stack.truncate(m);
                stack.push(V::Other);
            }
            // DUP
            b'2' => stack.push(stack.last()?.clone()),
            // BINPERSID
            b'Q' => {
                if let V::Tuple(t) = stack.pop()?
                    && let [V::Str(b"storage"), V::Global(b"torch", ty), V::Str(key), _, V::Int(n)] = &t[..]
                    && *n >= 0
                {
                    out.push((*key, *ty, *n as u64));
                }
                stack.push(V::Other);
            }
            _ => return None,
        }
    }
}

/// What a storage of a type of torch's (by its name) of `n` elements is, and the bytes of an
/// element.
fn data_of(ty: &[u8], n: u64) -> Option<(Data, u64)> {
    let float = |f: Float, k: u64| Some((Data::Float(f, n.checked_mul(k)?), k * f.bytes() as u64));
    let lanes = |w: u8| Some((Data::Lanes(w, n), w as u64));
    match ty {
        b"FloatStorage" => float(Float::F32, 1),
        b"HalfStorage" => float(Float::F16, 1),
        b"BFloat16Storage" => float(Float::Bf16, 1),
        b"DoubleStorage" => float(Float::F64, 1),
        b"ComplexFloatStorage" => float(Float::F32, 2),
        b"ComplexDoubleStorage" => float(Float::F64, 2),
        b"Float8_e4m3fnStorage" | b"Float8_e4m3fnuzStorage" => float(Float::E4m3, 1),
        b"Float8_e5m2Storage" | b"Float8_e5m2fnuzStorage" => float(Float::E5m2, 1),
        b"LongStorage" | b"UInt64Storage" => lanes(8),
        b"IntStorage" | b"UInt32Storage" => lanes(4),
        b"ShortStorage" | b"UInt16Storage" => lanes(2),
        b"CharStorage" | b"ByteStorage" | b"BoolStorage" => lanes(1),
        _ => None,
    }
}

/// The layout of file `d` of `torch.save`, None where it is not one (or its pickle does not
/// hold): its header the bytes before the first storage coded, the storages of floats coded
/// those of as many bytes as their elements (each once, where several tensors share it).
pub fn parse(d: &[u8]) -> Option<Tensors> {
    if !starts(d) {
        return None;
    }
    let entries = entries(d)?;
    let pkl = entries.iter().find(|e| e.name == b"data.pkl" || e.name.ends_with(b"/data.pkl"))?;
    let prefix = &pkl.name[..pkl.name.len() - b"data.pkl".len()];
    let entry = |rest: &[u8]| entries.iter().find(|e| e.name.strip_prefix(prefix) == Some(rest));
    if let Some(bo) = entry(b"byteorder")
        && d[bo.start..bo.start + bo.len].trim_ascii() != b"little"
    {
        return None;
    }
    let by_name: HashMap<&[u8], &Entry> =
        entries.iter().filter_map(|e| Some((e.name.strip_prefix(prefix)?.strip_prefix(b"data/")?, e))).collect();
    let mut seen = std::collections::HashSet::new();
    let mut coded = Vec::new();
    for (key, ty, n) in storages(&d[pkl.start..pkl.start + pkl.len])? {
        let (Some((what, elem)), Some(e)) = (data_of(ty, n), by_name.get(key)) else { continue };
        if n.checked_mul(elem) != Some(e.len as u64) || !seen.insert(key) {
            continue;
        }
        coded.push((e.start as u64, what, 0));
    }
    let data = coded.iter().map(|c| c.0 as usize).min()?;
    Some(tensors::apart(Container::Pth, d, data, coded))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pickle as `torch.save` writes it (protocol 2): the ids of two storages, the second's
    /// strings and type got from the memo.
    #[test]
    fn nested_tuples() {
        // (a hostile pickle of tuples in tuples must not overflow the stack when dropped)
        let mut p = vec![0x80, 2, b')'];
        p.extend(std::iter::repeat_n(0x85, 1 << 20));
        p.push(b'.');
        let t = std::thread::Builder::new().stack_size(1 << 20).spawn(move || storages(&p).is_some());
        assert!(t.unwrap().join().unwrap());
    }

    #[test]
    fn ids() {
        let mut p = vec![0x80, 2, b'}', b'q', 0, b'('];
        let s = |p: &mut Vec<u8>, s: &str| {
            p.push(b'X');
            p.extend_from_slice(&(s.len() as u32).to_le_bytes());
            p.extend_from_slice(s.as_bytes());
        };
        s(&mut p, "w");
        p.extend_from_slice(b"ctorch._utils\n_rebuild_tensor_v2\nq\x01((");
        s(&mut p, "storage");
        p.extend_from_slice(b"q\x02ctorch\nFloatStorage\nq\x03");
        s(&mut p, "0");
        s(&mut p, "cpu");
        p.extend_from_slice(b"q\x04J\x00\x00\x01\x00tq\x05QK\x00(M\x00\x01t(K\x01t\x89");
        p.extend_from_slice(b"ccollections\nOrderedDict\nq\x06)RtR");
        s(&mut p, "b");
        p.extend_from_slice(b"h\x01((h\x02h\x03");
        s(&mut p, "1");
        p.extend_from_slice(b"h\x04\x8a\x02\x00\x02tQK\x00)\x89h\x06)RtRu.");
        let got = storages(&p).unwrap();
        assert_eq!(got, vec![(&b"0"[..], &b"FloatStorage"[..], 65536), (&b"1"[..], &b"FloatStorage"[..], 512)]);
        // (an opcode not known, or a pickle cut: none)
        assert!(storages(&p[..p.len() - 1]).is_none());
        let mut bad = p.clone();
        bad[2] = 0xff;
        assert!(storages(&bad).is_none());
    }
}
