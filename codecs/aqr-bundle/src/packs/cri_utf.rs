//! CRI Middleware's `@UTF` tables, the container of ACB cue sheets, CPK archives' directories
//! and many other CRI files; big endian throughout. This is the shared reader the other `cri-*`
//! formats use; it is not a format of its own.
//!
//! A table is `@UTF`, its size (u32, of what follows it), then at offset 8 (all the offsets
//! below are relative to it): a version (u16), the offset of the rows (u16), of the string
//! pool (u32), of the data pool (u32), the table's name (u32, in the string pool), the number
//! of columns (u16), the width of a row (u16) and the number of rows (u32); the schema follows
//! (from 0x18 of that base), a column each: a flag byte (the low nibble the type: 0 u8, 1 s8,
//! 2 u16, 3 s16, 4 u32, 5 s32, 6 u64, 7 s64, 8 f32, 9 f64, 0xA string, 0xB data; the high
//! nibble the storage: 1 zero, 3 constant, 5 per row), the column's name (u32, in the string
//! pool) and, for a constant, its value. The per-row values follow in the columns' order in
//! each row. A string is an offset into the pool, data an offset and a size into the data pool
//! (a nested table, an embedded AWB, ...).
//!
//! A table may be XOR-encrypted by a stream mask (`m = 0x655F`, byte `i` is `^ (m & 0xFF)`,
//! then `m *= 0x4115`); it is taken as encrypted where its first four bytes are not `@UTF` but
//! unmasked are, and then read from a decrypted copy ([`Table::encrypted`]). The ranges it
//! gives are of the bytes given either way, since the mask keeps every offset.

use std::borrow::Cow;
use std::ops::Range;

use super::{u32be, within};

const MAGIC: &[u8; 4] = b"@UTF";

/// A value of a table's cell.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// A column of zero storage (no value stored).
    Null,
    /// An integer of any width, as 64 bits (signed ones sign-extended).
    Num(u64),
    Float(f64),
    /// A string: its bytes' range in the table.
    Str(Range<usize>),
    /// Data: its range in the table.
    Data(Range<usize>),
}

pub struct Table<'a> {
    /// The table's bytes (decrypted where they were masked).
    pub bytes: Cow<'a, [u8]>,
    pub encrypted: bool,
    pub name: Range<usize>,
    pub columns: Vec<Range<usize>>,
    pub rows: Vec<Vec<Value>>,
}

impl Table<'_> {
    pub fn column(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| &self.bytes[c.clone()] == name.as_bytes())
    }

    /// The cell of `row` in column `name`.
    pub fn get(&self, row: usize, name: &str) -> Option<&Value> {
        self.rows.get(row)?.get(self.column(name)?)
    }

    pub fn num(&self, row: usize, name: &str) -> Option<u64> {
        match self.get(row, name)? {
            Value::Num(n) => Some(*n),
            _ => None,
        }
    }

    pub fn str(&self, row: usize, name: &str) -> Option<&[u8]> {
        match self.get(row, name)? {
            Value::Str(r) => Some(&self.bytes[r.clone()]),
            _ => None,
        }
    }

    pub fn data(&self, row: usize, name: &str) -> Option<Range<usize>> {
        match self.get(row, name)? {
            Value::Data(r) => Some(r.clone()),
            _ => None,
        }
    }
}

fn u16be(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(d.get(at..at.checked_add(2)?)?.try_into().ok()?))
}

/// Whether `d` starts a table, plain or masked.
pub fn starts(d: &[u8]) -> bool {
    d.starts_with(MAGIC) || masked(d)
}

fn mask(d: &mut [u8]) {
    let mut m = 0x655fu32;
    for b in d {
        *b ^= m as u8;
        m = m.wrapping_mul(0x4115);
    }
}

fn masked(d: &[u8]) -> bool {
    let Some(head) = d.get(..4) else { return false };
    let mut h = [head[0], head[1], head[2], head[3]];
    mask(&mut h);
    &h == MAGIC
}

struct Col {
    ty: u8,
    storage: u8,
    value: Value,
}

/// The table at the start of `d` (its size from its header), or None where it is none or does
/// not read: offsets out of the table, rows beyond it.
pub fn parse(d: &[u8]) -> Option<Table<'_>> {
    let encrypted = !d.starts_with(MAGIC);
    if encrypted && !masked(d) {
        return None;
    }
    let mut head = d.get(..8)?.to_vec();
    if encrypted {
        mask(&mut head);
    }
    let size = u32be(&head, 4)? as u64;
    let range = within(d, 0, 8 + size)?;
    let bytes = if encrypted {
        let mut v = d[range].to_vec();
        mask(&mut v);
        Cow::Owned(v)
    } else {
        Cow::Borrowed(&d[range])
    };
    let t = &bytes[..];
    let base = 8usize;
    let rows_at = base + u16be(t, 10)? as usize;
    let strings = base + u32be(t, 12)? as usize;
    let data = base + u32be(t, 16)? as usize;
    let name_at = u32be(t, 20)? as usize;
    let ncols = u16be(t, 24)? as usize;
    let width = u16be(t, 26)? as usize;
    let nrows = u32be(t, 28)? as usize;
    if !(0x20 <= rows_at && rows_at <= strings && strings <= data && data <= t.len()) {
        return None;
    }
    // A string of the pool, which ends where the data's starts.
    let pool = |off: usize| -> Option<Range<usize>> {
        let start = strings.checked_add(off)?;
        let len = t.get(start..data)?.iter().position(|&b| b == 0)?;
        Some(start..start + len)
    };
    let name = pool(name_at)?;
    let rows_end = rows_at.checked_add(nrows.checked_mul(width)?)?;
    if rows_end > strings || nrows.checked_mul(ncols)? > 1 << 23 || (width == 0 && nrows > 1 << 16) {
        return None;
    }
    let mut at = base + 0x18;
    let mut columns = Vec::with_capacity(ncols);
    let mut cols = Vec::with_capacity(ncols);
    for _ in 0..ncols {
        let flag = *t.get(at)?;
        let cname = pool(u32be(t, at + 1)? as usize)?;
        at += 5;
        let (ty, storage) = (flag & 15, flag >> 4);
        let mut value = Value::Null;
        match storage {
            1 | 5 => {}
            3 => {
                let (v, len) = read(t, at, ty, strings, data)?;
                value = v;
                at += len;
            }
            _ => return None,
        }
        columns.push(cname);
        cols.push(Col { ty, storage, value });
    }
    if at > rows_at {
        return None;
    }
    let mut rows = Vec::with_capacity(nrows);
    for r in 0..nrows {
        let mut p = rows_at + r * width;
        let end = p + width;
        let mut row = Vec::with_capacity(ncols);
        for c in &cols {
            if c.storage == 5 {
                let (v, len) = read(t, p, c.ty, strings, data)?;
                p += len;
                if p > end {
                    return None;
                }
                row.push(v);
            } else {
                row.push(c.value.clone());
            }
        }
        rows.push(row);
    }
    Some(Table { bytes, encrypted, name, columns, rows })
}

/// The value of type `ty` at `at`, and its width there.
fn read(t: &[u8], at: usize, ty: u8, strings: usize, data: usize) -> Option<(Value, usize)> {
    let b = |n: usize| t.get(at..at.checked_add(n)?);
    Some(match ty {
        0 => (Value::Num(b(1)?[0] as u64), 1),
        1 => (Value::Num(b(1)?[0] as i8 as i64 as u64), 1),
        2 => (Value::Num(u16be(t, at)? as u64), 2),
        3 => (Value::Num(u16be(t, at)? as i16 as i64 as u64), 2),
        4 => (Value::Num(u32be(t, at)? as u64), 4),
        5 => (Value::Num(u32be(t, at)? as i32 as i64 as u64), 4),
        6 | 7 => (Value::Num(u64::from_be_bytes(b(8)?.try_into().ok()?)), 8),
        8 => (Value::Float(f32::from_bits(u32be(t, at)?) as f64), 4),
        9 => (Value::Float(f64::from_bits(u64::from_be_bytes(b(8)?.try_into().ok()?))), 8),
        0xa => {
            let start = strings.checked_add(u32be(t, at)? as usize)?;
            let len = t.get(start..data)?.iter().position(|&b| b == 0)?;
            (Value::Str(start..start + len), 4)
        }
        0xb => {
            let start = data.checked_add(u32be(t, at)? as usize)?;
            let end = start.checked_add(u32be(t, at + 4)? as usize)?;
            if end > t.len() {
                return None;
            }
            (Value::Data(start..end), 8)
        }
        _ => return None,
    })
}
