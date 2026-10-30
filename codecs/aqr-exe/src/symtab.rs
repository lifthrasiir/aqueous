//! The symbol table of a 64-bit Mach-O image (`LC_SYMTAB`'s `nlist_64` entries) and its string
//! table, as the columns of the entries' fields: the values predicted from the stabs of the
//! debug map and from each other, the names as references to the strings, and the strings each
//! once, front coded, in the order of the string table, which is that of their first use
//! (`exe-recons/macho-symtab`, FORMAT.md §S).

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;

use crate::util::to_u32;
#[cfg(feature = "encode")]
use crate::util::to_u64;

pub const NUM_STREAMS: usize = 9;

/// The streams of the module: the entries' types, sections and descriptions as they are, their
/// values and the sizes of the functions of the debug map (varints of the differences from
/// their predictions), the names (a byte each, and the varints of the back references), and the
/// strings (the prefixes' lengths and the suffixes).
pub const STREAM_NAMES: [&str; NUM_STREAMS] =
    ["TYPE", "SECT", "DESC", "VALUE", "SIZE", "NAME", "BACK", "PREFIX", "SUFFIX"];

/// Bytes of an `nlist_64` entry.
const ENTRY: usize = 16;

/// The longest head of the string table: names pointing into it are codes of their own.
pub const MAX_HEAD: usize = 252;

const N_STAB: u8 = 0xe0;
const N_TYPE: u8 = 0x0e;
const N_SECT: u8 = 0x0e;
const N_BNSYM: u8 = 0x2e;
const N_FUN: u8 = 0x24;
const N_ENSYM: u8 = 0x4e;

/// The codes of `NAME`: the next string, the name of the last entry of the same value, a back
/// reference in `BACK`, and from `HEAD`, a position in the head.
const NEW: u8 = 0;
const SAME_VALUE: u8 = 1;
const BACK: u8 = 2;
const HEAD: u8 = 3;

/// What the entries before tell of the next one's value.
struct Model {
    /// The last value of each type (of every type with `N_SECT`, as one).
    last: [u64; 256],
    /// The address of the last `N_BNSYM` and the size of the last function (`N_FUN` of no
    /// section).
    begin: u64,
    size: u64,
    /// The values of the entries of a section that are no stabs.
    addrs: BTreeSet<u64>,
    /// The string of the last entry of each value that has one past the head.
    by_value: BTreeMap<u64, u32>,
}

/// The prediction of a value: of `VALUE`, or of `SIZE` for the size of a function.
enum Pred {
    Value(u64),
    Size(u64),
}

impl Model {
    fn new() -> Model {
        Model { last: [0; 256], begin: 0, size: 0, addrs: BTreeSet::new(), by_value: BTreeMap::new() }
    }

    fn key(t: u8) -> usize {
        (if t & N_STAB == 0 && t & N_TYPE == N_SECT { N_SECT } else { t }) as usize
    }

    fn predict(&self, t: u8, sect: u8) -> Pred {
        match t {
            N_BNSYM => Pred::Value(self.begin.wrapping_add(self.size)),
            N_FUN if sect == 0 => {
                Pred::Size(self.addrs.range(self.begin.saturating_add(1)..).next().map_or(0, |a| a - self.begin))
            }
            N_FUN | N_ENSYM => Pred::Value(self.begin),
            _ => Pred::Value(self.last[Model::key(t)]),
        }
    }

    fn update(&mut self, t: u8, sect: u8, v: u64) {
        match t {
            N_BNSYM => self.begin = v,
            N_FUN if sect == 0 => self.size = v,
            _ if t & N_STAB == 0 && t & N_TYPE == N_SECT => {
                self.addrs.insert(v);
            }
            _ => {}
        }
        self.last[Model::key(t)] = v;
    }
}

/// The symbol table `symtab` and its string table `strtab`, the entries from `ext` on (the
/// external ones) first, as the parameters and the streams of [`STREAM_NAMES`]; `None` where
/// they are not as [`decode`] makes them.
#[cfg(feature = "encode")]
pub fn encode(symtab: &[u8], strtab: &[u8], ext: u32) -> Option<(Vec<u8>, [Vec<u8>; NUM_STREAMS])> {
    let (entries, []) = symtab.as_chunks::<ENTRY>() else { return None };
    let n = u32::try_from(entries.len()).ok()?;
    if ext > n || u32::try_from(strtab.len()).is_err() {
        return None;
    }
    let strx = |e: &[u8; ENTRY]| to_u32(&e[..4]) as usize;
    // the head: up to the first string that is not empty
    let head = entries.iter().map(strx).filter(|&x| strtab.get(x).is_some_and(|&c| c != 0)).min();
    let head = head.unwrap_or(strtab.len());
    if head > MAX_HEAD {
        return None;
    }
    let mut st: [Vec<u8>; NUM_STREAMS] = Default::default();
    let mut m = Model::new();
    let (mut ids, mut next, mut prev) = (BTreeMap::new(), head, &[][..]);
    for i in (ext..n).chain(0..ext) {
        let e = &entries[i as usize];
        let (x, t, sect, v) = (strx(e), e[4], e[5], to_u64(&e[8..]));
        st[0].push(t);
        st[1].push(sect);
        st[2].extend_from_slice(&e[6..8]);
        match m.predict(t, sect) {
            Pred::Value(p) => put_varint(&mut st[3], zigzag(v.wrapping_sub(p))),
            Pred::Size(p) => put_varint(&mut st[4], zigzag(v.wrapping_sub(p))),
        }
        if x < head {
            st[5].push(HEAD + x as u8);
        } else {
            let id = match ids.get(&x) {
                Some(&id) if m.by_value.get(&v) == Some(&id) => {
                    st[5].push(SAME_VALUE);
                    id
                }
                Some(&id) => {
                    st[5].push(BACK);
                    put_varint(&mut st[6], (ids.len() - 1 - id as usize) as u64);
                    id
                }
                None if x == next => {
                    let s = &strtab[x..x + strtab[x..].iter().position(|&c| c == 0)?];
                    let common = s.iter().zip(prev).take(255).take_while(|(a, b)| a == b).count();
                    st[5].push(NEW);
                    st[7].push(common as u8);
                    st[8].extend_from_slice(&s[common..]);
                    st[8].push(0);
                    (next, prev) = (x + s.len() + 1, s);
                    let id = ids.len() as u32;
                    ids.insert(x, id);
                    id
                }
                None => return None,
            };
            m.by_value.insert(v, id);
        }
        m.update(t, sect, v);
    }
    let mut params = Vec::with_capacity(8 + head);
    params.extend_from_slice(&n.to_le_bytes());
    params.extend_from_slice(&ext.to_le_bytes());
    params.extend_from_slice(&strtab[..head]);
    let len = symtab.len() + strtab.len();
    let back = decode(&params, core::array::from_fn(|k| &st[k][..]), len)?;
    (back[..symtab.len()] == *symtab && back[symtab.len()..] == *strtab).then_some((params, st))
}

/// The symbol table and its string table after it, `len` bytes, from the parameters `params`
/// and the streams of [`STREAM_NAMES`]; `None` where they are not valid (FORMAT.md §S4).
pub fn decode(params: &[u8], streams: [&[u8]; NUM_STREAMS], len: usize) -> Option<Vec<u8>> {
    let (Some(p), head) = (params.first_chunk::<8>(), params.get(8..)?) else { return None };
    let (n, ext) = (to_u32(&p[..4]), to_u32(&p[4..]));
    let [types, sects, descs, mut values, mut sizes, names, mut backs, prefixes, mut suffixes] = streams;
    let symtab = (n as usize).checked_mul(ENTRY)?;
    let fits = |at: usize| at <= len && at - symtab <= u32::MAX as usize;
    if ext > n || head.len() > MAX_HEAD || !fits(symtab.checked_add(head.len())?) {
        return None;
    }
    let n = n as usize;
    if types.len() != n || sects.len() != n || descs.len() != 2 * n || names.len() != n {
        return None;
    }
    let mut out = Vec::with_capacity(len);
    out.resize(symtab, 0);
    out.extend_from_slice(head);
    let mut m = Model::new();
    let (mut offsets, mut prev, mut prefixes) = (Vec::<u32>::new(), symtab..symtab, prefixes.iter());
    for k in 0..n {
        let i = (ext as usize + k) % n;
        let (t, sect) = (types[k], sects[k]);
        let v = match m.predict(t, sect) {
            Pred::Value(p) => p.wrapping_add(unzigzag(get_varint(&mut values)?)),
            Pred::Size(p) => p.wrapping_add(unzigzag(get_varint(&mut sizes)?)),
        };
        let id = match names[k] {
            NEW => {
                let common = *prefixes.next()? as usize;
                let end = suffixes.iter().position(|&c| c == 0)? + 1;
                if common > prev.len() || !fits(out.len() + common + end) {
                    return None;
                }
                let start = out.len();
                out.extend_from_within(prev.start..prev.start + common);
                out.extend_from_slice(&suffixes[..end]);
                suffixes = &suffixes[end..];
                prev = start..out.len() - 1;
                offsets.push((start - symtab) as u32);
                Some(offsets.len() - 1)
            }
            SAME_VALUE => Some(*m.by_value.get(&v)? as usize),
            BACK => Some(offsets.len().checked_sub(1)?.checked_sub(usize::try_from(get_varint(&mut backs)?).ok()?)?),
            c => {
                let x = (c - HEAD) as usize;
                if x >= head.len() {
                    return None;
                }
                None
            }
        };
        let x = match id {
            Some(id) => {
                m.by_value.insert(v, id as u32);
                offsets[id]
            }
            None => (names[k] - HEAD) as u32,
        };
        let e = &mut out[ENTRY * i..ENTRY * (i + 1)];
        e[..4].copy_from_slice(&x.to_le_bytes());
        e[4] = t;
        e[5] = sect;
        e[6..8].copy_from_slice(&descs[2 * k..2 * k + 2]);
        e[8..].copy_from_slice(&v.to_le_bytes());
        m.update(t, sect, v);
    }
    if !values.is_empty() || !sizes.is_empty() || !backs.is_empty() || prefixes.len() != 0 || !suffixes.is_empty() {
        return None;
    }
    out.resize(len, 0);
    Some(out)
}

#[cfg(feature = "encode")]
fn zigzag(v: u64) -> u64 {
    (v << 1) ^ ((v as i64 >> 63) as u64)
}

fn unzigzag(v: u64) -> u64 {
    (v >> 1) ^ (v & 1).wrapping_neg()
}

#[cfg(feature = "encode")]
fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// A varint of at most 10 bytes, below 2^64, taken off the front of `d`.
fn get_varint(d: &mut &[u8]) -> Option<u64> {
    let mut v = 0u64;
    for (i, &b) in d.iter().take(10).enumerate() {
        let bits = (b & 0x7f) as u64;
        if i == 9 && bits > 1 {
            return None;
        }
        v |= bits << (7 * i);
        if b < 0x80 {
            *d = &d[i + 1..];
            return Some(v);
        }
    }
    None
}

#[cfg(all(test, feature = "encode"))]
mod tests {
    use super::*;
    use alloc::vec;

    /// A symbol table of `(name, type, section, desc, value)` and its string table: `" \0"`, then
    /// the names in their first use, the entries from `ext` first, padded to 8 bytes. An empty
    /// name is at 1.
    fn tables(entries: &[(&str, u8, u8, u16, u64)], ext: usize) -> (Vec<u8>, Vec<u8>) {
        let mut strtab = b" \0".to_vec();
        let mut at = BTreeMap::new();
        let mut strx = vec![1u32; entries.len()];
        for i in (ext..entries.len()).chain(0..ext) {
            let name = entries[i].0;
            if !name.is_empty() {
                strx[i] = *at.entry(name).or_insert_with(|| {
                    let x = strtab.len() as u32;
                    strtab.extend_from_slice(name.as_bytes());
                    strtab.push(0);
                    x
                });
            }
        }
        strtab.resize(strtab.len().next_multiple_of(8), 0);
        let mut symtab = Vec::new();
        for (&(_, t, sect, desc, v), x) in entries.iter().zip(strx) {
            symtab.extend_from_slice(&x.to_le_bytes());
            symtab.extend_from_slice(&[t, sect]);
            symtab.extend_from_slice(&desc.to_le_bytes());
            symtab.extend_from_slice(&v.to_le_bytes());
        }
        (symtab, strtab)
    }

    /// Locals with the stabs of their functions, then the external ones.
    const SYMBOLS: &[(&str, u8, u8, u16, u64)] = &[
        ("_local_a", 0x0e, 1, 0, 0x1000),
        ("_local_b", 0x0e, 1, 0, 0x1040),
        ("/src/", 0x64, 0, 0, 0),
        ("a.c", 0x64, 0, 0, 0),
        ("", 0x2e, 1, 0, 0x1000),
        ("_local_a", 0x24, 1, 0, 0x1000),
        ("", 0x24, 0, 0, 0x40),
        ("", 0x4e, 1, 0, 0x1000),
        ("", 0x2e, 1, 0, 0x1040),
        ("_local_b", 0x24, 1, 0, 0x1040),
        ("", 0x24, 0, 0, 0x30),
        ("", 0x4e, 1, 0, 0x1040),
        ("", 0x2e, 1, 0, 0x1070),
        ("_main", 0x24, 1, 0, 0x1070),
        ("", 0x24, 0, 0, 0x90),
        ("", 0x4e, 1, 0, 0x1070),
        ("_global", 0x20, 0, 0, 0),
        ("", 0x64, 1, 0, 0),
        ("_global", 0x0f, 2, 0, 0x2000),
        ("_main", 0x0f, 1, 0, 0x1070),
        ("_printf", 0x01, 0, 0x100, 0),
    ];

    #[test]
    fn round_trip() {
        let (symtab, strtab) = tables(SYMBOLS, 18);
        let (params, st) = encode(&symtab, &strtab, 18).unwrap();
        assert_eq!(params[8..], *b" \0");
        // the sizes predicted by the addresses after, but the last (followed by _global's)
        assert_eq!(st[4][..2], [0, 0]);
        assert_eq!(st[5][..3], [NEW, NEW, NEW]);
        assert_eq!(st[5][7..11], [HEAD + 1, SAME_VALUE, HEAD + 1, HEAD + 1]);
        assert_eq!(st[6], [6]); // _global of N_GSYM, of value 0
        let mut whole = symtab.clone();
        whole.extend_from_slice(&strtab);
        let streams = core::array::from_fn(|k| &st[k][..]);
        assert_eq!(decode(&params, streams, whole.len()), Some(whole));
        // no externals told apart: their strings are not first then
        assert_eq!(encode(&symtab, &strtab, 0), None);
        assert!(encode(&[], b"\0\0\0\0", 0).is_some());
    }

    #[test]
    fn not_canonical() {
        let (symtab, mut strtab) = tables(SYMBOLS, 18);
        // a string no entry names
        let mut extra = strtab.clone();
        let end = extra.len();
        extra[end - 2] = b'x';
        assert_eq!(encode(&symtab, &extra, 18), None);
        // a name in the middle of a string
        let mut mid = symtab.clone();
        mid[0] += 1;
        assert_eq!(encode(&mid, &strtab, 18), None);
        // a string not ended
        strtab.truncate(strtab.iter().rposition(|&c| c != 0).unwrap() + 1);
        assert_eq!(encode(&symtab, &strtab, 18), None);
        assert_eq!(encode(&symtab[1..], &strtab, 18), None);
    }

    #[test]
    fn invalid() {
        let (symtab, strtab) = tables(SYMBOLS, 18);
        let (params, st) = encode(&symtab, &strtab, 18).unwrap();
        let len = symtab.len() + strtab.len();
        let streams = || -> [&[u8]; NUM_STREAMS] { core::array::from_fn(|k| &st[k][..]) };
        assert_eq!(decode(&params, streams(), len - 5), None);
        assert_eq!(decode(&params[..7], streams(), len), None);
        let mut s = streams();
        s[5] = &[HEAD + 2; 21];
        assert_eq!(decode(&params, s, len), None);
        let mut s = streams();
        let longer = [&st[3][..], &[0]].concat();
        s[3] = &longer;
        assert_eq!(decode(&params, s, len), None);
        assert_eq!(get_varint(&mut &[0xff; 9].iter().chain(&[2]).copied().collect::<Vec<_>>()[..]), None);
        assert_eq!(get_varint(&mut &[0xff; 9].iter().chain(&[1]).copied().collect::<Vec<_>>()[..]), Some(u64::MAX));
    }
}
