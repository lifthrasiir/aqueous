//! A pack's index (`.idx`, version 2) and reverse index (`.rev`, version 1), git's
//! `gitformat-pack`: both follow from the pack and the ids of its entries in order, which is
//! all that is stored of them (FORMAT.md, §6).
//!
//! The index is a header (`FF 74 4F 63` and the version 2), a fan-out table (for each first
//! byte of an id, the count of ids starting with it or less), the ids in order, the CRC-32 of
//! each entry, its offset in the pack (31 bits, or with bit 31 set the place of a 64-bit one
//! among those after), the pack's trailer and the index's SHA-1. The reverse index is a header
//! (`RIDX`, the version 1 and the hash function 1, SHA-1), then for each entry in the pack's
//! order its place in the index, the pack's trailer and its own SHA-1. All numbers big endian.

use crate::pack::{self, ID};
use crate::{Result, corrupt};

pub const IDX_MAGIC: [u8; 8] = [0xff, b't', b'O', b'c', 0, 0, 0, 2];
pub const REV_MAGIC: [u8; 12] = [b'R', b'I', b'D', b'X', 0, 0, 0, 1, 0, 0, 0, 1];

/// Offsets past this are 64-bit ones (git's `off32_limit`).
const OFF32_LIMIT: u64 = 0x7fff_ffff;

/// Bytes of the index of `n` entries, `large` of them at an offset past [`OFF32_LIMIT`].
pub fn idx_len(n: usize, large: usize) -> usize {
    IDX_MAGIC.len() + 256 * 4 + n * (ID + 8) + large * 8 + 2 * ID
}

/// Bytes of the reverse index of `n` entries.
pub fn rev_len(n: usize) -> usize {
    REV_MAGIC.len() + n * 4 + 2 * ID
}

/// The pack's index, and its reverse index where `rev`, after `out`: of the entries starting
/// at `starts` (in order, the last the end of the entries, where the trailer starts) in
/// `pack`, of the ids `ids` in the same order.
pub fn build(pack: &[u8], starts: &[usize], ids: &[u8], rev: bool, out: &mut Vec<u8>) -> Result<()> {
    let n = starts.len().checked_sub(1).ok_or_else(corrupt)?;
    let end = starts[n];
    let trailer = pack.get(end..end + ID).ok_or_else(corrupt)?;
    if ids.len() != n * ID || u32::try_from(n).is_err() {
        return Err(corrupt());
    }
    let id = |j: usize| &ids[j * ID..][..ID];
    // the entries in the index's order (stable: of the same id, as in the pack)
    let mut order: Vec<u32> = (0..n as u32).collect();
    order.sort_by(|&a, &b| id(a as usize).cmp(id(b as usize)));
    let large = (0..n).filter(|&j| starts[j] as u64 > OFF32_LIMIT).count();
    let at = out.len();
    out.reserve(idx_len(n, large) + if rev { rev_len(n) } else { 0 });
    out.extend_from_slice(&IDX_MAGIC);
    let mut fanout = [0u32; 256];
    for j in 0..n {
        fanout[id(j)[0] as usize] += 1;
    }
    let mut sum = 0;
    for c in fanout {
        sum += c;
        out.extend_from_slice(&sum.to_be_bytes());
    }
    for &j in &order {
        out.extend_from_slice(id(j as usize));
    }
    for &j in &order {
        let j = j as usize;
        let entry = pack.get(starts[j]..starts[j + 1]).ok_or_else(corrupt)?;
        out.extend_from_slice(&aqr_common::crc32::hash(entry).to_be_bytes());
    }
    let mut big = Vec::with_capacity(large);
    for &j in &order {
        let s = starts[j as usize] as u64;
        let o = if s > OFF32_LIMIT {
            big.push(s);
            0x8000_0000 | (big.len() - 1) as u32
        } else {
            s as u32
        };
        out.extend_from_slice(&o.to_be_bytes());
    }
    for s in big {
        out.extend_from_slice(&s.to_be_bytes());
    }
    out.extend_from_slice(trailer);
    let sum = pack::sha1(&out[at..]);
    out.extend_from_slice(&sum);
    if rev {
        let at = out.len();
        out.extend_from_slice(&REV_MAGIC);
        let mut place = vec![0u32; n];
        for (k, &j) in order.iter().enumerate() {
            place[j as usize] = k as u32;
        }
        for p in place {
            out.extend_from_slice(&p.to_be_bytes());
        }
        out.extend_from_slice(trailer);
        let sum = pack::sha1(&out[at..]);
        out.extend_from_slice(&sum);
    }
    Ok(())
}

/// The offsets that index `idx` tells, of its entries in its order, or None where it is not
/// one of version 2.
#[cfg(feature = "encode")]
fn offsets(idx: &[u8]) -> Option<Vec<u64>> {
    if !idx.starts_with(&IDX_MAGIC) || idx.len() < idx_len(0, 0) {
        return None;
    }
    let word = |p: usize| idx.get(p..p + 4).map(|w| u32::from_be_bytes(w.try_into().unwrap()));
    let n = word(IDX_MAGIC.len() + 255 * 4)? as usize;
    if idx.len() < idx_len(n, 0) {
        return None;
    }
    let at = IDX_MAGIC.len() + 256 * 4 + n * (ID + 4);
    let large = at + n * 4;
    (0..n)
        .map(|k| {
            let o = word(at + 4 * k)?;
            match o & 0x8000_0000 {
                0 => Some(o as u64),
                _ => Some(u64::from_be_bytes(
                    idx.get(large + 8 * (o & 0x7fff_ffff) as usize..)?.get(..8)?.try_into().ok()?,
                )),
            }
        })
        .collect()
}

/// Where the entries start that index `idx` tells of, in the pack's order, or None where it is
/// not an index of version 2 or two are at the same offset.
#[cfg(feature = "encode")]
pub fn starts(idx: &[u8]) -> Option<Vec<usize>> {
    let mut s: Vec<usize> = offsets(idx)?.into_iter().map(|o| usize::try_from(o).ok()).collect::<Option<_>>()?;
    s.sort_unstable();
    s.windows(2).all(|w| w[0] < w[1]).then_some(s)
}

/// The ids of the entries starting at `starts` (as [`build`] takes them) in `pack`, in order,
/// where `idx` (and `rev`, where given) is what [`build`] makes of them; else None.
#[cfg(feature = "encode")]
pub fn ids(pack: &[u8], starts: &[usize], idx: &[u8], rev: Option<&[u8]>) -> Option<Vec<u8>> {
    let n = starts.len().checked_sub(1)?;
    let offsets = offsets(idx)?;
    if offsets.len() != n {
        return None;
    }
    let names = IDX_MAGIC.len() + 256 * 4;
    // the place of each entry in the pack, by its offset
    let by_offset: std::collections::HashMap<u64, usize> = (0..n).map(|j| (starts[j] as u64, j)).collect();
    let mut ids = vec![0; n * ID];
    let mut seen = vec![false; n];
    for (k, o) in offsets.into_iter().enumerate() {
        let &j = by_offset.get(&o)?;
        if std::mem::replace(&mut seen[j], true) {
            return None;
        }
        ids[j * ID..][..ID].copy_from_slice(&idx[names + k * ID..][..ID]);
    }
    let mut made = Vec::new();
    build(pack, starts, &ids, rev.is_some(), &mut made).ok()?;
    let (i, r) = made.split_at(made.len().min(idx.len()));
    (i == idx && r == rev.unwrap_or_default()).then_some(ids)
}
