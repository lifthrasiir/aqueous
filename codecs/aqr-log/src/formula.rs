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
use aqr_num::style::{Style, choose_with};
use aqr_num::{dec, fixed};

use crate::module::{
    K_NEW, K_SAME, M_DIGITS, M_NUM, M_TEXT, M_TIME, MAX_COLUMNS, MAX_DIGITS, MAX_TEMPLATE_BYTES, MAX_TEMPLATES,
    P_COLUMN, P_LINE, P_RAW, Params,
};
use crate::times::{Span, Times};

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
const STYLE_SAMPLE: usize = 1024;

/// The form of a token (its digit runs masked) is a template's own where the segment has it
/// at least this many times, or as many times as 1 in [`RARE_PER`] lines, whichever is more;
/// else the token is a word kept in its column.
const RARE_MIN: usize = 64;
const RARE_PER: usize = 500;

/// The bytes of lines of a segment at least (but the last), coded on its own: what the encoder
/// and the module hold of a file at once.
pub const SEGMENT: usize = 64 << 20;

/// The most segments coded at once: each holds its working memory (the counts of its tokens'
/// forms, the timestamps and templates of its lines, its streams) on top of the input.
const AT_ONCE: usize = 4;

/// Whether `line` has a byte that marks a template (such a line is kept as it is).
fn has_mark(line: &[u8]) -> bool {
    memchr::memchr3(M_DIGITS, M_TEXT, M_NUM, line).is_some() || memchr::memchr(M_TIME, line).is_some()
}

/// The longest token that may be a whole number.
const MAX_NUMBER: usize = 40;

/// The bytes of a segment's start that the locale of its numbers is told from.
const LOCALE_SAMPLE: usize = 256 << 10;

/// The bytes that cut a line into tokens: a space only, or (`wide`) a space, `;` and TAB, and `,`
/// unless the segment's numbers have a decimal comma ([`decimal_comma`]).
fn separators(seg: &[u8], wide: bool) -> [bool; 256] {
    let mut sep = [false; 256];
    sep[b' ' as usize] = true;
    if wide {
        sep[b';' as usize] = true;
        sep[b'\t' as usize] = true;
        sep[b',' as usize] = !decimal_comma(seg);
    }
    sep
}

/// The bytes of a segment's start that the choice of separators is made on: coded both ways, the
/// streams' sizes (zstd -3) compared.
const TRIAL: usize = 1 << 20;

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
        raw += has_mark(&s[at..at + i]) as usize;
        at += i + 1;
    }
    lines >= MIN_LINES && raw * 10 < lines
}

/// A hasher of hashes and bytes, quick: Fx's multiplication, the result mixed.
#[derive(Default)]
pub(crate) struct Fx(u64);

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

pub(crate) type FxMap<Key, V> = HashMap<Key, V, BuildHasherDefault<Fx>>;

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

/// A piece of a line: a part of a token, a timestamp, or the separator after a token.
#[derive(Clone, Copy)]
enum Piece {
    Plain { start: usize, end: usize },
    Time { start: usize, end: usize, pat: u32, v: i64 },
    Sep(u8),
}

/// How lines are cut: at the separators, but for the timestamps found (a time of day with its
/// date, a syslog's `Dec 10 06:55:46`, may hold spaces), which are pieces on their own.
struct Splitter {
    sep: [bool; 256],
    times: Times,
    pieces: Vec<Piece>,
    /// The timestamps found in the lines of the first pass, to be given again to the others
    /// (they cut the same lines in the same order): for each line its length and how many
    /// there were, and all of them. Not kept beyond [`CACHE_LINES`] lines and [`CACHE_SPANS`]
    /// timestamps; the lines after are found again.
    seen: Vec<(u32, u32)>,
    kept: Vec<[u32; 3]>,
    kept_v: Vec<i64>,
    /// The lines cut in this pass so far, and where their timestamps are in `kept`.
    cursor: usize,
    next: usize,
    /// Whether the lines cut now are put in `seen`.
    recording: bool,
}

/// The most lines, and timestamps (20 bytes each), that a segment's are kept for.
const CACHE_LINES: usize = 4 << 20;
const CACHE_SPANS: usize = 2 << 20;

impl Splitter {
    fn new(seg: &[u8], wide: bool) -> Splitter {
        let mut n = seg.len().min(LOCALE_SAMPLE);
        if n < seg.len() {
            n = seg[..n].iter().rposition(|&b| b == b'\n').map_or(n, |i| i + 1);
        }
        Splitter {
            sep: separators(seg, wide),
            times: Times::new(&seg[..n]),
            pieces: Vec::new(),
            seen: Vec::new(),
            kept: Vec::new(),
            kept_v: Vec::new(),
            cursor: 0,
            next: 0,
            recording: true,
        }
    }

    /// The timestamps of `line` in `self.times.spans`: found in the first pass, and kept for
    /// the next ones.
    fn find(&mut self, line: &[u8]) {
        let at = self.cursor;
        self.cursor += 1;
        if self.recording {
            self.times.find(line);
            let n = self.times.spans.len();
            if at == self.seen.len() && at < CACHE_LINES && self.kept.len() + n <= CACHE_SPANS {
                // (a span is of a line that fits in 32 bits, which its length tells)
                if let Ok(len) = u32::try_from(line.len()) {
                    self.seen.push((len, n as u32));
                    for sp in &self.times.spans {
                        self.kept.push([sp.start as u32, sp.end as u32, sp.pat]);
                        self.kept_v.push(sp.v);
                    }
                }
            }
            return;
        }
        match self.seen.get(at) {
            Some(&(len, n)) if len as usize == line.len() => {
                let n = n as usize;
                self.times.spans.clear();
                let (kept, vs) = (&self.kept[self.next..self.next + n], &self.kept_v[self.next..self.next + n]);
                self.times.spans.extend(kept.iter().zip(vs).map(|(&[start, end, pat], &v)| Span {
                    start: start as usize,
                    end: end as usize,
                    pat,
                    v,
                }));
                self.next += n;
            }
            _ => {
                // (not kept, or not the same line: this and the lines after are found)
                self.seen.truncate(at);
                self.times.find(line);
            }
        }
    }

    /// Passes `line`, whose timestamps are not wanted.
    fn skip(&mut self, line: &[u8]) {
        match self.seen.get(self.cursor) {
            Some(&(len, n)) if len as usize == line.len() => self.next += n as usize,
            _ => self.seen.truncate(self.cursor),
        }
        self.cursor += 1;
    }

    /// Starts a pass over the same lines again, which are given the timestamps found.
    fn rewind(&mut self) {
        self.cursor = 0;
        self.next = 0;
        self.recording = false;
    }

    /// The pieces of `line` in `self.pieces`: of each token its parts around the timestamps in
    /// it (none where it has none, and the token is empty: a plain piece of no bytes), then the
    /// separator that ends it, if any.
    fn split(&mut self, line: &[u8]) {
        self.pieces.clear();
        self.find(line);
        let spans = &self.times.spans;
        let (mut at, mut sj) = (0, 0);
        loop {
            // the end of the token: the next separator that is not inside a timestamp
            let (mut p, mut k) = (at, sj);
            loop {
                if k < spans.len() && spans[k].start == p {
                    p = spans[k].end;
                    k += 1;
                } else if p >= line.len() || self.sep[line[p] as usize] {
                    break;
                } else {
                    p += 1;
                }
            }
            let mut q = at;
            for sp in &spans[sj..k] {
                if sp.start > q {
                    self.pieces.push(Piece::Plain { start: q, end: sp.start });
                }
                self.pieces.push(Piece::Time { start: sp.start, end: sp.end, pat: sp.pat, v: sp.v });
                q = sp.end;
            }
            if q < p || k == sj {
                self.pieces.push(Piece::Plain { start: q, end: p });
            }
            sj = k;
            if p >= line.len() {
                return;
            }
            self.pieces.push(Piece::Sep(line[p]));
            at = p + 1;
        }
    }
}

/// What a token's slot is: its kind (a mark of the template) and where it is in the line.
struct Slot {
    kind: u8,
    start: usize,
    len: usize,
    /// The integer of a time, and the digits of its pattern's fraction.
    v: i64,
    n: u32,
    /// Where the slot's first column is among the template's (a time with a fraction has two).
    col: usize,
}

/// The mark of a time's second column, that of its fraction (the module knows it by the pattern).
const M_FRACTION: u8 = 5;

/// Templates and the classification of lines into them, for the second and third passes.
struct Classifier {
    /// The forms (hashes) of the tokens and how many times each is in the segment.
    counts: FxMap<u64, u32>,
    rare: u32,
    split: Splitter,
    /// The templates' ids (from 1), by their bytes; the stream `templates`; the first column of
    /// each (by id - 1); the mark of each column; the most numbers of a template.
    index: FxMap<Vec<u8>, u32>,
    tbytes: Vec<u8>,
    /// Where each template starts in `tbytes` (by id - 1).
    tstart: Vec<usize>,
    col0: Vec<u32>,
    marks: Vec<u8>,
    numbers: usize,
    /// No template is added: a line of one not there is kept as it is.
    frozen: bool,
    tpl: Vec<u8>,
    slots: Vec<Slot>,
    /// The columns of the line's template so far.
    ncols: usize,
    scratch: Scratch,
}

impl Classifier {
    /// The counts of the forms of `seg`'s tokens.
    fn new(seg: &[u8], wide: bool) -> Classifier {
        let mut split = Splitter::new(seg, wide);
        let mut counts: FxMap<u64, u32> = FxMap::default();
        let mut lines = 0usize;
        for line in lines_of(seg) {
            lines += 1;
            if has_mark(line) {
                continue;
            }
            split.split(line);
            for p in &split.pieces {
                if let &Piece::Plain { start, end } = p {
                    *counts.entry(form(&line[start..end]).0).or_default() += 1;
                }
            }
        }
        split.rewind();
        let rare = RARE_MIN.max(lines / RARE_PER).min(u32::MAX as usize) as u32;
        Classifier {
            counts,
            rare,
            split,
            index: FxMap::default(),
            tbytes: Vec::new(),
            tstart: Vec::new(),
            col0: Vec::new(),
            marks: Vec::new(),
            numbers: 0,
            frozen: false,
            tpl: Vec::new(),
            slots: Vec::new(),
            ncols: 0,
            scratch: Scratch::default(),
        }
    }

    /// The template of `line` (an id from 1; its slots in `self.slots`), or 0 if the line is
    /// kept as it is.
    fn line(&mut self, line: &[u8]) -> u32 {
        if has_mark(line) {
            return 0;
        }
        self.tpl.clear();
        self.slots.clear();
        self.ncols = 0;
        self.split.split(line);
        for i in 0..self.split.pieces.len() {
            match self.split.pieces[i] {
                Piece::Plain { start, end } => self.token(&line[start..end], start),
                Piece::Time { start, end, pat, v } => {
                    self.tpl.push(M_TIME);
                    self.tpl.extend_from_slice(self.split.times.pattern(pat));
                    self.tpl.push(M_TIME);
                    let n = self.split.times.fraction(pat);
                    self.slots.push(Slot { kind: M_TIME, start, len: end - start, v, n, col: self.ncols });
                    self.ncols += 1 + (n > 0) as usize;
                }
                Piece::Sep(b) => self.tpl.push(b),
            }
        }
        if let Some(&id) = self.index.get(&self.tpl[..]) {
            return id;
        }
        let nums = self.ncols - self.slots.iter().filter(|s| s.kind == M_TEXT).count();
        if self.frozen
            || self.col0.len() >= MAX_TEMPLATES
            || self.marks.len() + self.ncols > MAX_COLUMNS
            || self.tbytes.len() + self.tpl.len() + 1 > MAX_TEMPLATE_BYTES
        {
            return 0;
        }
        let id = self.col0.len() as u32 + 1;
        self.index.insert(self.tpl.clone(), id);
        self.tstart.push(self.tbytes.len());
        self.tbytes.extend_from_slice(&self.tpl);
        self.tbytes.push(b'\n');
        self.col0.push(self.marks.len() as u32);
        for sl in &self.slots {
            self.marks.push(sl.kind);
            if sl.n > 0 {
                self.marks.push(M_FRACTION);
            }
        }
        self.numbers = self.numbers.max(nums);
        id
    }

    /// The slots of `line` of the template `id` (from 1), which it was given by [`Classifier::line`]:
    /// read off the template's marks, where `line` would find them again by the counts of the
    /// forms of the tokens and the numbers.
    fn replay(&mut self, line: &[u8], id: u32) {
        self.slots.clear();
        self.ncols = 0;
        self.split.split(line);
        let k = id as usize - 1;
        let end = self.tstart.get(k + 1).map_or(self.tbytes.len(), |&e| e) - 1;
        let tpl = &self.tbytes[self.tstart[k]..end];
        let mut tp = 0;
        for piece in &self.split.pieces {
            match *piece {
                Piece::Plain { start, end } => match tpl.get(tp) {
                    Some(&kind @ (M_NUM | M_TEXT)) => {
                        tp += 1;
                        self.slots.push(Slot { kind, start, len: end - start, v: 0, n: 0, col: self.ncols });
                        self.ncols += 1;
                    }
                    _ => {
                        // (a token of literal bytes and runs of digits)
                        let mut i = start;
                        while i < end {
                            if line[i].is_ascii_digit() {
                                let n = line[i..end].iter().take_while(|b| b.is_ascii_digit()).count();
                                debug_assert_eq!(tpl[tp], M_DIGITS);
                                self.slots.push(Slot { kind: M_DIGITS, start: i, len: n, v: 0, n: 0, col: self.ncols });
                                self.ncols += 1;
                                i += n;
                            } else {
                                i += 1;
                            }
                            tp += 1;
                        }
                    }
                },
                Piece::Time { start, end, pat, v } => {
                    let n = self.split.times.fraction(pat);
                    tp += self.split.times.pattern(pat).len() + 2;
                    self.slots.push(Slot { kind: M_TIME, start, len: end - start, v, n, col: self.ncols });
                    self.ncols += 1 + (n > 0) as usize;
                }
                Piece::Sep(_) => tp += 1,
            }
        }
        debug_assert_eq!(tp, tpl.len());
    }

    /// Adds token `t` (at `at` in the line) to the template and slots.
    fn token(&mut self, t: &[u8], at: usize) {
        let slot = |c: &mut Classifier, kind: u8, start: usize, len: usize| {
            c.tpl.push(kind);
            c.slots.push(Slot { kind, start, len, v: 0, n: 0, col: c.ncols });
            c.ncols += 1;
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

/// Puts the integer `v` at `scale` into the column `c`'s three ways of coding it (as it is, as a
/// change from the column's previous, as a change from the same number of the line before:
/// `prev`; `cur` the line's numbers so far), at the scale of the number.
fn emit(c: &mut Col, v: i64, scale: i32, prev: &[(i64, i32)], cur: &mut Vec<(i64, i32)>) {
    let (q, qs) = prev.get(cur.len()).copied().unwrap_or((0, 0));
    varint::put_signed(&mut c.by[0], v);
    varint::put_signed(&mut c.by[1], v.wrapping_sub(fixed::rescale(c.prev, c.prev_scale, scale)));
    varint::put_signed(&mut c.by[2], v.wrapping_sub(fixed::rescale(q, qs, scale)));
    c.prev = v;
    c.prev_scale = scale;
    cur.push((v, scale));
}

/// The size of the integers of the `toks` as changes from each other, and of their descriptors,
/// coded in `style` as a column does, compressed fast.
fn trial(toks: &[(usize, &[u8])], style: Style) -> usize {
    let mut sc = Scratch::default();
    let (mut nums, mut types, mut desc) = (Vec::new(), Vec::new(), Vec::new());
    let mut last: Option<(u8, u8, i32)> = None;
    let mut prev: (i64, i32) = (0, 0);
    for &(_, t) in toks {
        let r = style.encode(t, last.map_or(0, |l| l.2), &mut sc.tmp).and_then(|r| sc.exact(t, r));
        let Some((v, scale, mode, f)) = r.or_else(|| sc.whole(t)) else { continue };
        if last == Some((mode, f, scale)) {
            types.push(K_SAME);
        } else {
            last = Some((mode, f, scale));
            types.push(K_NEW);
            desc.extend_from_slice(&[mode, f]);
            varint::put_signed(&mut desc, scale as i64);
        }
        varint::put_signed(&mut nums, v.wrapping_sub(fixed::rescale(prev.0, prev.1, scale)));
        prev = (v, scale);
    }
    packed(&nums) + packed(&types) + packed(&desc)
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
    /// `other`'s streams put after these.
    fn append(&mut self, other: Streams) {
        let Streams { layout, raw, templates, columns, text, nums, widths, types, desc } = other;
        self.layout.extend_from_slice(&layout);
        self.raw.extend_from_slice(&raw);
        self.templates.extend_from_slice(&templates);
        self.columns.extend_from_slice(&columns);
        self.text.extend_from_slice(&text);
        self.nums.extend_from_slice(&nums);
        self.widths.extend_from_slice(&widths);
        self.types.extend_from_slice(&types);
        self.desc.extend_from_slice(&desc);
    }

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
    // the separators: those the streams of the first MiB come out smaller with
    let mut n = seg.len().min(TRIAL);
    if n < seg.len() {
        n = seg[..n].iter().rposition(|&b| b == b'\n').map_or(n, |i| i + 1);
    }
    let size = |wide: bool| -> usize {
        let mut t = Streams::default();
        segment_with(&seg[..n], false, &mut t, wide);
        [&t.layout, &t.raw, &t.templates, &t.columns, &t.text, &t.nums, &t.widths, &t.types, &t.desc]
            .iter()
            .map(|v| zstd::bulk::compress(v, 3).map_or(v.len(), |c| c.len()))
            .sum()
    };
    let wide = separators(seg, true) != separators(seg, false) && size(true) <= size(false);
    segment_with(seg, last, s, wide)
}

/// [`segment`] with the separators `wide` or not.
fn segment_with(seg: &[u8], last: bool, s: &mut Streams, wide: bool) -> (Params, usize) {
    let mut cl = Classifier::new(seg, wide);
    // the second pass: templates, and the samples of the numbers' styles
    let mut samples: Vec<Sample> = Vec::new();
    // (the templates the lines have, for the third pass: the same ones, only not made again)
    let mut ids: Vec<u32> = Vec::new();
    for line in lines_of(seg) {
        let id = cl.line(line);
        if ids.len() < CACHE_LINES {
            ids.push(id);
        }
        if id == 0 {
            continue;
        }
        if samples.len() < cl.marks.len() {
            samples.resize_with(cl.marks.len(), Sample::default);
        }
        let c0 = cl.col0[id as usize - 1] as usize;
        for sl in &cl.slots {
            let sm = &mut samples[c0 + sl.col];
            if sl.kind == M_NUM && sm.lens.len() < STYLE_SAMPLE {
                sm.bytes.extend_from_slice(&line[sl.start..][..sl.len]);
                sm.lens.push(sl.len as u8);
            }
        }
    }
    cl.frozen = true;
    cl.split.rewind();
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
        // (padded fixed saves the zeros of its digits and pays for each change of scale: the
        // sample coded both ways tells which is smaller)
        if dec::render_of(c.style.mode) == dec::R_FIXP {
            let plain = choose_with(&toks, false);
            if trial(&toks, plain) <= trial(&toks, c.style) {
                c.style = plain;
            }
        }
    }
    // the third: the fields
    let mut prev: Vec<(i64, i32)> = Vec::new();
    let mut cur: Vec<(i64, i32)> = Vec::new();
    let mut sc = Scratch::default();
    let mut raw = 0;
    for (n, line) in lines_of(seg).enumerate() {
        let id = match ids.get(n) {
            // (the line has no template, or it is the one it was given: its timestamps and its
            // slots are read off the template)
            Some(&0) => {
                if !has_mark(line) {
                    cl.split.skip(line);
                }
                0
            }
            Some(&id) => {
                cl.replay(line, id);
                id
            }
            None => cl.line(line),
        };
        varint::put(&mut s.layout, id as u64);
        if id == 0 {
            raw += line.len() + 1;
            s.raw.extend_from_slice(line);
            s.raw.push(b'\n');
            continue;
        }
        let c0 = cl.col0[id as usize - 1] as usize;
        cur.clear();
        for sl in &cl.slots {
            let c = &mut cols[c0 + sl.col];
            let t = &line[sl.start..][..sl.len];
            c.count += 1;
            let (v, scale) = match sl.kind {
                M_TEXT => {
                    c.text.extend_from_slice(t);
                    c.text.push(b'\n');
                    continue;
                }
                M_TIME if sl.n > 0 => {
                    // (the integer of the time without its fraction, and the fraction)
                    let pow = 10i64.pow(sl.n);
                    let (whole, frac) = (sl.v.div_euclid(pow), sl.v.rem_euclid(pow));
                    emit(c, whole, 0, &prev, &mut cur);
                    let c = &mut cols[c0 + sl.col + 1];
                    c.count += 1;
                    emit(c, frac, 0, &prev, &mut cur);
                    continue;
                }
                M_TIME => (sl.v, 0),
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
            emit(c, v, scale, &prev, &mut cur);
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
        } else if mark == M_NUM {
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
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    if !starts(input) {
        return None;
    }
    encode_in(input, SEGMENT, true, threads)
}

/// [`encode`] in segments of `segment` bytes of lines at least (the test of segments at lengths
/// shorter than [`SEGMENT`]); `check` that no tenth of the file is kept as it is. `None` for
/// an empty file. The segments are coded apart, on `threads`, and put in order: the same
/// formula whatever `threads` is.
pub fn encode_in(input: &[u8], segment_len: usize, check: bool, threads: usize) -> Option<Formula> {
    if input.is_empty() {
        return None;
    }
    let segment_len = segment_len.max(1);
    // the segments: lines taken while one starts within `segment_len` bytes of its start
    let mut cuts = vec![0];
    let mut end = 0;
    while end < input.len() {
        let from = end;
        while end < input.len() && end - from < segment_len {
            end = memchr::memchr(b'\n', &input[end..]).map_or(input.len(), |i| end + i + 1);
        }
        cuts.push(end);
    }
    let done = aqr_common::pool::par_map(cuts.len() - 1, threads.min(AT_ONCE), |k| {
        let mut s = Streams::default();
        let (params, raw) = segment(&input[cuts[k]..cuts[k + 1]], cuts[k + 1] == input.len(), &mut s);
        (s, params, raw)
    });
    let mut s = Streams::default();
    let mut parts: Vec<(Params, [u64; 9], u64)> = Vec::new();
    let mut raw = 0;
    for (k, (t, params, r)) in done.into_iter().enumerate() {
        raw += r;
        parts.push((params, std::array::from_fn(|j| t.lens()[j] as u64), (cuts[k + 1] - cuts[k]) as u64));
        s.append(t);
    }
    if check && raw * 10 >= input.len() {
        return None;
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
        let f = encode_in(d, usize::MAX, false, 1).unwrap();
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
        assert!(encode_in(b"", 10, false, 1).is_none());
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
        assert!(templates.contains("\x04b e H:M:S\x04 host kernel: temp \x03 C load \x03\n"), "{templates:?}");
        assert!(
            templates.contains("Accepted password for user\x01 from \x01.\x01.\x01.\x01 port \x03\n"),
            "{templates:?}"
        );
        // (the rare words are words of the column of text)
        assert!(String::from_utf8_lossy(&f.streams[4].data).contains("key-"));
        assert!(f.streams[1].data.is_empty());
        // zeros that pad a digit run are kept by its width
        let d = "id=0012 a\nid=0125 b\n".repeat(100);
        let f = roundtrip(d.as_bytes());
        assert!(f.streams[6].data.contains(&4));
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
        // (the templates of the wide separators, which a segment takes where its streams come out
        // smaller: not on every table)
        let wide = |t: &str| {
            let mut s = Streams::default();
            segment_with(t.as_bytes(), true, &mut s, true);
            String::from_utf8_lossy(&s.templates).into_owned()
        };
        for t in [&a, &b, &c] {
            roundtrip(t.as_bytes());
        }
        // semicolons, decimal commas: the numbers are whole tokens
        let t = wide(&a);
        assert!(t.contains("\x04d/m/Y\x04;\x04H:M:S\x04;\x03;\x03;\x03\n"), "{t:?}");
        let t = String::from_utf8_lossy(&roundtrip(a.as_bytes()).streams[2].data).into_owned();
        assert!(t.contains("\x03;\x03;\x03\n"), "{t:?}");
        // decimal points: commas and TABs cut
        let t = wide(&b);
        assert!(t.contains("\x03,\x03,\x03,\x03\t"), "{t:?}");
        let t = wide(&c);
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

    /// Timestamps are one integer with a pattern in the template, wherever they are in a line.
    /// The slots read off a template are those the line was given them as.
    #[test]
    fn replay_gives_the_slots() {
        let mut rng = aqr_common::rng::Rng::new(5);
        let mut mixed = log(3000);
        for i in 0..2000 {
            // (empty tokens, runs of digits in words, numbers of many kinds, lines kept as they are)
            mixed += &match i % 6 {
                0 => format!(
                    "a  b,{} x{}y{} ,, {}\n",
                    rng.below(99),
                    rng.below(9),
                    rng.below(9),
                    rng.below(7) as f64 / 8.0
                ),
                1 => format!("\n{}-{}\t{}\n", rng.below(9999), rng.below(99), 1u64 << rng.below(40)),
                2 => format!("id{:04} {} 2021-03-0{} 10:00:0{}\n", rng.below(9999), rng.below(3), 1 + i % 8, i % 9),
                3 => format!("{i} \u{1}mark {}\n", rng.below(5)),
                4 => format!("{}e{} 0x{:x} {}\n", rng.below(9), rng.below(30), rng.below(1 << 20), -(i as i64)),
                _ => format!("w{} {}:{}\n", rng.below(200), rng.below(24), rng.below(60)),
            };
        }
        let mut cl = Classifier::new(mixed.as_bytes(), false);
        let mut seen = 0;
        let rows = |c: &Classifier| -> Vec<(u8, usize, usize, i64, u32, usize)> {
            c.slots.iter().map(|s| (s.kind, s.start, s.len, s.v, s.n, s.col)).collect()
        };
        let mut lines = Vec::new();
        for line in lines_of(mixed.as_bytes()) {
            let id = cl.line(line);
            lines.push((line, id, rows(&cl), cl.ncols));
        }
        cl.frozen = true;
        cl.split.rewind();
        for (line, id, want, ncols) in lines {
            if id == 0 {
                if !has_mark(line) {
                    cl.split.skip(line);
                }
                continue;
            }
            cl.replay(line, id);
            assert_eq!((rows(&cl), cl.ncols), (want, ncols), "{}", String::from_utf8_lossy(line));
            seen += 1;
        }
        assert!(seen > 4000);
    }

    #[test]
    fn times() {
        let mut rng = aqr_common::rng::Rng::new(21);
        let mut d = String::new();
        let mut t = 1_088_000_000i64;
        for i in 0..3000 {
            t += rng.below(40) as i64;
            let (days, secs) = (t.div_euclid(86400), t.rem_euclid(86400));
            let (y, m, dd) = aqr_num::time::civil_from_days(days);
            let (h, mi, s) = (secs / 3600, secs / 60 % 60, secs % 60);
            let mon = String::from_utf8_lossy(aqr_num::time::MONTHS[m as usize - 1]).into_owned();
            let wd = String::from_utf8_lossy(aqr_num::time::WEEKDAYS[((days + 3).rem_euclid(7)) as usize]).into_owned();
            let micro = rng.below(1_000_000);
            // (a trimmed fraction: no trailing zeros, and none at all where it is zero)
            let frac =
                if i % 50 == 0 { String::new() } else { format!(".{micro:06}").trim_end_matches('0').to_string() };
            d += &match i % 6 {
                0 => format!("{y}-{m:02}-{dd:02} {h:02}:{mi:02}:{s:02}{frac} {} {}\n", i % 7, rng.below(100)),
                1 => format!(
                    "1.2.3.4 - - [{dd:02}/{mon}/{y}:{h:02}:{mi:02}:{s:02} -0400] \"GET /x{} HTTP/1.0\" 200 {}\n",
                    i % 5,
                    rng.below(9999)
                ),
                2 => format!("[{wd} {mon} {dd:02} {h:02}:{mi:02}:{s:02} {y}] [error] client {}\n", i % 3),
                3 => format!("{mon} {dd:2} {h:02}:{mi:02}:{s:02} host sshd[{}]: x\n", rng.below(999)),
                4 => format!("\"{y}{m:02}{dd:02}\",{y}-{m:02}-{dd:02}T{h:02}:{mi:02}:{s:02}Z,{h}:{mi:02}:{s:02}\n"),
                _ => format!("{dd:02}.{m:02}.{y} {h:02}:{mi:02};{h:02}:{mi:02}:{s:02}.123 ip 10.0.1.{}\n", i % 9),
            };
        }
        let f = roundtrip(d.as_bytes());
        let tpl = String::from_utf8_lossy(&f.streams[2].data).into_owned();
        for p in [
            "\x04Y-m-d H:M:St.6\x04 \x03 \x03\n",
            "\x04d/b/Y:H:M:St.6\x04",
            "[\x04a b d H:M:St.6 Y\x04] [error]",
            "\x04b e H:M:St.6\x04 host sshd[",
            "\x04Y-m-d\\TH:M:St.6\x04Z,\x04h:M:St.6\x04",
            "\x04d.m.Y H:M\x04;\x04H:M:St.6\x04 ip",
        ] {
            assert!(tpl.contains(p), "{p:?} in {tpl:?}");
        }
        // (the times are small changes: fewer bytes than the digits they held, whose random fractions and ids are the most of it)
        assert!(f.streams[5].data.len() < d.len() / 4, "{}", f.streams[5].data.len());
        // dates in lines of other shapes, and what is no time: kept in the template
        roundtrip(b"a 2023-02-29 b 2023-13-01 c 25:00:00 d 12:30:45:67 e 10.0.12.30 f Thu Jun 10 06:07:04 2005 g\n12:30:45.5 12:30:45,5 1:2:3 99-99-99\n");
        roundtrip("x 0000-01-01 9999-12-31T23:59:59 y 0001/01/01 0:00:00 Feb 29 00:00:00\n".repeat(30).as_bytes());
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
        let whole = encode_in(&d, usize::MAX, true, 1).unwrap();
        assert!(matches!(whole.root, Node::Apply(_)));
        assert_eq!(eval_segments(&whole), d);
        for segment in [1, 100, 1000, 4096, 50_000, d.len() / 2] {
            let f = encode_in(&d, segment, true, 1).unwrap();
            assert_eq!(eval_segments(&f), d, "segment {segment}");
            // (the same formula on any threads)
            assert_eq!(f, encode_in(&d, segment, true, 4).unwrap(), "threads at segment {segment}");
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
            assert_eq!(eval_segments(&encode_in(d, segment, true, 1).unwrap()), d);
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
        // a time's pattern that is none, or has no end, and integers that are no time
        let inputs = |streams: &[Vec<u8>]| -> crate::Result<Vec<u8>> {
            let inputs: Vec<&[u8]> = streams.iter().map(|s| &s[..]).collect();
            decode(&a.params, &inputs, s.len() as u64, 1)
        };
        let base: Vec<Vec<u8>> = f.streams.iter().map(|s| s.data.to_vec()).collect();
        assert!(inputs(&base).is_ok());
        let pat = base[2].windows(9).position(|w| w == b"\x04b e H:M:").expect("a time");
        for (at, to) in [(pat + 5, b'x'), (pat + 1, b'Q'), (pat + 10, b'M')] {
            let mut streams = base.clone();
            streams[2][at] = to;
            assert!(inputs(&streams).is_err(), "{at} {to}");
        }
        let mut streams = base.clone();
        let mut bad = vec![];
        varint::put_signed(&mut bad, i64::MAX);
        streams[5].splice(0..0, bad);
        let _ = inputs(&streams);
        // a pattern of a column whose integers are of no time of day
        let day = "a 12:00:00\n".repeat(2000);
        let f = encode_in(day.as_bytes(), usize::MAX, false, 1).unwrap();
        let Node::Apply(a2) = &f.root else { panic!() };
        let mut streams: Vec<Vec<u8>> = f.streams.iter().map(|s| s.data.to_vec()).collect();
        let at = streams[5].len() - 1;
        streams[5][at] = 0x7e;
        let inputs2: Vec<&[u8]> = streams.iter().map(|s| &s[..]).collect();
        assert!(decode(&a2.params, &inputs2, day.len() as u64, 1).is_err());
        // params that lie
        for p in [&[][..], &[2, 0, 0, 0], &[0, 255, 255, 255, 255, 15, 0, 0], &a.params[..a.params.len() - 1]] {
            let inputs: Vec<&[u8]> = f.streams.iter().map(|s| &s.data[..]).collect();
            assert!(decode(p, &inputs, s.len() as u64, 1).is_err());
        }
    }
}
