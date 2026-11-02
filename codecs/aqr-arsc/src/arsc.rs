//! The chunks of a resource table as the module sees them (FORMAT.md), and their decoding: the
//! file from its streams.

use aqr_common::varint;

use crate::{Result, corrupt};

/// The types of chunks.
pub mod ty {
    pub const STRING_POOL: u16 = 0x0001;
    pub const TABLE: u16 = 0x0002;
    pub const PACKAGE: u16 = 0x0200;
    pub const TYPE: u16 = 0x0201;
}

/// How a chunk is rebuilt (its mode, in `head`).
pub mod mode {
    /// Its body as it is, from `raw`.
    pub const RAW: u8 = 0;
    /// Chunks one after another.
    pub const CONTAINER: u8 = 1;
    /// A string pool (FORMAT.md §4).
    pub const POOL: u8 = 2;
    /// A type's entries (FORMAT.md §5).
    pub const TYPE: u8 = 3;
}

/// The streams, by their index.
pub mod s {
    pub const HEAD: usize = 0;
    pub const RAW: usize = 1;
    pub const TEXT: usize = 2;
    pub const TEXT16: usize = 3;
    pub const STYLE: usize = 4;
    pub const PRESENT: usize = 5;
    pub const FLAGS: usize = 6;
    pub const KEY: usize = 7;
    pub const VTYPE: usize = 8;
    pub const VSTR: usize = 9;
    pub const VDATA: usize = 10;
    pub const MAP: usize = 11;
    pub const N: usize = 12;
}

pub const STREAM_NAMES: [&str; s::N] =
    ["head", "raw", "text", "text16", "style", "present", "flags", "key", "vtype", "vstr", "vdata", "map"];

/// The module's inputs: each holds the streams listed, one after another, after the lengths of
/// all but the last (FORMAT.md §2).
pub const INPUTS: [&[usize]; 6] = [
    &[s::HEAD, s::RAW, s::STYLE, s::TEXT16],
    &[s::TEXT],
    &[s::PRESENT, s::FLAGS, s::KEY],
    &[s::VTYPE, s::VSTR],
    &[s::VDATA],
    &[s::MAP],
];

/// The names of the inputs (their classes are `arsc/NAME`).
pub const INPUT_NAMES: [&str; INPUTS.len()] = ["head", "text", "entries", "values", "data", "maps"];

/// The dataType of a string's index.
pub const STRING: u8 = 0x03;

pub fn le16(d: &[u8], at: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(*d.get(at..).and_then(|d| d.first_chunk()).ok_or_else(corrupt)?))
}

pub fn le32(d: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(*d.get(at..).and_then(|d| d.first_chunk()).ok_or_else(corrupt)?))
}

/// The UTF-16 units of UTF-8 bytes `s` (surrogates of three bytes each count one).
pub fn utf16_len(s: &[u8]) -> usize {
    s.iter().map(|&b| (b & 0xc0 != 0x80) as usize + (b >= 0xf0) as usize).sum()
}

/// A length of a UTF-8 pool's string: one byte below 0x80, else two with bit 7 set.
pub fn put_len8(out: &mut Vec<u8>, n: usize) -> Option<()> {
    match n {
        0..0x80 => out.push(n as u8),
        0x80..0x8000 => out.extend_from_slice(&[0x80 | (n >> 8) as u8, n as u8]),
        _ => return None,
    }
    Some(())
}

/// A length of a UTF-16 pool's string: one unit below 0x8000, else two with bit 15 set.
pub fn put_len16(out: &mut Vec<u8>, n: usize) -> Option<()> {
    match n {
        0..0x8000 => out.extend_from_slice(&(n as u16).to_le_bytes()),
        0x8000..0x8000_0000 => {
            out.extend_from_slice(&(0x8000 | (n >> 16) as u16).to_le_bytes());
            out.extend_from_slice(&(n as u16).to_le_bytes());
        }
        _ => return None,
    }
    Some(())
}

/// A stream as it is read.
pub struct R<'a> {
    d: &'a [u8],
    at: usize,
}

impl<'a> R<'a> {
    pub fn new(d: &'a [u8]) -> Self {
        R { d, at: 0 }
    }

    pub fn var(&mut self) -> Result<u64> {
        varint::get(self.d, &mut self.at).map_err(|_| corrupt())
    }

    pub fn var32(&mut self) -> Result<u32> {
        u32::try_from(self.var()?).map_err(|_| corrupt())
    }

    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let b = self.d.get(self.at..).and_then(|d| d.get(..n)).ok_or_else(corrupt)?;
        self.at += n;
        Ok(b)
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }

    /// The bytes up to the next zero byte, past which it goes.
    pub fn until0(&mut self) -> Result<&'a [u8]> {
        let rest = &self.d[self.at..];
        let n = rest.iter().position(|&b| b == 0).ok_or_else(corrupt)?;
        self.at += n + 1;
        Ok(&rest[..n])
    }

    /// The units up to the next zero unit, past which it goes.
    pub fn until00(&mut self) -> Result<&'a [u8]> {
        let rest = &self.d[self.at..];
        let n = rest.as_chunks::<2>().0.iter().position(|u| *u == [0, 0]).ok_or_else(corrupt)?;
        self.at += 2 * n + 2;
        Ok(&rest[..2 * n])
    }

    pub fn rest(&self) -> &'a [u8] {
        &self.d[self.at..]
    }

    pub fn done(&self) -> Result<()> {
        if self.at == self.d.len() { Ok(()) } else { Err(corrupt()) }
    }
}

/// The streams from the module's inputs.
pub fn unpack<'a>(inputs: &[&'a [u8]]) -> Result<Vec<&'a [u8]>> {
    if inputs.len() != INPUTS.len() {
        return Err(corrupt());
    }
    let mut st: Vec<&[u8]> = vec![&[]; s::N];
    for (group, &d) in INPUTS.iter().zip(inputs) {
        let mut r = R::new(d);
        let lens: Vec<u64> = (1..group.len()).map(|_| r.var()).collect::<Result<_>>()?;
        for (&l, &n) in group.iter().zip(&lens) {
            st[l] = r.bytes(usize::try_from(n).map_err(|_| corrupt())?)?;
        }
        st[group[group.len() - 1]] = r.rest();
    }
    Ok(st)
}

/// The module's inputs from the streams `st`.
pub fn pack(st: &[Vec<u8>]) -> Vec<Vec<u8>> {
    INPUTS
        .iter()
        .map(|group| {
            let mut v = Vec::new();
            for &l in &group[..group.len() - 1] {
                varint::put(&mut v, st[l].len() as u64);
            }
            for &l in *group {
                v.extend_from_slice(&st[l]);
            }
            v
        })
        .collect()
}

/// What the entries of earlier type chunks predict of an entry: its key and its value, by type
/// id and index (FORMAT.md §5).
#[derive(Default, Clone)]
pub struct Seen {
    by_type: Vec<Vec<(u32, u8, u32)>>,
}

impl Seen {
    pub fn get(&self, t: u8, i: usize) -> (u32, u8, u32) {
        self.by_type.get(t as usize).and_then(|v| v.get(i)).copied().unwrap_or((0, 0, 0))
    }

    pub fn of_type(&self, t: u8) -> Vec<(u32, u8, u32)> {
        self.by_type.get(t as usize).cloned().unwrap_or_default()
    }

    pub fn set_type(&mut self, t: u8, v: Vec<(u32, u8, u32)>) {
        if let Some(x) = self.by_type.get_mut(t as usize) {
            *x = v;
        }
    }

    pub fn set(&mut self, t: u8, i: usize, e: (u32, u8, u32)) {
        let t = t as usize;
        if self.by_type.len() <= t {
            self.by_type.resize(t + 1, Vec::new());
        }
        let v = &mut self.by_type[t];
        if v.len() <= i {
            v.resize(i + 1, (0, 0, 0));
        }
        v[i] = e;
    }
}

/// The state of decoding: the streams, what earlier entries predict, the last string index.
struct Dec<'a> {
    r: Vec<R<'a>>,
    seen: Seen,
    last_str: u32,
}

/// The file of `len` bytes from the module's inputs ([`INPUTS`], FORMAT.md §3).
pub fn decode(inputs: &[&[u8]], len: usize) -> Result<Vec<u8>> {
    let mut d = Dec { r: unpack(inputs)?.into_iter().map(R::new).collect(), seen: Seen::default(), last_str: 0 };
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        chunk(&mut d, &mut out, len)?;
    }
    if out.len() != len {
        return Err(corrupt());
    }
    for r in &d.r {
        r.done()?;
    }
    Ok(out)
}

/// A chunk appended to `out`, which it must not take past `end`.
fn chunk(d: &mut Dec, out: &mut Vec<u8>, end: usize) -> Result<()> {
    let start = out.len();
    let h8 = d.r[s::HEAD].bytes(8)?;
    let (t, hs, size) = (le16(h8, 0)?, le16(h8, 2)? as usize, le32(h8, 4)? as usize);
    if hs < 8 || size < hs || start.checked_add(size).is_none_or(|e| e > end) {
        return Err(corrupt());
    }
    out.extend_from_slice(h8);
    out.extend_from_slice(d.r[s::HEAD].bytes(hs - 8)?);
    let stop = start + size;
    match d.r[s::HEAD].u8()? {
        mode::RAW => out.extend_from_slice(d.r[s::RAW].bytes(size - hs)?),
        mode::CONTAINER => {
            while out.len() < stop {
                chunk(d, out, stop)?;
            }
        }
        mode::POOL if t == ty::STRING_POOL => pool(d, out, start, hs, stop)?,
        mode::TYPE if t == ty::TYPE => entries(d, out, start, hs, stop)?,
        _ => return Err(corrupt()),
    }
    if out.len() != stop {
        return Err(corrupt());
    }
    Ok(())
}

/// A string pool's body (FORMAT.md §4).
fn pool(d: &mut Dec, out: &mut Vec<u8>, start: usize, hs: usize, stop: usize) -> Result<()> {
    let h = &out[start..start + hs];
    if hs < 28 {
        return Err(corrupt());
    }
    let (n, ns, flags) = (le32(h, 8)? as usize, le32(h, 12)? as usize, le32(h, 16)?);
    let (strings, styles) = (le32(h, 20)? as usize, le32(h, 24)? as usize);
    let offs = out.len();
    if n == 0 || strings != hs + 4 * (n + ns) || (ns > 0 && styles < strings) {
        return Err(corrupt());
    }
    out.resize(offs + 4 * n, 0);
    out.extend_from_slice(d.r[s::STYLE].bytes(4 * ns)?);
    let base = start + strings;
    for i in 0..n {
        let o = u32::try_from(out.len() - base).map_err(|_| corrupt())?;
        out[offs + 4 * i..offs + 4 * i + 4].copy_from_slice(&o.to_le_bytes());
        if flags & 0x100 != 0 {
            let s = d.r[s::TEXT].until0()?;
            put_len8(out, utf16_len(s)).ok_or_else(corrupt)?;
            put_len8(out, s.len()).ok_or_else(corrupt)?;
            out.extend_from_slice(s);
            out.push(0);
        } else {
            let s = d.r[s::TEXT16].until00()?;
            put_len16(out, s.len() / 2).ok_or_else(corrupt)?;
            out.extend_from_slice(s);
            out.extend_from_slice(&[0, 0]);
        }
        if out.len() > stop {
            return Err(corrupt());
        }
    }
    // (zeros up to the styles or the end)
    let pad = if ns > 0 { start + styles } else { stop };
    if out.len() > pad {
        return Err(corrupt());
    }
    out.resize(pad, 0);
    out.extend_from_slice(d.r[s::STYLE].bytes(stop - pad)?);
    Ok(())
}

/// A type chunk's offsets and entries (FORMAT.md §5).
fn entries(d: &mut Dec, out: &mut Vec<u8>, start: usize, hs: usize, stop: usize) -> Result<()> {
    let h = &out[start..start + hs];
    if hs < 20 || h[9] != 0 {
        return Err(corrupt());
    }
    let tid = h[8];
    let (n, es) = (le32(h, 12)? as usize, le32(h, 16)? as usize);
    if es != hs.checked_add(n.checked_mul(4).ok_or_else(corrupt)?).ok_or_else(corrupt)? || start + es > stop {
        return Err(corrupt());
    }
    let offs = out.len();
    out.resize(start + es, 0);
    for i in 0..n {
        let o = match d.r[s::PRESENT].u8()? {
            0 => u32::MAX,
            1 => u32::try_from(out.len() - start - es).map_err(|_| corrupt())?,
            _ => return Err(corrupt()),
        };
        out[offs + 4 * i..offs + 4 * i + 4].copy_from_slice(&o.to_le_bytes());
        if o == u32::MAX {
            continue;
        }
        let flags = u16::try_from(d.r[s::FLAGS].var()?).map_err(|_| corrupt())?;
        let (pk, pt, pv) = d.seen.get(tid, i);
        let key = match d.r[s::KEY].var()? {
            0 => pk,
            k => u32::try_from(k - 1).map_err(|_| corrupt())?,
        };
        let complex = flags & 1 != 0;
        out.extend_from_slice(&(if complex { 16u16 } else { 8 }).to_le_bytes());
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&key.to_le_bytes());
        if complex {
            out.extend_from_slice(d.r[s::MAP].bytes(4)?);
            let count = d.r[s::FLAGS].var32()?;
            out.extend_from_slice(&count.to_le_bytes());
            let maps = (count as usize).checked_mul(12).filter(|&m| out.len() + m <= stop).ok_or_else(corrupt)?;
            out.extend_from_slice(d.r[s::MAP].bytes(maps)?);
            d.seen.set(tid, i, (key, pt, pv));
        } else {
            let vt = d.r[s::VTYPE].u8()?;
            let data = if vt == STRING {
                let v = crate::arsc::moved(d.last_str, d.r[s::VSTR].var()?)?;
                d.last_str = v;
                v
            } else {
                match d.r[s::VDATA].var()? {
                    0 if vt == pt => pv,
                    0 => return Err(corrupt()),
                    k => u32::try_from(k - 1).map_err(|_| corrupt())?,
                }
            };
            out.extend_from_slice(&[8, 0, 0, vt]);
            out.extend_from_slice(&data.to_le_bytes());
            d.seen.set(tid, i, (key, vt, data));
        }
        if out.len() > stop {
            return Err(corrupt());
        }
    }
    Ok(())
}

/// `prev` moved by the zigzag value `z`, within `u32`.
pub fn moved(prev: u32, z: u64) -> Result<u32> {
    u32::try_from(prev as i64 + varint::unzz(z)).map_err(|_| corrupt())
}
