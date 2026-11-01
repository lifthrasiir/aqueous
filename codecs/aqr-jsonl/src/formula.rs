//! aqr-jsonl as a module of entry formulas (see aqr-formula): JSON lines rebuilt from their
//! values stored by key path (FORMAT.md, "The module"), strings as they are and numbers as
//! integers, for the archive to compress.
//!
//! The encoder goes over the file twice. The first pass tells of each path whether its objects
//! are maps (keys that are data: their members in one child), the length of its arrays where
//! most are of one (tuples: an element's index a child of its own), and the style of its
//! numbers; the second writes the streams.

use std::collections::HashMap;
use std::hash::BuildHasherDefault;

use aqr_common::varint;
use aqr_formula::{Formula, Kind, Module, Node};
use aqr_num::style::{Style as NumStyle, choose};

use crate::parse::{self, S_CLOSE, S_COLON, S_COMMA, S_OPEN, Tok, Toks, skip};
use crate::tree::{
    E_COMMA, F_DELTA, F_MAP, F_SHARED, MAX_LISTS, MAX_PATHS, MAX_WIDTH, Style, T_ARR, T_FALSE, T_NEW, T_NULL, T_NUM,
    T_NUMTEXT, T_OBJ, T_STR, T_TRUE,
};

pub use crate::module::decode;

pub const MODULE: Module = Module::new("jsonl-recons", 1).with_memory(memory);

/// The memory of the module: the streams and the file, the paths' state (about 128 bytes each
/// on 32 bits, one for 8 bytes of `paths` at least) and the key lists (about 48 bytes a key on
/// 32 bits, one for a byte of `keys` at least).
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let &[_, _, paths, keys, ..] = u.inputs else { return None };
    Some(u.whole(128 * (paths / 8 + 1) + 48 * keys, 3))
}

/// The names of the inputs' streams, in order.
pub const STREAM_NAMES: [&str; 10] =
    ["layout", "raw", "paths", "keys", "types", "shapes", "names", "text", "nums", "desc"];

/// A file shorter than this is not taken.
pub const MIN_LEN: usize = 16 << 10;

/// The bytes at the start of a file that tell its style, unless its first lines are longer.
const SAMPLE: usize = 64 << 10;
/// The most bytes read for the first lines of a file.
const SAMPLE_MAX: usize = 4 << 20;

/// The tokens of a path that its numbers' style is chosen from.
const STYLE_SAMPLE: usize = 512;

/// A path's objects are maps once it has seen this many objects and its keys are data: at
/// least [`MAP_KEYS`] of them, each seen fewer than [`MAP_USES`] times on average (or more than
/// [`MAP_MAX_KEYS`] in all).
const MAP_OBJECTS: u64 = 32;
const MAP_KEYS: u64 = 16;
const MAP_USES: u64 = 4;
const MAP_MAX_KEYS: u64 = 4096;

/// The key lists the first pass keeps of a path (only to find their members' paths again).
const SEEN_LISTS: usize = 4096;

/// A path's arrays are tuples where 9 in 10 of them, of [`TUPLE_ARRAYS`] at least, are of one
/// length within 2..=[`MAX_WIDTH`].
const TUPLE_ARRAYS: u64 = 8;

/// The style of `d`, told from its start.
fn style_of(d: &[u8]) -> Option<Style> {
    let mut n = SAMPLE.min(d.len());
    // (at least the first 8 lines, where they are long)
    let mut lines = 0;
    let mut at = 0;
    while lines < 8 && at < d.len() && at < SAMPLE_MAX {
        at = parse::line(d, at).next;
        lines += 1;
    }
    if lines == 8 || at == d.len() {
        n = n.max(at.min(SAMPLE_MAX));
    }
    parse::style(&d[..n], n == d.len())
}

/// Whether `d` is a file this crate takes: of at least [`MIN_LEN`] bytes, starting as JSON lines
/// of a style ([`parse::style`]).
pub fn starts(d: &[u8]) -> bool {
    d.len() >= MIN_LEN && style_of(d).is_some()
}

/// A fast hash of a key list and its separators not of the style (`others`).
fn hash_keys(d: &[u8], toks: &[Tok], i: usize, others: &[u8]) -> u64 {
    let Tok::Obj { n, .. } = toks[i] else { unreachable!() };
    let mut h = n as u64;
    let mut j = i + 1;
    for _ in 0..n {
        let Tok::Key(a, b) = toks[j] else { unreachable!() };
        for c in d[a..b].chunks(8) {
            let mut w = [0u8; 8];
            w[..c.len()].copy_from_slice(c);
            h = (h.rotate_left(5) ^ u64::from_le_bytes(w)).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
        }
        h = (h.rotate_left(5) ^ 0xff).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
        j = skip(toks, j + 1);
    }
    for &o in others {
        h = (h.rotate_left(5) ^ o as u64).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
    h
}

/// Of the object at `i`, its spaces within the braces (bit 0 after the opening, bit 1 before the
/// closing), then each member's separators not of style `st` (bit 0 its colon, bit 1 the comma
/// before it), into `out`; left empty where there are none.
fn object_others(toks: &Toks, i: usize, st: Style, out: &mut Vec<u8>) {
    out.clear();
    let Tok::Obj { n, .. } = toks.t[i] else { unreachable!() };
    let mut j = i + 1;
    let mut any = within(toks.seps[i]);
    out.push(any);
    for m in 0..n {
        let s = toks.seps[j];
        let o =
            ((s & S_COLON != 0) != st.colon_space) as u8 | ((m > 0 && (s & S_COMMA != 0) != st.comma_space) as u8) << 1;
        any |= o;
        out.push(o);
        j = skip(&toks.t, j + 1);
    }
    if any == 0 {
        out.clear();
    }
}

/// The spaces within a container of separators `s`: bit 0 after the opening, bit 1 before the
/// closing.
fn within(s: u8) -> u8 {
    (s & S_OPEN != 0) as u8 | ((s & S_CLOSE != 0) as u8) << 1
}

/// A count `n` and, where any of `nbits` bits is set, the bits (FORMAT.md, "Separators").
fn put_bits(out: &mut Vec<u8>, n: u64, nbits: usize, bits: impl Iterator<Item = bool>) {
    let at = out.len();
    varint::put(out, n << 1);
    let start = out.len();
    out.resize(start + nbits.div_ceil(8), 0);
    let mut any = false;
    for (k, b) in bits.enumerate() {
        if b {
            out[start + k / 8] |= 1 << (k % 8);
            any = true;
        }
    }
    if any {
        out[at] |= 1;
    } else {
        out.truncate(start);
    }
}

/// Hashes already hashed.
#[derive(Default)]
struct Hashed(u64);

impl std::hash::Hasher for Hashed {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, _: &[u8]) {
        unreachable!()
    }
    fn write_u64(&mut self, v: u64) {
        self.0 = v;
    }
}

/// A key list: its keys, its separators not of the style ([`object_others`]), its members' paths.
struct List<'a> {
    keys: Vec<&'a [u8]>,
    others: Vec<u8>,
    kids: Vec<u32>,
}

/// The key lists of a path met so far, by the hash of their keys and separators.
#[derive(Default)]
struct Lists<'a> {
    index: HashMap<u64, Vec<u32>, BuildHasherDefault<Hashed>>,
    lists: Vec<List<'a>>,
}

impl<'a> Lists<'a> {
    /// The list of the object at `i`, of hash `h`, if met.
    fn find(&self, d: &[u8], toks: &[Tok], i: usize, others: &[u8], h: u64) -> Option<u32> {
        let Tok::Obj { n, .. } = toks[i] else { unreachable!() };
        self.index.get(&h)?.iter().copied().find(|&l| {
            let List { keys, others: o, .. } = &self.lists[l as usize];
            keys.len() == n as usize && o == others && {
                let mut j = i + 1;
                keys.iter().all(|k| {
                    let Tok::Key(a, b) = toks[j] else { unreachable!() };
                    j = skip(toks, j + 1);
                    *k == &d[a..b]
                })
            }
        })
    }

    fn add(&mut self, h: u64, keys: Vec<&'a [u8]>, others: Vec<u8>, kids: Vec<u32>) -> u32 {
        let l = self.lists.len() as u32;
        self.lists.push(List { keys, others, kids });
        self.index.entry(h).or_default().push(l);
        l
    }
}

/// The keys of the object at `i`.
fn keys_of<'a>(d: &'a [u8], toks: &[Tok], i: usize) -> Vec<&'a [u8]> {
    let Tok::Obj { n, .. } = toks[i] else { unreachable!() };
    let mut keys = Vec::with_capacity(n as usize);
    let mut j = i + 1;
    for _ in 0..n {
        let Tok::Key(a, b) = toks[j] else { unreachable!() };
        keys.push(&d[a..b]);
        j = skip(toks, j + 1);
    }
    keys
}

type Named<'a> = HashMap<(u32, &'a [u8]), u32>;

/// A path as the first pass sees it.
#[derive(Default)]
struct Seen<'a> {
    map: bool,
    objects: u64,
    /// Members of its objects (until it is a map).
    uses: u64,
    lists: Lists<'a>,
    /// Its keys (its children by name).
    keys: u64,
    star: Option<u32>,
    elem: Option<u32>,
    arrays: u64,
    /// Arrays by length (the last for the longer ones).
    lens: Vec<u64>,
    sample: Vec<&'a [u8]>,
}

/// The first pass: what the paths are.
struct First<'a> {
    d: &'a [u8],
    seen: Vec<Seen<'a>>,
    named: Named<'a>,
    /// The first strings of the file, in order, and their paths ([`shared_paths`]).
    strings: Vec<(u32, usize, usize)>,
    string_bytes: usize,
}

impl<'a> First<'a> {
    fn make(&mut self) -> u32 {
        self.seen.push(Seen::default());
        self.seen.len() as u32 - 1
    }

    fn value(&mut self, toks: &[Tok], i: usize, p: u32) {
        let d = self.d;
        match toks[i] {
            Tok::Num(a, b) => {
                let s = &mut self.seen[p as usize];
                if s.sample.len() < STYLE_SAMPLE {
                    s.sample.push(&d[a..b]);
                }
            }
            Tok::Str(a, b) => {
                // (an entry counted as 16 bytes more, so that their number is bounded too)
                if self.string_bytes < SHARED_SAMPLE {
                    self.strings.push((p, a, b));
                    self.string_bytes += b - a + 16;
                }
            }
            Tok::Obj { n, .. } => {
                let s = &mut self.seen[p as usize];
                s.objects += 1;
                if !s.map {
                    s.uses += n as u64;
                    // (separators do not matter here)
                    let h = hash_keys(d, toks, i, &[]);
                    let l = match s.lists.find(d, toks, i, &[], h) {
                        Some(l) => l,
                        None => {
                            // (the lists met kept up to a bound: those after it found again)
                            if s.lists.lists.len() >= SEEN_LISTS {
                                s.lists = Lists::default();
                            }
                            let keys = keys_of(d, toks, i);
                            let kids = keys
                                .iter()
                                .map(|&k| match self.named.get(&(p, k)) {
                                    Some(&c) => c,
                                    None => {
                                        let c = self.make();
                                        self.named.insert((p, k), c);
                                        self.seen[p as usize].keys += 1;
                                        c
                                    }
                                })
                                .collect();
                            self.seen[p as usize].lists.add(h, keys, Vec::new(), kids)
                        }
                    };
                    let s = &mut self.seen[p as usize];
                    if s.objects >= MAP_OBJECTS && (s.keys >= MAP_KEYS && s.uses < MAP_USES * s.keys)
                        || s.keys > MAP_MAX_KEYS
                    {
                        s.map = true;
                        s.lists = Lists::default();
                    } else {
                        let kids = s.lists.lists[l as usize].kids.clone();
                        let mut j = i + 1;
                        for c in kids {
                            self.value(toks, j + 1, c);
                            j = skip(toks, j + 1);
                        }
                        return;
                    }
                }
                if n > 0 {
                    let c = match self.seen[p as usize].star {
                        Some(c) => c,
                        None => {
                            let c = self.make();
                            self.seen[p as usize].star = Some(c);
                            c
                        }
                    };
                    let mut j = i + 1;
                    for _ in 0..n {
                        self.value(toks, j + 1, c);
                        j = skip(toks, j + 1);
                    }
                }
            }
            Tok::Arr { n, .. } => {
                let s = &mut self.seen[p as usize];
                s.arrays += 1;
                if s.lens.is_empty() {
                    s.lens = vec![0; MAX_WIDTH as usize + 1];
                }
                s.lens[(n as usize).min(MAX_WIDTH as usize)] += 1;
                if n > 0 {
                    let c = match s.elem {
                        Some(c) => c,
                        None => {
                            let c = self.make();
                            self.seen[p as usize].elem = Some(c);
                            c
                        }
                    };
                    let mut j = i + 1;
                    for _ in 0..n {
                        self.value(toks, j, c);
                        j = skip(toks, j);
                    }
                }
            }
            _ => {}
        }
    }
}

/// What the first pass tells of a path.
#[derive(Clone, Copy, Default)]
struct Kind1 {
    map: bool,
    /// Its strings in the shared part of `text`.
    shared: bool,
    width: usize,
    style: NumStyle,
}

impl Seen<'_> {
    fn kind(&self, shared: bool) -> Kind1 {
        let (len, &count) =
            self.lens.iter().enumerate().max_by_key(|&(l, c)| (c, std::cmp::Reverse(l))).unwrap_or((0, &0));
        let tuple =
            self.arrays >= TUPLE_ARRAYS && count * 10 >= self.arrays * 9 && (2..MAX_WIDTH as usize).contains(&len);
        let toks: Vec<(usize, &[u8])> = self.sample.iter().map(|&t| (0, t)).collect();
        Kind1 { map: self.map, shared, width: if tuple { len } else { 1 }, style: choose(&toks) }
    }
}

/// A path as the second pass writes it.
#[derive(Default)]
struct Path<'a> {
    /// The path of the first pass it is of, if any.
    seen: Option<u32>,
    kind: Kind1,
    types: Vec<u8>,
    shapes: Vec<u8>,
    names: Vec<u8>,
    text: Vec<u8>,
    desc: Vec<u8>,
    vals: Vec<i64>,
    last: Option<(u8, u8, i32)>,
    lists: Lists<'a>,
    /// Its key lists, the most recent first (as the module keeps them).
    recent: Vec<u32>,
    star: Option<u32>,
    elems: Vec<u32>,
}

/// The second pass: the streams.
struct Second<'a, 'b> {
    d: &'a [u8],
    style: Style,
    /// Scratch space for [`object_others`].
    others: Vec<u8>,
    first: &'b First<'a>,
    kinds: &'b [Kind1],
    paths: Vec<Path<'a>>,
    named: Named<'a>,
    keys: Vec<u8>,
    /// The shared part of `text`.
    shared: Vec<u8>,
    scratch: Vec<u8>,
    back: Vec<u8>,
    /// Whether there were more paths than [`MAX_PATHS`].
    over: bool,
}

impl<'a, 'b> Second<'a, 'b> {
    fn new(d: &'a [u8], style: Style, first: &'b First<'a>, kinds: &'b [Kind1]) -> Self {
        Second {
            d,
            style,
            others: Vec::new(),
            first,
            kinds,
            paths: Vec::new(),
            named: HashMap::new(),
            keys: Vec::new(),
            shared: Vec::new(),
            scratch: Vec::with_capacity(64),
            back: Vec::with_capacity(64),
            over: false,
        }
    }

    /// A new path, of the first pass's path `seen`.
    fn make(&mut self, seen: Option<u32>) -> u32 {
        if self.paths.len() == MAX_PATHS {
            self.over = true;
        }
        let kind = seen.map_or_else(Kind1::default, |s| self.kinds[s as usize]);
        self.paths.push(Path { seen, kind: Kind1 { width: kind.width.max(1), ..kind }, ..Path::default() });
        self.paths.len() as u32 - 1
    }

    fn value(&mut self, toks: &Toks, i: usize, p: u32) {
        let d = self.d;
        let path = &mut self.paths[p as usize];
        match toks.t[i] {
            Tok::Null => path.types.push(T_NULL),
            Tok::False => path.types.push(T_FALSE),
            Tok::True => path.types.push(T_TRUE),
            Tok::Str(a, b) => {
                path.types.push(T_STR);
                let text = if path.kind.shared { &mut self.shared } else { &mut path.text };
                text.extend_from_slice(&d[a..b]);
                text.push(b'\n');
            }
            Tok::Num(a, b) => {
                let t = &d[a..b];
                let num = path.kind.style.encode(t, path.last.map_or(0, |l| l.2), &mut self.scratch).filter(
                    |&(v, sc, mode, fl)| {
                        self.back.clear();
                        aqr_num::put(&mut self.back, v, sc, mode, fl);
                        self.back == t
                    },
                );
                match num {
                    Some((v, sc, mode, fl)) => {
                        path.vals.push(v);
                        if path.last == Some((mode, fl, sc)) {
                            path.types.push(T_NUM);
                        } else {
                            path.last = Some((mode, fl, sc));
                            path.desc.extend_from_slice(&[mode, fl]);
                            varint::put_signed(&mut path.desc, sc as i64);
                            path.types.push(T_NEW);
                        }
                    }
                    None => {
                        path.types.push(T_NUMTEXT);
                        let text = if path.kind.shared { &mut self.shared } else { &mut path.text };
                        text.extend_from_slice(t);
                        text.push(b'\n');
                    }
                }
            }
            Tok::Obj { n, .. } => {
                path.types.push(T_OBJ);
                if path.kind.map {
                    object_others(toks, i, self.style, &mut self.others);
                    let bits = self.others.iter().flat_map(|&o| [o & 1 != 0, o & 2 != 0]);
                    put_bits(&mut path.shapes, n as u64, 2 * n as usize + 2, bits);
                    let mut j = i + 1;
                    for _ in 0..n {
                        let Tok::Key(a, b) = toks.t[j] else { unreachable!() };
                        let path = &mut self.paths[p as usize];
                        path.names.extend_from_slice(&d[a..b]);
                        path.names.push(b'\n');
                        let c = match path.star {
                            Some(c) => c,
                            None => {
                                let s = path.seen.and_then(|s| self.first.seen[s as usize].star);
                                let c = self.make(s);
                                self.paths[p as usize].star = Some(c);
                                c
                            }
                        };
                        self.value(toks, j + 1, c);
                        j = skip(&toks.t, j + 1);
                    }
                } else {
                    let l = self.list(toks, i, p);
                    let kids = self.paths[p as usize].lists.lists[l as usize].kids.clone();
                    let mut j = i + 1;
                    for c in kids {
                        self.value(toks, j + 1, c);
                        j = skip(&toks.t, j + 1);
                    }
                }
            }
            Tok::Arr { n, .. } => {
                path.types.push(T_ARR);
                let comma = self.style.comma_space;
                let mut j = i + 1;
                let w = within(toks.seps[i]);
                let commas = (0..n).map(|e| {
                    let other = e > 0 && (toks.seps[j] & S_COMMA != 0) != comma;
                    j = skip(&toks.t, j);
                    other
                });
                let bits = [w & 1 != 0, w & 2 != 0].into_iter().chain(commas);
                put_bits(&mut path.shapes, n as u64, n as usize + 2, bits);
                let mut j = i + 1;
                for e in 0..n as usize {
                    let path = &self.paths[p as usize];
                    let k = e.min(path.kind.width - 1);
                    let c = match path.elems.get(k) {
                        Some(&c) => c,
                        None => {
                            let s = path.seen.and_then(|s| self.first.seen[s as usize].elem);
                            let c = self.make(s);
                            self.paths[p as usize].elems.push(c);
                            c
                        }
                    };
                    self.value(toks, j, c);
                    j = skip(&toks.t, j);
                }
            }
            Tok::Key(..) => unreachable!(),
        }
    }

    /// The key list of the object at `i` of path `p`: recalled, or defined (its members' paths
    /// found or made in order).
    fn list(&mut self, toks: &Toks, i: usize, p: u32) -> u32 {
        let d = self.d;
        object_others(toks, i, self.style, &mut self.others);
        let h = hash_keys(d, &toks.t, i, &self.others);
        let path = &mut self.paths[p as usize];
        let found = path.lists.find(d, &toks.t, i, &self.others, h);
        if let Some(l) = found
            && let Some(k) = path.recent.iter().position(|&r| r == l)
        {
            varint::put(&mut path.shapes, k as u64 + 1);
            path.recent.remove(k);
            path.recent.insert(0, l);
            return l;
        }
        // defined (again, where it was dropped)
        let keys = keys_of(d, &toks.t, i);
        path.shapes.push(0);
        let bits = self.others.iter().flat_map(|&o| [o & 1 != 0, o & 2 != 0]);
        put_bits(&mut path.shapes, keys.len() as u64, 2 * keys.len() + 2, bits);
        let mut kids = Vec::with_capacity(keys.len());
        for &k in &keys {
            self.keys.extend_from_slice(k);
            self.keys.push(b'\n');
            let c = match self.named.get(&(p, k)) {
                Some(&c) => c,
                None => {
                    let seen = self.paths[p as usize].seen;
                    let s = seen.and_then(|s| self.first.named.get(&(s, k)).copied());
                    let c = self.make(s);
                    self.named.insert((p, k), c);
                    c
                }
            };
            kids.push(c);
        }
        let path = &mut self.paths[p as usize];
        let l = found.unwrap_or_else(|| path.lists.add(h, keys, self.others.clone(), kids));
        if path.recent.len() == MAX_LISTS {
            path.recent.pop();
        }
        path.recent.insert(0, l);
        l
    }
}

/// The bytes of the first strings of a file that tell which paths' strings are shared.
const SHARED_SAMPLE: usize = 4 << 20;
/// Fewer bytes of strings than this are not shared.
const SHARED_MIN: usize = 64 << 10;
/// The paths of the most bytes of strings that are told apart; the others are told together.
const SHARED_TOP: usize = 32;

/// Of each path of the first pass, whether its strings are shared (in the order of the file)
/// rather than its own: where strings of a record repeat each other (the URLs of GitHub's events)
/// rather than those of a path (words of a text, ids). Told by zstd -3 of the strings of the
/// sample `strings`, each of the paths of the most bytes made shared in turn and the others
/// together, from none, where that makes them smaller.
fn shared_paths(d: &[u8], strings: &[(u32, usize, usize)], paths: usize) -> Vec<bool> {
    let mut shared = vec![false; paths];
    let mut bytes = vec![0usize; paths];
    for &(p, a, b) in strings {
        bytes[p as usize] += b - a + 1;
    }
    if bytes.iter().sum::<usize>() < SHARED_MIN {
        return shared;
    }
    let mut order: Vec<usize> = (0..paths).filter(|&p| bytes[p] > 0).collect();
    order.sort_by_key(|&p| (std::cmp::Reverse(bytes[p]), p));
    let (top, rest) = order.split_at(order.len().min(SHARED_TOP));
    let cost = |shared: &[bool]| {
        let mut own = vec![Vec::new(); paths];
        let mut row = Vec::new();
        for &(p, a, b) in strings {
            let t = if shared[p as usize] { &mut row } else { &mut own[p as usize] };
            t.extend_from_slice(&d[a..b]);
            t.push(b'\n');
        }
        let mut all = own.concat();
        all.extend_from_slice(&row);
        zstd::bulk::compress(&all, 3).map_or(all.len(), |c| c.len())
    };
    let mut best = cost(&shared);
    for unit in std::iter::once(rest).chain(top.chunks(1)) {
        if unit.is_empty() {
            continue;
        }
        unit.iter().for_each(|&p| shared[p] = true);
        let c = cost(&shared);
        if c < best {
            best = c;
        } else {
            unit.iter().for_each(|&p| shared[p] = false);
        }
    }
    shared
}

/// The values of a path, as the stream `nums` holds them as they are or as differences.
fn values(vals: &[i64], delta: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(vals.len() * 2);
    let mut prev = 0i64;
    for &v in vals {
        varint::put_signed(&mut out, if delta { v.wrapping_sub(prev) } else { v });
        prev = v;
    }
    out
}

/// The size of `b` compressed fast, which tells predictors apart.
fn packed(b: &[u8]) -> usize {
    zstd::bulk::compress(b, 1).map_or(b.len(), |c| c.len())
}

/// The formula of JSON lines `input`, or `None` where it is shorter than [`MIN_LEN`], does not
/// start as JSON lines ([`parse::style`]), a tenth of it or more is not of the style, or it has
/// more than [`MAX_PATHS`] paths. It is the same whatever `threads` is (one is used).
pub fn encode(input: &[u8], _threads: usize) -> Option<Formula> {
    if input.len() < MIN_LEN {
        return None;
    }
    let style = style_of(input)?;
    encode_as(input, style).filter(|(_, raw)| raw * 10 < input.len()).map(|f| f.0)
}

/// The bytes of lines of a segment at least (but the last), coded on its own (FORMAT.md,
/// "Encoding"): what the encoder and the module hold of a file at once.
pub const SEGMENT: usize = 64 << 20;

/// The formula of `input` as JSON lines of style `style` and the bytes of its lines kept as they
/// are, or `None` where a segment has more than [`MAX_PATHS`] paths.
pub fn encode_as(input: &[u8], style: Style) -> Option<(Formula, usize)> {
    encode_in(input, style, SEGMENT)
}

/// [`encode_as`] in segments of `segment` bytes of lines at least.
fn encode_in(input: &[u8], style: Style, segment: usize) -> Option<(Formula, usize)> {
    let d = input;
    // the first pass, over all of the file
    let mut toks = Toks::default();
    let mut first = First { d, seen: Vec::new(), named: HashMap::new(), strings: Vec::new(), string_bytes: 0 };
    let mut at = 0;
    while at < d.len() {
        let l = parse::line(d, at);
        at = l.next;
        if parse::tokens(d, &l, &mut toks) {
            if first.seen.is_empty() {
                first.make();
            }
            first.value(&toks.t, 0, 0);
        }
    }
    let shared = shared_paths(d, &first.strings, first.seen.len());
    let kinds: Vec<Kind1> = first.seen.iter().zip(shared).map(|(s, sh)| s.kind(sh)).collect();
    // the second, a segment at a time
    let mut f = Formula::default();
    let mut roots = Vec::new();
    let mut raw_len = 0;
    let mut at = 0;
    while at < d.len() || roots.is_empty() {
        let start = at;
        let mut s = Second::new(d, style, &first, &kinds);
        let (mut layout, mut raw) = (Vec::new(), Vec::new());
        while at < d.len() && at - start < segment {
            let l = parse::line(d, at);
            at = l.next;
            if parse::tokens(d, &l, &mut toks) {
                if s.paths.is_empty() {
                    s.make(Some(0));
                }
                s.value(&toks, 0, 0);
                varint::put(&mut layout, (l.term | if toks.comma { E_COMMA } else { 0 }) + 1);
            } else {
                layout.push(0);
                varint::put(&mut layout, (l.next - l.start) as u64);
                raw.extend_from_slice(&d[l.start..l.next]);
            }
        }
        if s.over {
            return None;
        }
        raw_len += raw.len();
        roots.push(s.finish(&mut f, layout, raw, (at - start) as u64));
    }
    f.root = match roots.len() {
        1 => roots.pop().unwrap(),
        _ => Node::concat(roots),
    };
    Some((f, raw_len))
}

impl Second<'_, '_> {
    /// The application of the segment of `len` bytes, its streams (of its `layout` and `raw`
    /// lines) put into `f` (and its store, [`aqr_formula::store::spill`]).
    fn finish(self, f: &mut Formula, layout: Vec<u8>, raw: Vec<u8>, len: u64) -> Node {
        let mut table = Vec::new();
        let [mut types, mut shapes, mut names, mut text, mut nums, mut desc] = [0; 6].map(|_| Vec::new());
        // (each path's parts let go as they are put in place)
        for p in self.paths {
            let (plain, delta) = (values(&p.vals, false), values(&p.vals, true));
            let (pred, n) =
                if !p.vals.is_empty() && packed(&delta) < packed(&plain) { (F_DELTA, delta) } else { (0, plain) };
            table.push(pred | if p.kind.map { F_MAP } else { 0 } | if p.kind.shared { F_SHARED } else { 0 });
            varint::put(&mut table, p.kind.width as u64);
            for (part, all) in [
                (&p.types, &mut types),
                (&p.shapes, &mut shapes),
                (&p.names, &mut names),
                (&p.text, &mut text),
                (&n, &mut nums),
                (&p.desc, &mut desc),
            ] {
                varint::put(&mut table, part.len() as u64);
                all.extend_from_slice(part);
            }
        }
        text.extend_from_slice(&self.shared);
        drop(self.shared);
        // (the strings, and the lines kept, text that aqr-compress recompresses in turn)
        let at = f.streams.len();
        let inputs = vec![
            f.push("jsonl/layout", layout),
            f.push_content("jsonl/raw", Kind::Text, raw),
            f.push("jsonl/paths", table),
            f.push_content("jsonl/keys", Kind::Text, self.keys),
            f.push("jsonl/types", types),
            f.push("jsonl/shapes", shapes),
            f.push_content("jsonl/names", Kind::Text, names),
            f.push_content("jsonl/text", Kind::Text, text),
            f.push("jsonl/nums", nums),
            f.push("jsonl/desc", desc),
        ];
        f.streams[at..].iter_mut().for_each(|s| aqr_formula::store::spill(&mut s.data));
        Node::apply(MODULE, self.style.to_params(), inputs, len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The file of formula `f` (an application, or segments' one after another), of `len`
    /// bytes in all.
    fn eval(f: &Formula, len: usize) -> crate::Result<Vec<u8>> {
        let apply = |a: &aqr_formula::Apply, len: u64| {
            let inputs: Vec<&[u8]> = a
                .inputs
                .iter()
                .map(|n| match n {
                    &Node::Slice { stream, start, len } => &f.streams[stream].data[start as usize..][..len as usize],
                    _ => panic!("not a slice"),
                })
                .collect();
            decode(&a.params, &inputs, len, 1)
        };
        match &f.root {
            Node::Apply(a) => apply(a, len as u64),
            Node::Concat(ns) => {
                let mut out = Vec::new();
                for n in ns {
                    let Node::Apply(a) = n else { panic!("not an application") };
                    out.extend(apply(a, a.len)?);
                }
                if out.len() == len { Ok(out) } else { Err(crate::corrupt()) }
            }
            _ => panic!("not a formula of aqr-jsonl"),
        }
    }

    fn roundtrip(d: &[u8], st: Style) -> Formula {
        let (f, _) = encode_as(d, st).unwrap();
        assert_eq!(eval(&f, d.len()).unwrap(), d, "{}", String::from_utf8_lossy(d));
        assert!(eval(&f, d.len() + 1).is_err());
        f
    }

    #[test]
    fn edge_cases() {
        let c = Style::default();
        for s in [
            &b"{\"a\":1}\n"[..],
            b"{\"a\":1}",
            b"{\"a\":1}\r\n[1,2]\r\n",
            b"[\n{\"a\":1},\n{\"a\":2},\r\n{\"a\":3}\n]\n",
            b"{\"a\":1},",
            b"\n\n{}\n[]\n\"x\"\n1\nnull\ntrue\nfalse\n",
            b"{\"a\":{\"b\":{\"c\":[[1],[2,[3]]]}},\"a\":2}\n",
            b"{\"k\\\"ey\":\"v\\u00e9\\n\\\\\",\"\":\"\"}\n",
            b"not json\n{\"a\":1} \n{\"a\": 1}\n{\"a\":1,}\n",
            b"[1,-0,0.10,1e5,1.5E+30,-1.5e-7,9223372036854775808,123456789012345678901234567890]\n",
            b"{\"x\":\xff\xfe}\n{\"x\":\"\xff\xfe\"}\n",
            b"{\"x\":\"a\0b\",\"\0\":0}\n",
            b"",
        ] {
            roundtrip(s, c);
        }
        let deep = "[".repeat(crate::tree::MAX_DEPTH) + &"]".repeat(crate::tree::MAX_DEPTH);
        roundtrip(deep.as_bytes(), c);
        let deeper = "[".repeat(crate::tree::MAX_DEPTH + 1) + &"]".repeat(crate::tree::MAX_DEPTH + 1);
        let f = roundtrip(deeper.as_bytes(), c);
        assert_eq!(f.streams[1].data.len(), deeper.len());
    }

    #[test]
    fn styles() {
        let s = "{\"a\": 1, \"b\": [1.5, \"x\"]}\n{\"b\": null, \"a\": 2}\n".repeat(500);
        for st in Style::ALL {
            roundtrip(s.as_bytes(), st);
        }
        let f = encode(s.as_bytes(), 1).unwrap();
        assert_eq!(eval(&f, s.len()).unwrap(), s.as_bytes());
        // (taken in the style of the file: nothing raw)
        assert!(f.streams[1].data.is_empty());
    }

    #[test]
    fn separators() {
        // separators of both kinds, in objects, maps and arrays
        let mut s = String::new();
        for i in 0..200 {
            s += &format!("{{\"type\": \"F\", \"p\": {{ \"h\": {i}}},\"g\": [ [1, 2],[3,4], [5, 6 ] ]}}\n");
            s += &format!("{{ \"e\": {{ }}, \"f\": [ ], \"g\": {{\"x{i}\": 1 }} }}\n");
            let sp = if i % 20 == 0 { " " } else { "" };
            s += &format!("{{\"m\":{{\"k{i}\": 1,\"j{i}\":2, \"l{i}\"{sp}:3}}, \"a\":[{i},{i}, {i}]}}\n");
        }
        for st in Style::ALL {
            roundtrip(s.as_bytes(), st);
        }
        let f = encode(s.as_bytes(), 1).unwrap();
        assert_eq!(eval(&f, s.len()).unwrap(), s.as_bytes());
        // (only the lines of `" :3` kept as they are)
        let raw: usize = s.lines().filter(|l| l.contains("\" :")).map(|l| l.len() + 1).sum();
        assert_eq!(f.streams[1].data.len(), raw);
    }

    #[test]
    fn maps_and_tuples() {
        let mut s = String::new();
        for i in 0..3000u32 {
            let h = i.wrapping_mul(2654435761);
            // a map of ids, tuples of coordinates, lists of keys that vary
            s += &format!(
                "{{\"id\":{i},\"files\":{{\"{h:08x}\":{{\"size\":{}}},\"{:08x}\":{{\"size\":1}}}},\"at\":[{}.{:02},{}.5]{}}}\n",
                i * 3,
                h ^ 1,
                i % 90,
                i % 100,
                i % 180,
                if i % 3 == 0 { ",\"extra\":true" } else { "" }
            );
        }
        let f = roundtrip(s.as_bytes(), Style::default());
        let Node::Apply(a) = &f.root else { panic!() };
        // root, id, files (a map), its member, size, at (a tuple), two elements, extra
        let flags: Vec<(u8, u64)> = path_flags(&f).into_iter().map(|(fl, w)| (fl & F_MAP, w)).collect();
        assert_eq!(flags.len(), 9, "{flags:?}");
        assert_eq!(flags[2], (F_MAP, 1));
        assert!(flags.contains(&(0, 2)));
        // (the keys of the files in `names`, not as key lists)
        assert!(f.streams[3].data.len() < 100, "{}", f.streams[3].data.len());
        assert!(!a.params.is_empty());
    }

    /// The flags of the paths of formula `f`.
    fn path_flags(f: &Formula) -> Vec<(u8, u64)> {
        let paths = &f.streams[2].data;
        let mut at = 0;
        let mut flags = Vec::new();
        while at < paths.len() {
            let fl = paths[at];
            at += 1;
            let w = varint::get(paths, &mut at).unwrap();
            for _ in 0..6 {
                varint::get(paths, &mut at).unwrap();
            }
            flags.push((fl, w));
        }
        flags
    }

    #[test]
    fn shared_strings() {
        // strings that repeat those of the same record (an id in URLs), and some that do not
        let mut rng = aqr_common::rng::Rng::new(7);
        let mut s = String::new();
        for i in 0..3000 {
            let id: String = (0..24).map(|_| (b'a' + rng.below(26) as u8) as char).collect();
            let kind = ["push", "pull", "issue"][i % 3];
            s += &format!(
                "{{\"id\":\"{id}\",\"type\":\"{kind}\",\"url\":\"https://api.example.com/repos/{id}/{kind}\",\"html\":\"https://example.com/{id}\"}}\n"
            );
        }
        let f = roundtrip(s.as_bytes(), Style::default());
        let flags = path_flags(&f);
        assert!(flags.iter().any(|&(fl, _)| fl & F_SHARED != 0), "{flags:?}");
        assert!(flags.iter().any(|&(fl, _)| fl & F_SHARED == 0), "{flags:?}");
    }

    #[test]
    fn segments() {
        // segments of a few lines each, coded on their own, and one of a line longer than they are
        let mut s = String::new();
        for i in 0..300 {
            s += &format!("{{\"i\":{i},\"s\":\"{}\",\"a\":[{i},{}]}}\n", "x".repeat(i % 7), i * 2);
            if i == 150 {
                s += &format!("{{\"long\":\"{}\"}}\nnot json\n", "y".repeat(5000));
            }
        }
        let d = s.as_bytes();
        let (one, _) = encode_in(d, Style::default(), usize::MAX).unwrap();
        assert!(matches!(one.root, Node::Apply(_)));
        let (f, raw) = encode_in(d, Style::default(), 1000).unwrap();
        let Node::Concat(ns) = &f.root else { panic!() };
        assert!(ns.len() >= 8, "{}", ns.len());
        assert_eq!(f.streams.len(), 10 * ns.len());
        assert_eq!(raw, 9);
        assert_eq!(eval(&f, d.len()).unwrap(), d);
        assert!(eval(&f, d.len() + 1).is_err());
    }

    #[test]
    fn many_lists() {
        // more key lists at a path than it keeps: some defined again
        let mut s = String::new();
        for i in 0..(MAX_LISTS * 3) {
            let k = (i * 7919) % (MAX_LISTS + 200);
            s += &format!("{{\"a\":{{\"x{}\":1,\"y{}\":2,\"z\":{}}}}}\n", k % 40, k / 40, i);
        }
        roundtrip(s.as_bytes(), Style::default());
    }

    #[test]
    fn numbers() {
        let mut s = String::new();
        for i in 0..2000 {
            let x = (i as f64 * 0.37).sin() * 1000.0;
            s += &format!("{{\"i\":{i},\"x\":{x},\"f\":{},\"t\":{:.3}}}\n", x as f32, 1.7e9 + i as f64 * 0.25);
        }
        let f = roundtrip(s.as_bytes(), Style::default());
        // (the numbers coded: no text)
        assert!(f.streams[7].data.is_empty(), "{}", f.streams[7].data.len());
    }

    #[test]
    fn not_jsonl() {
        assert!(encode(&b"words and more words\n".repeat(2000), 1).is_none());
        assert!(encode(&b"a,b\n1,2\n".repeat(2000), 1).is_none());
        assert!(encode(&b"{\"a\":1}\n".repeat(100), 1).is_none());
        assert!(starts(&b"{\"a\":1}\n".repeat(3000)));
    }

    #[test]
    fn corrupt_input() {
        let s = "{\"a\":[1,2.5,\"x\"],\"b\":{\"c\":null}}\n[true,false]\nraw\n".repeat(10);
        let f = roundtrip(s.as_bytes(), Style::default());
        let Node::Apply(a) = &f.root else { panic!() };
        let mut rng = aqr_common::rng::Rng::new(5);
        for _ in 0..20000 {
            let mut streams: Vec<Vec<u8>> = f.streams.iter().map(|s| s.data.to_vec()).collect();
            let k = rng.below(streams.len() as u64) as usize;
            if streams[k].is_empty() {
                continue;
            }
            let i = rng.below(streams[k].len() as u64) as usize;
            streams[k][i] = rng.below(256) as u8;
            let inputs: Vec<&[u8]> = streams.iter().map(|s| &s[..]).collect();
            let _ = decode(&a.params, &inputs, s.len() as u64, 1);
        }
    }
}
