//! `.debug_info` (FORMAT.md §3): its units' DIEs parsed by the abbreviations of `.debug_abbrev`,
//! their values coded by what they are: references to DIEs as indices of DIEs (a sibling's
//! derived from where its subtree ends), strings of `.debug_str` as indices of its strings (the
//! next one not used yet predicted), addresses, lines and offsets as deltas, each kind in a
//! stream of its own. A unit not rebuilt so is kept as it is.

use std::collections::HashMap;
use std::rc::Rc;

use aqr_common::varint;

use crate::{Result, corrupt};

/// The streams, and the inputs they are packed into (after `.debug_abbrev` and `.debug_str`), by
/// their indices.
pub const INPUTS: [(&str, &[usize]); 3] = [
    ("die", &[CODE, STR, REF]),
    ("num", &[ADDR, LINE, CALL, FILE, SEC, IDX, DATA, PCLEN]),
    ("bytes", &[UNIT, EXPR, CSTR, RAW]),
];
pub const STREAM_NAMES: [&str; 15] = [
    "unit", "code", "str", "ref", "addr", "line", "file", "sec", "idx", "data", "expr", "cstr", "raw", "call", "pclen",
];
const UNIT: usize = 0;
const CODE: usize = 1;
const STR: usize = 2;
const REF: usize = 3;
const ADDR: usize = 4;
const LINE: usize = 5;
const FILE: usize = 6;
const SEC: usize = 7;
const IDX: usize = 8;
const DATA: usize = 9;
const EXPR: usize = 10;
const CSTR: usize = 11;
const RAW: usize = 12;
const CALL: usize = 13;
const PCLEN: usize = 14;
pub const NUM_STREAMS: usize = STREAM_NAMES.len();

// attributes
const DW_AT_SIBLING: u64 = 0x01;
const DW_AT_LOW_PC: u64 = 0x11;
const DW_AT_HIGH_PC: u64 = 0x12;
const DW_AT_DECL_COLUMN: u64 = 0x39;
const DW_AT_DECL_FILE: u64 = 0x3a;
const DW_AT_DECL_LINE: u64 = 0x3b;
const DW_AT_CALL_COLUMN: u64 = 0x57;
const DW_AT_CALL_FILE: u64 = 0x58;
const DW_AT_CALL_LINE: u64 = 0x59;

// forms
const DW_FORM_ADDR: u64 = 0x01;
const DW_FORM_BLOCK2: u64 = 0x03;
const DW_FORM_BLOCK4: u64 = 0x04;
const DW_FORM_DATA2: u64 = 0x05;
const DW_FORM_DATA4: u64 = 0x06;
const DW_FORM_DATA8: u64 = 0x07;
const DW_FORM_STRING: u64 = 0x08;
const DW_FORM_BLOCK: u64 = 0x09;
const DW_FORM_BLOCK1: u64 = 0x0a;
const DW_FORM_DATA1: u64 = 0x0b;
const DW_FORM_FLAG: u64 = 0x0c;
const DW_FORM_SDATA: u64 = 0x0d;
const DW_FORM_STRP: u64 = 0x0e;
const DW_FORM_UDATA: u64 = 0x0f;
const DW_FORM_REF_ADDR: u64 = 0x10;
const DW_FORM_REF1: u64 = 0x11;
const DW_FORM_REF2: u64 = 0x12;
const DW_FORM_REF4: u64 = 0x13;
const DW_FORM_REF8: u64 = 0x14;
const DW_FORM_REF_UDATA: u64 = 0x15;
const DW_FORM_INDIRECT: u64 = 0x16;
const DW_FORM_SEC_OFFSET: u64 = 0x17;
const DW_FORM_EXPRLOC: u64 = 0x18;
const DW_FORM_FLAG_PRESENT: u64 = 0x19;
const DW_FORM_STRX: u64 = 0x1a;
const DW_FORM_ADDRX: u64 = 0x1b;
const DW_FORM_REF_SUP4: u64 = 0x1c;
const DW_FORM_STRP_SUP: u64 = 0x1d;
const DW_FORM_DATA16: u64 = 0x1e;
const DW_FORM_LINE_STRP: u64 = 0x1f;
const DW_FORM_REF_SIG8: u64 = 0x20;
const DW_FORM_IMPLICIT_CONST: u64 = 0x21;
const DW_FORM_LOCLISTX: u64 = 0x22;
const DW_FORM_RNGLISTX: u64 = 0x23;
const DW_FORM_REF_SUP8: u64 = 0x24;
const DW_FORM_STRX1: u64 = 0x25;
const DW_FORM_STRX4: u64 = 0x28;
const DW_FORM_ADDRX1: u64 = 0x29;
const DW_FORM_ADDRX4: u64 = 0x2c;
const DW_FORM_GNU_ADDR_INDEX: u64 = 0x1f01;
const DW_FORM_GNU_STR_INDEX: u64 = 0x1f02;
const DW_FORM_GNU_REF_ALT: u64 = 0x1f20;
const DW_FORM_GNU_STRP_ALT: u64 = 0x1f21;

const DW_UT_COMPILE: u8 = 1;
const DW_UT_TYPE: u8 = 2;
const DW_UT_PARTIAL: u8 = 3;
const DW_UT_SKELETON: u8 = 4;
const DW_UT_SPLIT_COMPILE: u8 = 5;
const DW_UT_SPLIT_TYPE: u8 = 6;

/// Abbreviation codes above this are not taken (the tables are indexed by code).
const MAX_CODE: u64 = 1 << 16;

/// An abbreviation: whether its DIEs have children, and its attributes (attribute, form,
/// implicit constant).
#[derive(Debug, Clone, Default)]
struct Abbrev {
    children: bool,
    attrs: Vec<(u64, u64)>,
}

/// The abbreviation tables of `.debug_abbrev`, parsed as units ask for them, by offset: each by
/// code (`None` where there is none).
struct Abbrevs<'a> {
    d: &'a [u8],
    tables: HashMap<u64, Rc<[Option<Abbrev>]>>,
    /// The slots and attributes that tables parsed may still take ([`budget`]).
    left: usize,
}

/// The slots of tables (one for each code up to the highest) and attributes that the tables
/// parsed of `.debug_abbrev` of `len` bytes may take in all, so that units naming tables at
/// many offsets cannot make the decoder take much more memory than the section.
pub fn budget(len: usize) -> usize {
    4 * len + 256
}

fn uleb(d: &[u8], at: &mut usize) -> Option<u64> {
    let start = *at;
    let v = varint::get(d, at).ok()?;
    // (only the shortest encodings are taken, as they are rebuilt so)
    (*at - start == varint::len(v)).then_some(v)
}

fn sleb(d: &[u8], at: &mut usize) -> Option<i64> {
    let (mut v, mut shift, start) = (0i64, 0, *at);
    loop {
        let b = *d.get(*at)?;
        *at += 1;
        if shift == 63 && b != 0 && b != 0x7f {
            return None;
        }
        v |= ((b & 0x7f) as i64) << shift;
        shift += 7;
        if b < 0x80 {
            if shift < 64 && b & 0x40 != 0 {
                v |= -1i64 << shift;
            }
            break;
        }
        if shift > 63 {
            return None;
        }
    }
    let mut back = Vec::new();
    put_sleb(&mut back, v);
    (back.len() == *at - start).then_some(v)
}

fn put_sleb(out: &mut Vec<u8>, mut v: i64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if (v == 0 && b & 0x40 == 0) || (v == -1 && b & 0x40 != 0) {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

impl<'a> Abbrevs<'a> {
    fn new(d: &'a [u8]) -> Abbrevs<'a> {
        Abbrevs { d, tables: HashMap::new(), left: budget(d.len()) }
    }

    /// The table at `off`, `None` where it does not parse. Implicit constants are kept in
    /// `.debug_abbrev`: the DIEs hold nothing of them.
    fn table(&mut self, off: u64) -> Option<Rc<[Option<Abbrev>]>> {
        if !self.tables.contains_key(&off) {
            let t = table_of(self.d, usize::try_from(off).ok()?)?;
            let size = t.len() + t.iter().flatten().map(|a| a.attrs.len()).sum::<usize>();
            self.left = self.left.checked_sub(size)?;
            self.tables.insert(off, t.into());
        }
        self.tables.get(&off).cloned()
    }
}

/// What [`parse_table`] gives of a table, in order: each abbreviation (its code, and whether
/// its DIEs have children), then its attributes (attribute, form).
enum Item {
    Abbrev(usize, bool),
    Attr(u64, u64),
}

/// Parses the table at `at` of `d`, giving `f` what it holds. `None` where it does not parse
/// (or `f` gives `None`). A code defined twice is for `f` to turn down.
fn parse_table(d: &[u8], mut at: usize, mut f: impl FnMut(Item) -> Option<()>) -> Option<()> {
    let mut entries = 0u64;
    loop {
        let code = varint::get(d, &mut at).ok()?;
        if code == 0 {
            return Some(());
        }
        // (tables are dense: a code far beyond the entries would cost slots for nothing)
        if code > MAX_CODE || code > 1024 + 16 * entries {
            return None;
        }
        entries += 1;
        let _tag = varint::get(d, &mut at).ok()?;
        let children = *d.get(at)? != 0;
        at += 1;
        f(Item::Abbrev(code as usize, children))?;
        loop {
            let attr = varint::get(d, &mut at).ok()?;
            let form = varint::get(d, &mut at).ok()?;
            if attr == 0 && form == 0 {
                break;
            }
            if form == DW_FORM_IMPLICIT_CONST {
                // (any LEB128 of it: it is not in the DIEs)
                loop {
                    let b = *d.get(at)?;
                    at += 1;
                    if b < 0x80 {
                        break;
                    }
                }
            }
            f(Item::Attr(attr, form))?;
        }
    }
}

/// The table at `at` of `d` by code (`None` where there is none).
fn table_of(d: &[u8], at: usize) -> Option<Vec<Option<Abbrev>>> {
    let mut t: Vec<Option<Abbrev>> = Vec::new();
    let mut code = 0;
    parse_table(d, at, |item| {
        match item {
            Item::Abbrev(c, children) => {
                if t.len() <= c {
                    t.resize(c + 1, None);
                }
                if t[c].is_some() {
                    return None;
                }
                t[c] = Some(Abbrev { children, attrs: Vec::new() });
                code = c;
            }
            Item::Attr(attr, form) => t[code].as_mut()?.attrs.push((attr, form)),
        }
        Some(())
    })?;
    Some(t)
}

/// The abbreviation tables a decoder takes, laid out flat: each table's entries (one for each
/// code up to its highest), each entry's attributes.
struct Tables {
    /// The tables by offset, in order: (offset, first slot, slots).
    tables: Vec<(u64, u32, u32)>,
    slots: Vec<Entry>,
    attrs: Vec<(u64, u64)>,
}

/// An abbreviation of a table: its attributes (from `first`, `n` of them; `n` is `NONE` where
/// the slot's code is none), and whether its DIEs have children.
#[derive(Clone, Copy)]
struct Entry {
    first: u32,
    n: u32,
    children: bool,
}

const NONE: u32 = u32::MAX;

/// Bytes of a table, a slot and an attribute on WebAssembly, for the plans.
const TABLE_BYTES: u64 = 16;
const SLOT_BYTES: u64 = 12;
const ATTR_BYTES: u64 = 16;
#[cfg(target_family = "wasm")]
const _: () = assert!(
    size_of::<(u64, u32, u32)>() as u64 == TABLE_BYTES
        && size_of::<Entry>() as u64 == SLOT_BYTES
        && size_of::<(u64, u64)>() as u64 == ATTR_BYTES
);

impl Tables {
    /// The tables of `.debug_abbrev` `d` at `offs` (in order, each once), within
    /// [`budget`]; `None` where one does not parse.
    fn new(d: &[u8], offs: &[u64]) -> Option<Tables> {
        let (slots, attrs) = Tables::sizes(d, offs)?;
        let mut t = Tables {
            tables: Vec::with_capacity(offs.len()),
            slots: vec![Entry { first: 0, n: NONE, children: false }; slots],
            attrs: Vec::with_capacity(attrs),
        };
        let mut base = 0;
        for &off in offs {
            let (mut n, mut cur) = (0, 0);
            let (slots, attrs) = (&mut t.slots, &mut t.attrs);
            parse_table(d, off as usize, |item| {
                match item {
                    Item::Abbrev(code, children) => {
                        let s = slots.get_mut(base + code)?;
                        if s.n != NONE {
                            return None;
                        }
                        *s = Entry { first: attrs.len() as u32, n: 0, children };
                        (cur, n) = (base + code, n.max(code + 1));
                    }
                    Item::Attr(attr, form) => {
                        attrs.push((attr, form));
                        slots[cur].n += 1;
                    }
                }
                Some(())
            })?;
            t.tables.push((off, base as u32, n as u32));
            base += n;
        }
        Some(t)
    }

    /// The slots and attributes of the tables at `offs`, within [`budget`].
    fn sizes(d: &[u8], offs: &[u64]) -> Option<(usize, usize)> {
        let (mut slots, mut attrs) = (0, 0);
        for &off in offs {
            let mut n = 0;
            parse_table(d, usize::try_from(off).ok()?, |item| {
                match item {
                    Item::Abbrev(code, _) => n = n.max(code + 1),
                    Item::Attr(..) => attrs += 1,
                }
                Some(())
            })?;
            slots += n;
        }
        (slots + attrs <= budget(d.len())).then_some((slots, attrs))
    }

    /// The memory plan of the tables ([`aqr_api::stack`]) of `.debug_abbrev` `d` at `offs`.
    fn plan(d: &[u8], offs: &[u64]) -> Option<u64> {
        use aqr_api::stack::block;
        let (slots, attrs) = Tables::sizes(d, offs)?;
        Some(
            block(TABLE_BYTES * offs.len() as u64)
                + block(SLOT_BYTES * slots as u64)
                + block(ATTR_BYTES * attrs as u64),
        )
    }

    /// The table at `off`: its slots.
    fn table(&self, off: u64) -> Option<&[Entry]> {
        let k = self.tables.binary_search_by_key(&off, |t| t.0).ok()?;
        let (_, first, n) = self.tables[k];
        Some(&self.slots[first as usize..][..n as usize])
    }

    fn attrs(&self, s: &Entry) -> &[(u64, u64)] {
        &self.attrs[s.first as usize..][..s.n as usize]
    }
}

/// Strings of a string section: each but the last ends with a NUL.
fn strings(s: &[u8]) -> usize {
    s.iter().filter(|&&c| c == 0).count() + (s.last().is_some_and(|&c| c != 0)) as usize
}

/// The strings of a string section: the offset of each, in order; `None` where there are more
/// than `most`.
fn string_starts(s: &[u8], most: usize) -> Option<Vec<u32>> {
    let mut starts = Vec::with_capacity(most);
    let mut at = 0;
    while at < s.len() {
        if starts.len() == most {
            return None;
        }
        starts.push(at as u32);
        at += s[at..].iter().position(|&c| c == 0).map_or(s.len() - at, |p| p + 1);
    }
    Some(starts)
}

/// What a unit's header holds of how its DIEs are read.
#[derive(Debug, Clone, Copy)]
struct UnitHead {
    version: u16,
    addr_size: u8,
}

/// The state both sides keep of what was coded so far, of which values are predicted.
struct State {
    /// The next string of `.debug_str` not used yet (by index), and the previous used.
    str_next: u32,
    str_prev: Vec<u32>,
    str_cache: Vec<u32>,
    /// The previous value of each attribute (by a hash of it), of lines and offsets.
    prev: Vec<u64>,
    prev_addr: u64,
    prev_strx: u64,
    prev_addrx: u64,
    prev_abbrev_off: u64,
    /// The previous value of each attribute's references (by a hash of it).
    prev_ref: Vec<u64>,
    ref_cache: Vec<u64>,
}

const SLOTS: usize = 1024;

fn slot(attr: u64) -> usize {
    (attr as usize ^ (attr >> 10) as usize) & (SLOTS - 1)
}

impl State {
    fn new() -> State {
        State {
            str_next: 0,
            str_prev: vec![0; SLOTS],
            str_cache: Vec::with_capacity(CACHE),
            prev: vec![0; SLOTS],
            prev_addr: 0,
            prev_strx: 0,
            prev_addrx: 0,
            prev_abbrev_off: 0,
            prev_ref: vec![0; SLOTS],
            ref_cache: Vec::with_capacity(REF_CACHE),
        }
    }
}

impl State {
    /// The symbol of string `i` of `.debug_str` for `attr`: 0 for the next not used yet, else at
    /// least 2 (1 is for an offset that is no string's).
    fn str_sym(&mut self, i: u32, attr: u64) -> u64 {
        let sl = slot(attr);
        let sym = if i == self.str_next {
            0
        } else if let Some(k) = self.str_cache.iter().position(|&c| c == i) {
            2 + k as u64
        } else {
            varint::zz(i as i64 - self.str_prev[sl] as i64) + 2 + CACHE as u64
        };
        self.str_used(i, sl);
        sym
    }

    /// The string of symbol `sym` (not 1) for `attr`.
    fn str_index(&mut self, sym: u64, attr: u64) -> Option<u32> {
        let sl = slot(attr);
        let i = match sym {
            0 => self.str_next,
            _ if sym < 2 + CACHE as u64 => *self.str_cache.get(sym as usize - 2)?,
            _ => u32::try_from((self.str_prev[sl] as i64).wrapping_add(varint::unzz(sym - 2 - CACHE as u64))).ok()?,
        };
        i.checked_add(1)?;
        self.str_used(i, sl);
        Some(i)
    }

    fn str_used(&mut self, i: u32, sl: usize) {
        self.str_next = self.str_next.max(i + 1);
        self.str_prev[sl] = i;
        let c = &mut self.str_cache;
        match c.iter().position(|&x| x == i) {
            Some(k) => c[..=k].rotate_right(1),
            None => {
                if c.len() == CACHE {
                    c.pop();
                }
                c.insert(0, i);
            }
        }
    }
}

const CACHE: usize = 64;

impl State {
    /// The symbol (at least 1) of a reference to DIE `i` of `attr`.
    fn ref_sym(&mut self, i: u64, attr: u64) -> u64 {
        let sl = slot(attr);
        let sym = match self.ref_cache.iter().position(|&c| c == i) {
            Some(k) => 1 + k as u64,
            None => 1 + REF_CACHE as u64 + varint::zz(i.wrapping_sub(self.prev_ref[sl]) as i64),
        };
        self.ref_used(i, sl);
        sym
    }

    /// The DIE of the reference of symbol `sym` (not 0) of `attr`.
    fn ref_index(&mut self, sym: u64, attr: u64) -> Option<u64> {
        let sl = slot(attr);
        let i = match sym {
            _ if sym < 1 + REF_CACHE as u64 => *self.ref_cache.get(sym as usize - 1)?,
            _ => self.prev_ref[sl].wrapping_add(varint::unzz(sym - 1 - REF_CACHE as u64) as u64),
        };
        self.ref_used(i, sl);
        Some(i)
    }

    fn ref_used(&mut self, i: u64, sl: usize) {
        self.prev_ref[sl] = i;
        let c = &mut self.ref_cache;
        match c.iter().position(|&x| x == i) {
            Some(k) => c[..=k].rotate_right(1),
            None => {
                if c.len() == REF_CACHE {
                    c.pop();
                }
                c.insert(0, i);
            }
        }
    }
}

const REF_CACHE: usize = 64;

/// The size of a form of fixed size, by the unit's header (`None` for others).
fn fixed_size(form: u64, h: UnitHead) -> Option<usize> {
    Some(match form {
        DW_FORM_ADDR => h.addr_size as usize,
        DW_FORM_DATA1 | DW_FORM_REF1 | DW_FORM_FLAG | DW_FORM_STRX1 | DW_FORM_ADDRX1 => 1,
        DW_FORM_DATA2 | DW_FORM_REF2 => 2,
        0x26 | 0x2a => 3, // strx3, addrx3
        DW_FORM_DATA4 | DW_FORM_REF4 | DW_FORM_REF_SUP4 | DW_FORM_STRX4 | DW_FORM_ADDRX4 => 4,
        0x27 | 0x2b => 2, // strx2, addrx2
        DW_FORM_DATA8 | DW_FORM_REF8 | DW_FORM_REF_SIG8 | DW_FORM_REF_SUP8 => 8,
        DW_FORM_DATA16 => 16,
        DW_FORM_STRP | DW_FORM_SEC_OFFSET | DW_FORM_LINE_STRP | DW_FORM_STRP_SUP | DW_FORM_GNU_REF_ALT
        | DW_FORM_GNU_STRP_ALT => 4,
        DW_FORM_REF_ADDR => {
            if h.version <= 2 {
                h.addr_size as usize
            } else {
                4
            }
        }
        DW_FORM_FLAG_PRESENT | DW_FORM_IMPLICIT_CONST => 0,
        _ => return None,
    })
}

fn get_le(d: &[u8], at: usize, n: usize) -> Option<u64> {
    let b = d.get(at..at + n)?;
    Some(b.iter().rev().fold(0u64, |v, &c| v << 8 | c as u64))
}

fn put_le(out: &mut Vec<u8>, v: u64, n: usize) {
    out.extend_from_slice(&v.to_le_bytes()[..n]);
}

/// A value of an attribute, as the encoder parsed it.
#[derive(Debug, Clone, Copy)]
enum Val {
    /// A number (of a fixed size, or LEB128).
    Num(u64),
    /// Bytes of the section (a block's, a string's without its NUL).
    Bytes(usize, usize),
}

/// The params ([`Shape`]) and the streams of `.debug_info` `d`, by the tables of `.debug_abbrev`
/// `abbrev` and the strings of `.debug_str` `strs`, or `None` where it is not rebuilt as it is.
pub fn encode(d: &[u8], abbrev: &[u8], strs: &[u8]) -> Option<(Vec<u8>, Vec<Vec<u8>>)> {
    let mut abbrevs = Abbrevs::new(abbrev);
    let strs_starts = string_starts(strs, strings(strs))?;
    // the units, and whether each is coded (else kept as it is): those whose DIEs parse, less
    // those that turn out not to code (whose DIEs are then not indexed, and the rest coded again)
    let mut units = Vec::new();
    let mut at = 0;
    while at < d.len() {
        let len = get_le(d, at, 4).filter(|&l| l < 0xffff_fff0 && at + 4 + l as usize <= d.len());
        let end = len.map_or(d.len(), |l| at + 4 + l as usize);
        units.push((at, end, len.is_some()));
        at = end;
    }
    loop {
        let mut e = Encoder {
            d,
            st: vec![Vec::new(); NUM_STREAMS],
            s: State::new(),
            strs: &strs_starts,
            starts: Vec::new(),
            ends: Vec::new(),
        };
        for u in units.iter_mut().filter(|u| u.2) {
            u.2 = e.walk(&mut abbrevs, u.0, u.1);
        }
        let mut failed = false;
        for u in units.iter_mut() {
            if u.2 && e.unit(&mut abbrevs, u.0, u.1).is_none() {
                u.2 = false;
                failed = true;
                break;
            }
            if !u.2 {
                e.st[UNIT].push(1);
                varint::put(&mut e.st[UNIT], (u.1 - u.0) as u64);
                e.st[RAW].extend_from_slice(&d[u.0..u.1]);
            }
        }
        if failed {
            continue;
        }
        let inputs: Vec<&[u8]> = [abbrev, strs].into_iter().chain(e.st.iter().map(|s| &s[..])).collect();
        // (the references after their DIEs those the decoder takes)
        let mut sh = Shape::of(&inputs, strs_starts.len()).ok()?;
        let (out, fixups) = decode_as(sh, &inputs, d.len() as u64).ok()?;
        sh.fixups = fixups;
        return (out == d).then_some((sh.to_bytes(), e.st));
    }
}

struct Encoder<'a> {
    d: &'a [u8],
    st: Vec<Vec<u8>>,
    s: State,
    strs: &'a [u32],
    /// The start of each DIE of the units coded, in order (its index its place), and where its
    /// subtree ends (`u32::MAX` where no null entry closes it).
    starts: Vec<u32>,
    ends: Vec<u32>,
}

impl Encoder<'_> {
    /// Indexes the DIEs of the unit at `start` to `end`; whether they parse (else none is).
    fn walk(&mut self, abbrevs: &mut Abbrevs, start: usize, end: usize) -> bool {
        let Some((h, abbrev_off, mut at)) = header(self.d, start, end) else { return false };
        let d = &self.d[..end];
        let Some(table) = abbrevs.table(abbrev_off) else { return false };
        let first = self.starts.len();
        // the DIEs whose children are open
        let mut open = Vec::new();
        let ok = (|| {
            while at < end {
                let die = at;
                let code = uleb(d, &mut at)?;
                if code == 0 {
                    if let Some(k) = open.pop() {
                        self.ends[k] = at as u32;
                    }
                    continue;
                }
                let a = table.get(code as usize)?.as_ref()?;
                self.starts.push(die as u32);
                self.ends.push(u32::MAX);
                for &(_, form) in &a.attrs {
                    skip(d, &mut at, form, h)?;
                }
                if a.children {
                    open.push(self.starts.len() - 1);
                } else {
                    *self.ends.last_mut().unwrap() = at as u32;
                }
            }
            Some(())
        })()
        .is_some();
        if !ok || at != end {
            self.starts.truncate(first);
            self.ends.truncate(first);
            return false;
        }
        true
    }

    fn put(&mut self, k: usize, v: u64) {
        varint::put(&mut self.st[k], v);
    }

    /// Codes the unit at `start` to `end` into the streams, its DIEs indexed; `None` where it
    /// does not code.
    fn unit(&mut self, abbrevs: &mut Abbrevs, start: usize, end: usize) -> Option<()> {
        let d = self.d;
        let (h, abbrev_off, mut at) = header(d, start, end)?;
        // the header
        self.st[UNIT].push(0);
        self.st[UNIT].extend_from_slice(&h.version.to_le_bytes());
        let delta = abbrev_off.wrapping_sub(self.s.prev_abbrev_off) as i64;
        varint::put_signed(&mut self.st[UNIT], delta);
        self.s.prev_abbrev_off = abbrev_off;
        if h.version >= 5 {
            // the unit type, the address size, and what follows the abbreviations' offset
            self.st[UNIT].push(d[start + 6]);
            self.st[UNIT].push(h.addr_size);
            self.st[UNIT].extend_from_slice(&d[start + 12..at]);
        } else {
            self.st[UNIT].push(h.addr_size);
        }
        let table = abbrevs.table(abbrev_off)?;
        let d = &d[..end];
        while at < end {
            let die = at;
            let code = uleb(d, &mut at)?;
            if code == 0 {
                self.put(CODE, 1);
                continue;
            }
            self.put(CODE, code + 1);
            let a = table.get(code as usize)?.as_ref()?;
            let cur = self.starts.binary_search(&(die as u32)).ok()?;
            let mut low_pc = None;
            for &(attr, form) in &a.attrs {
                self.value(d, &mut at, attr, form, h, start, cur, &mut low_pc)?;
            }
        }
        // the end of the unit
        self.put(CODE, 0);
        Some(())
    }

    #[allow(clippy::too_many_arguments)]
    fn value(
        &mut self,
        d: &[u8],
        at: &mut usize,
        attr: u64,
        form: u64,
        h: UnitHead,
        unit: usize,
        cur: usize,
        low_pc: &mut Option<u64>,
    ) -> Option<()> {
        let v = parse(d, at, form, h)?;
        let num = |v: Val| match v {
            Val::Num(n) => n,
            Val::Bytes(..) => unreachable!(),
        };
        match form {
            DW_FORM_INDIRECT => {
                let f = num(v);
                self.put(DATA, f);
                return self.value(d, at, attr, f, h, unit, cur, low_pc);
            }
            DW_FORM_STRP => {
                let v = num(v) as u32;
                match self.strs.binary_search(&v) {
                    Ok(i) => {
                        let sym = self.s.str_sym(i as u32, attr);
                        self.put(STR, sym);
                    }
                    Err(_) => {
                        self.put(STR, 1);
                        self.put(STR, v as u64);
                    }
                }
            }
            DW_FORM_REF1 | DW_FORM_REF2 | DW_FORM_REF4 | DW_FORM_REF8 | DW_FORM_REF_ADDR => {
                let v = num(v);
                let target = if form == DW_FORM_REF_ADDR { v } else { v.wrapping_add(unit as u64) };
                // a sibling is derived where it is the end of the DIE's subtree
                if attr == DW_AT_SIBLING {
                    if self.ends[cur] as u64 == target {
                        self.put(REF, 0);
                        return Some(());
                    }
                    self.put(REF, 1);
                }
                match u32::try_from(target).ok().and_then(|t| self.starts.binary_search(&t).ok()) {
                    Some(i) => {
                        let sym = self.s.ref_sym(i as u64, attr);
                        self.put(REF, sym);
                    }
                    None => {
                        self.put(REF, 0);
                        self.put(REF, v);
                    }
                }
            }
            DW_FORM_ADDR => {
                let v = num(v);
                let base = match (attr, *low_pc) {
                    (DW_AT_HIGH_PC, Some(l)) => l,
                    _ => self.s.prev_addr,
                };
                self.put(ADDR, varint::zz(v.wrapping_sub(base) as i64));
                if attr != DW_AT_HIGH_PC {
                    self.s.prev_addr = v;
                }
                if attr == DW_AT_LOW_PC {
                    *low_pc = Some(v);
                }
            }
            DW_FORM_STRING => {
                let Val::Bytes(s, e) = v else { unreachable!() };
                self.st[CSTR].extend_from_slice(&d[s..=e]);
            }
            DW_FORM_BLOCK1 | DW_FORM_BLOCK2 | DW_FORM_BLOCK4 | DW_FORM_BLOCK | DW_FORM_EXPRLOC => {
                let Val::Bytes(s, e) = v else { unreachable!() };
                self.put(EXPR, (e - s) as u64);
                self.st[EXPR].extend_from_slice(&d[s..e]);
            }
            DW_FORM_DATA16 | DW_FORM_REF_SIG8 => {
                let Val::Bytes(s, e) = v else { unreachable!() };
                self.st[EXPR].extend_from_slice(&d[s..e]);
            }
            DW_FORM_FLAG_PRESENT | DW_FORM_IMPLICIT_CONST => {}
            _ => {
                let (k, delta, sl) = how(attr, form);
                let v = num(v);
                if !delta {
                    match form {
                        DW_FORM_SDATA => varint::put_signed(&mut self.st[k], v as i64),
                        _ => self.put(k, v),
                    }
                } else {
                    let p = match sl {
                        Slot::Attr(sl) => &mut self.s.prev[sl],
                        Slot::Strx => &mut self.s.prev_strx,
                        Slot::Addrx => &mut self.s.prev_addrx,
                    };
                    let z = varint::zz(v.wrapping_sub(*p) as i64);
                    *p = v;
                    self.put(k, z);
                }
            }
        }
        Some(())
    }
}

/// Of which previous value a number is coded as the delta.
#[derive(Debug, Clone, Copy)]
enum Slot {
    Attr(usize),
    Strx,
    Addrx,
}

/// How a number of `attr` in `form` (of none of the forms coded otherwise) is coded: its stream,
/// whether as the delta from a previous value, and which.
fn how(attr: u64, form: u64) -> (usize, bool, Slot) {
    match form {
        DW_FORM_SEC_OFFSET | DW_FORM_LINE_STRP | DW_FORM_STRP_SUP | DW_FORM_GNU_STRP_ALT | DW_FORM_GNU_REF_ALT
        | DW_FORM_REF_SUP4 | DW_FORM_REF_SUP8 => (SEC, true, Slot::Attr(slot(attr ^ form << 16))),
        DW_FORM_LOCLISTX | DW_FORM_RNGLISTX => (IDX, true, Slot::Attr(slot(attr ^ form << 16))),
        DW_FORM_STRX | DW_FORM_STRX1..=DW_FORM_STRX4 | DW_FORM_GNU_STR_INDEX => (IDX, true, Slot::Strx),
        DW_FORM_ADDRX | DW_FORM_ADDRX1..=DW_FORM_ADDRX4 | DW_FORM_GNU_ADDR_INDEX => (IDX, true, Slot::Addrx),
        // data1/2/4/8, udata, sdata, flag, ref_udata
        _ => match attr {
            DW_AT_DECL_LINE | DW_AT_DECL_COLUMN => (LINE, true, Slot::Attr(slot(attr))),
            DW_AT_CALL_LINE | DW_AT_CALL_COLUMN => (CALL, true, Slot::Attr(slot(attr))),
            DW_AT_DECL_FILE | DW_AT_CALL_FILE => (FILE, false, Slot::Strx),
            DW_AT_HIGH_PC => (PCLEN, false, Slot::Strx),
            _ => (DATA, false, Slot::Strx),
        },
    }
}

/// The header of the unit at `start` (to `end`): what it holds, its abbreviations' offset and
/// where its DIEs start; `None` for units this module does not take (64-bit, of other versions).
fn header(d: &[u8], start: usize, end: usize) -> Option<(UnitHead, u64, usize)> {
    let version = get_le(d, start + 4, 2)? as u16;
    let (h, abbrev, at) = match version {
        2..=4 => {
            let abbrev = get_le(d, start + 6, 4)?;
            let addr_size = *d.get(start + 10)?;
            (UnitHead { version, addr_size }, abbrev, start + 11)
        }
        5 => {
            let unit_type = *d.get(start + 6)?;
            let addr_size = *d.get(start + 7)?;
            let abbrev = get_le(d, start + 8, 4)?;
            let extra = match unit_type {
                DW_UT_COMPILE | DW_UT_PARTIAL => 0,
                DW_UT_SKELETON | DW_UT_SPLIT_COMPILE => 8,
                DW_UT_TYPE | DW_UT_SPLIT_TYPE => 12,
                _ => return None,
            };
            (UnitHead { version, addr_size }, abbrev, start + 12 + extra)
        }
        _ => return None,
    };
    (matches!(h.addr_size, 4 | 8) && at <= end).then_some((h, abbrev, at))
}

/// Skips a value of `form` at `*at`.
fn skip(d: &[u8], at: &mut usize, form: u64, h: UnitHead) -> Option<()> {
    match parse(d, at, form, h)? {
        Val::Num(f) if form == DW_FORM_INDIRECT => skip(d, at, f, h),
        _ => Some(()),
    }
}

/// The value of `form` at `*at`, which it moves past it (an indirect form's value is the form,
/// its value after it left to the caller).
fn parse(d: &[u8], at: &mut usize, form: u64, h: UnitHead) -> Option<Val> {
    if let Some(n) = fixed_size(form, h) {
        if matches!(form, DW_FORM_DATA16 | DW_FORM_REF_SIG8) {
            let s = *at;
            *at += n;
            return (*at <= d.len()).then_some(Val::Bytes(s, *at));
        }
        let v = if n == 0 { 0 } else { get_le(d, *at, n)? };
        *at += n;
        return Some(Val::Num(v));
    }
    Some(match form {
        DW_FORM_SDATA => Val::Num(sleb(d, at)? as u64),
        DW_FORM_UDATA
        | DW_FORM_REF_UDATA
        | DW_FORM_INDIRECT
        | DW_FORM_STRX
        | DW_FORM_ADDRX
        | DW_FORM_LOCLISTX
        | DW_FORM_RNGLISTX
        | DW_FORM_GNU_ADDR_INDEX
        | DW_FORM_GNU_STR_INDEX => Val::Num(uleb(d, at)?),
        DW_FORM_STRING => {
            let s = *at;
            let n = d.get(s..)?.iter().position(|&c| c == 0)?;
            *at += n + 1;
            Val::Bytes(s, s + n)
        }
        DW_FORM_BLOCK1 | DW_FORM_BLOCK2 | DW_FORM_BLOCK4 | DW_FORM_BLOCK | DW_FORM_EXPRLOC => {
            let n = match form {
                DW_FORM_BLOCK1 => fixed(d, at, 1)?,
                DW_FORM_BLOCK2 => fixed(d, at, 2)?,
                DW_FORM_BLOCK4 => fixed(d, at, 4)?,
                _ => uleb(d, at)?,
            };
            let s = *at;
            *at = s.checked_add(usize::try_from(n).ok()?)?;
            if *at > d.len() {
                return None;
            }
            Val::Bytes(s, *at)
        }
        _ => return None,
    })
}

fn fixed(d: &[u8], at: &mut usize, n: usize) -> Option<u64> {
    let v = get_le(d, *at, n)?;
    *at += n;
    Some(v)
}

/// A reader of a stream.
struct Reader<'a> {
    d: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn get(&mut self) -> Result<u64> {
        varint::get(self.d, &mut self.at).map_err(|_| corrupt())
    }

    fn signed(&mut self) -> Result<i64> {
        self.get().map(varint::unzz)
    }

    fn bytes(&mut self, n: usize) -> Result<&[u8]> {
        let b = self.d.get(self.at..self.at.checked_add(n).ok_or_else(corrupt)?).ok_or_else(corrupt)?;
        self.at += n;
        Ok(b)
    }

    fn byte(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }

    fn done(&self) -> bool {
        self.at == self.d.len()
    }
}

/// Writes `v` as `form` does (a number of a fixed size, or LEB128).
fn put_num(out: &mut Vec<u8>, form: u64, v: u64, h: UnitHead) -> Result<()> {
    match fixed_size(form, h) {
        Some(n) => {
            if n < 8 && v >> (8 * n) != 0 {
                return Err(corrupt());
            }
            put_le(out, v, n);
        }
        None if form == DW_FORM_SDATA => put_sleb(out, v as i64),
        None => varint::put(out, v),
    }
    Ok(())
}

/// A reference to a DIE not decoded yet: where it goes, its size, the DIE's index and the
/// offset it is relative to.
struct Fixup {
    at: usize,
    size: usize,
    index: u64,
    base: u64,
}

/// Calls `f` with the abbreviation offset of each unit of the unit stream `d` coded (not
/// kept as it is), in order, as [`decode`] reads them.
fn unit_offsets(d: &[u8], mut f: impl FnMut(u64) -> Result<()>) -> Result<()> {
    let mut r = Reader { d, at: 0 };
    let mut prev = 0u64;
    while !r.done() {
        match r.byte()? {
            0 => {}
            1 => {
                r.get()?;
                continue;
            }
            _ => return Err(corrupt()),
        }
        let version = u16::from_le_bytes(r.bytes(2)?.try_into().unwrap());
        prev = prev.wrapping_add(r.signed()? as u64);
        match version {
            2..=4 => {
                r.byte()?;
            }
            5 => {
                let unit_type = r.byte()?;
                r.byte()?;
                r.bytes(match unit_type {
                    DW_UT_COMPILE | DW_UT_PARTIAL => 0,
                    DW_UT_SKELETON | DW_UT_SPLIT_COMPILE => 8,
                    DW_UT_TYPE | DW_UT_SPLIT_TYPE => 12,
                    _ => return Err(corrupt()),
                })?;
            }
            _ => return Err(corrupt()),
        }
        f(prev)?;
    }
    Ok(())
}

/// What [`decode`] holds of its inputs, told by its params: the strings of `.debug_str`, the
/// DIEs, the DIEs whose children are open, and the references to DIEs after them, at most.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shape {
    strings: usize,
    dies: usize,
    depth: usize,
    fixups: usize,
}

impl Shape {
    /// The params: varints of the strings, the DIEs, the DIEs open and the references after.
    fn to_bytes(self) -> Vec<u8> {
        let mut v = Vec::new();
        for x in [self.strings, self.dies, self.depth, self.fixups] {
            varint::put(&mut v, x as u64);
        }
        v
    }

    fn from_bytes(b: &[u8]) -> Result<Shape> {
        let mut at = 0;
        let mut num = || usize::try_from(varint::get(b, &mut at).map_err(|_| corrupt())?).map_err(|_| corrupt());
        let s = Shape { strings: num()?, dies: num()?, depth: num()?, fixups: num()? };
        if at != b.len() {
            return Err(corrupt());
        }
        Ok(s)
    }

    /// The shape of the inputs of [`decode`], of `strings` strings of `.debug_str`: the codes of
    /// each unit, as far as they read, of a DIE (whose children open) or of the null entry
    /// closing them; the references after their DIEs are those of symbols at most.
    fn of(inputs: &[&[u8]], strings: usize) -> Result<Shape> {
        let (c, tables) = Counts::new(inputs)?;
        let mut code = Reader { d: inputs[2 + CODE], at: 0 };
        let (mut dies, mut depth) = (0, 0);
        unit_offsets(inputs[2 + UNIT], |off| {
            let table = tables.table(off).ok_or_else(corrupt)?;
            let mut open = 0;
            loop {
                match code.get()? {
                    0 => return Ok(()),
                    1 => open -= (open > 0) as usize,
                    c => {
                        dies += 1;
                        let a = usize::try_from(c - 1).ok().and_then(|c| table.get(c)).ok_or_else(corrupt)?;
                        if a.n == NONE {
                            return Err(corrupt());
                        }
                        open += a.children as usize;
                        depth = depth.max(open);
                    }
                }
            }
        })?;
        Ok(Shape { strings, dies, depth, fixups: c.refs })
    }
}

/// What [`decode`] holds of its inputs, counted before it starts: the units coded, the tables'
/// offsets (in order, each once), and the references at most (one for each symbol).
struct Counts {
    units: usize,
    offs: Vec<u64>,
    refs: usize,
}

impl Counts {
    /// The counts of the inputs of [`decode`] (`.debug_abbrev`, `.debug_str`, the streams), and
    /// its tables.
    fn new(inputs: &[&[u8]]) -> Result<(Counts, Tables)> {
        let [abbrev, _, streams @ ..] = inputs else { return Err(corrupt()) };
        if streams.len() != NUM_STREAMS {
            return Err(corrupt());
        }
        let mut units = 0;
        unit_offsets(streams[UNIT], |_| {
            units += 1;
            Ok(())
        })?;
        let mut offs = Vec::with_capacity(units);
        unit_offsets(streams[UNIT], |o| {
            offs.push(o);
            Ok(())
        })?;
        offs.sort_unstable();
        offs.dedup();
        let tables = Tables::new(abbrev, &offs).ok_or_else(corrupt)?;
        // (each reference is a symbol at least)
        let refs = streams[REF].iter().filter(|&&b| b < 0x80).count();
        Ok((Counts { units, offs, refs }, tables))
    }
}

/// Bytes of a reader, of an open DIE and of a fixup on WebAssembly, for the plan.
const READER_BYTES: u64 = 12;
const OPEN_BYTES: u64 = 24;
const FIXUP_BYTES: u64 = 24;
#[cfg(target_family = "wasm")]
const _: () = assert!(
    size_of::<Reader>() as u64 == READER_BYTES
        && size_of::<Option<(usize, usize, u64)>>() as u64 == OPEN_BYTES
        && size_of::<Fixup>() as u64 == FIXUP_BYTES
);

/// The memory plan of [`decode`] ([`aqr_api::stack`]) of `params`, `inputs` and `len` bytes of
/// output: the readers, the strings' offsets, the tables' offsets and the tables, the state,
/// the output, the DIEs' starts, the references to them before them at most, and the DIEs
/// open. `None` where the params or inputs are not those it takes.
pub fn plan(params: &[u8], inputs: &[&[u8]], len: u64) -> Option<u64> {
    use aqr_api::stack::block;
    let sh = Shape::from_bytes(params).ok()?;
    let (c, _) = Counts::new(inputs).ok()?;
    let state =
        block(4 * SLOTS as u64) + block(4 * CACHE as u64) + 2 * block(8 * SLOTS as u64) + block(8 * REF_CACHE as u64);
    Some(
        block(READER_BYTES * NUM_STREAMS as u64)
            + block(4 * sh.strings as u64)
            + block(8 * c.units as u64)
            + Tables::plan(inputs[0], &c.offs)?
            + state
            + block(len)
            + block(4 * sh.dies as u64)
            + block(FIXUP_BYTES * sh.fixups as u64)
            + block(OPEN_BYTES * sh.depth as u64),
    )
}

/// `.debug_info` of `len` bytes from `params` ([`Shape`]) and `inputs`: `.debug_abbrev`,
/// `.debug_str`, then the streams of [`STREAM_NAMES`].
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    Ok(decode_as(Shape::from_bytes(params)?, inputs, len)?.0)
}

/// [`decode`] of the shape `sh`; also the references after their DIEs.
fn decode_as(sh: Shape, inputs: &[&[u8]], len: u64) -> Result<(Vec<u8>, usize)> {
    let [_, strs, streams @ ..] = inputs else { return Err(corrupt()) };
    if streams.len() != NUM_STREAMS {
        return Err(corrupt());
    }
    let len = usize::try_from(len).map_err(|_| corrupt())?;
    let mut r: Vec<Reader> = Vec::with_capacity(NUM_STREAMS);
    r.extend(streams.iter().map(|d| Reader { d, at: 0 }));
    let strs = string_starts(strs, sh.strings).ok_or_else(corrupt)?;
    let (_counts, tables) = Counts::new(inputs)?;
    let mut s = State::new();
    let mut out = Vec::with_capacity(len);
    // the start of each DIE decoded, the references to those not yet, and of each DIE whose
    // children are open, its sibling to derive where they close
    let mut dies = Vec::<u32>::with_capacity(sh.dies);
    let mut fixups = Vec::with_capacity(sh.fixups);
    let mut open: Vec<Option<(usize, usize, u64)>> = Vec::with_capacity(sh.depth);
    while !r[UNIT].done() {
        match r[UNIT].byte()? {
            0 => {}
            1 => {
                let n = usize::try_from(r[UNIT].get()?).map_err(|_| corrupt())?;
                out.extend_from_slice(r[RAW].bytes(n)?);
                continue;
            }
            _ => return Err(corrupt()),
        }
        let start = out.len();
        let version = u16::from_le_bytes(r[UNIT].bytes(2)?.try_into().unwrap());
        let abbrev_off = s.prev_abbrev_off.wrapping_add(r[UNIT].signed()? as u64);
        s.prev_abbrev_off = abbrev_off;
        let abbrev32 = u32::try_from(abbrev_off).map_err(|_| corrupt())?;
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&version.to_le_bytes());
        let h = match version {
            2..=4 => {
                let addr_size = r[UNIT].byte()?;
                out.extend_from_slice(&abbrev32.to_le_bytes());
                out.push(addr_size);
                UnitHead { version, addr_size }
            }
            5 => {
                let (unit_type, addr_size) = (r[UNIT].byte()?, r[UNIT].byte()?);
                out.extend_from_slice(&[unit_type, addr_size]);
                out.extend_from_slice(&abbrev32.to_le_bytes());
                let extra = match unit_type {
                    DW_UT_COMPILE | DW_UT_PARTIAL => 0,
                    DW_UT_SKELETON | DW_UT_SPLIT_COMPILE => 8,
                    DW_UT_TYPE | DW_UT_SPLIT_TYPE => 12,
                    _ => return Err(corrupt()),
                };
                out.extend_from_slice(r[UNIT].bytes(extra)?);
                UnitHead { version, addr_size }
            }
            _ => return Err(corrupt()),
        };
        if !matches!(h.addr_size, 4 | 8) {
            return Err(corrupt());
        }
        let table = tables.table(abbrev_off).ok_or_else(corrupt)?;
        open.clear();
        let resolve = |out: &mut Vec<u8>, (at, size, base): (usize, usize, u64)| -> Result<()> {
            let v = (out.len() as u64).wrapping_sub(base);
            if size < 8 && v >> (8 * size) != 0 {
                return Err(corrupt());
            }
            out[at..at + size].copy_from_slice(&v.to_le_bytes()[..size]);
            Ok(())
        };
        loop {
            if out.len() > len {
                return Err(corrupt());
            }
            let c = r[CODE].get()?;
            if c == 0 {
                break;
            }
            if c == 1 {
                out.push(0);
                if let Some(Some(sib)) = open.pop() {
                    resolve(&mut out, sib)?;
                }
                continue;
            }
            let code = c - 1;
            if dies.len() == sh.dies {
                return Err(corrupt());
            }
            dies.push(u32::try_from(out.len()).map_err(|_| corrupt())?);
            varint::put(&mut out, code);
            // (an index not of usize, of wasm32, is none, as of 64 bits)
            let a =
                usize::try_from(code).ok().and_then(|c| table.get(c)).filter(|a| a.n != NONE).ok_or_else(corrupt)?;
            let (mut low_pc, mut sibling) = (None, None);
            for &(attr, form) in tables.attrs(a) {
                let mut form = form;
                while form == DW_FORM_INDIRECT {
                    form = r[DATA].get()?;
                    varint::put(&mut out, form);
                }
                match form {
                    DW_FORM_STRP => {
                        let sym = r[STR].get()?;
                        let v = match sym {
                            1 => u32::try_from(r[STR].get()?).map_err(|_| corrupt())?,
                            _ => {
                                let i = s.str_index(sym, attr).ok_or_else(corrupt)?;
                                *strs.get(i as usize).ok_or_else(corrupt)?
                            }
                        };
                        out.extend_from_slice(&v.to_le_bytes());
                    }
                    DW_FORM_REF1 | DW_FORM_REF2 | DW_FORM_REF4 | DW_FORM_REF8 | DW_FORM_REF_ADDR => {
                        let size = fixed_size(form, h).unwrap();
                        let base = if form == DW_FORM_REF_ADDR { 0 } else { start as u64 };
                        if attr == DW_AT_SIBLING && r[REF].get()? == 0 {
                            if sibling.is_some() {
                                return Err(corrupt());
                            }
                            sibling = Some((out.len(), size, base));
                            out.resize(out.len() + size, 0);
                            continue;
                        }
                        let sym = r[REF].get()?;
                        if sym == 0 {
                            let v = r[REF].get()?;
                            put_num(&mut out, form, v, h)?;
                            continue;
                        }
                        let i = s.ref_index(sym, attr).ok_or_else(corrupt)?;
                        match usize::try_from(i).ok().and_then(|i| dies.get(i)) {
                            Some(&t) => put_num(&mut out, form, (t as u64).wrapping_sub(base), h)?,
                            None => {
                                if fixups.len() == sh.fixups {
                                    return Err(corrupt());
                                }
                                fixups.push(Fixup { at: out.len(), size, index: i, base });
                                out.resize(out.len() + size, 0);
                            }
                        }
                    }
                    DW_FORM_ADDR => {
                        let base = match (attr, low_pc) {
                            (DW_AT_HIGH_PC, Some(l)) => l,
                            _ => s.prev_addr,
                        };
                        let v = base.wrapping_add(r[ADDR].signed()? as u64);
                        put_num(&mut out, form, v, h)?;
                        if attr != DW_AT_HIGH_PC {
                            s.prev_addr = v;
                        }
                        if attr == DW_AT_LOW_PC {
                            low_pc = Some(v);
                        }
                    }
                    DW_FORM_STRING => {
                        let c = &r[CSTR];
                        let n = c.d[c.at..].iter().position(|&b| b == 0).ok_or_else(corrupt)?;
                        out.extend_from_slice(r[CSTR].bytes(n + 1)?);
                    }
                    DW_FORM_BLOCK1 | DW_FORM_BLOCK2 | DW_FORM_BLOCK4 | DW_FORM_BLOCK | DW_FORM_EXPRLOC => {
                        let n = r[EXPR].get()?;
                        let size_form = match form {
                            DW_FORM_BLOCK1 => DW_FORM_DATA1,
                            DW_FORM_BLOCK2 => DW_FORM_DATA2,
                            DW_FORM_BLOCK4 => DW_FORM_DATA4,
                            _ => DW_FORM_UDATA,
                        };
                        put_num(&mut out, size_form, n, h)?;
                        let n = usize::try_from(n).map_err(|_| corrupt())?;
                        out.extend_from_slice(r[EXPR].bytes(n)?);
                    }
                    DW_FORM_DATA16 | DW_FORM_REF_SIG8 => {
                        out.extend_from_slice(r[EXPR].bytes(fixed_size(form, h).unwrap())?);
                    }
                    DW_FORM_FLAG_PRESENT | DW_FORM_IMPLICIT_CONST => {}
                    _ => {
                        if fixed_size(form, h).is_none() && !is_leb(form) {
                            return Err(corrupt());
                        }
                        let (k, delta, sl) = how(attr, form);
                        let v = if !delta {
                            match form {
                                DW_FORM_SDATA => r[k].signed()? as u64,
                                _ => r[k].get()?,
                            }
                        } else {
                            let p = match sl {
                                Slot::Attr(sl) => &mut s.prev[sl],
                                Slot::Strx => &mut s.prev_strx,
                                Slot::Addrx => &mut s.prev_addrx,
                            };
                            *p = p.wrapping_add(r[k].signed()? as u64);
                            *p
                        };
                        put_num(&mut out, form, v, h)?;
                    }
                }
            }
            if a.children {
                if open.len() == sh.depth {
                    return Err(corrupt());
                }
                open.push(sibling);
            } else if let Some(sib) = sibling {
                resolve(&mut out, sib)?;
            }
        }
        // the unit's length
        let n = u32::try_from(out.len() - start - 4).ok().filter(|&n| n < 0xffff_fff0).ok_or_else(corrupt)?;
        out[start..start + 4].copy_from_slice(&n.to_le_bytes());
    }
    let n = fixups.len();
    for f in fixups {
        let t = *usize::try_from(f.index).ok().and_then(|i| dies.get(i)).ok_or_else(corrupt)? as u64;
        let v = t.wrapping_sub(f.base);
        if f.size < 8 && v >> (8 * f.size) != 0 {
            return Err(corrupt());
        }
        out[f.at..f.at + f.size].copy_from_slice(&v.to_le_bytes()[..f.size]);
    }
    if out.len() != len || !r.iter().all(Reader::done) {
        return Err(corrupt());
    }
    Ok((out, n))
}

/// Whether `form` is a number in LEB128.
fn is_leb(form: u64) -> bool {
    matches!(
        form,
        DW_FORM_UDATA
            | DW_FORM_SDATA
            | DW_FORM_REF_UDATA
            | DW_FORM_STRX
            | DW_FORM_ADDRX
            | DW_FORM_LOCLISTX
            | DW_FORM_RNGLISTX
            | DW_FORM_GNU_ADDR_INDEX
            | DW_FORM_GNU_STR_INDEX
    )
}
