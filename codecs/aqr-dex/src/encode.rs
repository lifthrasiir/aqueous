//! The streams of a DEX file (FORMAT.md §5, *encoder*): each section the module can take is
//! taken where it is laid out as the module rebuilds it, else kept as it is; the streams are
//! decoded again before they are given.

use std::collections::HashMap;

use aqr_common::varint::{self, zz};
use sha1::{Digest, Sha1};

use crate::dex::{self, HEADER, Item, le32, mode, s, ty};

/// Whether `d` starts like a DEX file: the magic, a version of three digits.
pub fn starts(d: &[u8]) -> bool {
    d.len() >= HEADER && d.starts_with(b"dex\n") && d[4..7].iter().all(u8::is_ascii_digit) && d[7] == 0
}

/// The streams of DEX file `d` ([`dex::STREAM_NAMES`]), or `None` where it is not one the
/// module rebuilds.
pub fn split(d: &[u8]) -> Option<[Vec<u8>; s::N]> {
    if !starts(d)
        // (version 41 holds several files in a container)
        || &d[4..7] >= b"041"
        || le32(d, 0x20).ok()? as usize != d.len()
        || le32(d, 0x24).ok()? as usize != HEADER
        || le32(d, 0x28).ok()? != 0x12345678
    {
        return None;
    }
    [true, false].into_iter().find_map(|take| {
        let st = streams(d, take)?;
        let refs: [&[u8]; s::N] = std::array::from_fn(|i| st[i].as_slice());
        (dex::decode(&refs, d.len()).ok()? == d).then_some(st)
    })
}

/// The streams of `d`, with the sections taken that can be where `take`, else none.
fn streams(d: &[u8], take: bool) -> Option<[Vec<u8>; s::N]> {
    let len = d.len();
    let map_off = le32(d, 0x34).ok()? as usize;
    let count = le32(d, map_off).ok()? as usize;
    let map = d.get(map_off + 4..)?.get(..count.checked_mul(12)?)?;
    let items = dex::items(map);
    let mut st: [Vec<u8>; s::N] = std::array::from_fn(|_| Vec::new());

    // What is taken: an item of each type that may be, the others kept as they are.
    let mut modes: Vec<u8> = items
        .iter()
        .map(|it| match it.ty {
            ty::HEADER | ty::MAP_LIST => mode::REST,
            t if take && dex::takes(t) && items.iter().filter(|e| e.ty == t).count() == 1 => mode::TAKEN,
            _ => mode::RAW,
        })
        .collect();
    let find = |t: u16| items.iter().position(|e| e.ty == t);
    let at = |modes: &[u8], t: u16, m: u8| find(t).filter(|&i| modes[i] == m).map(|i| items[i]);
    let drop = |modes: &mut [u8], t: u16| {
        if let Some(i) = find(t)
            && modes[i] == mode::TAKEN
        {
            modes[i] = mode::RAW;
        }
    };

    // The parts the decoder reads of the sections kept: type lists and code items.
    let lists = match at(&modes, ty::TYPE_LIST, mode::RAW) {
        Some(it) => dex::type_lists(d, it.off as usize, it.count).unwrap_or_default(),
        None => Vec::new(),
    };
    let code = match at(&modes, ty::CODE, mode::RAW) {
        Some(it) => dex::code_items(d, it.off as usize, it.count).unwrap_or_default(),
        None => Vec::new(),
    };
    let list_index = |off: u32| -> Option<u64> {
        match off {
            0 => Some(0),
            _ => lists.binary_search(&off).ok().map(|i| i as u64 + 1),
        }
    };
    let table = |modes: &[u8], t: u16| -> Option<(usize, usize)> {
        let it = at(modes, t, mode::TAKEN)?;
        let (off, n) = (it.off as usize, it.count as usize);
        (off.checked_add(n.checked_mul(dex::entry_size(t))?)? <= len).then_some((off, n))
    };
    // The ranges taken, but for the sections kept.
    let mut ranges = vec![(0, HEADER), (map_off, map_off + 4 + 12 * count)];

    // Strings.
    let mut strings: Vec<&[u8]> = Vec::new();
    let taken = match (table(&modes, ty::STRING_ID), at(&modes, ty::STRING_DATA, mode::TAKEN)) {
        (Some((off, n)), Some(data)) => split_strings(d, off, n, data, &mut st, &mut strings),
        _ => None,
    };
    match taken {
        Some(r) => {
            let (off, n) = table(&modes, ty::STRING_ID).unwrap();
            ranges.extend([r, (off, off + 4 * n)]);
        }
        None => {
            drop(&mut modes, ty::STRING_ID);
            drop(&mut modes, ty::STRING_DATA);
            strings.clear();
        }
    }

    // Type ids.
    if let Some((off, n)) = table(&modes, ty::TYPE_ID) {
        let mut prev = 0i64;
        for i in 0..n {
            let v = le32(d, off + 4 * i).ok()? as i64;
            varint::put(&mut st[s::TYPE], zz(v - prev));
            prev = v;
        }
        ranges.push((off, off + 4 * n));
    }

    // Prototypes.
    let types = find(ty::TYPE_ID).map(|i| items[i]);
    if let Some((off, n)) = table(&modes, ty::PROTO_ID) {
        match split_protos(d, off, n, &strings, types, &list_index) {
            Some((ret, params, shorty)) => {
                (st[s::PROTO_RET], st[s::PROTO_PARAMS], st[s::PROTO_SHORTY]) = (ret, params, shorty);
                ranges.push((off, off + 12 * n));
            }
            None => drop(&mut modes, ty::PROTO_ID),
        }
    }

    // Fields and methods.
    for (t, class, name, other) in [
        (ty::FIELD_ID, s::FIELD_CLASS, s::FIELD_NAME, s::FIELD_TYPE),
        (ty::METHOD_ID, s::METHOD_CLASS, s::METHOD_NAME, s::METHOD_PROTO),
    ] {
        if let Some((off, n)) = table(&modes, t) {
            let (mut pc, mut pn) = (0i64, 0i64);
            for e in d[off..off + 8 * n].as_chunks::<8>().0 {
                let c = u16::from_le_bytes([e[0], e[1]]) as i64;
                let nm = u32::from_le_bytes([e[4], e[5], e[6], e[7]]) as i64;
                varint::put(&mut st[class], zz(c - pc));
                varint::put(&mut st[name], if c == pc { zz(nm - pn) } else { nm as u64 });
                st[other].extend_from_slice(&e[2..4]);
                (pc, pn) = (c, nm);
            }
            ranges.push((off, off + 8 * n));
        }
    }

    // Classes and their data.
    if let Some((off, n)) = table(&modes, ty::CLASS_DEF) {
        let defs = &d[off..off + 32 * n];
        let data = at(&modes, ty::CLASS_DATA, mode::TAKEN).and_then(|it| split_class_data(d, defs, it, &code));
        if data.is_none() {
            drop(&mut modes, ty::CLASS_DATA);
        }
        match split_class_defs(defs, data.as_ref().map(|d| &d.2), &list_index) {
            Some(c) => {
                for (k, v) in (s::CDEF_CLASS..=s::CDEF_OFFS).zip(c) {
                    st[k] = v;
                }
                ranges.push((off, off + 32 * n));
                if let Some((range, streams, _)) = data {
                    ranges.push(range);
                    for (k, v) in (s::CDATA_SIZES..=s::CDATA_CODE).zip(streams) {
                        st[k] = v;
                    }
                }
            }
            None => {
                drop(&mut modes, ty::CLASS_DEF);
                drop(&mut modes, ty::CLASS_DATA);
                for v in &mut st[s::CDATA_SIZES..=s::CDATA_CODE] {
                    v.clear();
                }
            }
        }
    } else {
        drop(&mut modes, ty::CLASS_DATA);
    }

    // The sections kept as they are.
    for (i, it) in items.iter().enumerate() {
        if modes[i] == mode::RAW {
            let (start, end) = (it.off as usize, dex::raw_end(&items, i, len));
            st[dex::raw_stream(it.ty)].extend_from_slice(d.get(start..end)?);
            ranges.push((start, end));
        }
    }

    // The rest.
    ranges.sort_unstable();
    let mut at = 0;
    for (start, end) in ranges.into_iter().chain([(len, len)]) {
        st[s::REST].extend_from_slice(d.get(at..start)?);
        at = end;
    }

    // The header, with the checksum and the signature where they are right.
    let head = &mut st[s::HEAD];
    head.extend_from_slice(&d[..HEADER]);
    let mut flags = 0;
    if aqr_common::adler32::hash(&d[12..]) == le32(d, 8).ok()? {
        head[8..12].fill(0);
        flags |= 1;
    }
    if Sha1::digest(&d[32..])[..] == d[12..32] {
        head[12..32].fill(0);
        flags |= 2;
    }
    head.push(flags);
    head.extend_from_slice(&d[map_off..map_off + 4 + 12 * count]);
    head.extend_from_slice(&modes);
    Some(st)
}

/// An unsigned LEB128 value at `*at` in its shortest form, as the module writes it.
fn uleb(d: &[u8], at: &mut usize) -> Option<u32> {
    let start = *at;
    let v = dex::uleb(d, at).ok()?;
    (*at - start == varint::len(v as u64)).then_some(v)
}

/// The strings of the `n` ids at `off`, their data from `data` on, front-coded into `st`;
/// `strings` gets them. The range of their data, where it is as the module rebuilds it.
fn split_strings<'a>(
    d: &'a [u8],
    off: usize,
    n: usize,
    data: Item,
    st: &mut [Vec<u8>; s::N],
    strings: &mut Vec<&'a [u8]>,
) -> Option<(usize, usize)> {
    if data.count as usize != n {
        return None;
    }
    let mut p = data.off as usize;
    let mut prev: &[u8] = &[];
    let (mut prefix, mut suffix) = (Vec::new(), Vec::new());
    for i in 0..n {
        if le32(d, off + 4 * i).ok()? as usize != p {
            return None;
        }
        let v = uleb(d, &mut p)?;
        let s = &d[p..p + d.get(p..)?.iter().position(|&b| b == 0)?];
        if v != dex::utf16_len(s) {
            return None;
        }
        let k = s.iter().zip(prev).take_while(|(a, b)| a == b).count();
        varint::put(&mut prefix, k as u64);
        suffix.extend_from_slice(&s[k..]);
        suffix.push(0);
        strings.push(s);
        prev = s;
        p += s.len() + 1;
    }
    (st[s::STR_PREFIX], st[s::STR_SUFFIX]) = (prefix, suffix);
    Some((data.off as usize, p))
}

/// The streams of the `n` prototypes at `off`: return types, parameters and shorties.
fn split_protos(
    d: &[u8],
    off: usize,
    n: usize,
    strings: &[&[u8]],
    types: Option<Item>,
    list_index: &dyn Fn(u32) -> Option<u64>,
) -> Option<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    // (the decoder derives shorties from the strings and the type ids)
    let types = types?;
    if strings.is_empty() && n > 0 {
        return None;
    }
    let index = dex::string_index(strings.iter().copied());
    let desc = |t: u32| -> crate::Result<&[u8]> {
        if t >= types.count {
            return Err(crate::corrupt());
        }
        strings.get(le32(d, types.off as usize + 4 * t as usize)? as usize).copied().ok_or_else(crate::corrupt)
    };
    let (mut ret_s, mut params_s, mut shorty_s) = (Vec::new(), Vec::new(), Vec::new());
    let mut prev = 0i64;
    for e in d[off..off + 12 * n].as_chunks::<12>().0 {
        let [shorty, ret, params] = [0, 4, 8].map(|k| u32::from_le_bytes(e[k..k + 4].try_into().unwrap()));
        varint::put(&mut ret_s, zz(ret as i64 - prev));
        prev = ret as i64;
        varint::put(&mut params_s, list_index(params)?);
        let derived = dex::shorty(d, ret, params, desc).ok().and_then(|s| index.get(s.as_slice()).copied());
        varint::put(&mut shorty_s, if derived == Some(shorty) { 0 } else { shorty as u64 + 1 });
    }
    Some((ret_s, params_s, shorty_s))
}

/// The columns of the class definitions `defs` (`CDEF_CLASS` to `CDEF_OFFS`); where `ranks`
/// (the data taken), the offsets of their data as ranks in its section.
fn split_class_defs(
    defs: &[u8],
    ranks: Option<&HashMap<u32, u32>>,
    list_index: &dyn Fn(u32) -> Option<u64>,
) -> Option<[Vec<u8>; 6]> {
    let mut c: [Vec<u8>; 6] = Default::default();
    let mut prev = [0i64; 4];
    let mut k_data = 0i64;
    for e in defs.as_chunks::<32>().0 {
        let v: [u32; 8] = std::array::from_fn(|k| u32::from_le_bytes(e[4 * k..4 * k + 4].try_into().unwrap()));
        varint::put(&mut c[0], zz(v[0] as i64 - prev[0]));
        prev[0] = v[0] as i64;
        varint::put(&mut c[1], v[1] as u64);
        varint::put(&mut c[2], v[2].wrapping_add(1) as u64);
        varint::put(&mut c[3], list_index(v[3])?);
        varint::put(&mut c[4], v[4].wrapping_add(1) as u64);
        for (k, &o) in [v[5], v[6], v[7]].iter().enumerate() {
            let z = match (o, ranks) {
                (0, _) => 0,
                (_, Some(r)) if k == 1 => {
                    let z = zz(r[&o] as i64 - k_data) + 1;
                    k_data += 1;
                    z
                }
                _ => {
                    let z = zz(o as i64 - prev[1 + k]) + 1;
                    prev[1 + k] = o as i64;
                    z
                }
            };
            varint::put(&mut c[5], z);
        }
    }
    Some(c)
}

/// The streams of the classes' data (`CDATA_SIZES` to `CDATA_CODE`), its range and the rank
/// of each class's data in it, where the data of the classes of `defs` is one after the other
/// from `data` on.
#[allow(clippy::type_complexity)]
fn split_class_data(
    d: &[u8],
    defs: &[u8],
    data: Item,
    code: &[u32],
) -> Option<((usize, usize), [Vec<u8>; 6], HashMap<u32, u32>)> {
    let mut c: [Vec<u8>; 6] = Default::default();
    let mut code = dex::Code::new(code.to_vec());
    // (the start and end of each class's data)
    let mut spans = Vec::new();
    for e in defs.as_chunks::<32>().0 {
        let off = u32::from_le_bytes(e[24..28].try_into().unwrap()) as usize;
        if off == 0 {
            continue;
        }
        let mut p = off;
        let mut sizes = [0; 4];
        for v in &mut sizes {
            *v = uleb(d, &mut p)?;
            varint::put(&mut c[0], *v as u64);
        }
        for _ in 0..sizes[0] as u64 + sizes[1] as u64 {
            varint::put(&mut c[1], uleb(d, &mut p)? as u64);
            varint::put(&mut c[2], uleb(d, &mut p)? as u64);
        }
        let methods = sizes[2] as usize + sizes[3] as usize;
        let mut window = code.window(methods);
        for _ in 0..methods {
            varint::put(&mut c[3], uleb(d, &mut p)? as u64);
            varint::put(&mut c[4], uleb(d, &mut p)? as u64);
            let off = uleb(d, &mut p)?;
            let v = if off == 0 {
                0
            } else {
                let j = code.offs.binary_search(&off).ok()?;
                let v = match window.iter().position(|&w| w == j) {
                    Some(r) => {
                        window.remove(r);
                        r + 1
                    }
                    None => window.len() + 1 + zz(j as i64 - code.cursor() as i64) as usize,
                };
                code.use_(j);
                v
            };
            varint::put(&mut c[5], v as u64);
        }
        code.advance();
        spans.push((off, p));
    }
    if spans.len() != data.count as usize {
        return None;
    }
    // (one after the other, from the section's start)
    let mut sorted = spans.clone();
    sorted.sort_unstable();
    let mut p = data.off as usize;
    for &(s, e) in &sorted {
        if s != p {
            return None;
        }
        p = e;
    }
    let ranks = sorted.iter().enumerate().map(|(r, &(s, _))| (s as u32, r as u32)).collect();
    Some(((data.off as usize, p), c, ranks))
}
