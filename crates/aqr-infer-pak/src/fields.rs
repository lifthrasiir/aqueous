//! Fields of a table's records, and the candidates for them a bit profile gives.
//!
//! A field is a bit field of the 64-bit word at a byte of the record, either endianness:
//! `(word >> shift) & mask(bits)`, in bytes (no units: where members are aligned, their sizes
//! are stored as they are and rounded up, not the sizes rounded up; offsets scaled down tile a
//! smaller file as well, the same sizes scaled with them). Its byte may be outside the record (a
//! row of a table need not start where its records do), up to a record either way.

use crate::tables::Table;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Field {
    /// The word's first byte, from the row's start.
    pub pos: i64,
    pub be: bool,
    pub shift: u8,
    pub bits: u8,
}

#[inline]
pub fn mask(bits: u8) -> u64 {
    if bits >= 64 { !0 } else { (1u64 << bits) - 1 }
}

/// The 64-bit word at `p` (bytes outside `b` read as 0).
#[inline]
pub fn word(b: &[u8], p: i64, be: bool) -> u64 {
    let mut x = [0u8; 8];
    if p >= 0 && (p as usize) + 8 <= b.len() {
        x.copy_from_slice(&b[p as usize..p as usize + 8]);
    } else {
        for (i, v) in x.iter_mut().enumerate() {
            let q = p + i as i64;
            if q >= 0 && (q as usize) < b.len() {
                *v = b[q as usize];
            }
        }
    }
    if be { u64::from_be_bytes(x) } else { u64::from_le_bytes(x) }
}

impl Field {
    /// The raw word of a row, shifted (before the mask): its bits above the field tell whether
    /// the field is wider.
    #[inline]
    pub fn shifted(&self, b: &[u8], t: &Table, row: i64) -> u64 {
        word(b, t.start as i64 + row * t.k as i64 + self.pos, self.be) >> self.shift
    }

    #[inline]
    pub fn get(&self, b: &[u8], t: &Table, row: i64) -> u64 {
        self.shifted(b, t, row) & mask(self.bits)
    }

    pub fn values(&self, b: &[u8], t: &Table, rows: &[i64]) -> Vec<u64> {
        rows.iter().map(|&r| self.get(b, t, r)).collect()
    }

    /// The byte after the field's last.
    pub fn end(&self) -> i64 {
        if self.be {
            self.pos + 8 - self.shift as i64 / 8
        } else {
            self.pos + (self.shift as i64 + self.bits as i64 + 7) / 8
        }
    }

    pub fn with_bits(self, bits: u8) -> Field {
        Field { bits, ..self }
    }
}

impl std::fmt::Display for Field {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let e = if self.be { '>' } else { '<' };
        write!(f, "@{}{}[{}:+{}]", self.pos, e, self.shift, self.bits)
    }
}

/// The bit profile of a table: for each bit of the bytes from a record before a row to a
/// record after it, the share of rows where it is set.
pub struct Profile {
    k: usize,
    /// Bits where a little-endian field may start, from a record before the row.
    starts: Vec<usize>,
    /// Those after an (almost) constant bit: where the field below ends.
    rises: Vec<usize>,
}

/// Rows of a table sampled for its profile.
const PROFILE_ROWS: usize = 4096;

impl Profile {
    pub fn new(b: &[u8], t: &Table) -> Profile {
        let k = t.k;
        let n = t.rows();
        let w = 3 * k; // bytes [-k, 2k) of a row
        let mut ones = vec![0u32; 8 * w];
        let mut m = 0u32;
        let lo = 1usize;
        let hi = n.saturating_sub(2).max(lo);
        let step = ((hi - lo) / PROFILE_ROWS).max(1);
        let mut j = lo;
        while j < hi {
            let p = t.start as i64 + (j * k) as i64 - k as i64;
            if p >= 0 && p as usize + w <= b.len() {
                for (i, &byte) in b[p as usize..p as usize + w].iter().enumerate() {
                    let mut x = byte;
                    while x != 0 {
                        ones[8 * i + x.trailing_zeros() as usize] += 1;
                        x &= x - 1;
                    }
                }
                m += 1;
            }
            j += step;
        }
        let h: Vec<f64> = ones
            .iter()
            .map(|&c| {
                let p = c as f64 / m.max(1) as f64;
                let e = |q: f64| if q > 0.0 { -q * q.log2() } else { 0.0 };
                e(p) + e(1.0 - p)
            })
            .collect();
        // a field's lowest bit: informative after an (almost) constant bit (a rising start:
        // the bits below are the top of another field), or at a byte. "Almost" is loose: a
        // table found may hold a fifth of its neighbour's rows (whose bits there are random:
        // an entropy of 0.44 at 9% of the rows)
        let rising = |i: usize| h[i] > 0.6 && (i == 0 || h[i - 1] < 0.5);
        // a start inside a byte also at the byte (values in multiples of 2^j: sizes of 16)
        let mut starts: Vec<usize> =
            (0..h.len()).filter(|&i| rising(i) || h[i] > 0.6 && i % 8 == 0).flat_map(|i| [i, i & !7]).collect();
        starts.sort_unstable();
        starts.dedup();
        let rises = (0..h.len()).filter(|&i| rising(i)).collect();
        Profile { k, starts, rises }
    }

    /// The candidate fields for a target of length `l`: from each start, to the next rising
    /// start (the field's own end), to the widths a value below `l` takes (with flags above
    /// masked off) and, at a byte, to 16, 32 and 64 bits.
    pub fn candidates(&self, l: u64) -> Vec<Field> {
        let k = self.k as i64;
        let bl = (64 - l.leading_zeros()).max(1) as i64;
        let mut out = Vec::new();
        for &s in &self.starts {
            let pos = (s / 8) as i64 - k;
            let shift = (s % 8) as u8;
            let max = 64 - shift as i64;
            let gap = self.rises.iter().find(|&&t| t > s).map(|&t| (t - s) as i64).unwrap_or(max).min(max);
            let mut ws: Vec<i64> = (bl - 12..=bl).filter(|&w| w >= 1 && w <= max).collect();
            ws.push(gap);
            if shift == 0 {
                ws.extend([16, 32, 64]);
            }
            ws.sort_unstable();
            ws.dedup();
            for w in ws {
                out.push(Field { pos, be: false, shift, bits: w as u8 });
            }
        }
        // and the whole fields of 2, 4 and 8 bytes at every byte, either endianness (a field
        // mostly of one value has no start in the profile: Precache's entries are mostly 280
        // bytes long)
        for pos in -k..2 * k {
            for w in [2u8, 4, 8] {
                out.push(Field { pos, be: true, shift: 64 - 8 * w, bits: 8 * w });
                out.push(Field { pos, be: false, shift: 0, bits: 8 * w });
            }
        }
        out.sort_by_key(|f| (f.pos, f.be, f.shift, f.bits));
        out.dedup();
        out
    }
}
