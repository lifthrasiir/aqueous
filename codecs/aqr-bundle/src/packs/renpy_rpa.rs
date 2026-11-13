//! Ren'Py's archives (`.rpa`), versions 3.0 and 2.0. Written from the public descriptions and
//! unverified on real files: no sample was at hand, only archives the tests write.
//!
//! The first line: `RPA-3.0 `, the index's offset in 16 hex digits, a space, the key in hex and
//! a newline; version 2.0 is `RPA-2.0 `, the offset and a newline (no key). The index, from its
//! offset to the end of the file, is a zlib stream of a Python pickle: a dict of the paths to
//! lists (of one entry) of tuples `(offset, length)` or `(offset, length, prefix)`, the offset
//! and the length XORed with the key in 3.0. The pickle is read by a small machine of what
//! Ren'Py writes (protocols 2 to 4: dicts, lists, tuples, strings, bytes, integers, the memo, and
//! the `_codecs.encode(text, 'latin1')` that Python 3's protocol 2 makes of bytes); any other
//! opcode makes the archive unread. Versions 3.2 and the like are not read.
//!
//! A path's data is a plain file of the offset and the length. Where the entry has a prefix, Ren'Py
//! puts it before the file's contents when it reads: the prefix is in the index and not in the
//! archive, so that file is only the part of its contents the archive holds (whether the length
//! counts the prefix is not pinned down; it is taken not to).

use std::collections::HashMap;

use super::{file, u32le, within, zlib_inflate};
use aqr_disk::File;

pub fn starts(d: &[u8]) -> bool {
    d.starts_with(b"RPA-3.0 ") || d.starts_with(b"RPA-2.0 ")
}

#[derive(Clone, Debug)]
enum V {
    Int(i64),
    Str(Vec<u8>),
    Global(Vec<u8>),
    Tuple(Vec<V>),
    List(Vec<V>),
    Dict(Vec<(V, V)>),
    Mark,
}

impl V {
    /// How deep the value may nest: scalars 0, tuples 1, lists 2, dicts 3, each holding only
    /// values of a lower rank (so that nothing nests deeply).
    fn rank(&self) -> u8 {
        match self {
            V::Tuple(_) => 1,
            V::List(_) => 2,
            V::Dict(_) => 3,
            _ => 0,
        }
    }
    fn scalar(&self) -> bool {
        matches!(self, V::Int(_) | V::Str(_) | V::Global(_))
    }
}

fn take<'a>(p: &'a [u8], at: &mut usize, n: usize) -> Option<&'a [u8]> {
    let s = p.get(*at..at.checked_add(n)?)?;
    *at += n;
    Some(s)
}

/// The bytes up to a newline at `at`, and the position after it.
fn line(p: &[u8], at: usize) -> Option<(&[u8], usize)> {
    let n = p.get(at..)?.iter().position(|&b| b == b'\n')?;
    Some((&p[at..at + n], at + n + 1))
}

fn pop_mark(stack: &mut Vec<V>) -> Option<Vec<V>> {
    let m = stack.iter().rposition(|v| matches!(v, V::Mark))?;
    let items = stack.split_off(m + 1);
    stack.pop();
    Some(items)
}

fn unpickle(p: &[u8]) -> Option<V> {
    let mut stack: Vec<V> = Vec::new();
    let mut memo: HashMap<u32, V> = HashMap::new();
    let mut at = 0;
    let mut memoized = 0u32;
    loop {
        let op = *p.get(at)?;
        at += 1;
        match op {
            0x80 => {
                take(p, &mut at, 1)?;
            }
            0x95 => {
                take(p, &mut at, 8)?;
            }
            b'.' => return stack.pop(),
            b'}' => stack.push(V::Dict(Vec::new())),
            b']' => stack.push(V::List(Vec::new())),
            b')' => stack.push(V::Tuple(Vec::new())),
            b'(' => stack.push(V::Mark),
            b'q' | b'r' | 0x94 => {
                let n = match op {
                    b'q' => take(p, &mut at, 1)?[0] as u32,
                    b'r' => u32le(take(p, &mut at, 4)?, 0)?,
                    _ => {
                        memoized += 1;
                        memoized - 1
                    }
                };
                let top = stack.last()?;
                if top.scalar() {
                    memo.insert(n, top.clone());
                }
            }
            b'h' | b'j' => {
                let n = if op == b'h' { take(p, &mut at, 1)?[0] as u32 } else { u32le(take(p, &mut at, 4)?, 0)? };
                stack.push(memo.get(&n)?.clone());
            }
            b'X' | b'T' | b'B' | 0x8c | b'U' | b'C' => {
                let n = if matches!(op, b'X' | b'T' | b'B') {
                    u32le(take(p, &mut at, 4)?, 0)? as usize
                } else {
                    take(p, &mut at, 1)?[0] as usize
                };
                stack.push(V::Str(take(p, &mut at, n)?.to_vec()));
            }
            b'K' => stack.push(V::Int(take(p, &mut at, 1)?[0] as i64)),
            b'M' => stack.push(V::Int(u16::from_le_bytes(take(p, &mut at, 2)?.try_into().ok()?) as i64)),
            b'J' => stack.push(V::Int(i32::from_le_bytes(take(p, &mut at, 4)?.try_into().ok()?) as i64)),
            0x8a => {
                let n = take(p, &mut at, 1)?[0] as usize;
                let s = take(p, &mut at, n)?;
                if n > 8 {
                    return None;
                }
                let mut v = 0i64;
                for (i, &b) in s.iter().enumerate() {
                    v |= (b as i64) << (8 * i);
                }
                if n > 0 && n < 8 && s[n - 1] & 0x80 != 0 {
                    v |= -1i64 << (8 * n);
                }
                stack.push(V::Int(v));
            }
            b'c' => {
                let (m, next) = line(p, at)?;
                let (n, next) = line(p, next)?;
                at = next;
                let mut g = m.to_vec();
                g.push(b'.');
                g.extend_from_slice(n);
                stack.push(V::Global(g));
            }
            b'R' => {
                let V::Tuple(args) = stack.pop()? else { return None };
                let V::Global(f) = stack.pop()? else { return None };
                let [V::Str(s), V::Str(enc)] = &args[..] else { return None };
                if f != b"_codecs.encode" || enc != b"latin1" {
                    return None;
                }
                let mut bytes = Vec::with_capacity(s.len());
                for c in std::str::from_utf8(s).ok()?.chars() {
                    bytes.push(u8::try_from(c as u32).ok()?);
                }
                stack.push(V::Str(bytes));
            }
            0x85..=0x87 | b't' | b'l' => {
                let items = if op == b't' || op == b'l' {
                    pop_mark(&mut stack)?
                } else {
                    let n = (op - 0x84) as usize;
                    stack.split_off(stack.len().checked_sub(n)?)
                };
                let rank = if op == b'l' { 2 } else { 1 };
                if items.iter().any(|v| v.rank() >= rank) {
                    return None;
                }
                stack.push(if op == b'l' { V::List(items) } else { V::Tuple(items) });
            }
            b'a' | b'e' => {
                let items = if op == b'e' { pop_mark(&mut stack)? } else { vec![stack.pop()?] };
                let V::List(l) = stack.last_mut()? else { return None };
                if items.iter().any(|v| v.rank() >= 2) {
                    return None;
                }
                l.extend(items);
            }
            b's' | b'u' => {
                let items = if op == b'u' {
                    pop_mark(&mut stack)?
                } else {
                    let n = stack.len().checked_sub(2)?;
                    stack.split_off(n)
                };
                let V::Dict(m) = stack.last_mut()? else { return None };
                if items.len() % 2 != 0 || items.iter().any(|v| v.rank() >= 3) {
                    return None;
                }
                let mut it = items.into_iter();
                while let (Some(k), Some(v)) = (it.next(), it.next()) {
                    m.push((k, v));
                }
            }
            _ => return None,
        }
    }
}

pub fn files(d: &[u8]) -> Option<Vec<File>> {
    if !starts(d) {
        return None;
    }
    let version3 = d[4] == b'3';
    let first = d.get(8..)?.iter().take(64).position(|&b| b == b'\n')?;
    let first = std::str::from_utf8(&d[8..8 + first]).ok()?;
    let mut parts = first.split(' ');
    let offset = usize::try_from(u64::from_str_radix(parts.next()?, 16).ok()?).ok()?;
    let key = if version3 { u64::from_str_radix(parts.next()?, 16).ok()? as i64 } else { 0 };
    if parts.next().is_some() {
        return None;
    }
    let index = zlib_inflate(d.get(offset..)?, 256 << 20)?;
    let V::Dict(entries) = unpickle(&index)? else { return None };
    let mut out = Vec::new();
    for (k, v) in entries {
        let (V::Str(name), V::List(list)) = (k, v) else { return None };
        let [V::Tuple(t)] = &list[..] else { continue };
        let (V::Int(o), V::Int(l)) = (t.first()?, t.get(1)?) else { return None };
        let (o, l) = (o ^ key, l ^ key);
        if o < 0 || l <= 0 {
            continue;
        }
        let Some(r) = within(d, o as u64, l as u64) else { continue };
        let mut name = name;
        for b in &mut name {
            if *b == b'\\' {
                *b = b'/';
            }
        }
        out.push(file(name, r));
    }
    Some(out)
}
