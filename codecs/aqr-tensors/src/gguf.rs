//! GGUF files as far as the encoder reads them (FORMAT.md §2): the header, its metadata skipped
//! but for the alignment, and the tensors' types and places in the data.

use crate::layout::{Float, Quant};
use crate::tensors::{self, Container, Data, Tensors};

struct Cur<'a> {
    d: &'a [u8],
    at: usize,
}

impl Cur<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let s = self.d.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(s)
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn string(&mut self) -> Option<&[u8]> {
        let n = self.u64()?;
        self.take(usize::try_from(n).ok()?)
    }

    /// A value of type `t`: its bytes skipped, the number of a scalar told.
    fn value(&mut self, t: u32, depth: u32) -> Option<Option<u64>> {
        let n = match t {
            0 | 1 | 7 => 1,
            2 | 3 => 2,
            4..=6 => 4,
            10..=12 => 8,
            8 => {
                self.string()?;
                return Some(None);
            }
            9 if depth == 0 => {
                let et = self.u32()?;
                let count = self.u64()?;
                let fixed = match et {
                    0 | 1 | 7 => Some(1),
                    2 | 3 => Some(2),
                    4..=6 => Some(4),
                    10..=12 => Some(8),
                    _ => None,
                };
                match fixed {
                    Some(w) => _ = self.take(usize::try_from(count.checked_mul(w)?).ok()?)?,
                    None => {
                        // (each at least 8 bytes: a count past the file is not one)
                        if count > (self.d.len() - self.at) as u64 / 8 {
                            return None;
                        }
                        for _ in 0..count {
                            self.value(et, depth + 1)?;
                        }
                    }
                }
                return Some(None);
            }
            _ => return None,
        };
        let b = self.take(n)?;
        let mut v = [0u8; 8];
        v[..n].copy_from_slice(b);
        Some(Some(u64::from_le_bytes(v)))
    }
}

/// Whether `d` starts as a GGUF file of version 2 or 3.
pub fn starts(d: &[u8]) -> bool {
    d.len() >= 24 && &d[..4] == b"GGUF" && matches!(u32::from_le_bytes(d[4..8].try_into().unwrap()), 2 | 3)
}

/// What a tensor of ggml's type number `t` of `n` elements is, where it is not of blocks.
fn data_of(t: u32, n: u64) -> Option<Data> {
    Some(match t {
        0 => Data::Float(Float::F32, n),
        1 => Data::Float(Float::F16, n),
        30 => Data::Float(Float::Bf16, n),
        28 => Data::Float(Float::F64, n),
        24 => Data::Lanes(1, n),
        25 => Data::Lanes(2, n),
        26 => Data::Lanes(4, n),
        27 => Data::Lanes(8, n),
        _ => return None,
    })
}

/// The layout of GGUF file `d`, None where it is not one (or its header does not hold).
pub fn parse(d: &[u8]) -> Option<Tensors> {
    if !starts(d) {
        return None;
    }
    let mut c = Cur { d, at: 8 };
    let tensors = c.u64()?;
    let kvs = c.u64()?;
    let mut align = 32u64;
    for _ in 0..kvs {
        let align_key = c.string()? == b"general.alignment";
        let t = c.u32()?;
        let v = c.value(t, 0)?;
        if align_key {
            align = v.filter(|&a| a > 0 && t == 4)?;
        }
    }
    // (a tensor's info is at least 24 bytes)
    if tensors > (d.len() - c.at) as u64 / 24 {
        return None;
    }
    let mut info = Vec::with_capacity(tensors as usize);
    for _ in 0..tensors {
        c.string()?;
        let dims = c.u32()?;
        if dims > 8 {
            return None;
        }
        // (the first dimension the row's)
        let (mut n, mut row) = (1u64, 0);
        for k in 0..dims {
            let ne = c.u64()?;
            if k == 0 {
                row = ne;
            }
            n = n.checked_mul(ne)?;
        }
        let t = c.u32()?;
        let off = c.u64()?;
        info.push((off, t, n, row));
    }
    let data = c.at.div_ceil(align as usize).checked_mul(align as usize)?;
    if data > d.len() {
        return None;
    }
    let mut coded = Vec::new();
    for (off, t, n, row) in info {
        let (what, row) = if let Some(what) = data_of(t, n) {
            (what, row)
        } else if let Some(q) = Quant::of_ggml(t) {
            if n % q.weights() as u64 != 0 {
                continue;
            }
            (Data::Quant(q, n / q.weights() as u64), 0)
        } else {
            continue;
        };
        coded.push(((data as u64).checked_add(off)?, what, row));
    }
    Some(tensors::apart(Container::Gguf, d, data, coded))
}
