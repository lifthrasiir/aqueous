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

use crate::corner::{C_N, C_NEG, C_T, CACHE, Corners, Indices, SYM_ESC, SYM_NEW, SYM_X1, SYM_X2, parse_index, put_index};
use crate::recent::{Entry, NPRED, Recent, WIN, window_pred};
pub use crate::style::Styles;
pub use crate::style::next_styles;
use crate::style::exp_fallback;
use crate::{dec, num};
use crate::varint::{Rd, put_sv, put_uv};
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

/// A multiplicative hash for template keys (short byte strings, not attacker-sensitive:
/// the table is bounded).
#[derive(Default)]
struct KeyHasher(u64);

impl Hasher for KeyHasher {
    #[inline]
    fn write(&mut self, b: &[u8]) {
        let mut h = self.0;
        let mut c = b.chunks_exact(8);
        for w in &mut c {
            h = (h.rotate_left(5) ^ u64::from_le_bytes(w.try_into().unwrap())).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
        }
        for &x in c.remainder() {
            h = (h.rotate_left(5) ^ x as u64).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
        }
        self.0 = h;
    }

    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
}

type KeyMap = HashMap<Box<[u8]>, u32, BuildHasherDefault<KeyHasher>>;

pub const S_OPS: usize = 0;
pub const S_TPL: usize = 1;
pub const S_TEXT: usize = 2;
/// Block flags (one byte).
pub const S_HDR: usize = 3;
/// Scale changes of numbers not in a fixed format.
pub const S_SCALE: usize = 4;
/// The predicted component of unit normals, and its sign.
pub const S_NSEL: usize = 5;
/// Corner index symbols per attribute (v, vt, vn), then their escapes.
pub const S_FSYM: usize = 6;
pub const S_FESC: usize = 9;
/// Per v, vt, vn line: the distance back to an equal line of its kind, or 0.
pub const S_DUP: usize = 12;
/// Number residuals, by kind (v, vt, vn, vp) and position (0, 1, 2, the rest).
pub const S_VAL: usize = 15;
/// Residuals of the predicted components of unit normals.
pub const S_UNIT: usize = 31;
/// Per v, vt, vn line of two or three numbers (not a reference to an equal line): its
/// predictor.
pub const S_PRED: usize = 32;
pub const NSTREAMS: usize = 35;
pub const STREAM_NAMES: [&str; NSTREAMS] = [
    "ops", "tpl", "text", "hdr", "scale", "nsel", "fv", "ft", "fn", "fv-esc", "ft-esc", "fn-esc", "v-dup", "vt-dup",
    "vn-dup", "v0", "v1", "v2", "v3+", "vt0", "vt1", "vt2", "vt3+", "vn0", "vn1", "vn2", "vn3+", "vp0", "vp1", "vp2",
    "vp3+", "vn-unit", "v-pred", "vt-pred", "vn-pred",
];

/// The residual stream of position `j` of numeric kind `k`.
#[inline]
pub fn s_val(k: usize, j: usize) -> usize {
    S_VAL + k * 4 + j.min(3)
}

/// Block flag: normals (`vn` lines of three numbers) are coded as unit vectors.
const H_UNIT: u8 = 1;

/// Template kinds (the numeric ones first).
const K_VP: u8 = 3;
pub(crate) const K_F: u8 = 4;
const K_L: u8 = 5;
const KEYWORDS: [&[u8]; 6] = [b"v", b"vt", b"vn", b"vp", b"f", b"l"];

/// Number formats (first byte of a numeric item; the second is the format's flags).
pub(crate) const M_FIX: u8 = 0;
const M_GEN: u8 = 1;

/// Numbers per numeric line, corners per face line.
pub(crate) const MAX_NUMS: usize = 8;
const MAX_CORNERS: usize = 64;
const MAX_TEMPLATES: usize = 1 << 16;
const MAX_TEMPLATE_BYTES: usize = 4 << 20;
/// Ops for raw lines; templates are numbered from FIRST_TPL.
const OP_RAW: u64 = 0;
const OP_RAW_OPEN: u64 = 1;
const FIRST_TPL: u64 = 2;

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
        let idx = Indices { cache: [0; CACHE], max: 0, off: 0 };
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

/// The keyword kind of a line, and the keyword's length.
#[inline]
pub(crate) fn kind_of(l: &[u8]) -> Option<(u8, usize)> {
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

/// A line kept as text.
#[inline]
fn raw_line(l: &[u8], s: &mut [Vec<u8>; NSTREAMS]) {
    s[S_OPS].push(if l.last() == Some(&b'\n') { OP_RAW } else { OP_RAW_OPEN } as u8);
    s[S_TEXT].extend_from_slice(l);
}

/// Appends tail `t` to `key`, if it is a valid one.
#[inline]
fn tail(key: &mut Vec<u8>, t: &[u8]) -> bool {
    if t.len() > 255 || !valid_tail(t) {
        return false;
    }
    key.push(t.len() as u8);
    key.extend_from_slice(t);
    true
}

/// A corner token: its indices as written (relative ones not resolved), and its pattern.
#[inline]
fn parse_corner(t: &[u8]) -> Option<((i64, i64, i64), u8)> {
    let s1 = memchr::memchr(b'/', t);
    let Some(s1) = s1 else {
        let a = parse_index(t)?;
        return Some(((a.0, 0, 0), (a.1 as u8) * C_NEG));
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
        pat |= C_T | ((b.1 as u8) * (C_NEG << 1));
        tv = b.0;
    }
    if let Some(c) = c {
        pat |= C_N | ((c.1 as u8) * (C_NEG << 2));
        nv = c.0;
    }
    Some(((a.0, tv, nv), pat))
}

/// Parses a `v`, `vt`, `vn`, `vp`, `f` or `l` line in `styles`: its template into `key`,
/// its numbers into `vals` (zeros not in a fixed format at scale 0: `Encoder::code_line`
/// gives them the previous one's) or its corners onto `corners`. The kind and the count
/// of numbers or corners; None if the line does not parse back to itself. Needs no state:
/// it runs on other threads too.
#[inline]
fn parse_line(
    l: &[u8],
    styles: &Styles,
    key: &mut Vec<u8>,
    vals: &mut [(i64, i32, u8); MAX_NUMS],
    corners: &mut Vec<(i64, i64, i64)>,
    tmp: &mut Vec<u8>,
) -> Option<(u8, usize)> {
    let (kind, kw) = kind_of(l)?;
    key.clear();
    key.push(kind);
    key.push(0);
    let mut i = kw;
    let mut n = 0;
    if kind < K_F {
        let style = styles.kinds[kind as usize];
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
                return None;
            }
            let t = &l[i..te];
            let (v, sc, mode, f) = if style.mode != M_FIX
                && let Some((v, sc, f)) = dec::encode(t, style.mode, style.p, style.exp3, 0, tmp)
            {
                (v, sc, style.mode, f)
            } else if let Some((v, f)) = num::parse(t) {
                (v, (f & num::F_FRAC) as i32, M_FIX, f)
            } else if let Some((v, sc, f)) = num::parse_gen(t, style.p.clamp(6, 17), 0, style.exp3, tmp) {
                (v, sc, M_GEN, f)
            } else {
                exp_fallback(t, 0, tmp)?
            };
            vals[n] = (v, sc, mode);
            n += 1;
            key.extend_from_slice(&[mode, f, (i - ws) as u8]);
            key.extend_from_slice(&l[ws..i]);
            i = te;
        }
    } else {
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
                return None;
            }
            let (c, pat) = parse_corner(&l[i..te])?;
            corners.push(c);
            n += 1;
            key.extend_from_slice(&[pat, 0, (i - ws) as u8]);
            key.extend_from_slice(&l[ws..i]);
            i = te;
        }
    }
    if n == 0 || !tail(key, &l[i..]) {
        return None;
    }
    key[1] = n as u8;
    Some((kind, n))
}

/// A line of a block parsed on another thread: its text range, kind (RAW if it is kept as
/// text), count of numbers or corners, template key and items in the block's arrays.
pub struct ParsedLine {
    start: u32,
    end: u32,
    kind: u8,
    n: u16,
    key_len: u16,
    key: u32,
    item: u32,
}

const RAW: u8 = 255;

/// A block parsed on another thread (`parse_block`), for `Encoder::encode_parsed`.
#[derive(Default)]
pub struct ParsedBlock {
    lines: Vec<ParsedLine>,
    keys: Vec<u8>,
    vals: Vec<(i64, i32, u8)>,
    corners: Vec<(i64, i64, i64)>,
}

/// Parses the lines of a block (at most `MAX_BLOCK`, well under 4 GB) in `styles`.
pub fn parse_block(styles: &Styles, src: &[u8]) -> ParsedBlock {
    let mut b = ParsedBlock::default();
    let mut key = Vec::new();
    let mut tmp = Vec::new();
    let mut vals = [(0i64, 0i32, 0u8); MAX_NUMS];
    let mut p = 0;
    while p < src.len() {
        let e = memchr::memchr(b'\n', &src[p..]).map_or(src.len(), |i| p + i + 1);
        let mut r = ParsedLine { start: p as u32, end: e as u32, kind: RAW, n: 0, key_len: 0, key: 0, item: 0 };
        let c0 = b.corners.len();
        match parse_line(&src[p..e], styles, &mut key, &mut vals, &mut b.corners, &mut tmp) {
            Some((kind, n)) => {
                r.kind = kind;
                r.n = n as u16;
                r.key = b.keys.len() as u32;
                r.key_len = key.len() as u16;
                b.keys.extend_from_slice(&key);
                if kind < K_F {
                    r.item = b.vals.len() as u32;
                    b.vals.extend_from_slice(&vals[..n]);
                } else {
                    r.item = c0 as u32;
                }
            }
            // Corners of a line that failed half way.
            None => b.corners.truncate(c0),
        }
        b.lines.push(r);
        p = e;
    }
    b
}

pub struct Encoder {
    pub st: State,
    styles: Styles,
    map: KeyMap,
    /// The last template of each kind, to skip the lookup when a line repeats it.
    last: [u32; 6],
    keys: Vec<Box<[u8]>>,
    /// Bytes in `keys`.
    bytes: usize,
    key: Vec<u8>,
    tmp: Vec<u8>,
    recent: [Recent; 3],
    corners: Corners,
    /// The last predictor per kind (v, vt, vn).
    last_vp: [usize; 3],
    /// Corners of the current face line.
    fbuf: Vec<(i64, i64, i64)>,
}

impl Default for Encoder {
    fn default() -> Self {
        Encoder {
            st: State::default(),
            styles: Styles::default(),
            map: KeyMap::default(),
            last: [u32::MAX; 6],
            keys: vec![],
            bytes: 0,
            key: vec![],
            tmp: Vec::with_capacity(64),
            recent: [Recent::new(true), Recent::new(true), Recent::new(true)],
            corners: Corners::new(),
            last_vp: [0; 3],
            fbuf: vec![],
        }
    }
}

/// What a block's encoding changed, to undo it (when verification fails).
pub struct Mark {
    st: State,
    ntpl: usize,
    // The encoder's own choices carried from block to block, so that undoing blocks gives
    // the same output whenever it happens (a block at a time, or several at once).
    styles: Styles,
    last_vp: [usize; 3],
}

impl Encoder {
    /// The template keys, in id order (for analysis).
    pub fn keys(&self) -> &[Box<[u8]>] {
        &self.keys
    }

    pub fn mark(&self) -> Mark {
        Mark { st: self.st, ntpl: self.keys.len(), styles: self.styles, last_vp: self.last_vp }
    }

    /// Undoes the block coded since `m`, which is then stored whole. The corner table and
    /// the recent lines are cleared: the decoder never learns what the block would have
    /// taught it, and the encoder must only use what the decoder knows.
    pub fn reset(&mut self, m: &Mark) {
        self.st = m.st;
        (self.styles, self.last_vp) = (m.styles, m.last_vp);
        self.corners.clear();
        for r in &mut self.recent {
            r.clear();
        }
        for k in self.keys.drain(m.ntpl..) {
            self.bytes -= k.len();
            self.map.remove(&k);
        }
    }

    /// Codes `src` into `s` (cleared first).
    pub fn encode(&mut self, src: &[u8], s: &mut [Vec<u8>; NSTREAMS]) {
        self.styles = next_styles(&self.styles, src);
        self.begin(s);
        let mut p = 0;
        let mut vals = [(0i64, 0i32, 0u8); MAX_NUMS];
        let (mut key, mut corners) = (std::mem::take(&mut self.key), std::mem::take(&mut self.fbuf));
        while p < src.len() {
            let e = memchr::memchr(b'\n', &src[p..]).map_or(src.len(), |i| p + i + 1);
            let l = &src[p..e];
            corners.clear();
            let ok = match parse_line(l, &self.styles, &mut key, &mut vals, &mut corners, &mut self.tmp) {
                Some((kind, n)) => self.code_line(kind, n, &key, &mut vals, &mut corners, s),
                None => false,
            };
            if !ok {
                raw_line(l, s);
            }
            p = e;
        }
        (self.key, self.fbuf) = (key, corners);
    }

    /// Codes a block parsed on another thread (`parse_block`, with `styles`, which become
    /// the encoder's).
    pub fn encode_parsed(&mut self, styles: &Styles, src: &[u8], b: &ParsedBlock, s: &mut [Vec<u8>; NSTREAMS]) {
        self.styles = *styles;
        self.begin(s);
        let mut corners = std::mem::take(&mut self.fbuf);
        let mut vals = [(0i64, 0i32, 0u8); MAX_NUMS];
        for r in &b.lines {
            let l = &src[r.start as usize..r.end as usize];
            let key = &b.keys[r.key as usize..r.key as usize + r.key_len as usize];
            let ok = match r.kind {
                RAW => false,
                k if k < K_F => {
                    vals[..r.n as usize].copy_from_slice(&b.vals[r.item as usize..r.item as usize + r.n as usize]);
                    self.code_line(k, r.n as usize, key, &mut vals, &mut corners, s)
                }
                k => {
                    corners.clear();
                    corners.extend_from_slice(&b.corners[r.item as usize..r.item as usize + r.n as usize]);
                    self.code_line(k, r.n as usize, key, &mut vals, &mut corners, s)
                }
            };
            if !ok {
                raw_line(l, s);
            }
        }
        self.fbuf = corners;
    }

    /// The styles of the last block (those the next block's start from).
    pub fn styles(&self) -> Styles {
        self.styles
    }

    fn begin(&mut self, s: &mut [Vec<u8>; NSTREAMS]) {
        for v in s.iter_mut() {
            v.clear();
        }
        s[S_HDR].push(if self.styles.unit { H_UNIT } else { 0 });
    }

    /// Codes a parsed line (`parse_line`): template `key`, numbers `vals` or corner
    /// indices `corners` (as written; resolved here). False if it cannot (the template
    /// table is full): nothing is written then.
    #[inline]
    fn code_line(
        &mut self,
        kind: u8,
        n: usize,
        key: &[u8],
        vals: &mut [(i64, i32, u8); MAX_NUMS],
        corners: &mut [(i64, i64, i64)],
        s: &mut [Vec<u8>; NSTREAMS],
    ) -> bool {
        let Some(id) = self.template(key, s) else { return false };
        put_uv(&mut s[S_OPS], id);
        if kind >= K_F {
            self.code_corners(key, corners, s);
            return true;
        }
        let k = kind as usize;
        let prev = &mut self.st.prev[k];
        // Zeros not in a fixed format take the scale of the previous number.
        for (j, v) in vals[..n].iter_mut().enumerate() {
            if v.2 != M_FIX && v.0 == 0 {
                v.1 = prev[j].1;
            }
        }
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
        if self.styles.unit && kind == 2 && n == 3 {
            let x: [f64; 3] = std::array::from_fn(|j| to_f64(vals[j].0, vals[j].1));
            let j = (0..3).fold(0, |b, j| if x[j].abs() > x[b].abs() { j } else { b });
            s[S_NSEL].push(j as u8 | ((vals[j].0 < 0) as u8) << 2);
            sel = Some(j);
        }
        // The predictor that leaves the fewest residual bits, staying with the last one
        // unless another saves a few.
        let line = self.st.count[k.min(2)] as u64;
        let mut vp = 0;
        if k < 3 && (2..=3).contains(&n) {
            vp = self.line_pred(k, vals, n, sel, line);
            self.last_vp[k] = vp;
            s[S_PRED + k].push(vp as u8);
        }
        let prev = &mut self.st.prev[k];
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
            } else if vp > 0 {
                window_pred(&self.recent[k], line, vp, j, sc).expect("checked")
            } else {
                num::rescale(pv, ps, sc)
            };
            put_sv(&mut s[if Some(j) == sel { S_UNIT } else { s_val(k, j) }], v.wrapping_sub(pred));
            prev[j] = (v, sc);
        }
        if kind < K_VP {
            self.st.count[k] += 1;
        }
        true
    }

    /// Codes the corners of a face or polyline line (indices as written, resolved here).
    #[inline]
    fn code_corners(&mut self, key: &[u8], corners: &mut [(i64, i64, i64)], s: &mut [Vec<u8>; NSTREAMS]) {
        let mut k = 2;
        let mut pv = None;
        let count = self.st.count;
        let abs = |x: i64, neg: bool, k: usize| if neg { (count[k] + 1).wrapping_sub(x) } else { x };
        for c in corners.iter_mut() {
            let pat = key[k];
            k += 3 + key[k + 2] as usize;
            let v = abs(c.0, pat & C_NEG != 0, 0);
            c.0 = v;
            self.index(s, 0, v, v, pv);
            if pat & C_T != 0 {
                c.1 = abs(c.1, pat & (C_NEG << 1) != 0, 1);
                self.index(s, 1, c.1, v, pv);
            } else {
                c.1 = 0;
            }
            if pat & C_N != 0 {
                c.2 = abs(c.2, pat & (C_NEG << 2) != 0, 2);
                self.index(s, 2, c.2, v, pv);
            } else {
                c.2 = 0;
            }
            pv = Some(v);
        }
        self.corners.face(corners);
    }

    /// The template id of `key`, defining it if new; None when the table is full.
    #[inline]
    fn template(&mut self, key: &[u8], s: &mut [Vec<u8>; NSTREAMS]) -> Option<u64> {
        let kind = key[0] as usize;
        let l = self.last[kind];
        if let Some(k) = self.keys.get(l as usize)
            && **k == *key
        {
            return Some(l as u64 + FIRST_TPL);
        }
        if let Some(&id) = self.map.get(key) {
            self.last[kind] = id;
            return Some(id as u64 + FIRST_TPL);
        }
        if self.keys.len() >= MAX_TEMPLATES || self.bytes + key.len() > MAX_TEMPLATE_BYTES {
            return None;
        }
        self.bytes += key.len();
        let id = self.keys.len() as u32;
        self.last[kind] = id;
        let k: Box<[u8]> = key.into();
        self.map.insert(k.clone(), id);
        self.keys.push(k);
        put_uv(&mut s[S_TPL], key.len() as u64);
        s[S_TPL].extend_from_slice(key);
        Some(id as u64 + FIRST_TPL)
    }

    /// Codes index `a` of attribute `k` of a corner whose v index is `v`, after a corner
    /// with v index `pv`.
    #[inline]
    fn index(&mut self, s: &mut [Vec<u8>; NSTREAMS], k: usize, a: i64, v: i64, pv: Option<i64>) {
        let x = &mut self.st.idx[k];
        let cands = self.corners.cands(k, v, pv, x.off);
        let p = x.find(a);
        let sym = if cands[0] == Some(a) {
            SYM_X1
        } else if cands[1] == Some(a) {
            SYM_X2
        } else if let Some(p) = p {
            p as u8
        } else if a == x.max.wrapping_add(1) {
            SYM_NEW
        } else {
            // The difference from the nearest cached index (in bits, a few more for a
            // position other than the previous index's).
            let mut best = (u32::MAX, 0);
            for (q, &c) in x.cache.iter().enumerate() {
                let b = 64 - crate::varint::zz(a.wrapping_sub(c)).leading_zeros() + if q > 0 { 2 } else { 0 };
                if b < best.0 {
                    best = (b, q);
                }
            }
            put_sv(&mut s[S_FESC + k], a.wrapping_sub(x.cache[best.1]));
            SYM_ESC + best.1 as u8
        };
        s[S_FSYM + k].push(sym);
        x.update(a, p, v);
    }

    /// The predictor for line `line` of kind `kind` (v, vt, vn) with `n` numbers `vals`
    /// (value, scale), position `sel` predicted otherwise.
    #[inline]
    fn line_pred(&self, kind: usize, vals: &[(i64, i32, u8); MAX_NUMS], n: usize, sel: Option<usize>, line: u64) -> usize {
        // The window at the line's scales; p = 0 is the previous numbers.
        let rec = &self.recent[kind];
        let prev = &self.st.prev[kind];
        let mut w = [[0i64; 3]; WIN + 2];
        let mut have = 1usize;
        for j in 0..n {
            w[0][j] = num::rescale(prev[j].0, prev[j].1, vals[j].1);
        }
        for (k, wk) in w.iter_mut().enumerate().skip(1) {
            let Some(e) = line.checked_sub(k as u64).and_then(|l| rec.get(l)) else { break };
            for j in 0..n {
                let sc = vals[j].1;
                wk[j] = if e.s[j] as i32 == sc { e.v[j] } else { num::rescale(e.v[j], e.s[j] as i32, sc) };
            }
            have = k + 1;
        }
        // Positions that count (as 0 or 1), so that the cost is a fixed loop.
        let act: [u32; 3] = std::array::from_fn(|j| (j < n && Some(j) != sel) as u32);
        let cost = |pr: [i64; 3]| -> u32 {
            let mut c = 0;
            for j in 0..3 {
                c += act[j] * (64 - crate::varint::zz(vals[j].0.wrapping_sub(pr[j])).leading_zeros());
            }
            c
        };
        let mut best = (cost(w[0]), 0);
        let mut try_p = |p: usize, pr: [i64; 3]| {
            let c = cost(pr);
            if c < best.0 {
                best = (c, p);
            }
        };
        for p in 1..WIN.min(have.saturating_sub(1)) {
            try_p(p, w[p + 1]);
        }
        for k in 1..have.saturating_sub(1).min(WIN + 1) {
            let pr = std::array::from_fn(|j| w[1][j].wrapping_add(w[k][j]).wrapping_sub(w[k + 1][j]));
            try_p(WIN + k - 1, pr);
        }
        // The last predictor, if available, unless the best saves more than a few bits.
        let lp = self.last_vp[kind];
        let avail = lp == 0 || (lp < WIN && lp + 1 < have) || (lp >= WIN && lp - WIN + 2 < have);
        if avail && lp != best.1 {
            let pr = if lp == 0 {
                w[0]
            } else if lp < WIN {
                w[lp + 1]
            } else {
                let k = lp - WIN + 1;
                std::array::from_fn(|j| w[1][j].wrapping_add(w[k][j]).wrapping_sub(w[k + 1][j]))
            };
            if cost(pr) <= best.0 + 3 {
                return lp;
            }
        }
        best.1
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

/// Where a decoded block goes: its text (`Vec<u8>`), or records of it (`Deferred`) to
/// print later, on another thread.
pub trait Sink {
    fn bytes(&mut self, b: &[u8]);
    fn byte(&mut self, c: u8);
    /// A number: value, scale, mode and flags of its template item.
    fn number(&mut self, v: i64, s: i32, mode: u8, f: u8);
    /// A corner index, relative to `count` (the lines of its kind so far) if `neg`.
    fn index(&mut self, a: i64, neg: bool, count: i64);
}

impl Sink for Vec<u8> {
    #[inline]
    fn bytes(&mut self, b: &[u8]) {
        self.extend_from_slice(b);
    }

    #[inline]
    fn byte(&mut self, c: u8) {
        self.push(c);
    }

    #[inline]
    fn number(&mut self, v: i64, s: i32, mode: u8, f: u8) {
        dec::put(self, v, s, mode, f);
    }

    #[inline]
    fn index(&mut self, a: i64, neg: bool, count: i64) {
        put_index(self, a, neg, count);
    }
}

/// Records of a decoded block: bytes as they are, numbers and indices as values.
#[derive(Default)]
pub struct Deferred {
    rec: Vec<u8>,
}

const R_BYTES: u8 = 0;
const R_BYTE: u8 = 1;
const R_NUMBER: u8 = 2;
const R_INDEX: u8 = 3;
const R_NEG_INDEX: u8 = 4;

impl Sink for Deferred {
    #[inline]
    fn bytes(&mut self, b: &[u8]) {
        self.rec.push(R_BYTES);
        put_uv(&mut self.rec, b.len() as u64);
        self.rec.extend_from_slice(b);
    }

    #[inline]
    fn byte(&mut self, c: u8) {
        self.rec.extend_from_slice(&[R_BYTE, c]);
    }

    #[inline]
    fn number(&mut self, v: i64, s: i32, mode: u8, f: u8) {
        let mut r = [0u8; 12];
        r[0] = R_NUMBER;
        r[1..9].copy_from_slice(&v.to_le_bytes());
        // Scales stay in -60..=60 (clamped by the decoder).
        r[9..12].copy_from_slice(&[s as i8 as u8, mode, f]);
        self.rec.extend_from_slice(&r);
    }

    #[inline]
    fn index(&mut self, a: i64, neg: bool, count: i64) {
        let (t, x) = if neg { (R_NEG_INDEX, (count + 1).wrapping_sub(a)) } else { (R_INDEX, a) };
        let mut r = [0u8; 9];
        r[0] = t;
        r[1..9].copy_from_slice(&x.to_le_bytes());
        self.rec.extend_from_slice(&r);
    }
}

impl Deferred {
    pub fn clear(&mut self) {
        self.rec.clear();
    }

    /// Appends the text the records stand for to `out` (as `decode` into a `Vec` would).
    pub fn print(&self, out: &mut Vec<u8>) {
        let r = &self.rec;
        let mut p = 0;
        let i64_at = |p: usize| i64::from_le_bytes(r[p..p + 8].try_into().unwrap());
        while p < r.len() {
            match r[p] {
                R_BYTES => {
                    let mut rd = Rd::new(&r[p + 1..]);
                    let n = rd.uv() as usize;
                    let s = p + 1 + rd.p;
                    out.extend_from_slice(&r[s..s + n]);
                    p = s + n;
                }
                R_BYTE => {
                    out.push(r[p + 1]);
                    p += 2;
                }
                R_NUMBER => {
                    dec::put(out, i64_at(p + 1), r[p + 9] as i8 as i32, r[p + 10], r[p + 11]);
                    p += 12;
                }
                t => {
                    if t == R_NEG_INDEX {
                        out.push(b'-');
                    }
                    num::put(out, i64_at(p + 1), 0);
                    p += 9;
                }
            }
        }
    }
}

pub struct Decoder {
    st: State,
    /// Template keys, by offset and length into `arena`.
    tpls: Vec<(u32, u32)>,
    arena: Vec<u8>,
    recent: [Recent; 3],
    corners: Corners,
    /// Corners of the current face line.
    fbuf: Vec<(i64, i64, i64)>,
}

impl Default for Decoder {
    fn default() -> Self {
        Decoder {
            st: State::default(),
            tpls: vec![],
            arena: vec![],
            recent: [Recent::new(false), Recent::new(false), Recent::new(false)],
            corners: Corners::new(),
            fbuf: vec![],
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

    /// Undoes the block decoded since `m`, which is then stored whole. What the block taught
    /// the corner table stays, as it does in a decoder that never saw the block: the
    /// encoder forgets all of it instead, so it never picks a candidate the decoder lacks.
    pub fn reset(&mut self, m: &DecMark) {
        self.st = m.st;
        self.tpls.truncate(m.ntpl);
        self.arena.truncate(m.narena);
    }

    /// Appends the block coded in `s` to `out`. False if the streams are inconsistent (the
    /// state is then undefined).
    pub fn decode<O: Sink>(&mut self, s: &[Vec<u8>; NSTREAMS], out: &mut O) -> bool {
        let mut ops = Rd::new(&s[S_OPS]);
        let mut tpl = Rd::new(&s[S_TPL]);
        let text = &s[S_TEXT][..];
        let mut tp = 0;
        let mut vs: [Rd; 17] = std::array::from_fn(|i| Rd::new(&s[S_VAL + i]));
        let mut scale = Rd::new(&s[S_SCALE]);
        let mut fsym = [Rd::new(&s[S_FSYM]), Rd::new(&s[S_FSYM + 1]), Rd::new(&s[S_FSYM + 2])];
        let mut fesc = [Rd::new(&s[S_FESC]), Rd::new(&s[S_FESC + 1]), Rd::new(&s[S_FESC + 2])];
        let mut nsel = Rd::new(&s[S_NSEL]);
        let mut dup = [Rd::new(&s[S_DUP]), Rd::new(&s[S_DUP + 1]), Rd::new(&s[S_DUP + 2])];
        let mut lpred = [Rd::new(&s[S_PRED]), Rd::new(&s[S_PRED + 1]), Rd::new(&s[S_PRED + 2])];
        let mut hdr = Rd::new(&s[S_HDR]);
        let unit = hdr.u8() & H_UNIT != 0;
        while !ops.done() {
            let op = ops.uv();
            if op == OP_RAW {
                let Some(e) = memchr::memchr(b'\n', &text[tp..]) else { return false };
                out.bytes(&text[tp..tp + e + 1]);
                tp += e + 1;
                continue;
            }
            if op == OP_RAW_OPEN {
                out.bytes(&text[tp..]);
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
            out.bytes(KEYWORDS[kind as usize]);
            let mut i = 2;
            if kind < K_F {
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
                let line = self.st.count[kk.min(2)] as u64;
                let mut vp = 0;
                if d == 0 && kk < 3 && (2..=3).contains(&n) {
                    vp = (lpred[kk].u8() as usize).min(NPRED - 1);
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
                    } else if vp > 0 {
                        let Some(x) = window_pred(&self.recent[kk], line, vp, j, sc) else { return false };
                        x
                    } else {
                        num::rescale(pv, ps, sc)
                    };
                    let r = if Some(j) == sel { S_UNIT } else { s_val(kind as usize, j) };
                    prev[j] = (pred.wrapping_add(vs[r - S_VAL].sv()), sc);
                }
                if kk < 3 && n <= 3 {
                    self.recent[kk].put(self.st.count[kk] as u64, Entry::new(&prev[..n]));
                }
                for (j, &it) in items[..n].iter().enumerate() {
                    let (m, f, sl) = (k[it], k[it + 1], k[it + 2] as usize);
                    out.bytes(&k[it + 3..it + 3 + sl]);
                    let (v, sc) = prev[j];
                    out.number(v, sc, m, f);
                }
                if kind < K_VP {
                    self.st.count[kind as usize] += 1;
                }
            } else {
                let n = k[1] as usize;
                let mut face = std::mem::take(&mut self.fbuf);
                face.clear();
                face.resize(n, (0, 0, 0));
                let mut pv = None;
                for c in face[..n].iter_mut() {
                    let (pat, sl) = (k[i], k[i + 2] as usize);
                    out.bytes(&k[i + 3..i + 3 + sl]);
                    i += 3 + sl;
                    let (idx, count, cs) = (&mut self.st.idx, self.st.count, &self.corners);
                    let v = idx[0].decode(&mut fsym[0], &mut fesc[0], 0, cs.cands(0, 0, pv, 0));
                    c.0 = v;
                    out.index(v, pat & C_NEG != 0, count[0]);
                    if pat & C_T != 0 {
                        let a = idx[1].decode(&mut fsym[1], &mut fesc[1], v, cs.cands(1, v, pv, idx[1].off));
                        c.1 = a;
                        out.byte(b'/');
                        out.index(a, pat & (C_NEG << 1) != 0, count[1]);
                    }
                    if pat & C_N != 0 {
                        let a = idx[2].decode(&mut fsym[2], &mut fesc[2], v, cs.cands(2, v, pv, idx[2].off));
                        c.2 = a;
                        out.bytes(if pat & C_T != 0 { b"/" } else { b"//" });
                        out.index(a, pat & (C_NEG << 2) != 0, count[2]);
                    }
                    pv = Some(v);
                }
                self.corners.face(&face);
                self.fbuf = face;
            }
            out.bytes(&k[i + 1..]);
        }
        let all = [&ops, &tpl, &scale, &nsel, &hdr];
        tp == text.len() && all.iter().copied().chain(&fsym).chain(&fesc).chain(&dup).chain(&vs).chain(&lpred).all(|r| r.done())
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
