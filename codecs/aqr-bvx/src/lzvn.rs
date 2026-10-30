//! LZVN streams (`FORMAT.md` §4): instructions of a byte or more, each literals, a match, or
//! literals then a match.

use crate::{Error, Result, corrupt};

/// An instruction.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Op {
    /// `lits` literals (0 to 3) then a match of `len` (3 to 10) at a distance: of 11 bits
    /// (`sml_d`), 16 bits (`lrg_d`), or the one before (`pre_d`).
    Small {
        lits: u8,
        len: u8,
        dist: u16,
    },
    Large {
        lits: u8,
        len: u8,
        dist: u16,
    },
    Prev {
        lits: u8,
        len: u8,
    },
    /// `lits` literals (0 to 3) then a match of `len` (3 to 34) at a distance of 14 bits.
    Medium {
        lits: u8,
        len: u8,
        dist: u16,
    },
    /// Literals alone: 1 to 15 (`sml_l`), or 16 to 271 (`lrg_l`).
    Lits(u16),
    /// A match at the distance before: 1 to 15 (`sml_m`), or 16 to 271 (`lrg_m`).
    Match(u16),
    Nop(u8),
}

/// What the stream decoded: its instructions and its bytes, up to and with the end.
pub struct Parse {
    pub ops: Vec<Op>,
    pub used: usize,
}

/// Decodes the LZVN stream `s` onto `out`, which it must grow by exactly `n` bytes.
pub fn decode_onto(s: &[u8], out: &mut Vec<u8>, n: usize) -> Result<()> {
    parse_onto(s, out, n, false).map(|_| ())
}

/// [`decode_onto`], and the stream's instructions if `keep`.
pub fn parse_onto(s: &[u8], out: &mut Vec<u8>, n: usize, keep: bool) -> Result<Parse> {
    let start = out.len();
    let mut ops = Vec::new();
    let mut p = 0;
    let mut dist = 0usize;
    let byte = |i: usize| s.get(i).copied().ok_or_else(corrupt);
    loop {
        let b = byte(p)?;
        let (op, size) = match b {
            0x06 => break,
            0x0e | 0x16 => (Op::Nop(b), 1),
            0x1e | 0x26 | 0x2e | 0x36 | 0x3e | 0x70..=0x7f | 0xd0..=0xdf => return Err(Error("undefined instruction")),
            0xe0 => (Op::Lits(byte(p + 1)? as u16 + 16), 2),
            0xe1..=0xef => (Op::Lits(b as u16 & 15), 1),
            0xf0 => (Op::Match(byte(p + 1)? as u16 + 16), 2),
            0xf1..=0xff => (Op::Match(b as u16 & 15), 1),
            0xa0..=0xbf => {
                let w = byte(p + 1)? as u16 | (byte(p + 2)? as u16) << 8;
                (Op::Medium { lits: b >> 3 & 3, len: ((b & 7) << 2 | (w & 3) as u8) + 3, dist: w >> 2 }, 3)
            }
            _ => {
                let (lits, len) = (b >> 6, (b >> 3 & 7) + 3);
                match b & 7 {
                    6 => (Op::Prev { lits, len }, 1),
                    7 => (Op::Large { lits, len, dist: byte(p + 1)? as u16 | (byte(p + 2)? as u16) << 8 }, 3),
                    _ => (Op::Small { lits, len, dist: ((b & 7) as u16) << 8 | byte(p + 1)? as u16 }, 2),
                }
            }
        };
        p += size;
        let (lits, len, d) = match op {
            Op::Small { lits, len, dist } | Op::Large { lits, len, dist } | Op::Medium { lits, len, dist } => {
                (lits as usize, len as usize, Some(dist as usize))
            }
            Op::Prev { lits, len } => (lits as usize, len as usize, None),
            Op::Lits(l) => (l as usize, 0, None),
            Op::Match(m) => (0, m as usize, None),
            Op::Nop(_) => (0, 0, None),
        };
        if out.len() - start + lits + len > n {
            return Err(corrupt());
        }
        out.extend_from_slice(s.get(p..p + lits).ok_or_else(corrupt)?);
        p += lits;
        if let Some(d) = d {
            dist = d;
        }
        if len > 0 {
            if dist == 0 || dist > out.len() {
                return Err(corrupt());
            }
            for _ in 0..len {
                out.push(out[out.len() - dist]);
            }
        }
        if keep {
            ops.push(op);
        }
    }
    if out.len() - start != n {
        return Err(corrupt());
    }
    Ok(Parse { ops, used: p + 1 })
}

/// A match after `lits` literals, of `len` bytes at `dist` back.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Match {
    pub lits: u32,
    pub len: u32,
    pub dist: u32,
}

/// The matches of the instructions `ops` (a match continued at the same distance being one
/// match), and the literals after the last.
pub fn matches(ops: &[Op]) -> (Vec<Match>, u32) {
    let mut out: Vec<Match> = Vec::new();
    let (mut run, mut prev, mut joined) = (0, 0, false);
    for &op in ops {
        let (l, m, d) = match op {
            Op::Small { lits, len, dist } | Op::Large { lits, len, dist } | Op::Medium { lits, len, dist } => {
                (lits as u32, len as u32, dist as u32)
            }
            Op::Prev { lits, len } => (lits as u32, len as u32, prev),
            Op::Lits(l) => (l as u32, 0, prev),
            Op::Match(m) => (0, m as u32, prev),
            Op::Nop(_) => (0, 0, prev),
        };
        run += l;
        if m > 0 {
            match out.last_mut() {
                Some(x) if joined && run == 0 && matches!(op, Op::Match(_)) => x.len += m,
                _ => out.push(Match { lits: run, len: m, dist: d }),
            }
            run = 0;
        }
        joined = m > 0 || joined && l == 0;
        prev = d;
    }
    (out, run)
}

/// The instructions the compressor writes for `matches` and `tail` literals after
/// them (`FORMAT.md` §5.1).
pub fn ops(matches: &[Match], tail: u32) -> Vec<Op> {
    let mut out = Vec::new();
    let mut prev = 0;
    let runs = |out: &mut Vec<Op>, mut n: u32, f: fn(u16) -> Op| {
        while n > 0 {
            let k = n.min(271);
            out.push(f(k as u16));
            n -= k;
        }
    };
    for m in matches {
        let mut r = m.lits;
        if r >= 4 {
            runs(&mut out, r, Op::Lits);
            r = 0;
        }
        let cap = 10 - 2 * r;
        let k = if m.dist == prev {
            // (an instruction of no literals is a match alone, of the same cap)
            let k = m.len.min(cap);
            out.push(if r == 0 { Op::Match(k as u16) } else { Op::Prev { lits: r as u8, len: k as u8 } });
            k
        } else if m.dist < 1536 {
            let k = m.len.min(cap);
            out.push(Op::Small { lits: r as u8, len: k as u8, dist: m.dist as u16 });
            k
        } else if m.dist < 16384 && m.len > cap && m.len <= 34 - 2 * r {
            // (only where it takes the whole match)
            out.push(Op::Medium { lits: r as u8, len: m.len as u8, dist: m.dist as u16 });
            m.len
        } else {
            let k = m.len.min(cap);
            out.push(Op::Large { lits: r as u8, len: k as u8, dist: m.dist as u16 });
            k
        };
        runs(&mut out, m.len - k, Op::Match);
        prev = m.dist;
    }
    runs(&mut out, tail, Op::Lits);
    out
}

/// Writes the instructions `ops` over `data` (the literals taken from it in turn) and the end.
pub fn write(data: &[u8], ops: &[Op], out: &mut Vec<u8>) -> Result<()> {
    let mut pos = 0usize;
    for &op in ops {
        let (lits, len) = match op {
            Op::Small { lits, len, dist } => {
                if !(1..1536).contains(&dist) || lits > 3 || !(3..=10).contains(&len) {
                    return Err(corrupt());
                }
                out.extend([lits << 6 | (len - 3) << 3 | (dist >> 8) as u8, dist as u8]);
                (lits, len as u32)
            }
            Op::Large { lits, len, dist } => {
                if dist == 0 || lits > 3 || !(3..=10).contains(&len) {
                    return Err(corrupt());
                }
                out.extend([lits << 6 | (len - 3) << 3 | 7, dist as u8, (dist >> 8) as u8]);
                (lits, len as u32)
            }
            Op::Prev { lits, len } => {
                if !(1..=3).contains(&lits) || !(3..=10).contains(&len) {
                    return Err(corrupt());
                }
                out.push(lits << 6 | (len - 3) << 3 | 6);
                (lits, len as u32)
            }
            Op::Medium { lits, len, dist } => {
                if dist == 0 || dist >= 1 << 14 || lits > 3 || !(3..=34).contains(&len) {
                    return Err(corrupt());
                }
                let w = dist << 2 | (len - 3) as u16 & 3;
                out.extend([0xa0 | lits << 3 | (len - 3) >> 2, w as u8, (w >> 8) as u8]);
                (lits, len as u32)
            }
            Op::Lits(n) => {
                match n {
                    1..=15 => out.push(0xe0 | n as u8),
                    16..=271 => out.extend([0xe0, (n - 16) as u8]),
                    _ => return Err(corrupt()),
                }
                (0, 0)
            }
            Op::Match(n) => {
                match n {
                    1..=15 => out.push(0xf0 | n as u8),
                    16..=271 => out.extend([0xf0, (n - 16) as u8]),
                    _ => return Err(corrupt()),
                }
                (0, n as u32)
            }
            Op::Nop(b) => {
                out.push(b);
                (0, 0)
            }
        };
        let lits = if let Op::Lits(n) = op { n as usize } else { lits as usize };
        out.extend_from_slice(data.get(pos..pos + lits).ok_or_else(corrupt)?);
        pos += lits + len as usize;
    }
    out.extend([6, 0, 0, 0, 0, 0, 0, 0]);
    Ok(())
}
