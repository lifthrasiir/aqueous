//! aqr-xml as a module of entry formulas (see aqr-formula): XML rebuilt from its skeleton and
//! its values in containers by context (FORMAT.md, "The module"), for the archive to compress.
//!
//! The encoder goes over a segment once, mirroring the stack of names the module keeps, and
//! writes the skeleton, the containers (the values of each context, LF-terminated until one
//! holds an LF, then with their lengths apart) and the order the values were taken in.

use std::collections::HashMap;
use std::hash::BuildHasherDefault;

use aqr_common::varint;
use aqr_formula::{Formula, Kind, Module, Node};
use memchr::{memchr, memchr_iter, memmem};

use crate::module::{CLOSE, END, ESC, MAX_CONTAINERS, MAX_DEPTH, OPEN, VALUE, name_byte, space, special};

pub use crate::module::decode;

pub const MODULE: Module = Module::new("xml-recons", 1).with_plan(memory);

/// The memory of the module: its initial memory, and its plan ([`crate::module::plan`]).
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let &[len] = u.outputs else { return None };
    Some(u.initial() + crate::module::plan(u.params, u.inputs, len)?)
}

/// The names of the inputs' streams, in order.
pub const STREAM_NAMES: [&str; 4] = ["skeleton", "table", "lens", "values"];

/// A text shorter than this is not taken.
pub const MIN_LEN: usize = 64 << 10;

/// The most containers of a segment (a value of a context past them stays in the skeleton).
pub const CONTAINERS: usize = 4096;
/// The most elements open at once (a start tag past it stays in the skeleton).
pub const DEPTH: usize = 256;
/// The most elements open where a segment is cut.
pub const CUT_OPEN: usize = 4;
/// The longest value cut out.
const MAX_VALUE: usize = 1 << 30;
/// The containers of fewer bytes than this share a stream.
const SMALL: usize = 4096;

const _: () = assert!(CONTAINERS as u64 <= MAX_CONTAINERS && DEPTH as u64 <= MAX_DEPTH);

aqr_common::zopt_namespace!("xml", "XML (aqr-xml)");

aqr_common::zopt! {
    /// The bytes of text of a segment at least (but the last), coded on its own (FORMAT.md,
    /// "Encoding"): what the encoder and the module hold of a file at once.
    pub static SEGMENT: Opt<usize> = Opt::size("xml:segment", 64 << 20, "bytes of XML of a segment, coded on its own");
}

/// The bounds of an encoding: tests make them small.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub segment: usize,
    pub containers: usize,
    pub depth: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits { segment: SEGMENT.get(), containers: CONTAINERS, depth: DEPTH }
    }
}

/// Whether `d` starts as XML does: after a UTF-8 byte order mark and spaces, `<?xml` or a tag.
pub fn starts(d: &[u8]) -> bool {
    let d = d.strip_prefix(b"\xef\xbb\xbf").unwrap_or(d);
    let at = d.iter().position(|&b| !space(b)).unwrap_or(d.len());
    let d = &d[at..];
    d.starts_with(b"<?xml")
        || d.len() > 1 && d[0] == b'<' && (name_byte(d[1]) && d[1] != b'!' && d[1] != b'?')
        || d.starts_with(b"<!--")
        || d.starts_with(b"<!DOCTYPE")
}

/// The formula of text `input`, or `None` where it is shorter than [`MIN_LEN`] or nothing of it
/// is cut out. It is the same whatever `threads` is (one is used).
pub fn encode(input: &[u8], _threads: usize) -> Option<Formula> {
    if input.len() < MIN_LEN {
        return None;
    }
    let (f, containers) = encode_with(input, &Limits::default());
    (containers > 0).then_some(f)
}

/// The context of a container: its kind (0 a text: parent and element; 1 an attribute's value:
/// element and attribute), its two names, and how many values it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    pub kind: u8,
    pub a: Vec<u8>,
    pub b: Vec<u8>,
    pub values: usize,
}

/// The formula of `input` in segments as `lim` tells, and the containers of all of them.
pub fn encode_with(input: &[u8], lim: &Limits) -> (Formula, usize) {
    encode_report(input, lim, None)
}

/// [`encode_with`], the contexts of each segment's containers (in order) put in `report`.
pub fn encode_report(input: &[u8], lim: &Limits, mut report: Option<&mut Vec<Vec<Context>>>) -> (Formula, usize) {
    let d = input;
    let mut f = Formula::default();
    let mut roots = Vec::new();
    let mut containers = 0;
    let (mut at, mut open) = (0, Vec::new());
    while at < d.len() || roots.is_empty() {
        let s = Segment::run(d, at, open, lim);
        containers += s.conts.len();
        let (end, next) = (s.end, s.next.clone());
        if let Some(r) = report.as_deref_mut() {
            r.push(s.contexts());
        }
        roots.push(s.finish(&mut f, (end - at) as u64));
        (at, open) = (end, next);
    }
    f.root = Node::concat(roots);
    (f, containers)
}

/// A container: the values of a context.
#[derive(Default)]
struct Container {
    /// The values one after another, each with an LF after it until `lens` is set.
    data: Vec<u8>,
    /// The values' lengths, once one holds an LF (the data then without LFs added).
    lens: Option<Vec<u32>>,
    /// The next of `lens` that the order of values takes.
    next: usize,
}

impl Container {
    fn put(&mut self, v: &[u8]) {
        if let Some(l) = &mut self.lens {
            l.push(v.len() as u32);
        } else if memchr(b'\n', v).is_some() {
            // (every value before it has no LF: the LFs of its data end them)
            let mut lens = Vec::new();
            let mut start = 0;
            for p in memchr_iter(b'\n', &self.data) {
                lens.push((p - start) as u32);
                start = p + 1;
            }
            self.data.retain(|&b| b != b'\n');
            lens.push(v.len() as u32);
            self.lens = Some(lens);
        } else {
            self.data.extend_from_slice(v);
            self.data.push(b'\n');
            return;
        }
        self.data.extend_from_slice(v);
    }
}

/// An element open, as the encoder sees it. Elements that the module knows of (`real`) are
/// those of its stack; others (past a bound) are kept in the skeleton, with their end tags.
struct Frame<'d> {
    name: &'d [u8],
    real: bool,
    /// The container of its text: `UNSET` before the first value, `KEPT` where it stays in the
    /// skeleton.
    cont: u32,
}

const UNSET: u32 = u32::MAX;
const KEPT: u32 = u32::MAX - 1;

/// What FxHash does.
#[derive(Default)]
struct Fx(u64);

impl std::hash::Hasher for Fx {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, b: &[u8]) {
        let mut h = self.0;
        let (chunks, rest) = b.as_chunks::<8>();
        for c in chunks {
            h = (h.rotate_left(5) ^ u64::from_le_bytes(*c)).wrapping_mul(0x517c_c1b7_2722_0a95);
        }
        let mut last = 0u64;
        for &x in rest {
            last = last << 8 | x as u64;
        }
        self.0 = (h.rotate_left(5) ^ last).wrapping_mul(0x517c_c1b7_2722_0a95);
    }

    fn write_u8(&mut self, v: u8) {
        self.0 = (self.0.rotate_left(5) ^ v as u64).wrapping_mul(0x517c_c1b7_2722_0a95);
    }

    fn write_usize(&mut self, v: usize) {
        self.0 = (self.0.rotate_left(5) ^ v as u64).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}

type Key<'d> = (u8, &'d [u8], &'d [u8]);

/// What the token at a `<` is.
enum Tok {
    /// A comment, CDATA section, processing instruction or declaration, up to `end`.
    Kept(usize),
    /// A start tag (its attributes in [`Segment::attrs`]).
    Start { end: usize, name: (usize, usize), empty: bool },
    /// An end tag.
    End(usize),
}

/// A segment as it is encoded.
struct Segment<'d, 'l> {
    d: &'d [u8],
    lim: &'l Limits,
    skeleton: Vec<u8>,
    conts: Vec<Container>,
    index: HashMap<Key<'d>, u32, BuildHasherDefault<Fx>>,
    /// The container of each value, in the order they are taken.
    seq: Vec<u32>,
    stack: Vec<Frame<'d>>,
    reals: usize,
    /// The most elements the module has open.
    depth: usize,
    /// The names open at the start.
    open: Vec<&'d [u8]>,
    /// The last tag's attributes: name and value, as ranges of `d`.
    attrs: Vec<[usize; 4]>,
    /// Searches that found nothing, so that no later one will: comment, CDATA, PI and declaration
    /// ends, quotes.
    none: [bool; 7],
    /// Where the segment ends, and the elements open there.
    end: usize,
    next: Vec<&'d [u8]>,
}

const NO_COMMENT: usize = 0;
const NO_CDATA: usize = 1;
const NO_PI: usize = 2;
const NO_BRACKET: usize = 3;
const NO_GT: usize = 4;
const NO_QUOTE: usize = 5;

/// `s` put out, a special byte after ESC.
fn put_escaped(out: &mut Vec<u8>, s: &[u8]) {
    let mut from = 0;
    while let Some(p) = s[from..].iter().position(|&b| special(b)) {
        out.extend_from_slice(&s[from..from + p]);
        out.push(ESC);
        from += p;
        out.push(s[from]);
        from += 1;
    }
    out.extend_from_slice(&s[from..]);
}

impl<'d, 'l> Segment<'d, 'l> {
    /// The segment of `d` from `start`, `open` the element open there, cut after an end tag
    /// once it has `lim.segment` bytes.
    fn run(d: &'d [u8], start: usize, open: Vec<&'d [u8]>, lim: &'l Limits) -> Segment<'d, 'l> {
        let mut s = Segment {
            d,
            lim,
            skeleton: Vec::new(),
            conts: Vec::new(),
            index: HashMap::default(),
            seq: Vec::new(),
            stack: open.iter().map(|&name| Frame { name, real: true, cont: UNSET }).collect(),
            reals: open.len(),
            depth: open.len(),
            open,
            attrs: Vec::new(),
            none: [false; 7],
            end: d.len(),
            next: Vec::new(),
        };
        let (mut i, mut ts) = (start, start);
        let mut cut = false;
        while let Some(p) = memchr(b'<', &d[i..]) {
            let at = i + p;
            let Some(t) = s.token(at) else {
                i = at + 1;
                continue;
            };
            s.text(ts, at);
            match t {
                Tok::Kept(end) => {
                    put_escaped(&mut s.skeleton, &d[at..end]);
                    i = end;
                }
                Tok::Start { end, name, empty } => {
                    s.start(at, end, &d[name.0..name.1], empty);
                    i = end;
                    cut = empty && s.cuts(i - start);
                }
                Tok::End(end) => {
                    s.end_tag(at, end);
                    i = end;
                    cut = s.cuts(i - start);
                }
            }
            ts = i;
            if cut {
                break;
            }
        }
        if !cut {
            s.text(ts, d.len());
        } else {
            s.end = i;
            s.next = s.stack.iter().map(|f| f.name).collect();
        }
        s
    }

    /// Whether the segment may end here, `n` bytes long: it is long enough and few elements
    /// are open, all of them of the module's stack.
    fn cuts(&self, n: usize) -> bool {
        n >= self.lim.segment.max(1) && self.stack.len() <= CUT_OPEN && self.stack.iter().all(|f| f.real)
    }

    /// The token at the `<` at `at`, if there is one.
    fn token(&mut self, at: usize) -> Option<Tok> {
        let d = self.d;
        let rest = &d[at..];
        match *rest.get(1)? {
            b'!' if rest.starts_with(b"<!--") => self.find(at + 4, b"-->", NO_COMMENT).map(Tok::Kept),
            b'!' if rest.starts_with(b"<![CDATA[") => self.find(at + 9, b"]]>", NO_CDATA).map(Tok::Kept),
            b'!' => {
                // (a declaration: up to its `>`, past the `]` of an internal subset)
                let mut from = at + 2;
                if let Some(p) = memchr::memchr2(b'>', b'[', &d[from..]).filter(|&p| d[from + p] == b'[') {
                    from += p;
                    if self.none[NO_BRACKET] {
                        return None;
                    }
                    match memchr(b']', &d[from..]) {
                        Some(p) => from += p,
                        None => {
                            self.none[NO_BRACKET] = true;
                            return None;
                        }
                    }
                }
                if self.none[NO_GT] {
                    return None;
                }
                match memchr(b'>', &d[from..]) {
                    Some(p) => Some(Tok::Kept(from + p + 1)),
                    None => {
                        self.none[NO_GT] = true;
                        None
                    }
                }
            }
            b'?' => self.find(at + 2, b"?>", NO_PI).map(Tok::Kept),
            b'/' => {
                // (an end tag of an element open: bytes, none of them `<` or special, up to `>`)
                self.stack.last()?;
                let n = rest[2..].iter().position(|&b| b == b'>' || b == b'<' || special(b))?;
                (rest[2 + n] == b'>').then_some(Tok::End(at + 2 + n + 1))
            }
            _ => self.start_tag(at),
        }
    }

    /// The end of `pat` found from `from`, within a search that has found none (`none`) so far.
    fn find(&mut self, from: usize, pat: &[u8], none: usize) -> Option<usize> {
        if self.none[none] {
            return None;
        }
        match memmem::find(&self.d[from..], pat) {
            Some(p) => Some(from + p + pat.len()),
            None => {
                self.none[none] = true;
                None
            }
        }
    }

    /// The start tag at `at` of the grammar of FORMAT.md, its attributes put in `attrs`.
    fn start_tag(&mut self, at: usize) -> Option<Tok> {
        let d = self.d;
        let skip = |mut i: usize| {
            while d.get(i).is_some_and(|&b| space(b)) {
                i += 1;
            }
            i
        };
        let name = |mut i: usize| {
            let start = i;
            while d.get(i).is_some_and(|&b| name_byte(b)) {
                i += 1;
            }
            (i > start).then_some(i)
        };
        let n1 = name(at + 1)?;
        let mut i = n1;
        self.attrs.clear();
        loop {
            let j = skip(i);
            match d.get(j)? {
                b'>' => return Some(Tok::Start { end: j + 1, name: (at + 1, n1), empty: false }),
                b'/' if d.get(j + 1) == Some(&b'>') => {
                    return Some(Tok::Start { end: j + 2, name: (at + 1, n1), empty: true });
                }
                _ if j > i => {
                    let a1 = name(j)?;
                    let k = skip(a1);
                    if d.get(k) != Some(&b'=') {
                        return None;
                    }
                    let k = skip(k + 1);
                    let q = *d.get(k).filter(|&&q| q == b'"' || q == b'\'')?;
                    let flag = NO_QUOTE + (q == b'\'') as usize;
                    if self.none[flag] {
                        return None;
                    }
                    let Some(p) = memchr(q, &d[k + 1..]) else {
                        self.none[flag] = true;
                        return None;
                    };
                    if p > MAX_VALUE {
                        return None;
                    }
                    self.attrs.push([j, a1, k + 1, k + 1 + p]);
                    i = k + 2 + p;
                }
                _ => return None,
            }
        }
    }

    /// The container of context `key`, made if there is room for it.
    fn container(&mut self, key: Key<'d>) -> Option<u32> {
        if let Some(&c) = self.index.get(&key) {
            return Some(c);
        }
        if self.conts.len() >= self.lim.containers {
            return None;
        }
        let c = self.conts.len() as u32;
        self.conts.push(Container::default());
        self.index.insert(key, c);
        Some(c)
    }

    fn put(&mut self, c: u32, v: &[u8]) {
        self.skeleton.push(VALUE);
        self.conts[c as usize].put(v);
        self.seq.push(c);
    }

    /// The text `d[from..to]` before a token: a value of the element open, or kept.
    fn text(&mut self, from: usize, to: usize) {
        if from >= to {
            return;
        }
        let t = &self.d[from..to];
        if let Some(top) = self.stack.last()
            && top.real
            && t.len() <= MAX_VALUE
            && t.iter().any(|&b| !space(b))
        {
            let mut c = top.cont;
            if c == UNSET {
                let n = self.stack.len();
                let parent = self.stack[..n - 1].iter().rev().find(|f| f.real).map_or(&[][..], |f| f.name);
                c = self.container((0, parent, self.stack[n - 1].name)).unwrap_or(KEPT);
                self.stack[n - 1].cont = c;
            }
            if c != KEPT {
                self.put(c, t);
                return;
            }
        }
        put_escaped(&mut self.skeleton, t);
    }

    /// The start tag `d[at..end]` of element `name`, coded or (past a bound) kept.
    fn start(&mut self, at: usize, end: usize, name: &'d [u8], empty: bool) {
        let d = self.d;
        let new = self.attrs.iter().filter(|a| !self.index.contains_key(&(1, name, &d[a[0]..a[1]]))).count();
        let real = (empty || self.reals < self.lim.depth) && self.conts.len() + new <= self.lim.containers;
        if real {
            self.skeleton.push(OPEN);
            let mut from = at;
            for k in 0..self.attrs.len() {
                let [a0, a1, v0, v1] = self.attrs[k];
                self.skeleton.extend_from_slice(&d[from..v0]);
                from = v1;
                let c = self.container((1, name, &d[a0..a1])).expect("room made for it");
                self.put(c, &d[v0..v1]);
            }
            self.skeleton.extend_from_slice(&d[from..end]);
        } else {
            put_escaped(&mut self.skeleton, &d[at..end]);
        }
        if !empty {
            self.stack.push(Frame { name, real, cont: UNSET });
            if real {
                self.reals += 1;
                self.depth = self.depth.max(self.reals);
            }
        }
    }

    /// The end tag `d[at..end]` of the innermost element.
    fn end_tag(&mut self, at: usize, end: usize) {
        let d = self.d;
        let f = self.stack.pop().expect("an element open");
        if !f.real {
            put_escaped(&mut self.skeleton, &d[at..end]);
            return;
        }
        self.reals -= 1;
        if &d[at + 2..end - 1] == f.name {
            self.skeleton.push(CLOSE);
        } else {
            self.skeleton.push(END);
            self.skeleton.extend_from_slice(&d[at..end]);
        }
    }

    /// The contexts of the containers, in order.
    fn contexts(&self) -> Vec<Context> {
        let mut out: Vec<Context> =
            vec![Context { kind: 0, a: Vec::new(), b: Vec::new(), values: 0 }; self.conts.len()];
        for (&(kind, a, b), &c) in &self.index {
            out[c as usize] = Context { kind, a: a.to_vec(), b: b.to_vec(), values: 0 };
        }
        self.seq.iter().for_each(|&c| out[c as usize].values += 1);
        out
    }

    /// The application of the segment of `len` bytes, its streams put into `f` (and its store,
    /// [`aqr_formula::store::spill`]).
    fn finish(self, f: &mut Formula, len: u64) -> Node {
        let mut params = Vec::new();
        varint::put(&mut params, self.conts.len() as u64);
        varint::put(&mut params, self.depth as u64);
        varint::put(&mut params, self.open.len() as u64);
        for name in &self.open {
            varint::put(&mut params, name.len() as u64);
            params.extend_from_slice(name);
        }
        let (mut table, mut lens) = (Vec::new(), Vec::new());
        for c in &self.conts {
            table.push(c.lens.is_some() as u8);
            varint::put(&mut table, c.data.len() as u64);
        }
        let mut conts = self.conts;
        for &c in &self.seq {
            let c = &mut conts[c as usize];
            if let Some(l) = &c.lens {
                varint::put(&mut lens, l[c.next] as u64);
                c.next += 1;
            }
        }
        let first = f.streams.len();
        let skeleton = f.push_as("xml/skeleton", Kind::Text, self.skeleton);
        let table = f.push("xml/table", table);
        let lens = f.push("xml/lens", lens);
        // (a container of its own stream, text that aqr-compress recompresses in turn; the
        // small ones share one)
        let mut small = Vec::new();
        let mut parts = Vec::new();
        for c in conts {
            match c.data.len() < SMALL {
                true => {
                    parts.push(Err((small.len(), c.data.len())));
                    small.extend_from_slice(&c.data);
                }
                false => parts.push(Ok(f.push_content("xml/values", Kind::Text, c.data))),
            }
        }
        let shared = f.streams.len();
        if !small.is_empty() {
            f.push_content("xml/values", Kind::Text, small);
        }
        let values = parts.into_iter().map(|p| match p {
            Ok(n) => n,
            Err((at, n)) => Node::slice(shared, at as u64, n as u64),
        });
        let values = Node::concat(values.collect::<Vec<_>>());
        f.streams[first..].iter_mut().for_each(|s| aqr_formula::store::spill(&mut s.data));
        Node::apply(MODULE, params, vec![skeleton, table, lens, values], len)
    }
}

#[cfg(test)]
mod tests;
