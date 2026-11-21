//! ZIP archives: `coding`, each deflated entry's DEFLATE stream made again ([`super::redeflate`]),
//! the sizes and offsets that follow written again (local headers, data descriptors, central
//! headers, the zip64 records and the end record, each with its zip64 slots). The CRC-32 and the
//! uncompressed size, the method and the flags (the level hint of bits 1-2 too) are kept as they
//! are. Only an archive laid out plainly is taken: from its start, local entries, the central
//! directory, the zip64 records if any, the end record, with nothing between them. Encrypted
//! entries, methods but stored and deflated, and signatures (APK's signing block, MSIX's files)
//! are turned down.

use std::collections::HashMap;

use aqr_zip::zip::{self, Desc, Local, Rec};

use crate::Options;

/// The APK signing block's magic, which ends the block (it comes before the central directory).
const APK_MAGIC: &[u8; 16] = b"APK Sig Block 42";

/// Flags of an encrypted entry: bit 0, strong encryption (bit 6), masked local headers (bit 13).
const ENCRYPTED_BITS: u16 = 1 | 1 << 6 | 1 << 13;

/// `d` with its deflated entries' streams made again, None where none is; why it is kept where it
/// is not laid out plainly or is signed or encrypted.
pub(super) fn coding(d: &[u8], opt: &Options) -> Result<Option<Vec<u8>>, String> {
    super::rebuilds(opt)?;
    let (base, _, recs) = zip::records(d);
    // (an APK's signing block is a gap before the central directory)
    if recs.iter().any(|r| matches!(r, Rec::Gap(g) if g.end >= 16 && d[g.end - 16..g.end] == APK_MAGIC[..])) {
        return Err("signed (APK signing block)".into());
    }
    if base != 0 {
        return Err("data before the archive".into());
    }

    // the records in order
    let (mut locals, mut centrals) = (Vec::new(), Vec::new());
    let (mut end64, mut locator, mut end) = (None, None, None);
    let mut stage = 0;
    for r in &recs {
        let to = match r {
            Rec::Gap(g) if g.start == 0 => return Err("data before the archive".into()),
            Rec::Gap(g) if g.end == d.len() => return Err("data after the archive".into()),
            Rec::Gap(_) => return Err("data between the records".into()),
            Rec::Local(_) => 0,
            Rec::Central(_) => 1,
            Rec::End64 { .. } => 2,
            Rec::Locator { .. } => 3,
            Rec::End { .. } => 4,
        };
        if to < stage || (to == stage && to >= 2) {
            return Err("records out of order".into());
        }
        stage = to;
        match r {
            Rec::Local(l) => locals.push(l),
            Rec::Central(c) => centrals.push(c),
            Rec::End64 { pos, .. } => end64 = Some(*pos),
            Rec::Locator { pos } => locator = Some(*pos),
            Rec::End { pos, .. } => end = Some(*pos),
            Rec::Gap(_) => unreachable!(),
        }
    }
    end.ok_or("no end of central directory")?;
    if end64.is_some() != locator.is_some() {
        return Err("zip64 records not a pair".into());
    }
    if locals.len() != centrals.len() {
        return Err("entries not as many as the central directory's".into());
    }
    if locals.is_empty() {
        return Ok(None);
    }

    // every central header points to a local entry
    let by_pos: HashMap<usize, usize> = locals.iter().enumerate().map(|(i, l)| (l.pos, i)).collect();
    let mut of_local = vec![usize::MAX; locals.len()];
    for (ci, c) in centrals.iter().enumerate() {
        let (off, info) = c.info(d);
        let li = usize::try_from(off).ok().and_then(|o| by_pos.get(&o)).ok_or("a central header not of a local one")?;
        if of_local[*li] != usize::MAX {
            return Err("two central headers of one local entry".into());
        }
        of_local[*li] = ci;
        let l = locals[*li];
        let (cf, lf) = (zip::u16_at(d, c.pos + 8), l.flags(d));
        if cf & ENCRYPTED_BITS != 0 || lf & ENCRYPTED_BITS != 0 {
            return Err("encrypted".into());
        }
        let method = zip::u16_at(d, c.pos + 10);
        if method != l.method(d) {
            return Err("a method of the local and central headers differ".into());
        }
        if method != zip::STORED && method != zip::DEFLATED {
            return Err(format!("method {method} (only stored and deflated)"));
        }
        if matches!(&d[l.name.clone()], b"AppxSignature.p7x" | b"AppxBlockMap.xml")
            || matches!(&d[c.name.clone()], b"AppxSignature.p7x" | b"AppxBlockMap.xml")
        {
            return Err("signed (MSIX/AppX)".into());
        }
        if info.csize != l.body.len() as u64 {
            return Err("a central header not of its body's size".into());
        }
        // (the local header's size is the body's, or 0 where a descriptor tells it)
        let [_, lc] = local_sizes(d, l);
        if lc != l.body.len() as u64 && !(lc == 0 && lf & zip::DESCRIPTOR != 0) {
            return Err("a local header not of its body's size".into());
        }
    }

    // the streams made again
    let mut news: Vec<Option<Vec<u8>>> = Vec::with_capacity(locals.len());
    for l in &locals {
        news.push(if l.method(d) == zip::DEFLATED { super::redeflate(&d[l.body.clone()], opt) } else { None });
    }
    if news.iter().all(Option::is_none) {
        return Ok(None);
    }
    let new_len = |i: usize| news[i].as_ref().map_or(locals[i].body.len(), Vec::len);
    let first_central = centrals[0].pos;

    // R, record by record
    let mut r = Vec::with_capacity(d.len());
    let mut local_at = vec![0usize; locals.len()];
    let (mut cd_start, mut end64_at) = (0, 0);
    let mut li = 0;
    for rec in &recs {
        match rec {
            Rec::Local(l) => {
                local_at[li] = r.len();
                let n = new_len(li) as u64;
                let flags = l.flags(d);
                let start = r.len();
                r.extend_from_slice(&d[l.pos..l.body.start]);
                let keep = flags & zip::DESCRIPTOR != 0 && local_sizes(d, l)[1] == 0;
                let extra = l.extra.start - l.pos..l.extra.end - l.pos;
                // (usize at 22, csize at 18)
                patch(&mut r[start..], extra, &[(22, None), (18, (!keep).then_some(n))])?;
                match &news[li] {
                    Some(new) => r.extend_from_slice(new),
                    None => r.extend_from_slice(&d[l.body.clone()]),
                }
                let at = r.len();
                r.extend_from_slice(&d[l.body.end..l.end()]);
                if l.desc != Desc::None {
                    let f = at + if l.desc.sig() { 4 } else { 0 } + 4;
                    if l.desc.long() {
                        r[f..f + 8].copy_from_slice(&n.to_le_bytes());
                    } else {
                        let n32 = u32::try_from(n).map_err(|_| "sizes past zip64's reach")?;
                        r[f..f + 4].copy_from_slice(&n32.to_le_bytes());
                    }
                }
                li += 1;
            }
            Rec::Central(c) => {
                if cd_start == 0 {
                    cd_start = r.len();
                }
                let (off, _) = c.info(d);
                let i = by_pos[&(off as usize)];
                let start = r.len();
                r.extend_from_slice(&d[c.pos..c.comment.end]);
                let extra = c.extra.start - c.pos..c.extra.end - c.pos;
                // (usize at 24, csize at 20, offset at 42)
                let fields = [(24, None), (20, Some(new_len(i) as u64)), (42, Some(local_at[i] as u64))];
                patch(&mut r[start..], extra, &fields)?;
            }
            Rec::End64 { pos, ext } => {
                end64_at = r.len();
                if zip::u64_at(d, pos + 48) != first_central as u64 {
                    return Err("the directory's offset not where it is".into());
                }
                r.extend_from_slice(&d[*pos..ext.end]);
                r[end64_at + 48..end64_at + 56].copy_from_slice(&(cd_start as u64).to_le_bytes());
            }
            Rec::Locator { pos } => {
                if Some(zip::u64_at(d, pos + 8)) != end64.map(|p| p as u64) {
                    return Err("the zip64 end record's offset not where it is".into());
                }
                let at = r.len();
                r.extend_from_slice(&d[*pos..rec.end()]);
                r[at + 8..at + 16].copy_from_slice(&(end64_at as u64).to_le_bytes());
            }
            Rec::End { pos, comment } => {
                let at = r.len();
                r.extend_from_slice(&d[*pos..comment.end]);
                let off = zip::u32_at(d, pos + 16);
                if off != u32::MAX {
                    if off as usize != first_central {
                        return Err("the directory's offset not where it is".into());
                    }
                    let n =
                        u32::try_from(cd_start).ok().filter(|&n| n != u32::MAX).ok_or("sizes past zip64's reach")?;
                    r[at + 16..at + 20].copy_from_slice(&n.to_le_bytes());
                }
            }
            Rec::Gap(_) => unreachable!(),
        }
    }
    Ok(Some(r))
}

/// The uncompressed and compressed sizes of the local header of `l`, with its zip64 slots.
fn local_sizes(d: &[u8], l: &Local) -> [u64; 2] {
    let f = l.fixed(d);
    let z = zip::zip64(&d[l.extra.clone()]).map_or(&[][..], |r| &d[l.extra.start + r.start..l.extra.start + r.end]);
    zip::resolve([zip::u32_at(f, 18), zip::u32_at(f, 14)], z)
}

/// The 32-bit size or offset fields of the header `rec` (`fields`: where each is in it, in the
/// order of zip64 slots, with the value it takes where it is to change) set; a field of all ones
/// is set in its zip64 slot of `extra` instead (the range of the extra field in `rec`).
fn patch(rec: &mut [u8], extra: std::ops::Range<usize>, fields: &[(usize, Option<u64>)]) -> Result<(), String> {
    let z = zip::zip64(&rec[extra.clone()]).map(|r| extra.start + r.start..extra.start + r.end);
    let mut at = 0;
    for &(off, new) in fields {
        let v = zip::u32_at(rec, off);
        match &z {
            Some(z) if v == u32::MAX && z.start + at + 8 <= z.end => {
                let s = z.start + at;
                at += 8;
                if let Some(n) = new {
                    rec[s..s + 8].copy_from_slice(&n.to_le_bytes());
                }
            }
            _ => {
                if let Some(n) = new {
                    let n = u32::try_from(n).ok().filter(|&n| n != u32::MAX).ok_or("sizes past zip64's reach")?;
                    rec[off..off + 4].copy_from_slice(&n.to_le_bytes());
                }
            }
        }
    }
    Ok(())
}
