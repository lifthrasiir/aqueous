//! What the encoder and the module share: the style of a file (its params), the codes of the
//! streams, and the file rebuilt from its streams (`FORMAT.md`, "Rebuilding the file").

use std::collections::BTreeMap;

use aqr_common::varint;

use crate::{Result, corrupt};

/// The most paths a file has.
pub const MAX_PATHS: usize = 1 << 20;
/// The most children of an array path (elements from the last on share the last).
pub const MAX_WIDTH: u64 = 64;
/// The most key lists a path keeps; defining one more drops the least recent.
pub const MAX_LISTS: usize = 1024;
/// The most containers a value nests in (the line's value at depth 0).
pub const MAX_DEPTH: usize = 64;

/// A value's type (a byte of `types`).
pub const T_NULL: u8 = 0;
pub const T_FALSE: u8 = 1;
pub const T_TRUE: u8 = 2;
/// A string: its bytes between the quotes in `text`.
pub const T_STR: u8 = 3;
/// A number of the descriptor of the path's previous number.
pub const T_NUM: u8 = 4;
/// A number of a descriptor given (`desc`).
pub const T_NEW: u8 = 5;
/// A number as it is written, in `text`.
pub const T_NUMTEXT: u8 = 6;
pub const T_OBJ: u8 = 7;
pub const T_ARR: u8 = 8;

/// The end of a line: LF, CR LF, or nothing (the last line of a file).
pub const E_LF: u64 = 0;
pub const E_CRLF: u64 = 1;
pub const E_NONE: u64 = 2;
/// With a comma before the end of the line (the lines of a JSON array, an element each).
pub const E_COMMA: u64 = 4;

/// A path's flags: its objects' members all in one child (keys as data, in `names`).
pub const F_MAP: u8 = 1;
/// Its numbers as differences from the path's previous one.
pub const F_DELTA: u8 = 2;
/// Its strings in the part of `text` shared by such paths, in the order of the file.
pub const F_SHARED: u8 = 4;

/// How a file's values are written: a space after a comma, after a colon, or none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub comma_space: bool,
    pub colon_space: bool,
}

impl Style {
    pub const ALL: [Style; 4] = [
        Style { comma_space: false, colon_space: false },
        Style { comma_space: true, colon_space: true },
        Style { comma_space: false, colon_space: true },
        Style { comma_space: true, colon_space: false },
    ];

    pub fn to_params(self) -> Vec<u8> {
        vec![self.comma_space as u8 | (self.colon_space as u8) << 1]
    }

    pub fn from_params(p: &[u8]) -> Result<Style> {
        match p {
            &[b] if b < 4 => Ok(Style { comma_space: b & 1 != 0, colon_space: b & 2 != 0 }),
            _ => Err(corrupt()),
        }
    }

    /// A comma, of the style or (`other`) not.
    pub fn comma(self, other: bool) -> &'static [u8] {
        if self.comma_space != other { b", " } else { b"," }
    }

    /// A colon, of the style or (`other`) not.
    pub fn colon(self, other: bool) -> &'static [u8] {
        if self.colon_space != other { b": " } else { b":" }
    }
}

/// The streams of a file (`FORMAT.md`, "Streams"), as the module takes them.
pub struct View<'a> {
    pub layout: &'a [u8],
    pub raw: &'a [u8],
    pub paths: &'a [u8],
    pub keys: &'a [u8],
    pub types: &'a [u8],
    pub shapes: &'a [u8],
    pub names: &'a [u8],
    pub text: &'a [u8],
    pub nums: &'a [u8],
    pub desc: &'a [u8],
}

/// No path (a child not made yet).
const NONE: u32 = u32::MAX;

/// A path's parts of the streams, read from the front, and its state.
struct Path<'a> {
    flags: u8,
    width: usize,
    types: &'a [u8],
    shapes: &'a [u8],
    names: &'a [u8],
    text: &'a [u8],
    nums: &'a [u8],
    desc: &'a [u8],
    /// The descriptor (mode, flags, scale) of the path's previous number, if any.
    last: Option<(u8, u8, i32)>,
    prev: i64,
    /// Its key lists (of [`Decoder::lists`]), the most recent first.
    lists: Vec<u32>,
    /// The child of the members of its objects (a map's), and of its arrays' elements.
    star: u32,
    elems: Vec<u32>,
}

/// A key list: the keys, the paths of their members, and their separators not of the style.
struct List<'a> {
    keys: Vec<&'a [u8]>,
    kids: Vec<u32>,
    others: Option<&'a [u8]>,
}

/// The bits of `n` separators not of the style, after a count `m` (its bit 0 telling whether
/// there are any).
fn others<'a>(s: &mut &'a [u8], m: u64, n: u64) -> Result<Option<&'a [u8]>> {
    if m & 1 == 0 { Ok(None) } else { take(s, n.div_ceil(8)).map(Some) }
}

/// The space after a container's opening, where `bits` tells (bit 0).
#[inline]
fn open(bits: Option<&[u8]>, out: &mut Vec<u8>) {
    if bit(bits, 0) {
        out.push(b' ');
    }
}

/// The space before the closing of a container of `n` members or elements, where `bits` tells
/// (bit 1; none where it is empty).
#[inline]
fn close(n: u64, bits: Option<&[u8]>, out: &mut Vec<u8>) {
    if n > 0 && bit(bits, 1) {
        out.push(b' ');
    }
}

/// Bit `k` of `bits`, if any.
#[inline]
fn bit(bits: Option<&[u8]>, k: u64) -> bool {
    bits.is_some_and(|b| b[(k >> 3) as usize] >> (k & 7) & 1 != 0)
}

/// Takes the first `n` bytes of `s`.
fn take<'a>(s: &mut &'a [u8], n: u64) -> Result<&'a [u8]> {
    let n = usize::try_from(n).map_err(|_| corrupt())?;
    if n > s.len() {
        return Err(corrupt());
    }
    let (a, b) = s.split_at(n);
    *s = b;
    Ok(a)
}

fn get(s: &mut &[u8]) -> Result<u64> {
    let mut at = 0;
    let v = varint::get(s, &mut at).map_err(|_| corrupt())?;
    *s = &s[at..];
    Ok(v)
}

fn byte(s: &mut &[u8]) -> Result<u8> {
    let (&b, rest) = s.split_first().ok_or_else(corrupt)?;
    *s = rest;
    Ok(b)
}

/// The bytes of `s` up to LF, which is taken too (no string of a line holds one).
fn zstring<'a>(s: &mut &'a [u8]) -> Result<&'a [u8]> {
    let n = s.iter().position(|&b| b == b'\n').ok_or_else(corrupt)?;
    let a = &s[..n];
    *s = &s[n + 1..];
    Ok(a)
}

struct Decoder<'a> {
    style: Style,
    paths: Vec<Path<'a>>,
    /// The paths made so far (the first of `paths`).
    made: usize,
    lists: Vec<List<'a>>,
    /// The paths of objects' members by their parent and key.
    named: BTreeMap<(u32, &'a [u8]), u32>,
    keys: &'a [u8],
    /// The part of `text` after the paths' own.
    shared: &'a [u8],
    len: usize,
}

impl<'a> Decoder<'a> {
    /// The next path, made.
    fn make(&mut self) -> Result<u32> {
        if self.made == self.paths.len() {
            return Err(corrupt());
        }
        self.made += 1;
        Ok(self.made as u32 - 1)
    }

    /// The value at path `p`, at `depth`, appended to `out`.
    fn value(&mut self, p: u32, depth: usize, out: &mut Vec<u8>) -> Result<()> {
        let path = &mut self.paths[p as usize];
        match byte(&mut path.types)? {
            T_NULL => out.extend_from_slice(b"null"),
            T_FALSE => out.extend_from_slice(b"false"),
            T_TRUE => out.extend_from_slice(b"true"),
            T_STR => {
                out.push(b'"');
                let text = if path.flags & F_SHARED != 0 { &mut self.shared } else { &mut path.text };
                out.extend_from_slice(zstring(text)?);
                out.push(b'"');
            }
            T_NUMTEXT => {
                let text = if path.flags & F_SHARED != 0 { &mut self.shared } else { &mut path.text };
                out.extend_from_slice(zstring(text)?)
            }
            k @ (T_NUM | T_NEW) => {
                if k == T_NEW {
                    let (mode, f) = (byte(&mut path.desc)?, byte(&mut path.desc)?);
                    let s = varint::unzz(get(&mut path.desc)?);
                    if !aqr_num::valid(mode, f) || !(aqr_num::MIN_SCALE as i64..=aqr_num::MAX_SCALE as i64).contains(&s)
                    {
                        return Err(corrupt());
                    }
                    path.last = Some((mode, f, s as i32));
                }
                let (mode, f, s) = path.last.ok_or_else(corrupt)?;
                let r = varint::unzz(get(&mut path.nums)?);
                let v = if path.flags & F_DELTA != 0 { path.prev.wrapping_add(r) } else { r };
                path.prev = v;
                aqr_num::put(out, v, s, mode, f);
            }
            T_OBJ if depth < MAX_DEPTH => self.object(p, depth, out)?,
            T_ARR if depth < MAX_DEPTH => self.array(p, depth, out)?,
            _ => return Err(corrupt()),
        }
        Ok(())
    }

    fn object(&mut self, p: u32, depth: usize, out: &mut Vec<u8>) -> Result<()> {
        out.push(b'{');
        let path = &mut self.paths[p as usize];
        let (n, bits) = if path.flags & F_MAP != 0 {
            let m = get(&mut path.shapes)?;
            let n = m >> 1;
            let bits = others(&mut path.shapes, m, 2 * n + 2)?;
            open(bits, out);
            for i in 0..n {
                let path = &mut self.paths[p as usize];
                let key = zstring(&mut path.names)?;
                if path.star == NONE {
                    let c = self.make()?;
                    self.paths[p as usize].star = c;
                }
                let c = self.paths[p as usize].star;
                self.member(i, key, c, bits, depth, out)?;
            }
            (n, bits)
        } else {
            let s = get(&mut path.shapes)?;
            let l = if s == 0 { self.define(p)? } else { self.recall(p, s - 1)? };
            let (n, bits) = (self.lists[l as usize].keys.len(), self.lists[l as usize].others);
            open(bits, out);
            for i in 0..n {
                let list = &self.lists[l as usize];
                let (key, c) = (list.keys[i], list.kids[i]);
                self.member(i as u64, key, c, bits, depth, out)?;
            }
            (n as u64, bits)
        };
        close(n, bits, out);
        out.push(b'}');
        Ok(())
    }

    /// Member `i` of an object, of key `key` and path `c`, its separators not of the style where
    /// `bits` tells (bit 2i + 2 its colon, 2i + 3 the comma before it).
    fn member(
        &mut self,
        i: u64,
        key: &[u8],
        c: u32,
        bits: Option<&[u8]>,
        depth: usize,
        out: &mut Vec<u8>,
    ) -> Result<()> {
        if i > 0 {
            out.extend_from_slice(self.style.comma(bit(bits, 2 * i + 3)));
        }
        out.push(b'"');
        out.extend_from_slice(key);
        out.push(b'"');
        out.extend_from_slice(self.style.colon(bit(bits, 2 * i + 2)));
        self.value(c, depth + 1, out)?;
        // (a corrupt shape of many empty members stops here)
        if out.len() > self.len { Err(corrupt()) } else { Ok(()) }
    }

    /// A key list of path `p` defined, its members' paths found or made in order.
    fn define(&mut self, p: u32) -> Result<u32> {
        let shapes = &mut self.paths[p as usize].shapes;
        let m = get(shapes)?;
        let n = m >> 1;
        let bits = others(shapes, m, 2 * n + 2)?;
        let mut keys = Vec::new();
        let mut kids = Vec::new();
        for _ in 0..n {
            let key = zstring(&mut self.keys)?;
            let c = match self.named.get(&(p, key)) {
                Some(&c) => c,
                None => {
                    let c = self.make()?;
                    self.named.insert((p, key), c);
                    c
                }
            };
            keys.push(key);
            kids.push(c);
        }
        let l = self.lists.len() as u32;
        self.lists.push(List { keys, kids, others: bits });
        let lists = &mut self.paths[p as usize].lists;
        if lists.len() == MAX_LISTS {
            lists.pop();
        }
        lists.insert(0, l);
        Ok(l)
    }

    /// The `k`-th most recent key list of path `p`, made the most recent.
    fn recall(&mut self, p: u32, k: u64) -> Result<u32> {
        let lists = &mut self.paths[p as usize].lists;
        let k = usize::try_from(k).ok().filter(|&k| k < lists.len()).ok_or_else(corrupt)?;
        let l = lists.remove(k);
        lists.insert(0, l);
        Ok(l)
    }

    fn array(&mut self, p: u32, depth: usize, out: &mut Vec<u8>) -> Result<()> {
        out.push(b'[');
        let shapes = &mut self.paths[p as usize].shapes;
        let m = get(shapes)?;
        let n = m >> 1;
        let bits = others(shapes, m, n + 2)?;
        open(bits, out);
        for i in 0..n {
            if i > 0 {
                out.extend_from_slice(self.style.comma(bit(bits, i + 2)));
            }
            let path = &self.paths[p as usize];
            let j = (i as usize).min(path.width - 1);
            let c = match path.elems.get(j) {
                Some(&c) => c,
                None => {
                    let c = self.make()?;
                    self.paths[p as usize].elems.push(c);
                    c
                }
            };
            self.value(c, depth + 1, out)?;
            if out.len() > self.len {
                return Err(corrupt());
            }
        }
        close(n, bits, out);
        out.push(b']');
        Ok(())
    }
}

/// The file of `len` bytes of style `style` from its streams `v`.
pub fn build(style: Style, v: View, len: usize) -> Result<Vec<u8>> {
    let View { mut layout, mut raw, mut paths, keys, mut types, mut shapes, mut names, mut text, mut nums, mut desc } =
        v;
    let mut table = Vec::new();
    while !paths.is_empty() {
        if table.len() == MAX_PATHS {
            return Err(corrupt());
        }
        let flags = byte(&mut paths)?;
        let width = get(&mut paths)?;
        if flags > (F_MAP | F_DELTA | F_SHARED) || !(1..=MAX_WIDTH).contains(&width) {
            return Err(corrupt());
        }
        let mut n = [0; 6];
        for n in &mut n {
            *n = get(&mut paths)?;
        }
        let [nt, ns, nn, nx, nu, nd] = n;
        table.push(Path {
            flags,
            width: width as usize,
            types: take(&mut types, nt)?,
            shapes: take(&mut shapes, ns)?,
            names: take(&mut names, nn)?,
            text: take(&mut text, nx)?,
            nums: take(&mut nums, nu)?,
            desc: take(&mut desc, nd)?,
            last: None,
            prev: 0,
            lists: Vec::new(),
            star: NONE,
            elems: Vec::new(),
        });
    }
    // (the rest of `text` shared)
    if [types, shapes, names, nums, desc].iter().any(|s| !s.is_empty()) {
        return Err(corrupt());
    }
    let mut d =
        Decoder { style, paths: table, made: 0, lists: Vec::new(), named: BTreeMap::new(), keys, shared: text, len };
    let mut out = Vec::with_capacity(len);
    while !layout.is_empty() {
        let r = get(&mut layout)?;
        if r == 0 {
            let n = get(&mut layout)?;
            out.extend_from_slice(take(&mut raw, n)?);
        } else {
            // (the root, made by the first line of a value)
            if d.made == 0 {
                d.make()?;
            }
            d.value(0, 0, &mut out)?;
            let e = r - 1;
            if e & E_COMMA != 0 {
                out.push(b',');
            }
            match e & !E_COMMA {
                E_LF => out.push(b'\n'),
                E_CRLF => out.extend_from_slice(b"\r\n"),
                E_NONE => {}
                _ => return Err(corrupt()),
            }
        }
        if out.len() > len {
            return Err(corrupt());
        }
    }
    let left = d.paths.iter().any(|p| {
        !(p.types.is_empty()
            && p.shapes.is_empty()
            && p.names.is_empty()
            && p.text.is_empty()
            && p.nums.is_empty()
            && p.desc.is_empty())
    });
    if left
        || d.made != d.paths.len()
        || !raw.is_empty()
        || !d.keys.is_empty()
        || !d.shared.is_empty()
        || out.len() != len
    {
        return Err(corrupt());
    }
    Ok(out)
}
