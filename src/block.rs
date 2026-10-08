//! The transform of one block: lines into a template id each, plus values in streams.
//!
//! A line that parses as numbers (`v`, `vt`, `vn`, `vp`) or corners (`f`, `l`) becomes a
//! template (keyword, separators, number formats or corner patterns, trailing bytes and line
//! end) and its values. Templates are numbered in order of first use; a new one is defined
//! in the TPL stream where it is first used. Any other line is kept as text.
//!
//! A number is an integer at a scale (digits after the point). Its format is either fixed
//! (`%.Nf`-like: the scale is in the template) or C's `%.Pg` (the scale follows from the
//! magnitude and is coded as a change from the previous number's). It is coded as the
//! difference from the previous number in the same position of the same kind of line,
//! brought to its scale.
//!
//! State (previous values, templates) carries over from block to block, so that blocks are
//! coded in order, but its size does not depend on the input size.

use crate::{dec, num};
use crate::varint::{Rd, put_sv, put_uv};
use std::collections::HashMap;

pub const S_OPS: usize = 0;
pub const S_TPL: usize = 1;
pub const S_TEXT: usize = 2;
pub const S_V: usize = 3;
pub const S_VT: usize = 4;
pub const S_VN: usize = 5;
pub const S_VP: usize = 6;
/// Scale changes of `%g` numbers.
pub const S_SCALE: usize = 7;
/// Corner index symbols per attribute (v, vt, vn), then their escapes.
pub const S_FSYM: usize = 8;
pub const S_FESC: usize = 11;
/// Block flags (one byte).
pub const S_HDR: usize = 14;
/// The predicted component of unit normals, and its sign.
pub const S_NSEL: usize = 15;
/// Per v, vt, vn line: the distance back to an equal line of its kind, or 0.
pub const S_DUP: usize = 16;
pub const NSTREAMS: usize = 19;
pub const STREAM_NAMES: [&str; NSTREAMS] = [
    "ops", "tpl", "text", "v", "vt", "vn", "vp", "scale", "fv", "ft", "fn", "fv-esc", "ft-esc", "fn-esc", "hdr", "nsel",
    "v-dup", "vt-dup", "vn-dup",
];

/// Block flag: normals (`vn` lines of three numbers) are coded as unit vectors.
const H_UNIT: u8 = 1;

/// Template kinds; the numeric ones index their value stream from S_V.
const K_VP: u8 = 3;
const K_F: u8 = 4;
const K_L: u8 = 5;
const KEYWORDS: [&[u8]; 6] = [b"v", b"vt", b"vn", b"vp", b"f", b"l"];

/// Number formats (first byte of a numeric item; the second is the format's flags).
const M_FIX: u8 = 0;
const M_GEN: u8 = 1;

/// Numbers per numeric line, corners per face line.
const MAX_NUMS: usize = 8;
const MAX_CORNERS: usize = 64;
const MAX_TEMPLATES: usize = 1 << 16;
const MAX_TEMPLATE_BYTES: usize = 4 << 20;
/// Ops for raw lines; templates are numbered from FIRST_TPL.
const OP_RAW: u64 = 0;
const OP_RAW_OPEN: u64 = 1;
const FIRST_TPL: u64 = 2;

/// Corner patterns: "a", "a/b", "a//c", "a/b/c"; bits 2-4 mark negative (relative) indices.
const C_T: u8 = 1;
const C_N: u8 = 2;
const C_NEG: u8 = 4;

/// Corner index symbols: a position in the cache of recent indices, or one of these.
const CACHE: usize = 32;
/// One more than the largest index so far.
const SYM_NEW: u8 = CACHE as u8;
/// Coded as the difference from the previous index.
const SYM_ESC: u8 = SYM_NEW + 1;
/// (vt, vn) The corner's v index plus the previous corner's offset from it.
const SYM_OFF: u8 = SYM_NEW + 2;

/// Recent indices of one attribute, most recent first.
#[derive(Clone, Copy)]
struct Indices {
    cache: [i64; CACHE],
    max: i64,
    last: i64,
    /// Previous index minus its corner's v index.
    off: i64,
}

impl Indices {
    #[inline]
    fn find(&self, a: i64) -> Option<usize> {
        self.cache.iter().position(|&x| x == a)
    }

    /// Records `a` (found at `p` in the cache, if it is there).
    #[inline]
    fn update(&mut self, a: i64, p: Option<usize>, v: i64) {
        let p = p.unwrap_or(CACHE - 1);
        // A loop: copy_within calls memmove, slow for these few bytes.
        for i in (0..p).rev() {
            self.cache[i + 1] = self.cache[i];
        }
        self.cache[0] = a;
        self.max = self.max.max(a);
        self.last = a;
        self.off = a.wrapping_sub(v);
    }
}

/// Lines of a kind (v, vt, vn) kept for references to equal lines: the last RECENT.
const RECENT: usize = 1 << 16;

/// The values of a line of up to three numbers; absent ones are a value no number has.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Entry {
    /// Line number + 1 (0: empty).
    tag: u64,
    v: [i64; 3],
    s: [i8; 3],
}

impl Entry {
    fn new(vals: &[(i64, i32)]) -> Entry {
        let mut e = Entry { tag: 0, v: [i64::MIN; 3], s: [i8::MIN; 3] };
        for (j, &(v, s)) in vals.iter().enumerate() {
            e.v[j] = v;
            e.s[j] = s as i8;
        }
        e
    }

    #[inline]
    fn hash(&self) -> usize {
        let h = (self.v[0] as u64).wrapping_mul(0x9E3779B97F4A7C15)
            ^ (self.v[1] as u64).wrapping_mul(0xC2B2AE3D27D4EB4F).rotate_left(21)
            ^ (self.v[2] as u64).wrapping_mul(0x165667B19E3779F9).rotate_left(42)
            ^ (self.s[0] as u8 as u64) << 8
            ^ (self.s[1] as u8 as u64) << 16
            ^ (self.s[2] as u8 as u64) << 24;
        (h.wrapping_mul(0x9E3779B97F4A7C15) >> 48) as usize
    }
}

/// The recent lines of a kind, by line number; for the encoder also a table from values to
/// the last line with them. Entries carry their line number, so that ones written by a
/// block that was undone are not taken for older lines.
struct Recent {
    ring: Vec<Entry>,
    table: Vec<u64>,
}

impl Recent {
    fn new(table: bool) -> Self {
        let e = Entry { tag: 0, v: [0; 3], s: [0; 3] };
        Recent { ring: vec![e; RECENT], table: if table { vec![0; RECENT] } else { vec![] } }
    }

    #[inline]
    fn get(&self, line: u64) -> Option<&Entry> {
        let e = &self.ring[line as usize % RECENT];
        (e.tag == line + 1).then_some(e)
    }

    #[inline]
    fn put(&mut self, line: u64, mut e: Entry) {
        e.tag = line + 1;
        self.ring[line as usize % RECENT] = e;
    }

    /// The distance back from `line` to the last line equal to `e` (0 if none), and
    /// records `line`.
    #[inline]
    fn find_put(&mut self, line: u64, e: Entry) -> u64 {
        let h = e.hash();
        let c = self.table[h];
        let d = if c != 0 && c <= line && line + 1 - c <= RECENT as u64 && self.get(c - 1).is_some_and(|x| x.v == e.v && x.s == e.s) {
            line + 1 - c
        } else {
            0
        };
        self.table[h] = line + 1;
        self.put(line, e);
        d
    }
}

/// Prediction state, the same on both sides.
#[derive(Clone, Copy)]
pub struct State {
    /// Previous number per numeric kind and position, with its scale.
    prev: [[(i64, i32); MAX_NUMS]; 4],
    /// Lines coded per kind v, vt, vn (for relative indices).
    count: [i64; 3],
    idx: [Indices; 3],
}

impl Default for State {
    fn default() -> Self {
        let idx = Indices { cache: [0; CACHE], max: 0, last: 0, off: 0 };
        State { prev: [[(0, 0); MAX_NUMS]; 4], count: [0; 3], idx: [idx; 3] }
    }
}

#[inline]
fn is_ws(c: u8) -> bool {
    c == b' ' || c == b'\t'
}

/// Token end: the next whitespace or line end.
#[inline]
fn tok_end(l: &[u8], mut i: usize) -> usize {
    while i < l.len() && !matches!(l[i], b' ' | b'\t' | b'\r' | b'\n') {
        i += 1;
    }
    i
}

/// Whether `t` is a valid tail: blanks, then "\r\n", "\n" or nothing (at the end of a
/// block).
fn valid_tail(t: &[u8]) -> bool {
    let mut i = 0;
    while i < t.len() && is_ws(t[i]) {
        i += 1;
    }
    matches!(&t[i..], b"" | b"\n" | b"\r\n")
}

/// An index token: `[-]digits`, no leading zero, not zero.
#[inline]
fn parse_index(t: &[u8]) -> Option<(i64, bool)> {
    let neg = t.first() == Some(&b'-');
    let d = &t[neg as usize..];
    if d.is_empty() || d.len() > 18 || d[0] == b'0' {
        return None;
    }
    let mut v = 0i64;
    for &c in d {
        let x = c.wrapping_sub(b'0');
        if x > 9 {
            return None;
        }
        v = v * 10 + x as i64;
    }
    Some((v, neg))
}

/// The keyword kind of a line, and the keyword's length.
#[inline]
fn kind_of(l: &[u8]) -> Option<(u8, usize)> {
    let kw = tok_end(l, 0);
    let k = match &l[..kw] {
        b"v" => 0,
        b"vt" => 1,
        b"vn" => 2,
        b"vp" => 3,
        b"f" => K_F,
        b"l" => K_L,
        _ => return None,
    };
    Some((k, kw))
}

/// Powers of ten exact as doubles.
const F64_POW10: [f64; 23] = {
    let mut t = [1f64; 23];
    let mut i = 1;
    while i < 23 {
        t[i] = t[i - 1] * 10.0;
        i += 1;
    }
    t
};

/// `v` * 10^-`s`, roughly (exactly for |s| <= 22); only basic IEEE operations, so the same
/// everywhere.
#[inline]
fn to_f64(v: i64, s: i32) -> f64 {
    if s >= 0 { v as f64 / F64_POW10[(s as usize).min(22)] } else { v as f64 * F64_POW10[((-s) as usize).min(22)] }
}

/// The third component of a unit vector from the other two, at scale `s`, with the sign of
/// `neg`.
#[inline]
fn unit_pred(a: f64, b: f64, s: i32, neg: bool) -> i64 {
    let z = (1.0 - a * a - b * b).max(0.0).sqrt();
    let p = if s >= 0 { z * F64_POW10[(s as usize).min(22)] } else { z / F64_POW10[((-s) as usize).min(22)] };
    let p = p.round() as i64;
    if neg { -p } else { p }
}

/// The order numbers are coded in: the unit-predicted one (`sel`) last.
#[inline]
fn order(n: usize, sel: Option<usize>) -> ([usize; MAX_NUMS], usize) {
    let mut o = [0; MAX_NUMS];
    let mut m = 0;
    for j in 0..n {
        if Some(j) != sel {
            o[m] = j;
            m += 1;
        }
    }
    if let Some(j) = sel {
        o[m] = j;
    }
    (o, n)
}

/// How the encoder writes the numbers of a kind first: a mode (`dec`), the precision for
/// `%g` and `%e`, and the exponent width of `%g` tokens without an exponent.
#[derive(Clone, Copy, Default)]
struct Style {
    mode: u8,
    p: u8,
    exp3: bool,
}

/// Rough cost of coding `t` in `st`: digits of the integer, plus a scale change from the
/// previous number in its position (`prev`, updated), or a penalty. `dom` is the usual
/// number of digits after the point of fixed numbers (others cost a template change).
fn style_cost(t: &[u8], st: Style, dom: u8, prev: &mut i32, tmp: &mut Vec<u8>) -> usize {
    let nd = |v: i64| v.unsigned_abs().checked_ilog10().unwrap_or(0) as usize + 1;
    if st.mode == M_FIX {
        return num::parse(t).map_or(25, |(v, f)| nd(v) + if f & num::F_FRAC != dom { 3 } else { 0 });
    }
    dec::encode(t, st.mode, st.p, st.exp3, *prev, tmp).map_or(25, |(v, s, _)| {
        let c = nd(v) + if s != *prev { 2 } else { 0 };
        *prev = s;
        c
    })
}

/// Chooses the styles from lines spread over a block: `%.Pg` when most numbers of a kind
/// are that and their fixed formats vary; unit normals when most normals have length 1. A
/// kind not seen keeps its style (`st`, `unit`: those of the previous block).
fn choose_styles(src: &[u8], st: &mut [Style; 4], unit: &mut bool) {
    const PARTS: usize = 16;
    const PART: usize = 2 << 10;
    // Tokens with their position in the line.
    let mut toks: [Vec<(usize, &[u8])>; 4] = Default::default();
    for i in 0..PARTS {
        let a = src.len() / PARTS * i;
        let sample = &src[a..(a + PART).min(src.len())];
        // Whole lines only: from the first line start.
        let b = if a == 0 { 0 } else { memchr::memchr(b'\n', sample).map_or(sample.len(), |p| p + 1) };
        let mut lines = sample[b..].split(|&c| c == b'\n');
        lines.next_back();
        for l in lines {
            let Some((k, kw)) = kind_of(l) else { continue };
            if k >= K_F {
                continue;
            }
            for (j, t) in l[kw..].split(|&c| matches!(c, b' ' | b'\t' | b'\r')).filter(|t| !t.is_empty()).enumerate() {
                toks[k as usize].push((j.min(MAX_NUMS - 1), t));
            }
        }
    }
    for k in 0..4 {
        let ts = &toks[k];
        if ts.is_empty() {
            continue;
        }
        st[k] = Style::default();
        let mut hist = [0usize; 19];
        let mut maxsig = 1u8;
        let mut sci_p = 1u8;
        let mut exp3 = false;
        for &(_, t) in ts {
            if let Some((_, f)) = num::parse(t) {
                hist[(f & num::F_FRAC) as usize] += 1;
            }
            let e = t.iter().position(|&c| c == b'e' || c == b'E');
            exp3 |= e.is_some_and(|e| t.len() - e == 5);
            let e = e.unwrap_or(t.len());
            let digits: Vec<u8> = t[..e].iter().copied().filter(u8::is_ascii_digit).collect();
            let a = digits.iter().position(|&c| c != b'0').unwrap_or(digits.len());
            let b = digits.iter().rposition(|&c| c != b'0').map_or(a, |i| i + 1);
            maxsig = maxsig.max((b - a).min(17) as u8);
            if e < t.len() {
                sci_p = sci_p.max(digits.len().min(19) as u8);
            }
        }
        let dom = (0..19).max_by_key(|&i| hist[i]).unwrap() as u8;
        let s = |r, src, p| Style { mode: dec::mode(r, src), p, exp3 };
        let cands = [
            Style::default(),
            s(dec::R_GEN, dec::SRC_DEC, maxsig),
            s(dec::R_SCI, dec::SRC_DEC, sci_p),
            s(dec::R_PY, dec::SRC_DEC, 0),
            s(dec::R_JS, dec::SRC_DEC, 0),
            s(dec::R_POS, dec::SRC_DEC, 0),
            s(dec::R_GEN, dec::SRC_F32, maxsig),
            s(dec::R_GEN, dec::SRC_F64, maxsig),
            s(dec::R_SCI, dec::SRC_F32, sci_p),
            s(dec::R_SCI, dec::SRC_F64, sci_p),
            s(dec::R_PY, dec::SRC_F32, 0),
            s(dec::R_JS, dec::SRC_F32, 0),
        ];
        // Whole lines (consecutive tokens), up to about 512 numbers.
        let ts = &ts[..ts.len().min(512)];
        let mut best = (usize::MAX, Style::default());
        let mut tmp = Vec::with_capacity(64);
        for c in cands {
            let mut cost = 0;
            let mut prev = [0i32; MAX_NUMS];
            for &(j, t) in ts {
                cost += style_cost(t, c, dom, &mut prev[j], &mut tmp);
                if cost >= best.0 {
                    break;
                }
            }
            if cost < best.0 {
                best = (cost, c);
            }
        }
        st[k] = best.1;
    }
    // Normals of length 1 within rounding.
    let (mut nvn, mut nunit) = (0, 0);
    for t in toks[2].chunks_exact(3) {
        let x: Vec<f64> = t.iter().filter_map(|t| std::str::from_utf8(t.1).ok()?.parse().ok()).collect();
        if x.len() == 3 {
            nvn += 1;
            nunit += ((x[0] * x[0] + x[1] * x[1] + x[2] * x[2] - 1.0).abs() < 1e-3) as usize;
        }
    }
    if nvn > 0 {
        *unit = nunit * 10 >= nvn * 8;
    }
}

/// A token with an exponent in another style than its line's kind: `%e` with the
/// precision of its digits, or a shortest form. Returns the value, scale, mode and flags.
#[cold]
fn exp_fallback(t: &[u8], ps: i32, tmp: &mut Vec<u8>) -> Option<(i64, i32, u8, u8)> {
    let e = t.iter().position(|&c| c == b'e')?;
    let p = t[..e].iter().filter(|c| c.is_ascii_digit()).count().clamp(1, 19) as u8;
    let exp3 = t.len() - e == 5;
    let modes = [
        (dec::mode(dec::R_SCI, dec::SRC_DEC), p),
        (dec::mode(dec::R_SCI, dec::SRC_F64), p),
        (dec::mode(dec::R_SCI, dec::SRC_F32), p),
        (dec::mode(dec::R_PY, dec::SRC_DEC), 0),
        (dec::mode(dec::R_JS, dec::SRC_DEC), 0),
    ];
    modes.into_iter().find_map(|(m, p)| dec::encode(t, m, p, exp3, ps, tmp).map(|(v, s, f)| (v, s, m, f)))
}

pub struct Encoder {
    pub st: State,
    style: [Style; 4],
    unit: bool,
    map: HashMap<Box<[u8]>, u32>,
    keys: Vec<Box<[u8]>>,
    /// Bytes in `keys`.
    bytes: usize,
    key: Vec<u8>,
    tmp: Vec<u8>,
    recent: [Recent; 3],
}

impl Default for Encoder {
    fn default() -> Self {
        Encoder {
            st: State::default(),
            style: Default::default(),
            unit: false,
            map: HashMap::new(),
            keys: vec![],
            bytes: 0,
            key: vec![],
            tmp: Vec::with_capacity(64),
            recent: [Recent::new(true), Recent::new(true), Recent::new(true)],
        }
    }
}

/// What a block's encoding changed, to undo it (when verification fails).
pub struct Mark {
    st: State,
    ntpl: usize,
}

impl Encoder {
    /// The template keys, in id order (for analysis).
    pub fn keys(&self) -> &[Box<[u8]>] {
        &self.keys
    }

    pub fn mark(&self) -> Mark {
        Mark { st: self.st, ntpl: self.keys.len() }
    }

    pub fn reset(&mut self, m: &Mark) {
        self.st = m.st;
        for k in self.keys.drain(m.ntpl..) {
            self.bytes -= k.len();
            self.map.remove(&k);
        }
    }

    /// Codes `src` into `s` (cleared first).
    pub fn encode(&mut self, src: &[u8], s: &mut [Vec<u8>; NSTREAMS]) {
        for v in s.iter_mut() {
            v.clear();
        }
        choose_styles(src, &mut self.style, &mut self.unit);
        s[S_HDR].push(if self.unit { H_UNIT } else { 0 });
        let mut p = 0;
        while p < src.len() {
            let e = memchr::memchr(b'\n', &src[p..]).map_or(src.len(), |i| p + i + 1);
            let l = &src[p..e];
            if !self.line(l, s) {
                s[S_OPS].push(if l.last() == Some(&b'\n') { OP_RAW } else { OP_RAW_OPEN } as u8);
                s[S_TEXT].extend_from_slice(l);
            }
            p = e;
        }
    }

    /// Codes one line as a template, or returns false (nothing written).
    #[inline]
    fn line(&mut self, l: &[u8], s: &mut [Vec<u8>; NSTREAMS]) -> bool {
        let Some((kind, kw)) = kind_of(l) else { return false };
        self.key.clear();
        self.key.push(kind);
        self.key.push(0);
        let mut i = kw;
        let mut n = 0;
        if kind < K_F {
            let k = kind as usize;
            let style = self.style[k];
            // value, scale, mode
            let mut vals = [(0i64, 0i32, 0u8); MAX_NUMS];
            loop {
                let ws = i;
                while i < l.len() && is_ws(l[i]) {
                    i += 1;
                }
                let te = tok_end(l, i);
                if te == i || i == ws {
                    i = ws;
                    break;
                }
                if n == MAX_NUMS || i - ws > 255 {
                    return false;
                }
                let t = &l[i..te];
                let ps = self.st.prev[k][n].1;
                let (v, sc, mode, f) = if style.mode != M_FIX
                    && let Some((v, sc, f)) = dec::encode(t, style.mode, style.p, style.exp3, ps, &mut self.tmp)
                {
                    (v, sc, style.mode, f)
                } else if let Some((v, f)) = num::parse(t) {
                    (v, (f & num::F_FRAC) as i32, M_FIX, f)
                } else if let Some((v, sc, f)) = num::parse_gen(t, style.p.clamp(6, 17), ps, style.exp3, &mut self.tmp) {
                    (v, sc, M_GEN, f)
                } else if let Some(r) = exp_fallback(t, ps, &mut self.tmp) {
                    r
                } else {
                    return false;
                };
                vals[n] = (v, sc, mode);
                n += 1;
                self.key.extend_from_slice(&[mode, f, (i - ws) as u8]);
                self.key.extend_from_slice(&l[ws..i]);
                i = te;
            }
            if n == 0 || !self.tail(&l[i..]) {
                return false;
            }
            self.key[1] = n as u8;
            let Some(id) = self.template(s) else { return false };
            put_uv(&mut s[S_OPS], id);
            let prev = &mut self.st.prev[k];
            if k < 3 && n <= 3 {
                let line = self.st.count[k] as u64;
                let vs: [(i64, i32); 3] = [(vals[0].0, vals[0].1), (vals[1].0, vals[1].1), (vals[2].0, vals[2].1)];
                let d = self.recent[k].find_put(line, Entry::new(&vs[..n]));
                put_uv(&mut s[S_DUP + k], d);
                if d > 0 {
                    prev[..n].copy_from_slice(&vs[..n]);
                    self.st.count[k] += 1;
                    return true;
                }
            }
            let mut sel = None;
            if self.unit && kind == 2 && n == 3 {
                let x = vals.map(|(v, sc, _)| to_f64(v, sc));
                let j = (0..3).fold(0, |b, j| if x[j].abs() > x[b].abs() { j } else { b });
                s[S_NSEL].push(j as u8 | ((vals[j].0 < 0) as u8) << 2);
                sel = Some(j);
            }
            let (o, _) = order(n, sel);
            for &j in &o[..n] {
                let (v, sc, mode) = vals[j];
                let (pv, ps) = prev[j];
                if mode != M_FIX {
                    put_sv(&mut s[S_SCALE], (sc - ps) as i64);
                }
                let pred = if Some(j) == sel {
                    let (a, b) = ((j + 1) % 3, (j + 2) % 3);
                    unit_pred(to_f64(vals[a].0, vals[a].1), to_f64(vals[b].0, vals[b].1), sc, v < 0)
                } else {
                    num::rescale(pv, ps, sc)
                };
                put_sv(&mut s[S_V + k], v.wrapping_sub(pred));
                prev[j] = (v, sc);
            }
            if kind < K_VP {
                self.st.count[k] += 1;
            }
        } else {
            let mut corners = [(0i64, 0i64, 0i64); MAX_CORNERS];
            loop {
                let ws = i;
                while i < l.len() && is_ws(l[i]) {
                    i += 1;
                }
                let te = tok_end(l, i);
                if te == i || i == ws {
                    i = ws;
                    break;
                }
                if n == MAX_CORNERS || i - ws > 255 {
                    return false;
                }
                let Some((c, pat)) = self.corner(&l[i..te]) else { return false };
                corners[n] = c;
                n += 1;
                self.key.extend_from_slice(&[pat, 0, (i - ws) as u8]);
                self.key.extend_from_slice(&l[ws..i]);
                i = te;
            }
            if n == 0 || !self.tail(&l[i..]) {
                return false;
            }
            self.key[1] = n as u8;
            let Some(id) = self.template(s) else { return false };
            put_uv(&mut s[S_OPS], id);
            let mut k = 2;
            for &(v, t, nn) in &corners[..n] {
                let pat = self.key[k];
                k += 3 + self.key[k + 2] as usize;
                self.index(s, 0, v, v);
                if pat & C_T != 0 {
                    self.index(s, 1, t, v);
                }
                if pat & C_N != 0 {
                    self.index(s, 2, nn, v);
                }
            }
        }
        true
    }

    /// Codes index `a` of attribute `k` of a corner whose v index is `v`.
    #[inline]
    fn index(&mut self, s: &mut [Vec<u8>; NSTREAMS], k: usize, a: i64, v: i64) {
        let x = &mut self.st.idx[k];
        let p = x.find(a);
        let sym = if k > 0 && a == v.wrapping_add(x.off) {
            SYM_OFF
        } else if let Some(p) = p {
            p as u8
        } else if a == x.max.wrapping_add(1) {
            SYM_NEW
        } else {
            put_sv(&mut s[S_FESC + k], a.wrapping_sub(x.last));
            SYM_ESC
        };
        s[S_FSYM + k].push(sym);
        x.update(a, p, v);
    }

    /// A corner token: absolute indices (relative ones resolved) and its pattern.
    #[inline]
    fn corner(&self, t: &[u8]) -> Option<((i64, i64, i64), u8)> {
        let abs = |(x, neg): (i64, bool), k: usize| if neg { self.st.count[k] + 1 - x } else { x };
        let s1 = memchr::memchr(b'/', t);
        let Some(s1) = s1 else {
            let a = parse_index(t)?;
            return Some(((abs(a, 0), 0, 0), (a.1 as u8) * C_NEG));
        };
        let a = parse_index(&t[..s1])?;
        let r = &t[s1 + 1..];
        let (b, c) = match memchr::memchr(b'/', r) {
            None => (Some(parse_index(r)?), None),
            Some(0) => (None, Some(parse_index(&r[1..])?)),
            Some(s2) => (Some(parse_index(&r[..s2])?), Some(parse_index(&r[s2 + 1..])?)),
        };
        let mut pat = (a.1 as u8) * C_NEG;
        let (mut tv, mut nv) = (0, 0);
        if let Some(b) = b {
            pat |= C_T | (b.1 as u8) * (C_NEG << 1);
            tv = abs(b, 1);
        }
        if let Some(c) = c {
            pat |= C_N | (c.1 as u8) * (C_NEG << 2);
            nv = abs(c, 2);
        }
        Some(((abs(a, 0), tv, nv), pat))
    }

    #[inline]
    fn tail(&mut self, t: &[u8]) -> bool {
        if t.len() > 255 || !valid_tail(t) {
            return false;
        }
        self.key.push(t.len() as u8);
        self.key.extend_from_slice(t);
        true
    }

    /// The template id of `self.key`, defining it if new; None when the table is full.
    #[inline]
    fn template(&mut self, s: &mut [Vec<u8>; NSTREAMS]) -> Option<u64> {
        if let Some(&id) = self.map.get(&self.key[..]) {
            return Some(id as u64 + FIRST_TPL);
        }
        if self.keys.len() >= MAX_TEMPLATES || self.bytes + self.key.len() > MAX_TEMPLATE_BYTES {
            return None;
        }
        self.bytes += self.key.len();
        let id = self.keys.len() as u32;
        let k: Box<[u8]> = self.key.as_slice().into();
        self.map.insert(k.clone(), id);
        self.keys.push(k);
        put_uv(&mut s[S_TPL], self.key.len() as u64);
        s[S_TPL].extend_from_slice(&self.key);
        Some(id as u64 + FIRST_TPL)
    }
}

/// A key: `kind n (mode flags seplen sep...){n} taillen tail...`; for corners, mode is the
/// pattern and flags 0.
fn valid_key(k: &[u8]) -> bool {
    let (Some(&kind), Some(&n)) = (k.first(), k.get(1)) else { return false };
    let max = if kind < K_F { MAX_NUMS } else { MAX_CORNERS };
    if kind > K_L || n == 0 || n as usize > max {
        return false;
    }
    let mut i = 2;
    for _ in 0..n {
        let (Some(&m), Some(&f), Some(&sl)) = (k.get(i), k.get(i + 1), k.get(i + 2)) else { return false };
        let ok = if kind < K_F {
            dec::valid(m, f)
        } else {
            f == 0 && m < 32 && (m & C_T != 0 || m & (C_NEG << 1) == 0) && (m & C_N != 0 || m & (C_NEG << 2) == 0)
        };
        if !ok || sl == 0 {
            return false;
        }
        i += 3 + sl as usize;
    }
    k.get(i).is_some_and(|&tl| i + 1 + tl as usize == k.len())
}

pub struct Decoder {
    st: State,
    /// Template keys, by offset and length into `arena`.
    tpls: Vec<(u32, u32)>,
    arena: Vec<u8>,
    recent: [Recent; 3],
}

impl Default for Decoder {
    fn default() -> Self {
        Decoder {
            st: State::default(),
            tpls: vec![],
            arena: vec![],
            recent: [Recent::new(false), Recent::new(false), Recent::new(false)],
        }
    }
}

pub struct DecMark {
    st: State,
    ntpl: usize,
    narena: usize,
}

impl Decoder {
    pub fn mark(&self) -> DecMark {
        DecMark { st: self.st, ntpl: self.tpls.len(), narena: self.arena.len() }
    }

    pub fn reset(&mut self, m: &DecMark) {
        self.st = m.st;
        self.tpls.truncate(m.ntpl);
        self.arena.truncate(m.narena);
    }

    /// Appends the block coded in `s` to `out`. False if the streams are inconsistent (the
    /// state is then undefined).
    pub fn decode(&mut self, s: &[Vec<u8>; NSTREAMS], out: &mut Vec<u8>) -> bool {
        let mut ops = Rd::new(&s[S_OPS]);
        let mut tpl = Rd::new(&s[S_TPL]);
        let text = &s[S_TEXT][..];
        let mut tp = 0;
        let mut vs = [Rd::new(&s[S_V]), Rd::new(&s[S_VT]), Rd::new(&s[S_VN]), Rd::new(&s[S_VP])];
        let mut scale = Rd::new(&s[S_SCALE]);
        let mut fsym = [Rd::new(&s[S_FSYM]), Rd::new(&s[S_FSYM + 1]), Rd::new(&s[S_FSYM + 2])];
        let mut fesc = [Rd::new(&s[S_FESC]), Rd::new(&s[S_FESC + 1]), Rd::new(&s[S_FESC + 2])];
        let mut nsel = Rd::new(&s[S_NSEL]);
        let mut dup = [Rd::new(&s[S_DUP]), Rd::new(&s[S_DUP + 1]), Rd::new(&s[S_DUP + 2])];
        let mut hdr = Rd::new(&s[S_HDR]);
        let unit = hdr.u8() & H_UNIT != 0;
        while !ops.done() {
            let op = ops.uv();
            if op == OP_RAW {
                let Some(e) = memchr::memchr(b'\n', &text[tp..]) else { return false };
                out.extend_from_slice(&text[tp..tp + e + 1]);
                tp += e + 1;
                continue;
            }
            if op == OP_RAW_OPEN {
                out.extend_from_slice(&text[tp..]);
                tp = text.len();
                continue;
            }
            let id = op - FIRST_TPL;
            if id == self.tpls.len() as u64 {
                let n = tpl.uv() as usize;
                let Some(k) = tpl.bytes(n) else { return false };
                if !valid_key(k) || self.tpls.len() >= MAX_TEMPLATES || self.arena.len() + n > MAX_TEMPLATE_BYTES {
                    return false;
                }
                self.tpls.push((self.arena.len() as u32, n as u32));
                self.arena.extend_from_slice(k);
            }
            let Some(&(o, n)) = self.tpls.get(id as usize) else { return false };
            let k = &self.arena[o as usize..(o + n) as usize];
            let kind = k[0];
            out.extend_from_slice(KEYWORDS[kind as usize]);
            let mut i = 2;
            if kind < K_F {
                let r = &mut vs[kind as usize];
                let prev = &mut self.st.prev[kind as usize];
                let n = k[1] as usize;
                // Item positions, then values in coding order, then the text.
                let mut items = [0usize; MAX_NUMS];
                for it in items.iter_mut().take(n) {
                    *it = i;
                    i += 3 + k[i + 2] as usize;
                }
                let kk = kind as usize;
                let mut d = 0;
                if kk < 3 && n <= 3 {
                    d = dup[kk].uv();
                    if d > 0 {
                        let line = self.st.count[kk] as u64;
                        let Some(e) = line.checked_sub(d).and_then(|l| self.recent[kk].get(l)) else { return false };
                        for (j, p) in prev[..n].iter_mut().enumerate() {
                            *p = (e.v[j], e.s[j] as i32);
                        }
                    }
                }
                let mut sel = None;
                let mut neg = false;
                if d == 0 && unit && kind == 2 && n == 3 {
                    let c = nsel.u8();
                    sel = Some(((c & 3) as usize).min(2));
                    neg = c & 4 != 0;
                }
                let (o, _) = order(n, sel);
                for &j in &o[..if d == 0 { n } else { 0 }] {
                    let (m, f) = (k[items[j]], k[items[j] + 1]);
                    let (pv, ps) = prev[j];
                    let sc = if m != M_FIX {
                        (ps as i64).wrapping_add(scale.sv()).clamp(num::MIN_SCALE as i64, num::MAX_SCALE as i64) as i32
                    } else {
                        (f & num::F_FRAC) as i32
                    };
                    let pred = if Some(j) == sel {
                        let (a, b) = ((j + 1) % 3, (j + 2) % 3);
                        unit_pred(to_f64(prev[a].0, prev[a].1), to_f64(prev[b].0, prev[b].1), sc, neg)
                    } else {
                        num::rescale(pv, ps, sc)
                    };
                    prev[j] = (pred.wrapping_add(r.sv()), sc);
                }
                if kk < 3 && n <= 3 {
                    self.recent[kk].put(self.st.count[kk] as u64, Entry::new(&prev[..n]));
                }
                for (j, &it) in items[..n].iter().enumerate() {
                    let (m, f, sl) = (k[it], k[it + 1], k[it + 2] as usize);
                    out.extend_from_slice(&k[it + 3..it + 3 + sl]);
                    let (v, sc) = prev[j];
                    dec::put(out, v, sc, m, f);
                }
                if kind < K_VP {
                    self.st.count[kind as usize] += 1;
                }
            } else {
                for _ in 0..k[1] {
                    let (pat, sl) = (k[i], k[i + 2] as usize);
                    out.extend_from_slice(&k[i + 3..i + 3 + sl]);
                    i += 3 + sl;
                    let (idx, count) = (&mut self.st.idx, self.st.count);
                    let v = idx[0].decode(&mut fsym[0], &mut fesc[0], 0);
                    put_index(out, v, pat & C_NEG != 0, count[0]);
                    if pat & C_T != 0 {
                        let a = idx[1].decode(&mut fsym[1], &mut fesc[1], v);
                        out.push(b'/');
                        put_index(out, a, pat & (C_NEG << 1) != 0, count[1]);
                    }
                    if pat & C_N != 0 {
                        let a = idx[2].decode(&mut fsym[2], &mut fesc[2], v);
                        out.extend_from_slice(if pat & C_T != 0 { b"/" } else { b"//" });
                        put_index(out, a, pat & (C_NEG << 2) != 0, count[2]);
                    }
                }
            }
            out.extend_from_slice(&k[i + 1..]);
        }
        let all = [&ops, &tpl, &vs[0], &vs[1], &vs[2], &vs[3], &scale, &nsel, &hdr];
        tp == text.len() && all.iter().copied().chain(&fsym).chain(&fesc).chain(&dup).all(|r| r.done())
    }

}

impl Indices {
    /// Decodes an index of a corner whose v index is `v`.
    #[inline]
    fn decode(&mut self, sym: &mut Rd, esc: &mut Rd, v: i64) -> i64 {
        let x = self;
        let c = sym.u8();
        let (a, p) = match c {
            SYM_OFF => {
                let a = v.wrapping_add(x.off);
                (a, x.find(a))
            }
            SYM_NEW => (x.max.wrapping_add(1), None),
            SYM_ESC => (x.last.wrapping_add(esc.sv()), None),
            _ => {
                let p = (c as usize).min(CACHE - 1);
                (x.cache[p], Some(p))
            }
        };
        x.update(a, p, v);
        a
    }
}

/// Writes index `a`, relative to `count` (the lines of its kind so far) if `neg`.
#[inline]
fn put_index(out: &mut Vec<u8>, a: i64, neg: bool, count: i64) {
    if neg {
        out.push(b'-');
        num::put(out, (count + 1).wrapping_sub(a), 0);
    } else {
        num::put(out, a, 0);
    }
}

/// A readable form of a template key, for analysis: fixed numbers by their digits after the
/// point ("i" for none), `%g` ones as "gP", corners as patterns, separators and tails
/// escaped.
pub fn describe(k: &[u8]) -> String {
    let esc = |b: &[u8]| {
        b.iter()
            .map(|&c| match c {
                b' ' => "_".to_string(),
                b'\t' => "\\t".to_string(),
                b'\r' => "\\r".to_string(),
                b'\n' => "\\n".to_string(),
                c => (c as char).to_string(),
            })
            .collect::<String>()
    };
    let kind = k[0];
    let mut s = String::from_utf8_lossy(KEYWORDS[kind as usize]).into_owned();
    let mut i = 2;
    for _ in 0..k[1] {
        let (m, f, sl) = (k[i], k[i + 1], k[i + 2] as usize);
        s += &esc(&k[i + 3..i + 3 + sl]);
        i += 3 + sl;
        if kind >= K_F {
            s += ["a", "a/b", "a//c", "a/b/c"][(m & 3) as usize];
            if m & 0x1c != 0 {
                s += &format!("~{}", m >> 2);
            }
        } else if m != M_FIX {
            let src = ["", "f32:", "f64:"][(dec::src_of(m) as usize).min(2)];
            let r = ["", "g", "e", "py", "js", "pos"][(dec::render_of(m) as usize).min(5)];
            let p = if matches!(dec::render_of(m), dec::R_GEN | dec::R_SCI) { format!("{}", f & num::G_PREC) } else { String::new() };
            s += &format!("{src}{r}{p}{}{}", if f & num::G_EXP3 != 0 { "E" } else { "" }, if f & num::G_NEGZ != 0 { "-" } else { "" });
        } else {
            if f & num::F_NEGZ != 0 {
                s += "-";
            }
            if f & num::F_NOINT != 0 {
                s += "#";
            }
            s += &if f & num::F_POINT != 0 { format!("{}", f & num::F_FRAC) } else { "i".into() };
        }
    }
    s + "|" + &esc(&k[i + 1..])
}
