//! The entries of an archive of several files (spec, "Archive model" and "Metadata block"): a
//! [`Tree`] of directories, files and links, with the optional metadata that this crate knows
//! of each ([`Extra`]: the modification time and the Unix permissions), and its metadata block.
//!
//! The optional metadata is CBOR: this module writes the little it needs of it, and reads any
//! well-formed CBOR, taking what it knows and leaving the rest.

use aqr_common::varint;

use super::{Decoded, invalid};

/// The entries of an archive, below its root directory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tree {
    /// Of the root directory.
    pub root: Extra,
    /// Each after its parent.
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The directory it is in (by index in [`Tree::entries`]), or the root.
    pub parent: Option<usize>,
    /// The name as an entry name is encoded ([`name_from`]): a non-empty WTF-8 string, or
    /// `C1` and any bytes.
    pub name: Vec<u8>,
    pub node: EntryNode,
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryNode {
    Dir,
    /// A file of a content (by index, among the contents archived).
    File(usize),
    /// A symbolic link, its target encoded as a name is.
    Link(Vec<u8>),
    /// A special entry of another kind (`special_kind`: a FIFO, a device or one undefined),
    /// as an archive decoded holds it; not encoded.
    Special(u64),
}

/// What an entry has of the optional metadata, as far as this crate knows it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Extra {
    pub mtime: Option<Time>,
    /// `unix-mode`: the permission bits (0o7777).
    pub mode: Option<u32>,
}

impl Extra {
    pub fn is_empty(&self) -> bool {
        self.mtime.is_none() && self.mode.is_none()
    }
}

/// A point in time, as seconds and nanoseconds since 1970-01-01T00:00:00Z.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Time {
    pub secs: i64,
    pub nanos: u32,
}

/// The encoding of an entry name or link target of `bytes`: as they are where they are UTF-8
/// (WTF-8 even), else after `C1`.
pub fn name_from(bytes: &[u8]) -> Vec<u8> {
    if wtf8(bytes) { bytes.to_vec() } else { [&[0xc1][..], bytes].concat() }
}

/// The bytes of an encoded name or link target, and whether they were not WTF-8.
pub fn name_bytes(name: &[u8]) -> (&[u8], bool) {
    match name.split_first() {
        Some((0xc1, rest)) => (rest, true),
        _ => (name, false),
    }
}

/// Whether `d` is WTF-8: UTF-8, with surrogates allowed alone (a pair is 4 bytes).
fn wtf8(d: &[u8]) -> bool {
    let mut i = 0;
    let mut high = false;
    while i < d.len() {
        let b = d[i];
        let (n, min) = match b {
            0x00..=0x7f => (1, 0),
            0xc2..=0xdf => (2, 0x80),
            0xe0..=0xef => (3, 0x800),
            0xf0..=0xf4 => (4, 0x1_0000),
            _ => return false,
        };
        let Some(cont) = d.get(i + 1..i + n) else { return false };
        if cont.iter().any(|&c| c & 0xc0 != 0x80) {
            return false;
        }
        let mut c = if n == 1 { b as u32 } else { (b as u32) & (0x7f >> n) };
        cont.iter().for_each(|&x| c = (c << 6) | (x & 0x3f) as u32);
        if c < min || c > 0x10_ffff {
            return false;
        }
        // a high surrogate followed by a low one is a pair, which WTF-8 takes in 4 bytes
        let low = (0xdc00..0xe000).contains(&c);
        if high && low {
            return false;
        }
        high = (0xd800..0xdc00).contains(&c);
        i += n;
    }
    true
}

/// Whether encoded `name` is an entry name the spec allows.
fn valid_name(name: &[u8]) -> bool {
    let (bytes, raw) = name_bytes(name);
    !bytes.is_empty()
        && !bytes.iter().any(|&b| b == 0 || b == b'/')
        && bytes != b"."
        && bytes != b".."
        && (raw || wtf8(bytes))
}

/// Whether encoded `target` is a link target the spec allows.
fn valid_target(target: &[u8]) -> bool {
    let (bytes, raw) = name_bytes(target);
    !bytes.is_empty() && !bytes.contains(&0) && (raw || wtf8(bytes))
}

/// The entries numbered as a metadata block does: directories, then files, then links, each in
/// the order of the tree (so that a directory comes before what it holds). By the entry's
/// index, its number (from 1).
pub(crate) fn numbers(t: &Tree) -> Vec<usize> {
    let mut number = vec![0; t.entries.len()];
    let mut n = 0;
    for pass in 0..3 {
        for (i, e) in t.entries.iter().enumerate() {
            let p = match e.node {
                EntryNode::Dir => 0,
                EntryNode::File(_) => 1,
                EntryNode::Link(_) | EntryNode::Special(_) => 2,
            };
            if p == pass {
                n += 1;
                number[i] = n;
            }
        }
    }
    number
}

/// Checks that `t` is a tree the spec allows: each entry after its parent, which is a
/// directory, of a valid name not taken in it already; and at least one entry.
pub(crate) fn check(t: &Tree, contents: usize) -> Result<(), String> {
    if t.entries.is_empty() {
        return Err("no entries".into());
    }
    let mut names = std::collections::HashSet::new();
    for (i, e) in t.entries.iter().enumerate() {
        if let Some(p) = e.parent
            && (p >= i || t.entries[p].node != EntryNode::Dir)
        {
            return Err(format!("entry #{i} not after its directory"));
        }
        if !valid_name(&e.name) {
            return Err(format!("entry #{i} of an invalid name"));
        }
        if !names.insert((e.parent, &e.name[..])) {
            return Err(format!("entry #{i} of a name taken"));
        }
        match &e.node {
            EntryNode::File(c) if *c >= contents => return Err(format!("entry #{i} of no content")),
            EntryNode::Link(target) if !valid_target(target) => return Err(format!("entry #{i} of an invalid target")),
            EntryNode::Special(_) => return Err(format!("entry #{i} of a special kind")),
            _ => {}
        }
        if e.extra.mode.is_some_and(|m| m > 0o7777) {
            return Err(format!("entry #{i} of a mode beyond its permissions"));
        }
    }
    Ok(())
}

/// What a file's parameters in a metadata block point to: a leaf block (by where it starts in
/// the archive), its output, and the dictionary it takes (by index).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileParam {
    pub leaf: usize,
    pub output: u64,
    pub size: u64,
    pub dictionary: Option<usize>,
}

/// The internal structure of the metadata block at `at` (in the archive), of a segment of the
/// module, resource and dictionary blocks at `modules`, `resources` and `dictionaries`; the
/// files' parameters by their entries' indices (`None` for the other entries).
pub(crate) fn metadata(
    t: &Tree,
    at: usize,
    modules: &[usize],
    resources: &[usize],
    dictionaries: &[usize],
    files: &[Option<FileParam>],
) -> Vec<u8> {
    let number = numbers(t);
    let n = t.entries.len();
    // by number - 1, the entries' indices
    let mut order = vec![0; n];
    number.iter().enumerate().for_each(|(i, &k)| order[k - 1] = i);
    let count = |p: fn(&EntryNode) -> bool| t.entries.iter().filter(|e| p(&e.node)).count();
    let dirs = count(|n| matches!(n, EntryNode::Dir));
    let nfiles = count(|n| matches!(n, EntryNode::File(_)));

    // (from the end of the header magic, plus 1)
    let prev = (at - super::HEADER_MAGIC.len() + 1) as u64;
    let mut entries = Vec::new();
    let mut entry_offsets = Vec::new();
    for (k, &i) in order.iter().enumerate() {
        if k > 0 {
            entry_offsets.push(entries.len() as u64);
        }
        let e = &t.entries[i];
        entries.extend_from_slice(&e.name);
        entries.push(0);
        match &e.node {
            EntryNode::Dir => {}
            EntryNode::File(_) => varint::put(&mut entries, files[i].expect("a file's parameters").size),
            EntryNode::Link(target) => {
                varint::put(&mut entries, 1);
                entries.extend_from_slice(target);
                entries.push(0);
            }
            EntryNode::Special(_) => unreachable!("checked"),
        }
    }
    let opt = optional(t, &order);

    let mut m = Vec::new();
    varint::put(&mut m, prev);
    let counts = [modules.len(), resources.len(), dictionaries.len(), dirs, nfiles, n - dirs - nfiles];
    for x in counts.into_iter().chain([entries.len(), opt.len()]) {
        varint::put(&mut m, x as u64);
    }
    for &b in modules.iter().chain(resources).chain(dictionaries) {
        put_u(&mut m, prev, (at - b) as u64);
    }
    for &i in &order[1..] {
        put_u(&mut m, dirs as u64, t.entries[i].parent.map_or(0, |p| number[p]) as u64);
    }
    for &i in &order[dirs..dirs + nfiles] {
        let p = files[i].expect("a file's parameters");
        let offset = (at - p.leaf) as u64;
        // (1-based, zero for none: zero-sized where the segment has no dictionary)
        put_u(&mut m, dictionaries.len() as u64, p.dictionary.map_or(0, |d| d as u64 + 1));
        put_u(&mut m, prev, offset + (p.output > 0) as u64);
        if p.output > 0 {
            varint::put(&mut m, p.output - 1);
        }
    }
    for o in entry_offsets {
        put_u(&mut m, entries.len() as u64, o);
    }
    m.extend_from_slice(&entries);
    m.extend_from_slice(&opt);
    m
}

/// Bytes of `u(n)`.
fn width(n: u64) -> usize {
    match n {
        0 => 0,
        1..0x100 => 1,
        0x100..0x1_0000 => 2,
        0x1_0000..0x1_0000_0000 => 4,
        _ => 8,
    }
}

/// Appends `v` as `u(n)`.
fn put_u(out: &mut Vec<u8>, n: u64, v: u64) {
    debug_assert!(v <= n);
    out.extend_from_slice(&v.to_le_bytes()[..width(n)]);
}

/// The optional metadata of `t`, its entries in `order`: `entry-extras` where any entry has an
/// extra, else nothing (an empty map).
fn optional(t: &Tree, order: &[usize]) -> Vec<u8> {
    let extras: Vec<&Extra> = std::iter::once(&t.root).chain(order.iter().map(|&i| &t.entries[i].extra)).collect();
    let Some(last) = extras.iter().rposition(|e| !e.is_empty()) else { return Vec::new() };
    let mut c = Vec::new();
    head(&mut c, 5, 1);
    int(&mut c, ENTRY_EXTRAS);
    head(&mut c, 4, last as u64 + 1);
    for e in &extras[..=last] {
        if e.is_empty() {
            c.push(NULL);
            continue;
        }
        head(&mut c, 5, e.mtime.is_some() as u64 + e.mode.is_some() as u64);
        if let Some(t) = e.mtime {
            int(&mut c, MTIME);
            match t.nanos {
                0 => int(&mut c, t.secs as i128),
                n => {
                    head(&mut c, 6, TAG_NS);
                    int(&mut c, t.secs as i128 * 1_000_000_000 + n as i128);
                }
            }
        }
        if let Some(m) = e.mode {
            int(&mut c, UNIX_MODE);
            int(&mut c, m as i128);
        }
    }
    c
}

const ENTRY_EXTRAS: i128 = -2;
const MTIME: i128 = -3;
const UNIX_MODE: i128 = -70;
const NULL: u8 = 0xf6;
/// The tags of `$aqr-instant` and `#6.100`, by their scale in nanoseconds.
const TAG_NS: u64 = 126;
const TAG_SCALES: [(u64, i128); 7] = [
    (100, 86400 * 1_000_000_000),
    (121, 2_000_000_000),
    (122, 10_000_000),
    (123, 1_000_000),
    (124, 1_000),
    (125, 100),
    (126, 1),
];

/// A CBOR head of major type `major` and argument `v`, in the shortest encoding.
fn head(out: &mut Vec<u8>, major: u8, v: u64) {
    let m = major << 5;
    match v {
        0..24 => out.push(m | v as u8),
        24..0x100 => out.extend_from_slice(&[m | 24, v as u8]),
        0x100..0x1_0000 => {
            out.push(m | 25);
            out.extend_from_slice(&(v as u16).to_be_bytes());
        }
        0x1_0000..0x1_0000_0000 => {
            out.push(m | 26);
            out.extend_from_slice(&(v as u32).to_be_bytes());
        }
        _ => {
            out.push(m | 27);
            out.extend_from_slice(&v.to_be_bytes());
        }
    }
}

/// A CBOR integer (within 64 bits, positive or negative).
fn int(out: &mut Vec<u8>, v: i128) {
    if v >= 0 { head(out, 0, v as u64) } else { head(out, 1, (-1 - v) as u64) }
}

/// A CBOR data item, as far as it is read.
#[derive(Debug, Clone, PartialEq)]
enum Cbor {
    Int(i128),
    Bytes,
    Text,
    Array(Vec<Cbor>),
    Map(Vec<(Cbor, Cbor)>),
    Tag(u64, Box<Cbor>),
    /// Simple values (null among them) and floats.
    Simple(u8),
    Float,
}

/// Maps and arrays nested deeper than this are not read (a resource failure).
const CBOR_DEPTH: usize = 64;

/// Reads a CBOR data item at `*at` of `d`.
fn item(d: &[u8], at: &mut usize, depth: usize) -> Decoded<Cbor> {
    if depth > CBOR_DEPTH {
        return super::resource("optional metadata nested too deep");
    }
    let bad = || invalid::<Cbor>("bad CBOR in the optional metadata");
    let Some(&b) = d.get(*at) else { return bad() };
    *at += 1;
    let (major, ai) = (b >> 5, b & 0x1f);
    // the argument, or `None` for an indefinite length
    let arg = match ai {
        0..24 => Some(ai as u64),
        24..28 => {
            let n = 1 << (ai - 24);
            let Some(bytes) = d.get(*at..*at + n) else { return bad() };
            *at += n;
            Some(bytes.iter().fold(0u64, |v, &x| (v << 8) | x as u64))
        }
        31 if matches!(major, 2..=5) => None,
        31 if major == 7 => return bad(),
        _ => return bad(),
    };
    let len = |n: u64, at: usize| usize::try_from(n).ok().filter(|&n| n <= d.len() - at.min(d.len()));
    Ok(match (major, arg) {
        (0, Some(v)) => Cbor::Int(v as i128),
        (1, Some(v)) => Cbor::Int(-1 - v as i128),
        (2 | 3, Some(n)) => {
            let Some(n) = len(n, *at) else { return bad() };
            if major == 3 && std::str::from_utf8(&d[*at..*at + n]).is_err() {
                return bad();
            }
            *at += n;
            if major == 2 { Cbor::Bytes } else { Cbor::Text }
        }
        // chunks of the same major type, definite each, up to a break
        (2 | 3, None) => {
            while d.get(*at) != Some(&0xff) {
                let Some(&c) = d.get(*at) else { return bad() };
                if c >> 5 != major || c & 0x1f == 31 {
                    return bad();
                }
                item(d, at, depth + 1)?;
            }
            *at += 1;
            if major == 2 { Cbor::Bytes } else { Cbor::Text }
        }
        (4 | 5, arg) => {
            let n = match arg {
                Some(n) => Some(len(n, *at).ok_or_else(|| super::Failure::Invalid("bad CBOR length".into()))?),
                None => None,
            };
            let mut items = Vec::new();
            let per = if major == 5 { 2 } else { 1 };
            loop {
                match n {
                    Some(n) if items.len() == n * per => break,
                    None if d.get(*at) == Some(&0xff) => {
                        *at += 1;
                        break;
                    }
                    _ => items.push(item(d, at, depth + 1)?),
                }
            }
            if major == 4 {
                Cbor::Array(items)
            } else {
                if items.len() % 2 == 1 {
                    return bad();
                }
                let mut pairs = Vec::new();
                let mut it = items.into_iter();
                while let (Some(k), Some(v)) = (it.next(), it.next()) {
                    if pairs.iter().any(|(q, _)| *q == k) {
                        return invalid("duplicate keys in the optional metadata");
                    }
                    pairs.push((k, v));
                }
                Cbor::Map(pairs)
            }
        }
        (6, Some(tag)) => Cbor::Tag(tag, Box::new(item(d, at, depth + 1)?)),
        (7, Some(v)) => match ai {
            0..24 => Cbor::Simple(v as u8),
            // (two-byte simple values below 32 are not well-formed)
            24 if v >= 32 => Cbor::Simple(v as u8),
            25..28 => Cbor::Float,
            _ => return bad(),
        },
        _ => return bad(),
    })
}

/// The extras of the entries (from the root, by number) in optional metadata `d`, of `n`
/// entries besides the root.
pub(crate) fn extras(d: &[u8], n: usize) -> Decoded<Vec<Extra>> {
    let mut out = vec![Extra::default(); n + 1];
    if d.is_empty() {
        return Ok(out);
    }
    let mut at = 0;
    let top = item(d, &mut at, 0)?;
    if at != d.len() {
        return invalid("bytes after the optional metadata");
    }
    let Cbor::Map(top) = top else { return invalid("optional metadata not a map") };
    let Some((_, list)) = top.iter().find(|(k, _)| *k == Cbor::Int(ENTRY_EXTRAS)) else { return Ok(out) };
    let Cbor::Array(list) = list else { return invalid("entry-extras not an array") };
    if list.len() > n + 1 {
        return invalid("more entry-extras than entries");
    }
    for (e, x) in list.iter().zip(&mut out) {
        match e {
            Cbor::Simple(22) => {}
            Cbor::Map(m) => {
                for (k, v) in m {
                    match (k, v) {
                        (Cbor::Int(MTIME), t) => x.mtime = time(t),
                        (Cbor::Int(UNIX_MODE), &Cbor::Int(m)) if (0..=0o7777).contains(&m) => x.mode = Some(m as u32),
                        (Cbor::Int(UNIX_MODE), _) => return invalid("a bad unix-mode"),
                        _ => {}
                    }
                }
            }
            _ => return invalid("an entry extra not a map"),
        }
    }
    Ok(out)
}

/// The UTC time of an `aqr-time`, where it is a base time this crate reads (else `None`: a
/// local time, or an extended one).
fn time(t: &Cbor) -> Option<Time> {
    let ns = match t {
        &Cbor::Int(s) => s.checked_mul(1_000_000_000)?,
        Cbor::Tag(tag, v) => {
            let &Cbor::Int(v) = &**v else { return None };
            v.checked_mul(TAG_SCALES.iter().find(|s| s.0 == *tag)?.1)?
        }
        _ => return None,
    };
    let secs = i64::try_from(ns.div_euclid(1_000_000_000)).ok()?;
    Some(Time { secs, nanos: ns.rem_euclid(1_000_000_000) as u32 })
}

/// A metadata block's internal structure read: the entries (by number) and the files'
/// parameters, with the relative offsets of the module and resource blocks.
pub(crate) struct Metadata {
    pub prev: u64,
    pub modules: Vec<u64>,
    pub resources: Vec<u64>,
    pub dictionaries: Vec<u64>,
    /// Of each entry from 1: its parent's number, its name, and what it is (for a file, the
    /// relative offset of its leaf block, its output, its size and its dictionary).
    pub entries: Vec<(usize, Vec<u8>, Read)>,
    pub extras: Vec<Extra>,
}

pub(crate) enum Read {
    Dir,
    File {
        offset: u64,
        output: u64,
        size: u64,
        /// 1-based, zero for none.
        dictionary: u64,
    },
    Link(Vec<u8>),
    /// A special entry this crate does not take, by its kind.
    Special(u64),
}

/// Reads the internal structure of a metadata block.
pub(crate) fn read(d: &[u8]) -> Decoded<Metadata> {
    let mut at = 0;
    let leb = |at: &mut usize| super::leb(d, at);
    let prev = leb(&mut at)?;
    let mut counts = [0usize; 8];
    for c in &mut counts {
        *c = usize::try_from(leb(&mut at)?)
            .ok()
            .filter(|&n| n <= d.len())
            .ok_or_else(|| super::Failure::Invalid("a count out of range".into()))?;
    }
    let [modules, resources, dictionaries, dirs, files, specials, entries_size, opt_size] = counts;
    let n = dirs + files + specials;
    if n == 0 {
        return invalid("a metadata block of no entries");
    }
    let get_u = |at: &mut usize, bound: u64| -> Decoded<u64> {
        let w = width(bound);
        let b = d.get(*at..*at + w).ok_or_else(|| super::Failure::Invalid("truncated metadata".into()))?;
        *at += w;
        let v = b.iter().rev().fold(0u64, |v, &x| (v << 8) | x as u64);
        if v > bound { invalid("a field out of range") } else { Ok(v) }
    };
    let module_offsets = (0..modules).map(|_| get_u(&mut at, prev)).collect::<Decoded<Vec<_>>>()?;
    let resource_offsets = (0..resources).map(|_| get_u(&mut at, prev)).collect::<Decoded<Vec<_>>>()?;
    let dictionary_offsets = (0..dictionaries).map(|_| get_u(&mut at, prev)).collect::<Decoded<Vec<_>>>()?;
    let mut parents = vec![0];
    for k in 2..=n {
        let p = get_u(&mut at, dirs as u64)? as usize;
        if p >= k {
            return invalid("a parent after its entry");
        }
        parents.push(p);
    }
    let mut params = Vec::new();
    for _ in 0..files {
        let dictionary = get_u(&mut at, dictionaries as u64)?;
        let offset = get_u(&mut at, prev)?;
        let output = match offset % 2 {
            1 => leb(&mut at)?.checked_add(1).ok_or_else(|| super::Failure::Invalid("bad file output".into()))?,
            _ => 0,
        };
        params.push((offset - offset % 2, output, dictionary));
    }
    let offsets = (2..=n).map(|_| get_u(&mut at, entries_size as u64)).collect::<Decoded<Vec<_>>>()?;
    let entries = d.get(at..at + entries_size).ok_or_else(|| super::Failure::Invalid("truncated entries".into()))?;
    at += entries_size;
    let opt = d.get(at..at + opt_size).ok_or_else(|| super::Failure::Invalid("truncated optional metadata".into()))?;
    if at + opt_size != d.len() {
        return invalid("bytes after the optional metadata");
    }

    let mut out = Vec::with_capacity(n);
    let mut p = 0;
    for k in 1..=n {
        if k >= 2 && offsets[k - 2] as usize != p {
            return invalid("an entry offset not where its entry is");
        }
        let end = offsets.get(k - 1).map_or(entries.len(), |&o| o as usize);
        let e = entries.get(p..end).ok_or_else(|| super::Failure::Invalid("bad entry offsets".into()))?;
        let mut q =
            e.iter().position(|&b| b == 0).ok_or_else(|| super::Failure::Invalid("an unended entry name".into()))?;
        let name = e[..q].to_vec();
        if !valid_name(&name) {
            return invalid("an invalid entry name");
        }
        q += 1;
        let read = if k <= dirs {
            Read::Dir
        } else if k <= dirs + files {
            let size = super::leb(e, &mut q)?;
            let (offset, output, dictionary) = params[k - dirs - 1];
            Read::File { offset, output, size, dictionary }
        } else {
            match super::leb(e, &mut q)? {
                1 => {
                    let t = e[q..]
                        .iter()
                        .position(|&b| b == 0)
                        .ok_or_else(|| super::Failure::Invalid("an unended link target".into()))?;
                    let target = e[q..q + t].to_vec();
                    if !valid_target(&target) {
                        return invalid("an invalid link target");
                    }
                    q += t + 1;
                    Read::Link(target)
                }
                kind @ (0 | 2..=4) => {
                    if kind >= 3 {
                        super::leb(e, &mut q)?;
                        super::leb(e, &mut q)?;
                    }
                    Read::Special(kind)
                }
                kind => {
                    q = e.len();
                    Read::Special(kind)
                }
            }
        };
        if q != e.len() {
            return invalid("bytes after an entry");
        }
        out.push((parents[k - 1], name, read));
        p = end;
    }
    if p != entries.len() {
        return invalid("bytes after the entries");
    }
    Ok(Metadata {
        prev,
        modules: module_offsets,
        resources: resource_offsets,
        dictionaries: dictionary_offsets,
        entries: out,
        extras: extras(opt, n)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wtf8_names() {
        assert!(wtf8("añ日🦀".as_bytes()));
        // a lone surrogate, but not a pair of them
        assert!(wtf8(&[0xed, 0xa0, 0x80]));
        assert!(!wtf8(&[0xed, 0xa0, 0x80, 0xed, 0xb0, 0x80]));
        assert!(!wtf8(&[0xc0, 0x80]) && !wtf8(&[0xff]) && !wtf8(&[0xe2, 0x82]));
        assert_eq!(name_from(b"a\xffb"), b"\xc1a\xffb");
        assert_eq!(name_bytes(b"\xc1a\xffb"), (&b"a\xffb"[..], true));
        assert!(
            !valid_name(b"..")
                && !valid_name(b"a/b")
                && !valid_name(b"")
                && !valid_name(b"\xc1..")
                && valid_name(b"\xc1\xff")
        );
    }

    #[test]
    fn optional_metadata() {
        let mut t = Tree::default();
        let x = Extra { mtime: Some(Time { secs: -5, nanos: 7 }), mode: Some(0o755) };
        for (i, extra) in [Extra::default(), x, Extra { mtime: Some(Time { secs: 1 << 40, nanos: 0 }), mode: None }]
            .into_iter()
            .enumerate()
        {
            let node = EntryNode::File(i);
            t.entries.push(Entry { parent: None, name: vec![b'a' + i as u8], node, extra });
        }
        let order: Vec<usize> = (0..3).collect();
        let c = optional(&t, &order);
        let got = extras(&c, 3).unwrap();
        assert_eq!(got[1..], [t.entries[0].extra, t.entries[1].extra, t.entries[2].extra]);
        assert_eq!(optional(&Tree { entries: t.entries[..1].to_vec(), ..Tree::default() }, &[0]), b"");
        // other keys, tags and indefinite lengths read
        let other = [0xbf, 0x21, 0x9f, 0xa1, 0x22, 0xd8, 0x79, 0x0a, 0xf6, 0xff, 0x01, 0x5f, 0x41, 0x00, 0xff, 0xff];
        let got = extras(&other, 2).unwrap();
        assert_eq!(got[0].mtime, Some(Time { secs: 20, nanos: 0 }));
        assert!(extras(&[0xa2, 0x01, 0x00, 0x01, 0x00], 1).is_err());
        assert!(extras(&[0xa1, 0x21, 0x83, 0xf6, 0xf6, 0xf6], 1).is_err());
    }
}
