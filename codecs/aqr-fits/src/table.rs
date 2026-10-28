//! Binary tables' layout (the FITS Standard 4.0, §7.3): the fields of a row, from the `TFORMn`
//! keywords.

use crate::fits::Header;

/// A field of a row of a binary table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    /// Its offset in the row and its bytes.
    pub at: usize,
    pub len: usize,
    /// The data type's letter (`L`, `X`, `B`, `I`, `J`, `K`, `A`, `E`, `D`, `C`, `M`, `P`, `Q`).
    pub ty: u8,
    /// The repeat count.
    pub repeat: usize,
    /// `TTYPEn`, if any.
    pub name: Option<String>,
}

impl Field {
    /// Bytes of an element: of the type, of a descriptor for `P` (8) and `Q` (16); 0 for bits.
    pub fn width(&self) -> usize {
        width(self.ty).unwrap_or(0)
    }
}

/// Bytes of an element of type `ty`, None for bits (`X`) and types not known.
fn width(ty: u8) -> Option<usize> {
    Some(match ty {
        b'L' | b'B' | b'A' => 1,
        b'I' => 2,
        b'J' | b'E' => 4,
        b'K' | b'D' | b'C' | b'P' => 8,
        b'M' | b'Q' => 16,
        _ => return None,
    })
}

/// The fields of the rows of binary table header `h`, if they fit `NAXIS1` exactly.
pub fn fields(h: &Header) -> Option<Vec<Field>> {
    let n = usize::try_from(h.int("TFIELDS")?).ok()?;
    let row = usize::try_from(h.int("NAXIS1")?).ok()?;
    if n > 999 {
        return None;
    }
    let mut at = 0;
    let mut out = Vec::with_capacity(n);
    for i in 1..=n {
        let form = h.str(&format!("TFORM{i}"))?;
        let form = form.trim().as_bytes();
        let digits = form.iter().position(|b| !b.is_ascii_digit())?;
        let repeat = if digits == 0 { 1 } else { std::str::from_utf8(&form[..digits]).ok()?.parse().ok()? };
        let ty = form[digits].to_ascii_uppercase();
        let len = match ty {
            b'X' => usize::div_ceil(repeat, 8),
            _ => width(ty)?.checked_mul(repeat)?,
        };
        let name = h.str(&format!("TTYPE{i}"));
        out.push(Field { at, len, ty, repeat, name });
        at = at.checked_add(len)?;
    }
    (at == row).then_some(out)
}

/// How a field of a row is stored apart from the rows: its bytes in planes, one per byte of
/// its elements, each perhaps as the differences from the row above.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Column {
    /// Bytes of the field.
    pub len: usize,
    /// Bytes of an element (1, 2, 4 or 8), dividing `len`.
    pub width: usize,
    /// Whether the elements are stored as their differences from the row above's, as
    /// big-endian numbers wrapping in their bytes.
    pub delta: bool,
}

impl Column {
    fn valid(&self) -> bool {
        matches!(self.width, 1 | 2 | 4 | 8) && self.len.is_multiple_of(self.width)
    }
}

/// Bytes of an element of type `ty` as [`Column`] takes it: of a number (each of the two of
/// a complex one), of a descriptor's numbers, else 1.
fn element(ty: u8) -> usize {
    match ty {
        b'I' => 2,
        b'J' | b'E' | b'C' | b'P' => 4,
        b'K' | b'D' | b'M' | b'Q' => 8,
        _ => 1,
    }
}

/// The element at `at` of `width` bytes, big-endian.
#[inline(always)]
fn get(d: &[u8], at: usize, width: usize) -> u64 {
    d[at..at + width].iter().fold(0, |a, &b| a << 8 | b as u64)
}

#[inline(always)]
fn put(d: &mut [u8], at: usize, width: usize, v: u64) {
    for i in 0..width {
        d[at + i] = (v >> (8 * (width - 1 - i))) as u8;
    }
}

/// The planes of `rows` (of `row` bytes each) laid out by `cols`: for each column, each byte
/// of its elements (the most significant first), each element, each row.
pub fn split(rows: &[u8], row: usize, cols: &[Column]) -> Vec<u8> {
    let n = rows.len() / row;
    let mut out = vec![0u8; rows.len()];
    let mut o = 0;
    let mut at = 0;
    for c in cols {
        let elems = c.len / c.width;
        for e in 0..elems {
            let field = at + e * c.width;
            let mut last = 0u64;
            for r in 0..n {
                let v = get(rows, r * row + field, c.width);
                let s = if c.delta { v.wrapping_sub(last) } else { v };
                last = v;
                for b in 0..c.width {
                    out[o + (b * elems) * n + r] = (s >> (8 * (c.width - 1 - b))) as u8;
                }
            }
            o += n;
        }
        o += (c.width - 1) * elems * n;
        at += c.len;
    }
    out
}

/// The rows of [`split`]'s `planes`, `n` rows of `row` bytes. None where `cols` do not fit.
pub fn join(planes: &[u8], row: usize, n: usize, cols: &[Column]) -> Option<Vec<u8>> {
    if !cols.iter().all(Column::valid) || cols.iter().map(|c| c.len).sum::<usize>() != row {
        return None;
    }
    if row.checked_mul(n)? != planes.len() {
        return None;
    }
    let mut rows = vec![0u8; planes.len()];
    let (mut o, mut at) = (0, 0);
    for c in cols {
        let elems = c.len / c.width;
        for e in 0..elems {
            let field = at + e * c.width;
            let mut last = 0u64;
            for r in 0..n {
                let mut s = 0u64;
                for b in 0..c.width {
                    s = s << 8 | planes[o + (b * elems) * n + r] as u64;
                }
                let v = if c.delta { s.wrapping_add(last) } else { s };
                let v = if c.width < 8 { v & ((1 << (8 * c.width)) - 1) } else { v };
                last = v;
                put(&mut rows, r * row + field, c.width, v);
            }
            o += n;
        }
        o += (c.width - 1) * elems * n;
        at += c.len;
    }
    Some(rows)
}

/// Order-0 entropy, in bits, of the bytes counted.
fn entropy(counts: &[u32; 256]) -> f64 {
    let n: u32 = counts.iter().sum();
    counts.iter().filter(|&&c| c > 0).map(|&c| c as f64 * (n as f64 / c as f64).log2()).sum()
}

/// The columns of `fields`, each as differences where its planes have the less order-0
/// entropy so.
pub fn columns(rows: &[u8], row: usize, fields: &[Field]) -> Vec<Column> {
    let n = rows.len() / row;
    fields
        .iter()
        .map(|f| {
            let width = if f.len % element(f.ty) == 0 { element(f.ty) } else { 1 };
            let elems = f.len / width;
            let cost = |delta: bool| {
                let mut bits = 0.0;
                let mut counts = vec![[0u32; 256]; width];
                for e in 0..elems {
                    let mut last = 0u64;
                    counts.iter_mut().for_each(|c| *c = [0; 256]);
                    for r in 0..n {
                        let v = get(rows, r * row + f.at + e * width, width);
                        let s = if delta { v.wrapping_sub(last) } else { v };
                        last = v;
                        for (b, c) in counts.iter_mut().enumerate() {
                            c[(s >> (8 * (width - 1 - b))) as u8 as usize] += 1;
                        }
                    }
                    bits += counts.iter().map(entropy).sum::<f64>();
                }
                bits
            };
            let delta = width > 1 && n > 1 && cost(true) < cost(false);
            Column { len: f.len, width, delta }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planes() {
        let cols = [
            Column { len: 3, width: 1, delta: false },
            Column { len: 8, width: 4, delta: true },
            Column { len: 8, width: 8, delta: true },
            Column { len: 4, width: 2, delta: false },
        ];
        let row = 23;
        let rows: Vec<u8> = (0..row * 17).map(|i| (i * 7919 % 251) as u8).collect();
        let p = split(&rows, row, &cols);
        assert_eq!(join(&p, row, 17, &cols).unwrap(), rows);
    }
}
