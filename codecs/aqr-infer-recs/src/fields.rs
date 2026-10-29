//! Records split into fields: each field a few bytes of the record read as an integer, coded
//! as it is, against the record before or against an earlier field of the record, and written
//! as byte planes (least significant first), one field after another.

use std::collections::HashMap;

use crate::{kt, zcost};

/// How a field's values are coded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum How {
    Raw,
    /// Less the field of the record before (the first record less 0).
    Delta,
    /// Xor the field of the record before.
    Xor,
    /// Less the field of the same width at this offset of the record.
    Sub(usize),
    /// Xor the field of the same width at this offset of the record.
    XorRef(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Field {
    pub off: usize,
    pub width: usize,
    pub big: bool,
    pub how: How,
}

/// What a field's planes cost while the fields are chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cost {
    /// Adaptive order-0 models, a plane each.
    Kt,
    /// zstd -3 of the planes one after another.
    Zstd,
}

/// Records up to this size also try fields against earlier fields of their width.
pub const REF_K: usize = 128;
/// How far back in the record those fields are.
const REF_BACK: usize = 32;
/// The description of a field, which breaks ties towards fewer, wider fields.
const FIELD_BITS: f64 = 16.0;
/// Fields are chosen on 4 runs of this many records spread over the records, or fewer of big
/// records: FIELD_BYTES in all.
pub const FIELD_ROWS: usize = 256;
const FIELD_BYTES: usize = 1 << 16;

/// The field (`off`, `width`, `big`) of each record of m (rows of k bytes).
fn values(m: &[u8], k: usize, off: usize, width: usize, big: bool) -> Vec<u64> {
    m.chunks_exact(k)
        .map(|r| {
            let b = &r[off..off + width];
            if big {
                b.iter().fold(0, |v, &x| v << 8 | x as u64)
            } else {
                b.iter().rev().fold(0, |v, &x| v << 8 | x as u64)
            }
        })
        .collect()
}

fn mask(width: usize) -> u64 {
    if width == 8 { !0 } else { (1 << (8 * width)) - 1 }
}

/// The values v of field f coded.
fn code(v: &[u64], m: &[u8], k: usize, f: &Field) -> Vec<u64> {
    let mk = mask(f.width);
    match f.how {
        How::Raw => v.to_vec(),
        How::Delta | How::Xor => {
            let mut prev = 0u64;
            v.iter()
                .map(|&x| {
                    let t = if f.how == How::Delta { x.wrapping_sub(prev) & mk } else { x ^ prev };
                    prev = x;
                    t
                })
                .collect()
        }
        How::Sub(r) | How::XorRef(r) => {
            let refs = values(m, k, r, f.width, f.big);
            v.iter()
                .zip(refs)
                .map(|(&x, y)| if matches!(f.how, How::Sub(_)) { x.wrapping_sub(y) & mk } else { x ^ y })
                .collect()
        }
    }
}

/// Values as byte planes, least significant first.
fn planes(t: &[u64], width: usize, out: &mut Vec<u8>) {
    for i in 0..width {
        out.extend(t.iter().map(|&x| (x >> (8 * i)) as u8));
    }
}

/// The records of m (rows of k bytes) as the planes of their fields.
pub fn encode(m: &[u8], k: usize, fields: &[Field]) -> Vec<u8> {
    let mut out = Vec::with_capacity(m.len());
    for f in fields {
        planes(&code(&values(m, k, f.off, f.width, f.big), m, k, f), f.width, &mut out);
    }
    out
}

/// The n records of k bytes back from the planes of their fields (None if too short).
pub fn decode(planes: &[u8], n: usize, k: usize, fields: &[Field]) -> Option<Vec<u8>> {
    let mut m = vec![0u8; n * k];
    let mut at = 0;
    for f in fields {
        let p = planes.get(at..at + n * f.width)?;
        at += n * f.width;
        let mk = mask(f.width);
        let refs = match f.how {
            How::Sub(r) | How::XorRef(r) => values(&m, k, r, f.width, f.big),
            _ => Vec::new(),
        };
        let mut prev = 0u64;
        for i in 0..n {
            let t = (0..f.width).fold(0u64, |t, j| t | (p[j * n + i] as u64) << (8 * j));
            let v = match f.how {
                How::Raw => t,
                How::Delta => t.wrapping_add(prev) & mk,
                How::Xor => t ^ prev,
                How::Sub(_) => t.wrapping_add(refs[i]) & mk,
                How::XorRef(_) => t ^ refs[i],
            };
            prev = v;
            let r = &mut m[i * k + f.off..i * k + f.off + f.width];
            for (j, b) in r.iter_mut().enumerate() {
                let sh = if f.big { 8 * (f.width - 1 - j) } else { 8 * j };
                *b = (v >> sh) as u8;
            }
        }
    }
    Some(m)
}

/// FIELD_ROWS records from each of 4 places over m (all of m if that is not fewer).
pub fn sample(m: &[u8], k: usize) -> std::borrow::Cow<'_, [u8]> {
    let n = m.len() / k;
    // fewer of big records: about FIELD_BYTES in all
    let rows = (FIELD_BYTES / k / 4).clamp(16, FIELD_ROWS);
    if n <= 4 * rows {
        return m.into();
    }
    let mut out = Vec::with_capacity(4 * rows * k);
    for i in 0..4 {
        let a = (n - rows) * i / 3;
        out.extend_from_slice(&m[a * k..(a + rows) * k]);
    }
    out.into()
}

/// Fields of the records m (rows of k bytes) by DP over the code length of their planes,
/// measured on sample(m): the fields, their bits and the bits of the bytes as columns.
pub fn split(m: &[u8], k: usize, cost: Cost, refs: bool) -> (Vec<Field>, f64, f64) {
    let m = sample(m, k);
    let m = &m[..];
    let n = m.len() / k;
    let mut scratch = Scratch { counts: [0; 256], coded: Vec::with_capacity(n), buf: Vec::new() };
    // the values of the fields of each width and endianness at each offset, kept where fields
    // refer to them
    let refs = refs && k <= REF_K;
    let mut cache: HashMap<(usize, usize, bool), Vec<u64>> = HashMap::new();
    let mut values_of = |off: usize, width: usize, big: bool| -> Vec<u64> {
        if refs {
            cache.entry((off, width, big)).or_insert_with(|| values(m, k, off, width, big)).clone()
        } else {
            values(m, k, off, width, big)
        }
    };
    let raw: Vec<f64> = (0..k)
        .map(|off| {
            let v = values_of(off, 1, false);
            scratch.cost(&v, None, 1, How::Raw, cost)
        })
        .collect();
    let mut best = vec![f64::INFINITY; k + 1];
    let mut back: Vec<Option<Field>> = vec![None; k + 1];
    best[0] = 0.0;
    let mut cands = Vec::new();
    for o in 0..k {
        if best[o] == f64::INFINITY {
            continue;
        }
        cands.clear();
        cands.push((Field { off: o, width: 1, big: false, how: How::Raw }, raw[o] + FIELD_BITS));
        for w in [1, 2, 4, 8] {
            if o + w > k {
                break;
            }
            for big in if w == 1 { [false].as_slice() } else { [false, true].as_slice() } {
                let v = values_of(o, w, *big);
                for how in [How::Delta, How::Xor] {
                    cands.push((
                        Field { off: o, width: w, big: *big, how },
                        scratch.cost(&v, None, w, how, cost) + FIELD_BITS,
                    ));
                }
                if refs && w > 1 && o >= w {
                    // fields of a width lie a multiple of it apart, whatever the records' phase
                    for r in (o.saturating_sub(REF_BACK)..=o - w).filter(|r| (o - r) % w == 0) {
                        let rv = values_of(r, w, *big);
                        for how in [How::Sub(r), How::XorRef(r)] {
                            let c = scratch.cost(&v, Some(&rv), w, how, cost) + FIELD_BITS;
                            cands.push((Field { off: o, width: w, big: *big, how }, c));
                        }
                    }
                }
            }
        }
        for &(f, c) in &cands {
            let end = o + f.width;
            if best[o] + c < best[end] {
                best[end] = best[o] + c;
                back[end] = Some(f);
            }
        }
    }
    let mut fields = Vec::new();
    let mut o = k;
    while o > 0 {
        let f = back[o].unwrap();
        fields.push(f);
        o = f.off;
    }
    fields.reverse();
    let bits = best[k] - FIELD_BITS * fields.len() as f64;
    (fields, bits, raw.iter().sum())
}

/// What the costs of the fields are counted in, kept from field to field.
struct Scratch {
    counts: [u32; 256],
    coded: Vec<u64>,
    buf: Vec<u8>,
}

impl Scratch {
    /// The cost of values v of a width coded (against refs for the fields that refer).
    fn cost(&mut self, v: &[u64], refs: Option<&[u64]>, width: usize, how: How, cost: Cost) -> f64 {
        let mk = mask(width);
        self.coded.clear();
        match how {
            How::Raw => self.coded.extend_from_slice(v),
            How::Delta | How::Xor => {
                let mut prev = 0u64;
                self.coded.extend(v.iter().map(|&x| {
                    let t = if how == How::Delta { x.wrapping_sub(prev) & mk } else { x ^ prev };
                    prev = x;
                    t
                }));
            }
            How::Sub(_) => self.coded.extend(v.iter().zip(refs.unwrap()).map(|(&x, &y)| x.wrapping_sub(y) & mk)),
            How::XorRef(_) => self.coded.extend(v.iter().zip(refs.unwrap()).map(|(&x, &y)| x ^ y)),
        }
        match cost {
            Cost::Kt => {
                let mut bits = 0.0;
                for i in 0..width {
                    self.counts.fill(0);
                    for &x in &self.coded {
                        self.counts[(x >> (8 * i)) as u8 as usize] += 1;
                    }
                    bits += kt::bits(&self.counts);
                }
                bits
            }
            Cost::Zstd => {
                self.buf.clear();
                planes(&self.coded, width, &mut self.buf);
                zcost::plane_bits(&self.buf)
            }
        }
    }
}

/// The fields in a line: `delta32@0 raw@4-7 sub@8:16@12 …`.
pub fn describe(fields: &[Field]) -> String {
    let mut out: Vec<(String, usize, usize)> = Vec::new();
    for f in fields {
        if f.how == How::Raw {
            if let Some(last) = out.last_mut().filter(|l| l.0 == "raw" && l.2 == f.off) {
                last.2 += 1;
                continue;
            }
            out.push(("raw".into(), f.off, f.off + 1));
            continue;
        }
        let name = match f.how {
            How::Delta => "delta".into(),
            How::Xor => "xor".into(),
            How::Sub(r) => format!("sub@{r}:"),
            How::XorRef(r) => format!("xor@{r}:"),
            How::Raw => unreachable!(),
        };
        out.push((
            format!("{name}{}{}", if f.big && f.width > 1 { "be" } else { "" }, 8 * f.width),
            f.off,
            f.off + f.width,
        ));
    }
    out.iter()
        .map(|(t, a, b)| if t == "raw" && b - a > 1 { format!("raw@{a}-{}", b - 1) } else { format!("{t}@{a}") })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_invert() {
        // u32 counting up, u64 rising, u16 equal to the first two bytes, a byte
        let n = 3000;
        let mut m = Vec::new();
        for i in 0..n as u64 {
            m.extend((1000 + i as u32).to_le_bytes());
            m.extend((1_700_000_000_000 + i * 997 + i % 13).to_be_bytes());
            m.extend(((1000 + i) as u16).to_le_bytes());
            m.push((i * 7 % 5) as u8);
        }
        let k = 15;
        for cost in [Cost::Kt, Cost::Zstd] {
            let (fields, bits, raw) = split(&m, k, cost, true);
            assert!(bits < raw / 4.0, "{cost:?}: {}", describe(&fields));
            let p = encode(&m, k, &fields);
            assert_eq!(p.len(), m.len());
            assert_eq!(decode(&p, n, k, &fields).unwrap(), m);
        }
    }
}
