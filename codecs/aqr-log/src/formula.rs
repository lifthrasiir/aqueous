//! aqr-log as a module of entry formulas (see aqr-formula): lines of text rebuilt from a
//! template for each line and the fields cut out of it, stored column by column (FORMAT.md,
//! "The module"), for the archive to compress.
//!
//! The encoder goes over a segment of lines three times. The first counts the forms of the
//! tokens (their digit runs masked) to tell the words that vary from those that make templates;
//! the second finds each line's template and gathers the tokens that tell each column's number
//! style; the third writes the streams.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

use aqr_common::varint;
use aqr_formula::{Formula, Kind, Module, Node};
use aqr_num::fixed;
use aqr_num::style::{Style, choose_with};

use crate::module::{
    K_NEW, K_SAME, M_DIGITS, M_NUM, M_TEXT, MAX_COLUMNS, MAX_DIGITS, MAX_TEMPLATE_BYTES, MAX_TEMPLATES, P_COLUMN,
    P_LINE, P_RAW, Params,
};

pub use crate::module::decode;

pub const MODULE: Module = Module::new("log-recons", 1).with_plan(memory);

/// The memory of the module: its initial memory, and its plan ([`crate::module::plan`]).
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let &[len] = u.outputs else { return None };
    Some(u.initial() + crate::module::plan(u.params, u.inputs, len)?)
}

/// The names of the inputs' streams, in order.
pub const STREAM_NAMES: [&str; 9] =
    ["layout", "raw", "templates", "columns", "text", "nums", "widths", "types", "desc"];

/// A file shorter than this is not taken.
pub const MIN_LEN: usize = 16 << 10;

/// The bytes at the start of a file that tell whether it is lines of text this crate takes.
const SAMPLE: usize = 64 << 10;
/// The lines of a file's start there must be at least.
const MIN_LINES: usize = 16;

/// The tokens of a column that its numbers' style is chosen from.
const STYLE_SAMPLE: usize = 512;

/// The form of a token (its digit runs masked) is a template's own where the segment has it
/// at least this many times, or as many times as 1 in [`RARE_PER`] lines, whichever is more;
/// else the token is a word kept in its column.
const RARE_MIN: usize = 64;
const RARE_PER: usize = 500;

/// The bytes of lines of a segment at least (but the last), coded on its own: what the encoder
/// and the module hold of a file at once.
pub const SEGMENT: usize = 64 << 20;

/// The longest token that may be a whole number.
const MAX_NUMBER: usize = 40;

/// The bytes of a segment's start that the locale of its numbers is told from.
const LOCALE_SAMPLE: usize = 256 << 10;

/// The bytes that cut a line into tokens: a space, `;` and TAB always, and `,` unless the
/// segment's numbers have a decimal comma ([`decimal_comma`]).
fn separators(seg: &[u8]) -> [bool; 256] {
    let mut sep = [false; 256];
    for b in [b' ', b';', b'\t'] {
        sep[b as usize] = true;
    }
    sep[b',' as usize] = !decimal_comma(seg);
    sep
}

/// Whether the numbers of a segment, from its first [`LOCALE_SAMPLE`] bytes, are written with a
/// decimal comma: tokens (cut at spaces, `;` and TAB) that are a whole number with a comma, such
/// as `4,216` or `-0,05`, outnumber those with a point, and are not likelier a thousands
/// grouping. A comma followed by exactly three digits, after one to three digits not starting
/// with a zero (`1,234`), may be either: those count for grouping, and the others (`0,418`,
/// `18,5`, `1234,567`) for the decimal comma, which must be at least as many.
fn decimal_comma(seg: &[u8]) -> bool {
    let mut n = seg.len().min(LOCALE_SAMPLE);
    if n < seg.len() {
        n = seg[..n].iter().rposition(|&b| b == b'\n').map_or(n, |i| i + 1);
    }
    let (mut comma, mut grouping, mut point) = (0usize, 0usize, 0usize);
    for line in lines_of(&seg[..n]) {
        for t in line.split(|&b| matches!(b, b' ' | b';' | b'\t')) {
            // (a number: a sign, digits, one point or comma, digits, an exponent)
            let t = t.strip_prefix(b"-").or_else(|| t.strip_prefix(b"+")).unwrap_or(t);
            let int = t.iter().take_while(|b| b.is_ascii_digit()).count();
            let Some(&sep @ (b'.' | b',')) = t.get(int) else { continue };
            let frac = t[int + 1..].iter().take_while(|b| b.is_ascii_digit()).count();
            let rest = &t[int + 1 + frac..];
            let exp = rest.is_empty() || (matches!(rest[0], b'e' | b'E') && rest.len() > 1);
            if frac == 0 || !exp || t.len() > MAX_NUMBER {
                continue;
            }
            match sep {
                b'.' => point += 1,
                _ if frac == 3 && (1..=3).contains(&int) && t[0] != b'0' && rest.is_empty() => grouping += 1,
                _ => comma += 1,
            }
        }
    }
    comma > point && comma >= grouping
}

/// Whether `d` is a file this crate takes: of at least [`MIN_LEN`] bytes, of at least
/// [`MIN_LINES`] lines at its start of which 9 in 10 or more are not kept as they are.
pub fn starts(d: &[u8]) -> bool {
    if d.len() < MIN_LEN {
        return false;
    }
    let s = &d[..d.len().min(SAMPLE)];
    let (mut lines, mut raw) = (0, 0);
    let mut at = 0;
    while let Some(i) = memchr::memchr(b'\n', &s[at..]) {
        lines += 1;
        raw += memchr::memchr3(M_DIGITS, M_TEXT, M_NUM, &s[at..at + i]).is_some() as usize;
        at += i + 1;
    }
    lines >= MIN_LINES && raw * 10 < lines
}

/// A hasher of hashes and bytes, quick: Fx's multiplication, the result mixed.
#[derive(Default)]
struct Fx(u64);

const K: u64 = 0x517c_c1b7_2722_0a95;

impl Hasher for Fx {
    fn finish(&self) -> u64 {
        let h = self.0 ^ (self.0 >> 32);
        (h.wrapping_mul(K) ^ (h >> 29)).wrapping_mul(K)
    }
    fn write(&mut self, b: &[u8]) {
        let (words, r) = b.as_chunks::<8>();
        for w in words {
            self.write_u64(u64::from_le_bytes(*w));
        }
        if !r.is_empty() {
            let mut w = [0u8; 8];
            w[..r.len()].copy_from_slice(r);
            self.write_u64(u64::from_le_bytes(w) ^ (r.len() as u64) << 56);
        }
    }
    fn write_u64(&mut self, v: u64) {
        self.0 = (self.0.rotate_left(5) ^ v).wrapping_mul(K);
    }
}

type FxMap<Key, V> = HashMap<Key, V, BuildHasherDefault<Fx>>;

/// The hash of a token's form (digit runs each as one mark), and whether each of them is of at
/// most [`MAX_DIGITS`] digits.
fn form(t: &[u8]) -> (u64, bool) {
    let (mut h, mut run, mut ok) = (0u64, 0usize, true);
    for &b in t {
        if b.is_ascii_digit() {
            if run == 0 {
                h = (h.rotate_left(5) ^ 1).wrapping_mul(K);
            }
            run += 1;
            ok &= run <= MAX_DIGITS;
        } else {
            run = 0;
            h = (h.rotate_left(5) ^ (b as u64 + 256)).wrapping_mul(K);
        }
    }
    (h, ok)
}

/// Whether token `t` may be a number: of a sign, a digit or a point first, and short.
fn numeric(t: &[u8]) -> bool {
    matches!(t.first(), Some(b'0'..=b'9' | b'-' | b'+' | b'.')) && t.len() <= MAX_NUMBER
}

/// Scratch space of the numbers' coding.
#[derive(Default)]
struct Scratch {
    tmp: Vec<u8>,
    back: Vec<u8>,
}

impl Scratch {
    /// `t` printed back from a coding, if it is the same.
    fn exact(&mut self, t: &[u8], r: (i64, i32, u8, u8)) -> Option<(i64, i32, u8, u8)> {
        self.back.clear();
        aqr_num::put(&mut self.back, r.0, r.1, r.2, r.3);
        (self.back == t).then_some(r)
    }

    /// The coding of `t` as a number on its own, if it is one: what decides it is one.
    fn whole(&mut self, t: &[u8]) -> Option<(i64, i32, u8, u8)> {
        if !numeric(t) {
            return None;
        }
        // (a decimal comma: the fixed token with the comma as its point)
        let comma = t.contains(&b',');
        let dotted: Vec<u8>;
        let fixed_of = if comma {
            dotted = t.iter().map(|&b| if b == b',' { b'.' } else { b }).collect();
            if t.contains(&b'.') {
                return None;
            }
            &dotted[..]
        } else {
            t
        };
        if let Some((v, f)) = fixed::parse(fixed_of) {
            let mode = aqr_num::M_FIX | if comma { aqr_num::COMMA } else { 0 };
            return Some((v, (f & fixed::F_FRAC) as i32, mode, f));
        }
        if !t.iter().all(|b| matches!(b, b'0'..=b'9' | b'.' | b',' | b'e' | b'E' | b'+' | b'-'))
            || !t.iter().any(u8::is_ascii_digit)
        {
            return None;
        }
        let r = choose_with(&[(0, t)], true).encode(t, 0, &mut self.tmp)?;
        self.exact(t, r)
    }
}

/// What a token's slot is: its kind (a mark of the template) and where it is in the line.
struct Slot {
    kind: u8,
    start: usize,
    len: usize,
}

/// Templates and the classification of lines into them, for the second and third passes.
struct Classifier {
    /// The forms (hashes) of the tokens and how many times each is in the segment.
    counts: FxMap<u64, u32>,
    rare: u32,
    /// The bytes that cut a line into tokens.
    sep: [bool; 256],
    /// The templates' ids (from 1), by their bytes; the stream `templates`; the first column of
    /// each (by id - 1); the mark of each column; the most numbers of a template.
    index: FxMap<Vec<u8>, u32>,
    tbytes: Vec<u8>,
    col0: Vec<u32>,
    marks: Vec<u8>,
    numbers: usize,
    /// No template is added: a line of one not there is kept as it is.
    frozen: bool,
    tpl: Vec<u8>,
    slots: Vec<Slot>,
    scratch: Scratch,
}

impl Classifier {
    /// The counts of the forms of `seg`'s tokens.
    fn new(seg: &[u8]) -> Classifier {
        let sep = separators(seg);
        let mut counts: FxMap<u64, u32> = FxMap::default();
        let mut lines = 0usize;
        for line in lines_of(seg) {
            lines += 1;
            if memchr::memchr3(M_DIGITS, M_TEXT, M_NUM, line).is_some() {
                continue;
            }
            for t in line.split(|&b| sep[b as usize]) {
                *counts.entry(form(t).0).or_default() += 1;
            }
        }
        let rare = RARE_MIN.max(lines / RARE_PER).min(u32::MAX as usize) as u32;
        Classifier {
            counts,
            rare,
            sep,
            index: FxMap::default(),
            tbytes: Vec::new(),
            col0: Vec::new(),
            marks: Vec::new(),
            numbers: 0,
            frozen: false,
            tpl: Vec::new(),
            slots: Vec::new(),
            scratch: Scratch::default(),
        }
    }

    /// The template of `line` (an id from 1; its slots in `self.slots`), or 0 if the line is
    /// kept as it is.
    fn line(&mut self, line: &[u8]) -> u32 {
        if memchr::memchr3(M_DIGITS, M_TEXT, M_NUM, line).is_some() {
            return 0;
        }
        self.tpl.clear();
        self.slots.clear();
        let mut at = 0;
        loop {
            let end = line[at..].iter().position(|&b| self.sep[b as usize]).map_or(line.len(), |i| at + i);
            self.token(&line[at..end], at);
            if end == line.len() {
                break;
            }
            self.tpl.push(line[end]);
            at = end + 1;
        }
        if let Some(&id) = self.index.get(&self.tpl[..]) {
            return id;
        }
        let nums = self.slots.iter().filter(|s| s.kind != M_TEXT).count();
        if self.frozen
            || self.col0.len() >= MAX_TEMPLATES
            || self.marks.len() + self.slots.len() > MAX_COLUMNS
            || self.tbytes.len() + self.tpl.len() + 1 > MAX_TEMPLATE_BYTES
        {
            return 0;
        }
        let id = self.col0.len() as u32 + 1;
        self.index.insert(self.tpl.clone(), id);
        self.tbytes.extend_from_slice(&self.tpl);
        self.tbytes.push(b'\n');
        self.col0.push(self.marks.len() as u32);
        self.marks.extend(self.slots.iter().map(|s| s.kind));
        self.numbers = self.numbers.max(nums);
        id
    }

    /// Adds token `t` (at `at` in the line) to the template and slots.
    fn token(&mut self, t: &[u8], at: usize) {
        let slot = |c: &mut Classifier, kind: u8, start: usize, len: usize| {
            c.tpl.push(kind);
            c.slots.push(Slot { kind, start, len });
        };
        if numeric(t) && self.scratch.whole(t).is_some() {
            return slot(self, M_NUM, at, t.len());
        }
        let (h, ok) = form(t);
        if !ok || self.counts.get(&h).is_none_or(|&n| n < self.rare) {
            return slot(self, M_TEXT, at, t.len());
        }
        let mut i = 0;
        while i < t.len() {
            if t[i].is_ascii_digit() {
                let n = t[i..].iter().take_while(|b| b.is_ascii_digit()).count();
                slot(self, M_DIGITS, at + i, n);
                i += n;
            } else {
                self.tpl.push(t[i]);
                i += 1;
            }
        }
    }
}

/// The lines of `seg`: each cut at a LF; the last one with none, if the segment does not end
/// with it.
fn lines_of(seg: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut at = 0;
    std::iter::from_fn(move || {
        if at >= seg.len() {
            return None;
        }
        let e = memchr::memchr(b'\n', &seg[at..]).map_or(seg.len(), |i| at + i);
        let line = &seg[at..e];
        at = e + 1;
        Some(line)
    })
}

/// The first tokens of a column's numbers, which its style is chosen from.
#[derive(Default)]
struct Sample {
    bytes: Vec<u8>,
    lens: Vec<u8>,
}

/// A column's parts as the third pass makes them.
#[derive(Default)]
struct Col {
    text: Vec<u8>,
    /// The integers as varints by each predictor.
    by: [Vec<u8>; 3],
    widths: Vec<u8>,
    types: Vec<u8>,
    desc: Vec<u8>,
    style: Style,
    prev: i64,
    /// The scale of the column's previous number (0 before the first, and for digit runs).
    prev_scale: i32,
    last: Option<(u8, u8, i32)>,
    count: usize,
}

/// The size of `b` compressed fast, which tells predictors apart.
fn packed(b: &[u8]) -> usize {
    zstd::bulk::compress(b, 1).map_or(b.len(), |c| c.len())
}

/// The streams of a file: a segment's are put one after the other.
#[derive(Default)]
struct Streams {
    layout: Vec<u8>,
    raw: Vec<u8>,
    templates: Vec<u8>,
    columns: Vec<u8>,
    text: Vec<u8>,
    nums: Vec<u8>,
    widths: Vec<u8>,
    types: Vec<u8>,
    desc: Vec<u8>,
}

impl Streams {
    /// The lengths of the streams, in the order of [`STREAM_NAMES`].
    fn lens(&self) -> [usize; 9] {
        [
            &self.layout,
            &self.raw,
            &self.templates,
            &self.columns,
            &self.text,
            &self.nums,
            &self.widths,
            &self.types,
            &self.desc,
        ]
        .map(|v| v.len())
    }
}

/// Appends segment `seg` (the last of the file if `last`) to `s`: its params, and the bytes of
/// its lines kept as they are.
fn segment(seg: &[u8], last: bool, s: &mut Streams) -> (Params, usize) {
    let mut cl = Classifier::new(seg);
    // the second pass: templates, and the samples of the numbers' styles
    let mut samples: Vec<Sample> = Vec::new();
    for line in lines_of(seg) {
        let id = cl.line(line);
        if id == 0 {
            continue;
        }
        if samples.len() < cl.marks.len() {
            samples.resize_with(cl.marks.len(), Sample::default);
        }
        let c0 = cl.col0[id as usize - 1] as usize;
        for (k, sl) in cl.slots.iter().enumerate() {
            let sm = &mut samples[c0 + k];
            if sl.kind == M_NUM && sm.lens.len() < STYLE_SAMPLE {
                sm.bytes.extend_from_slice(&line[sl.start..][..sl.len]);
                sm.lens.push(sl.len as u8);
            }
        }
    }
    cl.frozen = true;
    let mut cols: Vec<Col> = (0..cl.marks.len()).map(|_| Col::default()).collect();
    for (c, sm) in cols.iter_mut().zip(samples) {
        let mut at = 0;
        let toks: Vec<(usize, &[u8])> = (sm.lens.iter())
            .map(|&n| {
                at += n as usize;
                (0, &sm.bytes[at - n as usize..at])
            })
            .collect();
        c.style = choose_with(&toks, true);
    }
    // the third: the fields
    let (mut prev, mut cur): (Vec<(i64, i32)>, Vec<(i64, i32)>) = (Vec::new(), Vec::new());
    let mut sc = Scratch::default();
    let mut raw = 0;
    for line in lines_of(seg) {
        let id = cl.line(line);
        varint::put(&mut s.layout, id as u64);
        if id == 0 {
            raw += line.len() + 1;
            s.raw.extend_from_slice(line);
            s.raw.push(b'\n');
            continue;
        }
        let c0 = cl.col0[id as usize - 1] as usize;
        cur.clear();
        for (k, sl) in cl.slots.iter().enumerate() {
            let c = &mut cols[c0 + k];
            let t = &line[sl.start..][..sl.len];
            c.count += 1;
            let (v, scale) = match sl.kind {
                M_TEXT => {
                    c.text.extend_from_slice(t);
                    c.text.push(b'\n');
                    continue;
                }
                M_DIGITS => {
                    // (a width where leading zeros pad it)
                    c.widths.push(if t.len() > 1 && t[0] == b'0' { t.len() as u8 } else { 0 });
                    (t.iter().fold(0i64, |v, &d| v * 10 + (d - b'0') as i64), 0)
                }
                _ => {
                    let last = c.last.map_or(0, |l| l.2);
                    let r = c.style.encode(t, last, &mut sc.tmp).and_then(|r| sc.exact(t, r));
                    let (v, scale, mode, f) = r.or_else(|| sc.whole(t)).expect("a token of a number slot is a number");
                    if c.last == Some((mode, f, scale)) {
                        c.types.push(K_SAME);
                    } else {
                        c.last = Some((mode, f, scale));
                        c.types.push(K_NEW);
                        c.desc.extend_from_slice(&[mode, f]);
                        varint::put_signed(&mut c.desc, scale as i64);
                    }
                    (v, scale)
                }
            };
            // (the predictions at the scale of the number)
            let (q, qs) = prev.get(cur.len()).copied().unwrap_or((0, 0));
            varint::put_signed(&mut c.by[0], v);
            varint::put_signed(&mut c.by[1], v.wrapping_sub(fixed::rescale(c.prev, c.prev_scale, scale)));
            varint::put_signed(&mut c.by[2], v.wrapping_sub(fixed::rescale(q, qs, scale)));
            c.prev = v;
            c.prev_scale = scale;
            cur.push((v, scale));
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    // the columns' parts, put in order
    s.templates.extend_from_slice(&cl.tbytes);
    for (c, &mark) in cols.iter_mut().zip(&cl.marks) {
        let [raw_v, col_v, line_v] = std::mem::take(&mut c.by);
        let (pred, nums) = match mark {
            M_TEXT => (P_RAW, Vec::new()),
            _ => {
                // (the predictor whose varints zstd packs smaller, the shortest where they are few)
                let cands = [(P_RAW, raw_v), (P_COLUMN, col_v), (P_LINE, line_v)];
                let few = c.count <= 8;
                let cost = |b: &Vec<u8>| if few { b.len() } else { packed(b) };
                let best = (0..3).min_by_key(|&i| (cost(&cands[i].1), i)).unwrap();
                cands.into_iter().nth(best).unwrap()
            }
        };
        if mark == M_TEXT {
            varint::put(&mut s.columns, c.text.len() as u64);
            s.text.extend_from_slice(&c.text);
            continue;
        }
        s.columns.push(pred);
        varint::put(&mut s.columns, nums.len() as u64);
        s.nums.extend_from_slice(&nums);
        if mark == M_DIGITS {
            s.widths.extend_from_slice(&c.widths);
        } else {
            varint::put(&mut s.columns, c.desc.len() as u64);
            s.types.extend_from_slice(&c.types);
            s.desc.extend_from_slice(&c.desc);
        }
    }
    let numbers = cl.numbers;
    let params = Params {
        no_final_lf: last && seg.last() != Some(&b'\n'),
        templates: cl.col0.len(),
        columns: cl.marks.len(),
        numbers,
    };
    (params, raw)
}

/// The formula of `input` as lines of text, or `None` where it is shorter than [`MIN_LEN`],
/// does not start as such ([`starts`]), or a tenth of it or more is kept as it is. It is the
/// same whatever `threads` is (one is used).
pub fn encode(input: &[u8], _threads: usize) -> Option<Formula> {
    if !starts(input) {
        return None;
    }
    encode_in(input, SEGMENT, true)
}

/// [`encode`] in segments of `segment` bytes of lines at least (the test of segments at lengths
/// shorter than [`SEGMENT`]); `check` that no tenth of the file is kept as it is. `None` for
/// an empty file.
pub fn encode_in(input: &[u8], segment_len: usize, check: bool) -> Option<Formula> {
    if input.is_empty() {
        return None;
    }
    let segment_len = segment_len.max(1);
    let mut s = Streams::default();
    let mut parts: Vec<(Params, [u64; 9], u64)> = Vec::new();
    let (mut from, mut raw) = (0, 0);
    while from < input.len() {
        // the segment: lines taken while one starts within `segment_len` bytes of its start
        let mut end = from;
        while end < input.len() && end - from < segment_len {
            end = memchr::memchr(b'\n', &input[end..]).map_or(input.len(), |i| end + i + 1);
        }
        let before = s.lens();
        let (params, r) = segment(&input[from..end], end == input.len(), &mut s);
        raw += r;
        if check && raw * 10 >= input.len() {
            return None;
        }
        let now = s.lens();
        parts.push((params, std::array::from_fn(|j| (now[j] - before[j]) as u64), (end - from) as u64));
        from = end;
    }
    let mut f = Formula::default();
    let kinds = [
        Kind::Unknown,
        Kind::Text,
        Kind::Text,
        Kind::Unknown,
        Kind::Text,
        Kind::Unknown,
        Kind::Unknown,
        Kind::Unknown,
        Kind::Unknown,
    ];
    let data = [s.layout, s.raw, s.templates, s.columns, s.text, s.nums, s.widths, s.types, s.desc];
    let whole: Vec<Node> = (STREAM_NAMES.iter().zip(kinds).zip(data))
        .map(|((name, kind), d)| f.push_as(format!("log/{name}"), kind, d))
        .collect();
    let base = f.streams.len() - 9;
    if let [(params, _, len)] = &parts[..] {
        f.root = Node::apply(MODULE, params.to_bytes(), whole, *len);
        return Some(f);
    }
    let mut at = [0u64; 9];
    let nodes: Vec<Node> = (parts.into_iter())
        .map(|(params, lens, len)| {
            let inputs: Vec<Node> = (0..9).map(|j| Node::slice(base + j, at[j], lens[j])).collect();
            at.iter_mut().zip(lens).for_each(|(a, n)| *a += n);
            Node::apply(MODULE, params.to_bytes(), inputs, len)
        })
        .collect();
    f.root = Node::concat(nodes);
    Some(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(f: &Formula, len: usize) -> crate::Result<Vec<u8>> {
        let Node::Apply(a) = &f.root else { panic!() };
        let inputs: Vec<&[u8]> = f.streams.iter().map(|s| &s.data[..]).collect();
        decode(&a.params, &inputs, len as u64, 1)
    }

    fn roundtrip(d: &[u8]) -> Formula {
        let f = encode_in(d, usize::MAX, false).unwrap();
        assert_eq!(eval(&f, d.len()).unwrap(), d, "{}", String::from_utf8_lossy(d));
        assert!(eval(&f, d.len() + 1).is_err());
        f
    }

    #[test]
    fn edge_cases() {
        for s in [
            &b"a\n"[..],
            b"a",
            b"\n",
            b"\n\n\n",
            b"a b\n",
            b"a b ",
            b"a  b   c\n",
            b" a\n a \n",
            b"x 1\r\ny 2\r\nz 3",
            b"x 1\r\ny 2\r\nz 3\r\n",
            b"007 0 00 000000000000000000 0000000000000000000 -0 -0.0 1.50 .5 1e5 1E5 1.5e+30 +3 9223372036854775808\n",
            b"a\x01b 1\nc\x02d 2\ne\x03f 3\n",
            b"\x00\xff\xfe 1\n",
            b"ver 1.2.3 build 20240101 at 12:34:56.789 id=0042 id=0043\n",
            b"a 1\n\xe2\x82\xac \xff 2",
        ] {
            roundtrip(s);
        }
        assert!(encode_in(b"", 10, false).is_none());
    }

    /// A log of lines of a few templates, in which what varies is cut out.
    fn log(lines: usize) -> String {
        let mut rng = aqr_common::rng::Rng::new(3);
        let mut s = String::new();
        for i in 0..lines {
            let (h, m) = (i / 3600 % 24, i / 60 % 60);
            let t = format!("Dec {:2} {h:02}:{m:02}:{:02}", 10 + i / 90000, i % 60);
            match i % 5 {
                0 => {
                    s += &format!(
                        "{t} host sshd[{}]: Accepted password for user{} from 10.0.{}.{} port {}\n",
                        1000 + i / 7,
                        rng.below(60),
                        rng.below(4),
                        rng.below(256),
                        40000 + rng.below(9999)
                    )
                }
                1 => {
                    s += &format!(
                        "{t} host sshd[{}]: Failed password for {} from 10.0.{}.{} port {} ssh2\n",
                        1000 + i / 7,
                        ["root", "admin", "bob"][i % 3],
                        rng.below(4),
                        rng.below(256),
                        40000 + rng.below(9999)
                    )
                }
                2 => {
                    s += &format!(
                        "{t} host kernel: temp {}.{:02} C load {:.2}\n",
                        40 + rng.below(20),
                        rng.below(100),
                        rng.below(300) as f64 / 100.0
                    )
                }
                3 => {
                    let w: String = (0..8).map(|_| (b'a' + rng.below(26) as u8) as char).collect();
                    s += &format!("{t} host app: key-{w} value {}\n", -(rng.below(100) as i64))
                }
                _ => s += &format!("{t}\n"),
            }
        }
        s
    }

    #[test]
    fn templates() {
        let d = log(6000);
        let f = roundtrip(d.as_bytes());
        let templates = String::from_utf8_lossy(&f.streams[2].data).into_owned();
        // (the marks: 1 a digit run, 2 a word, 3 a number)
        assert!(templates.contains("Dec \x03 \x01:\x01:\x01 host kernel: temp \x03 C load \x03\n"), "{templates:?}");
        assert!(
            templates.contains("Accepted password for user\x01 from \x01.\x01.\x01.\x01 port \x03\n"),
            "{templates:?}"
        );
        // (the rare words are words of the column of text)
        assert!(String::from_utf8_lossy(&f.streams[4].data).contains("key-"));
        assert!(f.streams[1].data.is_empty());
        // zeros that pad a digit run are kept by its width
        let d = "12:05:09 a\n12:15:59 b\n".repeat(100);
        let f = roundtrip(d.as_bytes());
        assert!(f.streams[6].data.contains(&2));
    }

    /// Tokens are cut at `;`, TAB and `,` too, `,` excepted where the numbers have a decimal comma.
    #[test]
    fn separators() {
        let mut rng = aqr_common::rng::Rng::new(11);
        let (mut a, mut b, mut c) = (String::new(), String::new(), String::new());
        for i in 0..3000 {
            let (x, y, z) = (rng.below(2000), rng.below(1000), 4000 + i * 3 + rng.below(3));
            a += &format!("16/12/2006;17:24:00;{},{:03};{},{:02};{z},{}\n", x / 10, x % 1000, y / 100, y % 100, y % 10);
            b += &format!("{i},{x}.{y:03},{z}.5,-{y}.25\tk{}\n", i % 3);
            c += &format!("{i},\"a b\",{x}.{y:03};{y}\n");
        }
        let at = |t: &str| String::from_utf8_lossy(&roundtrip(t.as_bytes()).streams[2].data).into_owned();
        // semicolons, decimal commas: the numbers are whole tokens
        let t = at(&a);
        assert!(t.contains("\x01/\x01/\x01;\x01:\x01:\x01;\x03;\x03;\x03\n"), "{t:?}");
        // decimal points: commas and TABs cut
        let t = at(&b);
        assert!(t.contains("\x03,\x03,\x03,\x03\t"), "{t:?}");
        let t = at(&c);
        assert!(t.contains("\x03,"), "{t:?}");
    }

    /// Fixed numbers whose writer pads zeros are padded fixed (integers of their digits); a change
    /// of scale predicts from the previous number at the new scale.
    #[test]
    fn padded_numbers() {
        let mut rng = aqr_common::rng::Rng::new(5);
        let (mut d, mut price) = (String::new(), 4_228_358i64);
        for i in 0..4000 {
            price += rng.below(301) as i64 - 150;
            let q = rng.below(100000);
            d += &format!(
                "{},{}.{:02}000000,{}.{:05}000,{}\n",
                1704067200000i64 + i * 60000,
                price / 100,
                price % 100,
                q / 1000,
                q % 1000,
                i % 7
            );
        }
        let f = roundtrip(d.as_bytes());
        // (padded fixed, mode 6, of the price: not the values at 8 decimals)
        assert!(f.streams[8].data.contains(&6), "{:?}", &f.streams[8].data[..20]);
        assert!(f.streams[5].data.len() < 4000 * 8, "{}", f.streams[5].data.len());
        // a line before at another scale, an integer after a decimal
        roundtrip("x 1.50 1.5\ny 1 2.25\nz 0.1 3\nx 100 0.5\n".repeat(50).as_bytes());
    }

    #[test]
    fn raw_lines() {
        let mut d = log(2000).into_bytes();
        d.extend_from_slice(b"a line with a \x02 mark\n");
        d.extend_from_slice(&log(10).into_bytes());
        d.extend_from_slice(b"\x01\x03");
        let f = roundtrip(&d);
        assert_eq!(f.streams[1].data, b"a line with a \x02 mark\n\x01\x03\n"[..]);
        // a token of more than 18 digits is a word
        let d = "x 12345678901234567890z 7\n".repeat(200);
        let f = roundtrip(d.as_bytes());
        assert!(String::from_utf8_lossy(&f.streams[4].data).starts_with("12345678901234567890z\n"));
    }

    #[test]
    fn caps() {
        // more templates than a segment holds: the lines of the others are kept as they are
        let mut d = String::new();
        for i in 0..MAX_TEMPLATES + 500 {
            let word =
                |k: usize| format!("{}{}", (b'a' + (k / 26 % 26) as u8) as char, (b'a' + (k % 26) as u8) as char);
            d += &format!("{} {}\n", word(i % 300), word(i / 300));
        }
        let f = roundtrip(d.as_bytes());
        assert!(!f.streams[1].data.is_empty());
        let Node::Apply(a) = &f.root else { panic!() };
        assert_eq!(crate::module::Params::from_bytes(&a.params).unwrap().templates, MAX_TEMPLATES);
    }

    #[test]
    fn predictors() {
        // (a counter, steady differences against the line before, and as they are)
        let mut d = String::new();
        let mut rng = aqr_common::rng::Rng::new(9);
        for i in 0..3000u64 {
            d += &format!(
                "{} {} {} {}\n",
                1_000_000 + i * 7,
                rng.below(90) + 10,
                5000 - i,
                1_000_000 + i * 7 + rng.below(3)
            );
        }
        let f = roundtrip(d.as_bytes());
        let columns = &f.streams[3].data;
        assert!(columns.len() < 20, "{columns:?}");
        // (differences taken: smaller than 3000 numbers of 4 bytes)
        assert!(f.streams[5].data.len() < 3000 * 4 * 2, "{}", f.streams[5].data.len());
    }

    /// The file of a formula of segments (an application, or a Concat of them).
    fn eval_segments(f: &Formula) -> Vec<u8> {
        let apply = |n: &Node| {
            let Node::Apply(a) = n else { panic!("not an application") };
            let inputs: Vec<&[u8]> = (a.inputs.iter())
                .map(|n| match n {
                    &Node::Slice { stream, start, len } => &f.streams[stream].data[start as usize..][..len as usize],
                    _ => panic!("not a slice"),
                })
                .collect();
            decode(&a.params, &inputs, a.len, 1).unwrap()
        };
        match &f.root {
            Node::Concat(ns) => ns.iter().flat_map(apply).collect(),
            n => apply(n),
        }
    }

    #[test]
    fn segments() {
        let d = log(4000).into_bytes();
        let whole = encode_in(&d, usize::MAX, true).unwrap();
        assert!(matches!(whole.root, Node::Apply(_)));
        assert_eq!(eval_segments(&whole), d);
        for segment in [1, 100, 1000, 4096, 50_000, d.len() / 2] {
            let f = encode_in(&d, segment, true).unwrap();
            assert_eq!(eval_segments(&f), d, "segment {segment}");
            assert_eq!(f.streams.len(), 9);
            let Node::Concat(ns) = &f.root else { panic!("not segments at {segment}") };
            let mut at = 0;
            for n in ns {
                let Node::Apply(a) = n else { panic!() };
                at += a.len as usize;
                // (the segments end at lines' ends)
                assert!(at == d.len() || d[at - 1] == b'\n');
            }
            assert_eq!(at, d.len());
        }
        // a file without the last LF: the last segment only says so
        let d = &d[..d.len() - 1];
        for segment in [1, 1000, usize::MAX] {
            assert_eq!(eval_segments(&encode_in(d, segment, true).unwrap()), d);
        }
    }

    #[test]
    fn not_taken() {
        assert!(!starts(&b"a b\n".repeat(100)));
        assert!(encode(&b"a b\n".repeat(100), 1).is_none());
        // (a tenth or more kept as it is)
        let d = "x\x01y\n".repeat(3000) + &log(100);
        assert!(encode(d.as_bytes(), 1).is_none());
        assert!(!starts(&[b'a'; 20000]));
    }

    #[test]
    fn corrupt_input() {
        let s = log(400);
        let f = roundtrip(s.as_bytes());
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
        // params that lie
        for p in [&[][..], &[2, 0, 0, 0], &[0, 255, 255, 255, 255, 15, 0, 0], &a.params[..a.params.len() - 1]] {
            let inputs: Vec<&[u8]> = f.streams.iter().map(|s| &s.data[..]).collect();
            assert!(decode(p, &inputs, s.len() as u64, 1).is_err());
        }
    }
}
