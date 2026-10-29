//! LevelDB tables (`.ldb`, `.sst`): the blocks found by the footer, the index and the
//! metaindex, each followed by its type and masked CRC, then the footer. `FORMAT.md` §4.

use aqr_common::varint;

use crate::snappy::{self, Hash};
use crate::{Error, Result, corrupt, crc32c};

/// The last 8 bytes of a table (`kTableMagicNumber`, little endian).
pub const MAGIC: [u8; 8] = 0xdb47_7524_8b80_fb57u64.to_le_bytes();
pub const FOOTER: usize = 48;
/// Type byte and masked CRC after each block.
pub const TRAILER: usize = 5;

/// Block types.
pub const RAW: u8 = 0;
pub const SNAPPY: u8 = 1;

/// A block of a table: where its stored bytes are, and its type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block {
    pub offset: usize,
    pub size: usize,
    pub kind: u8,
}

/// Whether `d` ends like a table: the magic number, after handles within `d`.
pub fn is_table(d: &[u8]) -> bool {
    footer(d).is_ok()
}

/// The metaindex and index handles of the footer of `d`.
fn footer(d: &[u8]) -> Result<[(usize, usize); 2]> {
    let f = d.len().checked_sub(FOOTER).map(|p| &d[p..]).ok_or_else(corrupt)?;
    if f[40..] != MAGIC {
        return Err(Error("not a LevelDB table"));
    }
    let mut p = 0;
    let mut handle = || -> Result<(usize, usize)> {
        let off = varint::get(&f[..40], &mut p).map_err(|_| corrupt())? as usize;
        let size = varint::get(&f[..40], &mut p).map_err(|_| corrupt())? as usize;
        (off.checked_add(size).and_then(|e| e.checked_add(TRAILER)).is_some_and(|e| e <= d.len() - FOOTER))
            .then_some((off, size))
            .ok_or_else(corrupt)
    };
    Ok([handle()?, handle()?])
}

/// The contents of the block at `h`, decoded.
fn contents(d: &[u8], (off, size): (usize, usize)) -> Result<Vec<u8>> {
    let b = &d[off..off + size];
    match d[off + size] {
        RAW => Ok(b.to_vec()),
        SNAPPY => snappy::decode(b, 1 << 30),
        _ => Err(Error("LevelDB block of an unknown type")),
    }
}

/// The handles that are the values of the entries of an index or metaindex block.
fn handles(block: &[u8]) -> Result<Vec<(usize, usize)>> {
    let n = u32::from_le_bytes(*block.last_chunk().ok_or_else(corrupt)?) as usize;
    let end = n.checked_mul(4).and_then(|r| block.len().checked_sub(4 + r)).ok_or_else(corrupt)?;
    let (mut p, mut out) = (0, Vec::new());
    let get = |p: &mut usize| varint::get(&block[..end], p).map(|v| v as usize).map_err(|_| corrupt());
    while p < end {
        let _shared = get(&mut p)?;
        let unshared = get(&mut p)?;
        let vlen = get(&mut p)?;
        p = p.checked_add(unshared).filter(|&q| q <= end).ok_or_else(corrupt)?;
        let v = block.get(p..p.checked_add(vlen).ok_or_else(corrupt)?).ok_or_else(corrupt)?;
        p += vlen;
        let mut q = 0;
        let off = varint::get(v, &mut q).map_err(|_| corrupt())? as usize;
        let size = varint::get(v, &mut q).map_err(|_| corrupt())? as usize;
        out.push((off, size));
    }
    Ok(out)
}

/// The blocks of table `d`, in order. They must tile it up to the footer, and the last two
/// must be the metaindex and the index.
pub fn blocks(d: &[u8]) -> Result<Vec<Block>> {
    let [meta, index] = footer(d)?;
    let mut hs = handles(&contents(d, index)?)?;
    hs.extend(handles(&contents(d, meta)?)?);
    hs.extend([meta, index]);
    hs.sort_unstable();
    hs.dedup();
    let mut p = 0;
    let mut out = Vec::with_capacity(hs.len());
    for (off, size) in hs {
        if off != p {
            return Err(Error("LevelDB blocks do not tile the table"));
        }
        p = off.checked_add(size).and_then(|e| e.checked_add(TRAILER)).ok_or_else(corrupt)?;
        if p > d.len() - FOOTER {
            return Err(corrupt());
        }
        out.push(Block { offset: off, size, kind: d[off + size] });
    }
    if p != d.len() - FOOTER
        || out.len() < 2
        || out[out.len() - 2..] != [meta, index].map(|(o, s)| Block { offset: o, size: s, kind: d[o + s] })
    {
        return Err(Error("LevelDB blocks do not end with the metaindex and the index"));
    }
    Ok(out)
}

/// How a block is rebuilt (the codes of the layout, `FORMAT.md` §4.2).
pub const CODE_RAW: u8 = 0;
pub const CODE_SNAPPY: u8 = 1;
pub const CODE_KEPT: u8 = 2;

/// Table rebuilt from `data` (the contents of blocks rebuilt), `layout` and `kept` (blocks
/// kept as they are), compressing by `hash`.
pub fn build(data: &[u8], layout: &[u8], kept: &[u8], hash: Hash) -> Result<Vec<u8>> {
    build_in(data, layout, kept, hash, usize::MAX)
}

/// [`build`] into a vector of `capacity` bytes at first (the length expected, where it is
/// known).
pub fn build_in(data: &[u8], layout: &[u8], kept: &[u8], hash: Hash, capacity: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(capacity.min(data.len() / 2 + kept.len() + FOOTER));
    let (mut lp, mut dp, mut kp) = (0, 0, 0);
    let mut last = [(0, 0); 2];
    let mut blocks = 0usize;
    let take = |d: &[u8], p: &mut usize, n: usize| -> Result<std::ops::Range<usize>> {
        let r = *p..p.checked_add(n).filter(|&e| e <= d.len()).ok_or_else(corrupt)?;
        *p = r.end;
        Ok(r)
    };
    while lp < layout.len() {
        let n = varint::get(layout, &mut lp).map_err(|_| corrupt())? as usize;
        let code = *layout.get(lp).ok_or_else(corrupt)?;
        lp += 1;
        let at = out.len();
        let kind = match code {
            CODE_RAW => {
                out.extend_from_slice(&data[take(data, &mut dp, n)?]);
                RAW
            }
            CODE_SNAPPY => {
                snappy::compress_onto(&data[take(data, &mut dp, n)?], hash, &mut out)?;
                SNAPPY
            }
            CODE_KEPT => {
                let kind = *layout.get(lp).ok_or_else(corrupt)?;
                lp += 1;
                out.extend_from_slice(&kept[take(kept, &mut kp, n)?]);
                kind
            }
            _ => return Err(corrupt()),
        };
        let sum = crc32c::masked(&[&out[at..], &[kind]]);
        last = [last[1], (at, out.len() - at)];
        out.push(kind);
        out.extend_from_slice(&sum.to_le_bytes());
        blocks += 1;
    }
    if blocks < 2 || dp != data.len() || kp != kept.len() {
        return Err(corrupt());
    }
    let at = out.len();
    for (off, size) in last {
        varint::put(&mut out, off as u64);
        varint::put(&mut out, size as u64);
    }
    out.resize(at + 40, 0);
    out.extend_from_slice(&MAGIC);
    Ok(out)
}
