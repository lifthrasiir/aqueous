//! The free bytes of a volume of NTFS: the clusters that the cluster bitmap (`$Bitmap`, record 6
//! of the `$MFT`) tells free. Read by a parser of its own (not aqr-ntfs's), so that this and
//! aqr-disk's listing of the files are independent reads.
//!
//! The volume's clusters are its total sectors (the boot sector's) over the sectors of a
//! cluster; what lies after them (the backup boot sector in the last sector, a partial cluster)
//! and the boot sector are never free. Nothing else of the structures is trusted blindly: every
//! record of the `$MFT` is read (update sequence fixups checked), and a free cluster in any run
//! of an in-use record's non-resident attributes (so of `$MFT`, `$MFTMirr`, `$LogFile`,
//! `$Bitmap`, `$Boot`, `$BadClus`, and the files') turns the volume down. File tails past their
//! sizes (allocated, not initialized) are left alone: only free clusters are told.

use std::ops::Range;

const USA_STRIDE: usize = 512;
/// The most that a record is (NTFS writes 1 KiB, or 4 KiB on 4Kn disks).
const RECORD_MAX: usize = 65536;

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at.checked_add(2)?)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at.checked_add(8)?)?.try_into().ok()?))
}

/// A run of a non-resident attribute: `len` clusters from `lcn`, None where sparse.
#[derive(Clone, Copy, Debug)]
struct Run {
    lcn: Option<u64>,
    len: u64,
}

/// The geometry of a volume.
struct Geometry {
    cluster: usize,
    clusters: u64,
    mft: u64,
}

/// The runs of a runlist (`list`, of an attribute whose `count` clusters it tells), the LCNs
/// all within `clusters`.
fn runs(list: &[u8], count: u64, clusters: u64) -> Result<Vec<Run>, String> {
    const BAD: &str = "a damaged run list";
    let (mut out, mut at, mut lcn, mut total) = (Vec::new(), 0usize, 0i64, 0u64);
    loop {
        let head = *list.get(at).ok_or(BAD)?;
        at += 1;
        if head == 0 {
            break;
        }
        let (ls, os) = (usize::from(head & 15), usize::from(head >> 4));
        if ls == 0 || ls > 8 || os > 8 {
            return Err(BAD.into());
        }
        let lb = list.get(at..at + ls).ok_or(BAD)?;
        let len = lb.iter().rev().fold(0u64, |a, &b| a << 8 | u64::from(b));
        at += ls;
        if len == 0 || len > clusters {
            return Err(BAD.into());
        }
        let l = if os == 0 {
            None
        } else {
            let ob = list.get(at..at + os).ok_or(BAD)?;
            at += os;
            let mut d = ob.iter().rev().fold(0i64, |a, &b| a << 8 | i64::from(b));
            if ob[os - 1] & 0x80 != 0 && os < 8 {
                d -= 1i64 << (8 * os);
            }
            lcn = lcn.checked_add(d).ok_or(BAD)?;
            let l = u64::try_from(lcn).map_err(|_| BAD)?;
            if l.checked_add(len).is_none_or(|e| e > clusters) {
                return Err("a run outside the volume".into());
            }
            Some(l)
        };
        total = total.checked_add(len).ok_or(BAD)?;
        out.push(Run { lcn: l, len });
    }
    if total != count {
        return Err("run lists not of their attributes' lengths".into());
    }
    Ok(out)
}

/// An attribute of a record: its type, name length, and (non-resident) its runs, or (resident)
/// its value.
struct Attr<'a> {
    kind: u32,
    named: bool,
    /// The VCN it starts at, and its real size, of a non-resident one.
    start: u64,
    real: u64,
    body: Body<'a>,
}

enum Body<'a> {
    Resident(&'a [u8]),
    Runs(Vec<Run>),
}

/// A record of the `$MFT`, fixups undone: whether it is in use, and its attributes. None for a
/// record of zeros (never used).
fn parse_record<'a>(rec: &'a mut [u8], g: &Geometry) -> Result<Option<(bool, Vec<Attr<'a>>)>, String> {
    if rec.iter().all(|&b| b == 0) {
        return Ok(None);
    }
    if &rec[..4] != b"FILE" {
        return Err("a record of the MFT not of FILE".into());
    }
    let n = rec.len() / USA_STRIDE;
    let (usa, count) = (usize::from(u16_at(rec, 4).unwrap()), usize::from(u16_at(rec, 6).unwrap()));
    if count != n + 1 || usa < 0x2A || usa % 2 != 0 || usa + 2 * count > USA_STRIDE - 2 {
        return Err("a record with a damaged update sequence array".into());
    }
    let usn = [rec[usa], rec[usa + 1]];
    for i in 1..=n {
        let end = i * USA_STRIDE;
        if rec[end - 2..end] != usn {
            return Err("a record failing its fixup".into());
        }
        let fix = [rec[usa + 2 * i], rec[usa + 2 * i + 1]];
        rec[end - 2..end].copy_from_slice(&fix);
    }
    let rec: &'a [u8] = rec;
    let used = u32_at(rec, 0x18).unwrap() as usize;
    let in_use = u16_at(rec, 0x16).unwrap() & 1 != 0;
    if !in_use {
        return Ok(Some((false, Vec::new())));
    }
    if used > rec.len() || used < 0x30 {
        return Err("a record with inconsistent sizes".into());
    }
    let (body, mut at) = (&rec[..used], usize::from(u16_at(rec, 0x14).unwrap()));
    let mut attrs = Vec::new();
    loop {
        let kind = u32_at(body, at).ok_or("an attribute past its record")?;
        if kind == 0xFFFF_FFFF {
            break;
        }
        let len = u32_at(body, at + 4).ok_or("an attribute past its record")? as usize;
        let a = body.get(at..at.checked_add(len).ok_or("an attribute past its record")?);
        let a = a.filter(|_| len >= 0x18 && len.is_multiple_of(8)).ok_or("an attribute of a bad length")?;
        let (nonres, name) = (a[8] != 0, a[9] != 0);
        let attr = if !nonres {
            let (vl, vo) = (u32_at(a, 0x10).unwrap() as usize, usize::from(u16_at(a, 0x14).unwrap()));
            let v = a.get(vo..vo.checked_add(vl).ok_or("a damaged value")?).ok_or("a value past its attribute")?;
            Attr { kind, named: name, start: 0, real: vl as u64, body: Body::Resident(v) }
        } else {
            if len < 0x40 {
                return Err("a short non-resident attribute".into());
            }
            let (first, last) = (u64_at(a, 0x10).unwrap(), u64_at(a, 0x18).unwrap() as i64);
            let ro = usize::from(u16_at(a, 0x20).unwrap());
            let count = (last + 1).checked_sub(first as i64).and_then(|c| u64::try_from(c).ok());
            let count = count.ok_or("an attribute of damaged VCNs")?;
            let list = a.get(ro..).filter(|_| ro >= 0x40).ok_or("a run list outside its attribute")?;
            let real = u64_at(a, 0x30).unwrap();
            Attr { kind, named: name, start: first, real, body: Body::Runs(runs(list, count, g.clusters)?) }
        };
        attrs.push(attr);
        at += len;
    }
    Ok(Some((true, attrs)))
}

/// `len` bytes from byte `at` of a non-resident attribute of `runs`, which must be all of clusters.
fn read(d: &[u8], g: &Geometry, runs: &[Run], at: u64, len: usize) -> Result<Vec<u8>, String> {
    let cs = g.cluster as u64;
    let mut out = Vec::with_capacity(len);
    let (mut pos, end) = (at, at + len as u64);
    let mut base = 0u64;
    for r in runs {
        let span = base..base + r.len * cs;
        if pos < span.end && pos < end {
            let l = r.lcn.ok_or("a sparse run where bytes are read")?;
            let (from, to) = (pos - span.start, end.min(span.end) - span.start);
            let o = (l * cs + from) as usize;
            out.extend_from_slice(d.get(o..o + (to - from) as usize).ok_or("a run outside the volume")?);
            pos = span.start + to;
        }
        base = span.end;
    }
    if pos != end {
        return Err("bytes past the runs of an attribute".into());
    }
    Ok(out)
}

/// Whether any cluster of `lcn..lcn + len` is free in `bitmap`.
fn any_free(bitmap: &[u8], lcn: u64, len: u64) -> bool {
    let (mut i, end) = (lcn, lcn + len);
    while i < end {
        if i % 8 == 0 && end - i >= 8 {
            if bitmap[(i / 8) as usize] != 0xFF {
                return true;
            }
            i += 8;
        } else {
            if bitmap[(i / 8) as usize] >> (i % 8) & 1 == 0 {
                return true;
            }
            i += 1;
        }
    }
    false
}

/// The free bytes of the NTFS volume `d` (relative, in order, apart); why not where it is not
/// to be trusted.
pub(super) fn free(d: &[u8]) -> Result<Vec<Range<usize>>, String> {
    let boot = d.get(..512).ok_or("shorter than a boot sector")?;
    if &boot[3..11] != b"NTFS    " || boot[510..] != [0x55, 0xAA] {
        return Err("not a boot sector of NTFS".into());
    }
    let bps = usize::from(u16_at(boot, 0x0B).unwrap());
    if !(512..=4096).contains(&bps) || !bps.is_power_of_two() {
        return Err("an odd sector size".into());
    }
    let spc = match boot[0x0D] {
        0 => return Err("no sectors in a cluster".into()),
        n @ 1..=0x80 if n.is_power_of_two() => usize::from(n),
        n if n >= 0xF0 => 1usize << (256 - usize::from(n)),
        _ => return Err("an odd cluster size".into()),
    };
    let cluster = bps * spc;
    let sectors = u64_at(boot, 0x28).unwrap();
    if sectors == 0 || sectors.checked_mul(bps as u64).is_none_or(|b| b > d.len() as u64) {
        return Err("a volume larger than its bytes".into());
    }
    let clusters = sectors / spc as u64;
    let record = match boot[0x40] as i8 {
        n if n > 0 => usize::from(n as u8) * cluster,
        n if n >= -16 => 1usize << -n,
        _ => return Err("an odd record size".into()),
    };
    if record % USA_STRIDE != 0 || record > RECORD_MAX {
        return Err("an odd record size".into());
    }
    let mft = u64_at(boot, 0x30).unwrap();
    if mft.checked_mul(cluster as u64).and_then(|o| o.checked_add(record as u64)).is_none_or(|e| e > d.len() as u64)
        || mft >= clusters
    {
        return Err("the MFT outside the volume".into());
    }
    let g = Geometry { cluster, clusters, mft };

    // Record 0: the $MFT's runs.
    let o = (g.mft * cluster as u64) as usize;
    let mut r0 = d[o..o + record].to_vec();
    let (_, attrs0) = parse_record(&mut r0, &g)?.filter(|(u, _)| *u).ok_or("record 0 not in use")?;
    if attrs0.iter().any(|a| a.kind == 0x20) {
        return Err("an attribute list in the record of the MFT".into());
    }
    let data = attrs0.iter().find(|a| a.kind == 0x80 && !a.named).ok_or("no data in record 0")?;
    let (Body::Runs(mruns), 0) = (&data.body, data.start) else {
        return Err("the MFT's data resident or not from its start".into());
    };
    if mruns.iter().any(|r| r.lcn.is_none()) || data.real % record as u64 != 0 {
        return Err("the MFT's data sparse or not of whole records".into());
    }
    let count = data.real / record as u64;
    if count < 16 || mruns.iter().map(|r| r.len).sum::<u64>() * (cluster as u64) < data.real {
        return Err("a short MFT".into());
    }
    if count > d.len() as u64 / record as u64 {
        return Err("an MFT larger than the volume".into());
    }

    // Every record: all the runs of the in-use ones; $Bitmap's data.
    let mut all: Vec<Run> = Vec::new();
    let mut bitmap: Option<Vec<u8>> = None;
    let need = clusters.div_ceil(8) as usize;
    for n in 0..count {
        let mut rec = read(d, &g, mruns, n * record as u64, record)?;
        let Some((used, attrs)) = parse_record(&mut rec, &g)? else { continue };
        if !used {
            if matches!(n, 0 | 6) {
                return Err("a system file not in use".into());
            }
            continue;
        }
        if attrs.iter().any(|a| a.kind == 0x20) && (n == 0 || n == 6) {
            return Err("an attribute list in a system file's record".into());
        }
        for a in &attrs {
            if let Body::Runs(rs) = &a.body {
                all.extend(rs.iter().filter(|r| r.lcn.is_some()));
            }
            if n == 6 && a.kind == 0x80 && !a.named {
                if bitmap.is_some() {
                    return Err("two bitmaps".into());
                }
                bitmap = Some(match &a.body {
                    Body::Resident(v) => v.to_vec(),
                    Body::Runs(rs) => {
                        if a.start != 0 || a.real < need as u64 {
                            return Err("a bitmap shorter than the clusters".into());
                        }
                        read(d, &g, rs, 0, need)?
                    }
                });
            }
        }
    }
    let bitmap = bitmap.ok_or("no bitmap")?;
    if bitmap.len() < need {
        return Err("a bitmap shorter than the clusters".into());
    }
    if all.iter().any(|r| any_free(&bitmap, r.lcn.unwrap(), r.len)) {
        return Err("a free cluster in a file's runs".into());
    }

    // The free clusters, as byte ranges.
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut c = 0u64;
    while c < clusters {
        if c.is_multiple_of(8) && bitmap[(c / 8) as usize] == 0xFF {
            c += 8;
            continue;
        }
        if bitmap[(c / 8) as usize] >> (c % 8) & 1 == 0 {
            let (s, e) = (c as usize * cluster, (c as usize + 1) * cluster);
            match out.last_mut() {
                Some(l) if l.end == s => l.end = e,
                _ => out.push(s..e),
            }
        }
        c += 1;
    }
    Ok(out)
}
