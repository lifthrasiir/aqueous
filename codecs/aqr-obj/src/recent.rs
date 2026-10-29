//! Recent lines of a kind: references to equal lines, and the window line predictors use.

use aqr_num::fixed;

/// Lines of a kind (v, vt, vn) kept for references to equal lines: the last RECENT.
pub(crate) const RECENT: usize = 1 << 16;

/// The values of a line of up to three numbers; absent ones are a value no number has.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Entry {
    /// Line number + 1 (0: empty).
    pub(crate) tag: u64,
    pub(crate) v: [i64; 3],
    pub(crate) s: [i8; 3],
}

impl Entry {
    pub(crate) fn new(vals: &[(i64, i32)]) -> Entry {
        let mut e = Entry { tag: 0, v: [i64::MIN; 3], s: [i8::MIN; 3] };
        for (j, &(v, s)) in vals.iter().enumerate() {
            e.v[j] = v;
            e.s[j] = s as i8;
        }
        e
    }

    #[inline]
    pub(crate) fn hash(&self) -> usize {
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
/// the last line with them. Entries carry their line number (a slot holds one line in
/// RECENT). The encoder clears all after undoing a block, so that it only refers to lines
/// the decoder has.
pub(crate) struct Recent {
    ring: Vec<Entry>,
    table: Vec<u64>,
}

impl Recent {
    /// The ring grows as lines come (small files never fill it).
    pub(crate) fn new(table: bool) -> Self {
        Recent { ring: vec![], table: if table { vec![0; RECENT] } else { vec![] } }
    }

    /// Room for the lines of a file of `n` lines at most, so that the ring is not grown.
    pub(crate) fn reserve(&mut self, n: usize) {
        self.ring.reserve_exact(n.min(RECENT));
    }

    #[inline]
    pub(crate) fn get(&self, line: u64) -> Option<&Entry> {
        let e = self.ring.get(line as usize % RECENT)?;
        (e.tag == line + 1).then_some(e)
    }

    #[inline]
    pub(crate) fn put(&mut self, line: u64, mut e: Entry) {
        e.tag = line + 1;
        let i = line as usize % RECENT;
        if i >= self.ring.len() {
            self.ring.resize(i + 1, Entry { tag: 0, v: [0; 3], s: [0; 3] });
        }
        self.ring[i] = e;
    }

    /// Forgets everything (the encoder, after undoing a block: an entry of the undone block
    /// could carry the number of a later line that is never recorded).
    pub(crate) fn clear(&mut self) {
        self.ring.clear();
        self.table.fill(0);
    }

    /// The distance back from `line` to the last line equal to `e` (0 if none), and
    /// records `line`.
    #[inline]
    pub(crate) fn find_put(&mut self, line: u64, e: Entry) -> u64 {
        let h = e.hash();
        let c = self.table[h];
        let d = if c != 0
            && c <= line
            && line + 1 - c <= RECENT as u64
            && self.get(c - 1).is_some_and(|x| x.v == e.v && x.s == e.s)
        {
            line + 1 - c
        } else {
            0
        };
        self.table[h] = line + 1;
        self.put(line, e);
        d
    }
}

/// Predictors of v, vt, vn lines: 0 the previous numbers of each position; 1..WIN the line
/// p+1 back; WIN.. the parallelograms x[-1] + x[-k] - x[-k-1], k = p-WIN+1.
pub(crate) const WIN: usize = 16;
pub(crate) const NPRED: usize = 2 * WIN;

/// Predictor `p` (> 0) of position `j` at scale `sc` for line `line`, from the recent
/// lines; None if one it needs is not there.
#[inline]
pub(crate) fn window_pred(rec: &Recent, line: u64, p: usize, j: usize, sc: i32) -> Option<i64> {
    let e = |k: u64| rec.get(line.checked_sub(k)?);
    let at = |e: &Entry| fixed::rescale(e.v[j], e.s[j] as i32, sc);
    if p < WIN {
        Some(at(e(p as u64 + 1)?))
    } else {
        let k = (p - WIN + 1) as u64;
        Some(at(e(1)?).wrapping_add(at(e(k)?)).wrapping_sub(at(e(k + 1)?)))
    }
}
