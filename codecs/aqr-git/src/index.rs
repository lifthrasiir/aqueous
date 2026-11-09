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

use aqr_api::stream::{Pull, Put, Slice};
use sha1::{Digest, Sha1};

use crate::layout::Entries;
use crate::pack::ID;
use crate::{Result, corrupt};

pub const IDX_MAGIC: [u8; 8] = [0xff, b't', b'O', b'c', 0, 0, 0, 2];
pub const REV_MAGIC: [u8; 12] = [b'R', b'I', b'D', b'X', 0, 0, 0, 1, 0, 0, 0, 1];

/// Offsets past this are 64-bit ones (git's `off32_limit`).
const OFF32_LIMIT: u64 = 0x7fff_ffff;

/// Bytes of the index of `n` entries, `large` of them at an offset past `OFF32_LIMIT`.
pub fn idx_len(n: usize, large: usize) -> usize {
    IDX_MAGIC.len() + 256 * 4 + n * (ID + 8) + large * 8 + 2 * ID
}

/// Bytes of the reverse index of `n` entries.
pub fn rev_len(n: usize) -> usize {
    REV_MAGIC.len() + n * 4 + 2 * ID
}

/// Entries of `starts` (as [`build`] takes them) at an offset past `OFF32_LIMIT`.
fn large(starts: &[usize]) -> usize {
    starts[..starts.len().saturating_sub(1)].iter().filter(|&&s| s as u64 > OFF32_LIMIT).count()
}

/// Bytes that [`build`] writes of the entries starting at `starts`: the index, and the reverse
/// index where `rev`.
pub fn len(starts: &[usize], rev: bool) -> usize {
    let n = starts.len().saturating_sub(1);
    idx_len(n, large(starts)) + if rev { rev_len(n) } else { 0 }
}

/// Bytes that [`fn@write`] takes of [`aqr_api::stack`] on top of the entries' offsets and CRC-32s
/// ([`layout::Entries`]), for `n` entries: the ids, the entries in the index's order, and a
/// piece for the output, with (on WebAssembly) the CRC-32's tables (made on its first use, held
/// after).
///
/// [`layout::Entries`]: crate::layout::Entries
pub fn plan(sim: &mut aqr_api::stack::Sim, n: u64) {
    sim.take(ID as u64 * n);
    sim.take(4 * n);
    sim.take(PIECE as u64);
}

/// Bytes of the pieces in which [`write`] puts the indexes.
const PIECE: usize = 1 << 14;

/// The pack's index, and its reverse index where `rev`, put to `out` in order (the bytes put are
/// [`len`] of the entries): of the entries `entries` (their offsets, the last the end of the
/// entries, and their CRC-32s), the pack's trailer `trailer`, and the ids of the entries in the
/// same order pulled from `ids` (`20n` bytes). Returns the bytes put.
pub fn write(ids: &mut dyn Pull, entries: &Entries, trailer: &[u8; ID], rev: bool, out: &mut dyn Put) -> Result<u64> {
    let n = entries.crcs.len();
    // (the reverse index's inversion marks its places with the top bit)
    if entries.starts.len() != n + 1 || n >= 1 << 31 {
        return Err(corrupt());
    }
    let mut names = vec![0u8; n * ID];
    let mut got = 0;
    while got < names.len() {
        match ids.pull(&mut names[got..]) {
            0 => return Err(corrupt()),
            k => got += k,
        }
    }
    let id = |j: usize| &names[j * ID..][..ID];
    // the entries in the index's order (of the same id, as in the pack)
    let mut order: Vec<u32> = Vec::with_capacity(n);
    order.extend(0..n as u32);
    order.sort_unstable_by(|&a, &b| id(a as usize).cmp(id(b as usize)).then(a.cmp(&b)));
    let mut w = Hashed { out, buf: Vec::with_capacity(PIECE), sha: Sha1::new(), total: 0 };
    w.put(&IDX_MAGIC)?;
    let mut fanout = [0u32; 256];
    for j in 0..n {
        fanout[id(j)[0] as usize] += 1;
    }
    let mut sum = 0;
    for c in fanout {
        sum += c;
        w.put(&sum.to_be_bytes())?;
    }
    for &j in &order {
        w.put(id(j as usize))?;
    }
    for &j in &order {
        w.put(&entries.crcs[j as usize].to_be_bytes())?;
    }
    // the offsets, the 64-bit ones after them
    let mut big = 0u32;
    for &j in &order {
        let s = entries.starts[j as usize];
        if s > OFF32_LIMIT {
            w.put(&(0x8000_0000 | big).to_be_bytes())?;
            big += 1;
        } else {
            w.put(&(s as u32).to_be_bytes())?;
        }
    }
    for &j in &order {
        let s = entries.starts[j as usize];
        if s > OFF32_LIMIT {
            w.put(&s.to_be_bytes())?;
        }
    }
    w.put(trailer)?;
    w.finish()?;
    if rev {
        w.put(&REV_MAGIC)?;
        // (each entry's place in the index's order: the inverse of `order`, made where it is)
        const MARK: u32 = 1 << 31;
        for i in 0..n {
            if order[i] & MARK != 0 {
                continue;
            }
            let (mut prev, mut cur) = (i as u32, order[i]);
            loop {
                let next = order[cur as usize];
                order[cur as usize] = prev | MARK;
                if cur as usize == i {
                    break;
                }
                (prev, cur) = (cur, next);
            }
        }
        for &p in &order {
            w.put(&(p & !MARK).to_be_bytes())?;
        }
        w.put(trailer)?;
        w.finish()?;
    }
    Ok(w.total)
}

/// Bytes put in pieces, and hashed up to the SHA-1 that closes each index.
struct Hashed<'a> {
    out: &'a mut dyn Put,
    buf: Vec<u8>,
    sha: Sha1,
    total: u64,
}

impl Hashed<'_> {
    fn put(&mut self, b: &[u8]) -> Result<()> {
        if b.len() > self.buf.capacity() - self.buf.len() {
            self.flush()?;
        }
        self.buf.extend_from_slice(b);
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.sha.update(&self.buf);
        self.out.put(&self.buf).ok_or_else(corrupt)?;
        self.total += self.buf.len() as u64;
        self.buf.clear();
        Ok(())
    }

    /// Ends the index with the SHA-1 of what it has.
    fn finish(&mut self) -> Result<()> {
        self.flush()?;
        let sum: [u8; ID] = std::mem::take(&mut self.sha).finalize().into();
        self.out.put(&sum).ok_or_else(corrupt)?;
        self.total += ID as u64;
        Ok(())
    }
}

/// The pack's index, and its reverse index where `rev`, written to `out` (of [`len`] bytes): of
/// the entries starting at `starts` (in order, the last the end of the entries, where the
/// trailer starts) in `pack`, of the ids `ids` in the same order.
pub fn build(pack: &[u8], starts: &[usize], ids: &[u8], rev: bool, out: &mut [u8]) -> Result<()> {
    let n = starts.len().checked_sub(1).ok_or_else(corrupt)?;
    let trailer: [u8; ID] = pack.get(starts[n]..starts[n] + ID).ok_or_else(corrupt)?.try_into().unwrap();
    if ids.len() != n * ID || out.len() != len(starts, rev) {
        return Err(corrupt());
    }
    let crcs = (0..n).map(|j| Ok(aqr_common::crc32::hash(pack.get(starts[j]..starts[j + 1]).ok_or_else(corrupt)?)));
    let entries = Entries { starts: starts.iter().map(|&s| s as u64).collect(), crcs: crcs.collect::<Result<_>>()? };
    let mut made = Vec::with_capacity(out.len());
    write(&mut Slice(ids), &entries, &trailer, rev, &mut made)?;
    if made.len() != out.len() {
        return Err(corrupt());
    }
    out.copy_from_slice(&made);
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
    let mut made = vec![0; len(starts, rev.is_some())];
    build(pack, starts, &ids, rev.is_some(), &mut made).ok()?;
    let (i, r) = made.split_at(made.len().min(idx.len()));
    (i == idx && r == rev.unwrap_or_default()).then_some(ids)
}
