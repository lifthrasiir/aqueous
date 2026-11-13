//! Apple's accelerator tables (`__apple_names`, `__apple_types`, `__apple_namespac`,
//! `__apple_objc`; FORMAT.md §2): hash tables of names, each hash the DJB hash of a string of
//! `.debug_str`, each name with the offsets of its DIEs. The tables are rebuilt from their
//! entries in the order of their strings (the hashes, buckets and offsets follow from them and
//! `.debug_str`), the DIE offsets of each as deltas.

use aqr_common::varint;

use crate::{Result, corrupt};

/// `HASH`, little-endian.
const MAGIC: u32 = 0x4841_5348;
const DW_ATOM_DIE_OFFSET: u16 = 1;
const DW_FORM_DATA2: u16 = 0x05;
const DW_FORM_DATA4: u16 = 0x06;
const DW_FORM_DATA8: u16 = 0x07;
const DW_FORM_DATA1: u16 = 0x0b;

/// The inputs after `.debug_str`.
pub const STREAM_NAMES: [&str; 4] = ["strp", "count", "die", "atoms"];

/// The DJB hash of `s`, as the tables' hash function 0.
pub fn djb(s: &[u8]) -> u32 {
    s.iter().fold(5381u32, |h, &c| h.wrapping_mul(33).wrapping_add(c as u32))
}

/// The header of a table: its bytes (the params), its buckets, its hashes and the sizes of the
/// atoms after the first (the DIE offset).
struct Header<'a> {
    bytes: &'a [u8],
    buckets: u32,
    hashes: u32,
    atoms: Vec<usize>,
}

fn u16le(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(*d.get(at..)?.first_chunk()?))
}

fn u32le(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(*d.get(at..)?.first_chunk()?))
}

impl<'a> Header<'a> {
    /// The header at the start of `d`, of the tables this module takes: version 1, hash
    /// function 0, the first atom the DIE offset in 4 bytes.
    fn parse(d: &'a [u8]) -> Option<Header<'a>> {
        let (magic, version, function) = (u32le(d, 0)?, u16le(d, 4)?, u16le(d, 6)?);
        let (buckets, hashes, data_len) = (u32le(d, 8)?, u32le(d, 12)?, u32le(d, 16)? as usize);
        if magic != MAGIC || version != 1 || function != 0 || (buckets == 0 && hashes != 0) {
            return None;
        }
        let count = u32le(d, 24)? as usize;
        // (the atoms' descriptions, 4 bytes each, are in the table)
        if count == 0 || count > d.len() / 4 || data_len != 8 + 4 * count {
            return None;
        }
        let mut atoms = Vec::with_capacity(count - 1);
        for k in 0..count {
            let (kind, form) = (u16le(d, 28 + 4 * k)?, u16le(d, 30 + 4 * k)?);
            let size = match form {
                DW_FORM_DATA1 => 1,
                DW_FORM_DATA2 => 2,
                DW_FORM_DATA4 => 4,
                DW_FORM_DATA8 => 8,
                _ => return None,
            };
            if k == 0 && (kind != DW_ATOM_DIE_OFFSET || size != 4) {
                return None;
            }
            if k > 0 {
                atoms.push(size);
            }
        }
        Some(Header { bytes: &d[..20 + data_len], buckets, hashes, atoms })
    }
}

/// The string at `at` of `strs`, without its NUL.
fn string(strs: &[u8], at: u32) -> Option<&[u8]> {
    let s = strs.get(at as usize..)?;
    Some(&s[..s.iter().position(|&c| c == 0)?])
}

/// An entry of a table: a name and its records.
struct Entry {
    strp: u32,
    /// The records' DIE offsets.
    dies: Vec<u32>,
    /// The records' other atoms, as they are.
    atoms: Vec<u8>,
}

/// The params and streams ([`STREAM_NAMES`]) of the table `d` of the strings `strs`, or `None`
/// where it is not one this module rebuilds as it is.
pub fn encode(d: &[u8], strs: &[u8]) -> Option<(Vec<u8>, [Vec<u8>; 4])> {
    let h = Header::parse(d)?;
    let rest: usize = h.atoms.iter().sum();
    let (nb, nh) = (h.buckets as usize, h.hashes as usize);
    let offsets = h.bytes.len() + 4 * nb + 4 * nh;
    if d.len() < offsets + 4 * nh {
        return None;
    }
    let mut entries = Vec::new();
    for i in 0..nh {
        let mut at = u32le(d, offsets + 4 * i)? as usize;
        loop {
            let strp = u32le(d, at)?;
            at += 4;
            if strp == 0 {
                break;
            }
            let n = u32le(d, at)? as usize;
            at += 4;
            let records = d.get(at..at.checked_add(n.checked_mul(4 + rest)?)?)?;
            at += records.len();
            let mut e = Entry { strp, dies: Vec::with_capacity(n), atoms: Vec::with_capacity(n * rest) };
            for r in records.chunks_exact(4 + rest) {
                e.dies.push(u32::from_le_bytes(*r.first_chunk().unwrap()));
                e.atoms.extend_from_slice(&r[4..]);
            }
            entries.push(e);
        }
    }
    entries.sort_by_key(|e| e.strp);
    let mut st: [Vec<u8>; 4] = Default::default();
    let (mut strp, mut first) = (0u32, 0u32);
    for e in &entries {
        varint::put(&mut st[0], (e.strp - strp) as u64);
        strp = e.strp;
        varint::put(&mut st[1], e.dies.len() as u64);
        let mut prev = first;
        for (k, &die) in e.dies.iter().enumerate() {
            varint::put_signed(&mut st[2], die as i64 - prev as i64);
            if k == 0 {
                first = die;
            }
            prev = die;
        }
        st[3].extend_from_slice(&e.atoms);
    }
    let params = h.bytes.to_vec();
    let inputs: Vec<&[u8]> = [strs].into_iter().chain(st.iter().map(|s| &s[..])).collect();
    (decode(&params, &inputs, d.len() as u64).ok()? == d).then_some((params, st))
}

/// Entries and records of the streams `strps` and `counts`: a varint of the first for each
/// entry, of the second its records. `None` where the counts do not decode.
fn counts(strps: &[u8], counts: &[u8]) -> Option<(usize, u64)> {
    let entries = strps.iter().filter(|&&b| b < 0x80).count();
    let mut at = 0;
    let mut records = 0u64;
    while at < counts.len() {
        records = records.checked_add(varint::get(counts, &mut at).ok()?)?;
    }
    Some((entries, records))
}

/// Bytes of an entry on WebAssembly, for the plan.
const ENTRY_BYTES: u64 = 16;

/// The memory plan of [`decode`] ([`aqr_api::stack`]): what it takes beyond its initial memory
/// at most, of `params`, inputs of `inputs` bytes, of which `strps` and `counts` are the first
/// two streams, and `len` bytes of output: what `aqr_api::whole` copies, the atoms' sizes, the
/// entries, their records' DIE offsets and ends, two orders of the entries, and the counts of
/// the sort or the output. `None` for params or inputs it does not take.
pub fn plan(params: &[u8], inputs: &[u64], strps: &[u8], counts_in: &[u8], len: u64) -> Option<u64> {
    use aqr_api::stack::{block, whole};
    let h = Header::parse(params).filter(|h| h.bytes.len() == params.len())?;
    let (entries, records) = counts(strps, counts_in)?;
    let e = entries as u64;
    let sort = block(4 * ((1u64 << 16).max(h.buckets as u64) + 1));
    Some(
        whole(params.len() as u64, &[], inputs)
            + block(4 * h.atoms.len() as u64)
            + block(ENTRY_BYTES * e)
            + block(4 * records)
            + block(4 * e)
            + 2 * block(4 * e)
            + sort.max(block(len)),
    )
}

/// The table of `len` bytes from `params` (its header) and `inputs` (`.debug_str`, then the
/// streams of [`STREAM_NAMES`]).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let h = Header::parse(params).filter(|h| h.bytes.len() == params.len()).ok_or_else(corrupt)?;
    let &[strs, strps, counts_in, dies_in, atoms] = inputs else { return Err(corrupt()) };
    let len = usize::try_from(len).ok().filter(|&l| l <= u32::MAX as usize).ok_or_else(corrupt)?;
    let rest: usize = h.atoms.iter().sum();
    let get = |d: &[u8], at: &mut usize| varint::get(d, at).map_err(|_| corrupt());
    // the entries, by their strings: (bucket, hash, string, first record); their DIE offsets
    let (n_entries, n_records) = counts(strps, counts_in).ok_or_else(corrupt)?;
    // (each record is at least a byte of `dies_in`)
    if n_records > dies_in.len() as u64 {
        return Err(corrupt());
    }
    let (mut at, mut ac, mut ad) = (0, 0, 0);
    let (mut strp, mut first) = (0u32, 0u32);
    let (mut entries, mut dies) = (Vec::with_capacity(n_entries), Vec::with_capacity(n_records as usize));
    let nb = h.buckets;
    while at < strps.len() {
        strp = u32::try_from((strp as u64).checked_add(get(strps, &mut at)?).ok_or_else(corrupt)?)
            .map_err(|_| corrupt())?;
        let n = get(counts_in, &mut ac)?;
        if n > (dies_in.len() - ad) as u64 || nb == 0 {
            return Err(corrupt());
        }
        let hash = djb(string(strs, strp).ok_or_else(corrupt)?);
        entries.push((hash % nb, hash, strp, dies.len() as u32));
        let mut prev = first;
        for k in 0..n {
            let die = (prev as i64).wrapping_add(varint::get_signed(dies_in, &mut ad).map_err(|_| corrupt())?);
            let die = u32::try_from(die).map_err(|_| corrupt())?;
            if k == 0 {
                first = die;
            }
            prev = die;
            dies.push(die);
        }
    }
    if ac != counts_in.len() || ad != dies_in.len() || dies.len() * rest != atoms.len() {
        return Err(corrupt());
    }
    // the records of entry `k`
    let mut ends: Vec<u32> = Vec::with_capacity(entries.len());
    ends.extend(entries.iter().skip(1).map(|e| e.3).chain([dies.len() as u32]));
    let records = |k: usize| entries[k].3 as usize..ends[k] as usize;
    // (by hash, then by bucket, each pass stable: entries of one string keep their order)
    if 4 * nb as u64 > len as u64 {
        return Err(corrupt());
    }
    let mut order: Vec<u32> = Vec::with_capacity(entries.len());
    order.extend(0..entries.len() as u32);
    let mut other = vec![0; entries.len()];
    let mut at = vec![0u32; (1 << 16).max(nb as usize) + 1];
    counting(&order, &mut other, &mut at, |k| entries[k as usize].1 as usize & 0xffff, 1 << 16);
    counting(&other, &mut order, &mut at, |k| (entries[k as usize].1 >> 16) as usize, 1 << 16);
    counting(&order, &mut other, &mut at, |k| entries[k as usize].0 as usize, nb as usize);
    drop(at);
    let order = other;
    let same = |a: &u32, b: &u32| entries[*a as usize].1 == entries[*b as usize].1;
    let nh = order.chunk_by(same).count();
    if nh != h.hashes as usize {
        return Err(corrupt());
    }
    let nb = nb as usize;
    let data_at = h.bytes.len() + 4 * nb + 8 * nh;
    if data_at + entries.len() * 8 + dies.len() * (4 + rest) + 4 * nh != len {
        return Err(corrupt());
    }
    let mut out = Vec::with_capacity(len);
    out.extend_from_slice(h.bytes);
    out.resize(data_at, 0);
    let put = |out: &mut Vec<u8>, at: usize, v: u32| out[at..at + 4].copy_from_slice(&v.to_le_bytes());
    for b in 0..nb {
        put(&mut out, h.bytes.len() + 4 * b, u32::MAX);
    }
    let (hashes, offsets) = (h.bytes.len() + 4 * nb, h.bytes.len() + 4 * nb + 4 * nh);
    for (i, g) in order.chunk_by(same).enumerate() {
        let e = &entries[g[0] as usize];
        let b = h.bytes.len() + 4 * e.0 as usize;
        if out[b..b + 4] == [0xff; 4] {
            put(&mut out, b, i as u32);
        }
        put(&mut out, hashes + 4 * i, e.1);
        let o = out.len() as u32;
        put(&mut out, offsets + 4 * i, o);
        for &k in g {
            let r = records(k as usize);
            out.extend_from_slice(&entries[k as usize].2.to_le_bytes());
            out.extend_from_slice(&(r.len() as u32).to_le_bytes());
            for j in r {
                out.extend_from_slice(&dies[j].to_le_bytes());
                out.extend_from_slice(&atoms[j * rest..][..rest]);
            }
        }
        out.extend_from_slice(&0u32.to_le_bytes());
    }
    Ok(out)
}

/// `order` sorted by `key` (below `range`) into `out`, stably, with the counts `at` (more than
/// `range` of them).
fn counting(order: &[u32], out: &mut [u32], at: &mut [u32], key: impl Fn(u32) -> usize, range: usize) {
    let at = &mut at[..range + 1];
    at.fill(0);
    for &k in order {
        at[key(k) + 1] += 1;
    }
    for i in 1..=range {
        at[i] += at[i - 1];
    }
    for &k in order {
        let a = &mut at[key(k)];
        out[*a as usize] = k;
        *a += 1;
    }
}
