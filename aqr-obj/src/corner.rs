//! Face and line corners: per attribute (v, vt, vn) a move-to-front cache of recent
//! indices and candidates from the faces before (shared edges, pairings).

use crate::varint::Rd;
use aqr_num::fixed;

/// Corner patterns: "a", "a/b", "a//c", "a/b/c"; bits 2-4 mark negative (relative) indices.
pub(crate) const C_T: u8 = 1;
pub(crate) const C_N: u8 = 2;
pub(crate) const C_NEG: u8 = 4;

/// Corner index symbols: a position in the cache of recent indices, or one of these.
pub(crate) const CACHE: usize = 32;
/// One more than the largest index so far.
pub(crate) const SYM_NEW: u8 = CACHE as u8;
/// The first and second candidate. For v: the vertices before and after the previous
/// corner's in the last face that had it (a shared edge). For vt, vn: the corner's v index
/// plus the previous corner's offset from it, and the index last paired with this v.
pub(crate) const SYM_X1: u8 = SYM_NEW + 1;
pub(crate) const SYM_X2: u8 = SYM_NEW + 2;
/// SYM_ESC + q: coded as the difference from the index at position q of the cache (0 is
/// the previous index).
pub(crate) const SYM_ESC: u8 = SYM_NEW + 3;

/// Per v index (direct mapped), its neighbours and vt, vn in the last face that had it.
pub(crate) const CORNERS: usize = 1 << 16;

#[derive(Clone, Copy)]
pub(crate) struct Corner {
    v: i64,
    before: i64,
    after: i64,
    t: i64,
    n: i64,
}

pub(crate) struct Corners(Vec<Corner>);

impl Corners {
    /// The table grows as vertices come (small files never fill it).
    pub(crate) fn new() -> Self {
        Corners(vec![])
    }

    #[inline]
    pub(crate) fn get(&self, v: i64) -> Option<&Corner> {
        let c = self.0.get(v as usize % CORNERS)?;
        (c.v == v).then_some(c)
    }

    /// Records a face's corners (v, vt, vn; 0 for absent ones).
    #[inline]
    pub(crate) fn face(&mut self, f: &[(i64, i64, i64)]) {
        let n = f.len();
        for (j, &(v, t, nn)) in f.iter().enumerate() {
            let c = Corner { v, before: f[(j + n - 1) % n].0, after: f[(j + 1) % n].0, t, n: nn };
            let i = v as usize % CORNERS;
            if i >= self.0.len() {
                self.0.resize(i + 1, Corner { v: 0, before: 0, after: 0, t: 0, n: 0 });
            }
            self.0[i] = c;
        }
    }

    /// Forgets everything (the encoder, after undoing a block).
    pub(crate) fn clear(&mut self) {
        self.0.clear();
    }

    /// Candidates for corner attribute `k` with v index `v`, after a corner with v index
    /// `pv` (None for the first), `off` the attribute's last offset from v.
    #[inline]
    pub(crate) fn cands(&self, k: usize, v: i64, pv: Option<i64>, off: i64) -> [Option<i64>; 2] {
        if k == 0 {
            match pv.and_then(|p| self.get(p)) {
                Some(c) => [Some(c.before), Some(c.after)],
                None => [None, None],
            }
        } else {
            [Some(v.wrapping_add(off)), self.get(v).map(|c| if k == 1 { c.t } else { c.n })]
        }
    }
}

/// Recent indices of one attribute, most recent first.
#[derive(Clone, Copy)]
pub(crate) struct Indices {
    pub(crate) cache: [i64; CACHE],
    pub(crate) max: i64,
    /// Previous index minus its corner's v index.
    pub(crate) off: i64,
}

impl Indices {
    #[inline]
    pub(crate) fn find(&self, a: i64) -> Option<usize> {
        self.cache.iter().position(|&x| x == a)
    }

    /// Records `a` (found at `p` in the cache, if it is there).
    #[inline]
    pub(crate) fn update(&mut self, a: i64, p: Option<usize>, v: i64) {
        let p = p.unwrap_or(CACHE - 1);
        // A loop: copy_within calls memmove, slow for these few bytes.
        for i in (0..p).rev() {
            self.cache[i + 1] = self.cache[i];
        }
        self.cache[0] = a;
        self.max = self.max.max(a);
        self.off = a.wrapping_sub(v);
    }
}

/// An index token: `[-]digits`, no leading zero, not zero.
#[inline]
pub(crate) fn parse_index(t: &[u8]) -> Option<(i64, bool)> {
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

impl Indices {
    /// Decodes an index of a corner whose v index is `v`, given the candidates.
    #[inline]
    pub(crate) fn decode(&mut self, sym: &mut Rd, esc: &mut Rd, v: i64, cands: [Option<i64>; 2]) -> i64 {
        let x = self;
        let c = sym.u8();
        let (a, p) = match c {
            SYM_X1 | SYM_X2 => {
                let a = cands[(c - SYM_X1) as usize].unwrap_or(0);
                (a, x.find(a))
            }
            SYM_NEW => (x.max.wrapping_add(1), None),
            _ if c >= SYM_ESC => {
                let q = ((c - SYM_ESC) as usize).min(CACHE - 1);
                (x.cache[q].wrapping_add(esc.sv()), None)
            }
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
pub(crate) fn put_index(out: &mut Vec<u8>, a: i64, neg: bool, count: i64) {
    if neg {
        out.push(b'-');
        fixed::put(out, (count + 1).wrapping_sub(a), 0);
    } else {
        fixed::put(out, a, 0);
    }
}
