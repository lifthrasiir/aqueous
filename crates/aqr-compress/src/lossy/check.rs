//! Whether a file rewritten (R) is what it was (X), for each rewriter of [`super::REWRITERS`]: by
//! code of its own, not the rewriter's (its parsers written again here, DEFLATE inflated by
//! flate2's zlib-rs rather than aqr-flate), so that a bug of one is not passed by the other.
//! Each compares what the format's readers see (its view, docs/lossy-design.md §1), streaming
//! the data of compressed streams rather than holding it.

use std::io::Read;

/// Whether the data of the raw DEFLATE streams at the starts of `x` and `r` is the same, and
/// the bytes of each stream (where it ends in `x` and in `r`); an error where either does not
/// inflate.
pub(super) fn same_deflate(x: &[u8], r: &[u8]) -> Result<(bool, usize, usize), String> {
    let mut a = flate2::bufread::DeflateDecoder::new(x);
    let mut b = flate2::bufread::DeflateDecoder::new(r);
    let same = same_read(&mut a, &mut b)?;
    Ok((same, a.total_in() as usize, b.total_in() as usize))
}

/// Whether `a` and `b` read the same bytes to their ends.
fn same_read(a: &mut dyn Read, b: &mut dyn Read) -> Result<bool, String> {
    let (mut x, mut y) = (vec![0u8; 1 << 16], vec![0u8; 1 << 16]);
    loop {
        let n = fill(a, &mut x).map_err(|e| format!("the original does not inflate: {e}"))?;
        let m = fill(b, &mut y).map_err(|e| format!("the file rewritten does not inflate: {e}"))?;
        if x[..n] != y[..m] {
            return Ok(false);
        }
        if n < x.len() {
            return Ok(true);
        }
    }
}

/// `buf` filled from `r` as far as it goes: short only at its end.
fn fill(r: &mut dyn Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

/// A region file: of the same length and header, every chunk's bytes (its length, compression
/// type and data, as its location and length tell them) the same, and every other byte the same
/// or zero.
pub(super) fn mca(x: &[u8], r: &[u8]) -> Result<(), String> {
    const SECTOR: usize = 4096;
    if x.len() != r.len() {
        return Err("of another length".into());
    }
    if x.len() < 2 * SECTOR || x[..2 * SECTOR] != r[..2 * SECTOR] {
        return Err("of another header".into());
    }
    let mut live: Vec<(usize, usize)> = vec![(0, 2 * SECTOR)];
    for i in 0..1024 {
        let loc = u32::from_be_bytes(x[4 * i..4 * i + 4].try_into().unwrap());
        let (sector, count) = ((loc >> 8) as usize, (loc & 0xff) as usize);
        if sector < 2 || count == 0 {
            continue;
        }
        let at = sector * SECTOR;
        let len = x.get(at..at + 4).ok_or(format!("chunk {i} past the end"))?;
        let end = at + 4 + u32::from_be_bytes(len.try_into().unwrap()) as usize;
        if end > at + count * SECTOR || end > x.len() {
            return Err(format!("chunk {i} longer than its sectors"));
        }
        live.push((at, end));
    }
    live.sort_unstable();
    let mut p = 0;
    for (start, end) in live {
        if r[p.max(start)..p.max(end)] != x[p.max(start)..p.max(end)] {
            return Err("a chunk changed".into());
        }
        if start > p && r[p..start].iter().zip(&x[p..start]).any(|(&a, &b)| a != 0 && a != b) {
            return Err("bytes between chunks changed but to zeros".into());
        }
        p = p.max(end);
    }
    if r[p..].iter().zip(&x[p..]).any(|(&a, &b)| a != 0 && a != b) {
        return Err("bytes after the chunks changed but to zeros".into());
    }
    Ok(())
}

/// Bytes of the header of the gzip member at the start of `d` (RFC 1952), where it is one.
fn gzip_header(d: &[u8]) -> Option<usize> {
    if d.len() < 10 || d[..3] != [0x1f, 0x8b, 8] || d[3] & 0xe0 != 0 {
        return None;
    }
    let flg = d[3];
    let mut p = 10;
    if flg & 4 != 0 {
        p += 2 + u16::from_le_bytes([*d.get(p)?, *d.get(p + 1)?]) as usize;
    }
    for bit in [8, 16] {
        if flg & bit != 0 {
            p += d.get(p..)?.iter().position(|&b| b == 0)? + 1;
        }
    }
    if flg & 2 != 0 {
        p += 2;
    }
    (p <= d.len()).then_some(p)
}

/// gzip files: of the same members, each of the same header and trailer, and of the same data,
/// its CRC-32 and size those of the trailer.
pub(super) fn gzip(x: &[u8], r: &[u8]) -> Result<(), String> {
    let (mut p, mut q) = (0, 0);
    let mut member = 0;
    while p < x.len() || q < r.len() {
        let h = gzip_header(&x[p..]).ok_or(format!("member {member} of the original not a gzip member"))?;
        if r.get(q..q + h) != Some(&x[p..p + h]) {
            return Err(format!("member {member} of another header"));
        }
        let (same, a, b) = same_deflate(&x[p + h..], &r[q + h..])?;
        if !same {
            return Err(format!("member {member} of other data"));
        }
        let (tx, tr) = (p + h + a, q + h + b);
        let trailer = x.get(tx..tx + 8).ok_or(format!("member {member} cut short"))?;
        if r.get(tr..tr + 8) != Some(trailer) {
            return Err(format!("member {member} of another trailer"));
        }
        // (the trailer of the data as it inflates)
        let mut crc = flate2::Crc::new();
        let mut dec = flate2::bufread::DeflateDecoder::new(&r[q + h..tr]);
        let mut buf = vec![0u8; 1 << 16];
        loop {
            match fill(&mut dec, &mut buf).map_err(|e| e.to_string())? {
                0 => break,
                n => crc.update(&buf[..n]),
            }
        }
        if crc.sum().to_le_bytes() != trailer[..4] || crc.amount().to_le_bytes() != trailer[4..] {
            return Err(format!("member {member} of a trailer not of its data"));
        }
        (p, q, member) = (tx + 8, tr + 8, member + 1);
    }
    Ok(())
}

/// A little-endian integer of `n` bytes at `p` of `d`.
fn le(d: &[u8], p: usize, n: usize) -> Result<u64, String> {
    let b = d.get(p..p.checked_add(n).ok_or("a field past the end")?).ok_or("a field past the end")?;
    Ok(b.iter().rev().fold(0u64, |v, &b| v << 8 | b as u64))
}

/// The 32-bit fields of a header at the offsets `fields` of `d`, resolved by the zip64 slots of
/// its extra field `extra` (APPNOTE 4.5.3: each field of all ones takes the next 8 bytes, in the
/// order given): each field's value and the slot's offset where it has one.
fn resolve(d: &[u8], fields: &[usize], extra: std::ops::Range<usize>) -> Result<Vec<(u64, Option<usize>)>, String> {
    let (mut p, mut z) = (extra.start, None);
    while p + 4 <= extra.end {
        let (id, size) = (le(d, p, 2)?, le(d, p + 2, 2)? as usize);
        if p + 4 + size > extra.end {
            break;
        }
        if id == 1 {
            z = Some(p + 4..p + 4 + size);
            break;
        }
        p += 4 + size;
    }
    let mut at = 0;
    fields
        .iter()
        .map(|&f| {
            let v = le(d, f, 4)?;
            match &z {
                Some(z) if v == 0xffff_ffff && z.start + at + 8 <= z.end => {
                    at += 8;
                    Ok((le(d, z.start + at - 8, 8)?, Some(z.start + at - 8)))
                }
                _ => Ok((v, None)),
            }
        })
        .collect()
}

/// The bytes `range` of `d` with the 32-bit fields `fields` (offset, zip64 slot) zeroed.
fn masked(d: &[u8], range: std::ops::Range<usize>, fields: &[(usize, Option<usize>)]) -> Vec<u8> {
    let mut v = d[range.clone()].to_vec();
    for &(f, slot) in fields {
        v[f - range.start..f - range.start + 4].fill(0);
        if let Some(s) = slot {
            v[s - range.start..s - range.start + 8].fill(0);
        }
    }
    v
}

/// A ZIP entry as the central directory tells it, and where its local entry is in a plain layout.
struct Entry {
    /// The central header, and where its offset and compressed size fields are (with slots).
    central: std::ops::Range<usize>,
    cfields: [(usize, Option<usize>); 2],
    flags: u16,
    method: u16,
    crc: u32,
    usize: u64,
    csize: u64,
    /// The local header's offset; the header (its compressed size field with its slot), body and descriptor.
    offset: usize,
    header: std::ops::Range<usize>,
    lfield: (usize, Option<usize>),
    body: std::ops::Range<usize>,
    desc: std::ops::Range<usize>,
}

/// A ZIP archive laid out plainly: local entries one after another from the start, the central
/// directory, the zip64 records if any, the end record; each byte in one of them.
struct Zip {
    entries: Vec<Entry>,
    /// The zip64 end record and locator, the end record with its comment.
    end64: Option<std::ops::Range<usize>>,
    locator: Option<std::ops::Range<usize>>,
    end: std::ops::Range<usize>,
}

impl Zip {
    fn parse(d: &[u8]) -> Result<Zip, String> {
        // the end record from the end, its comment ending with the file
        let last = d.len().checked_sub(22).ok_or("shorter than an end record")?;
        let q = (last.saturating_sub(0xffff)..=last)
            .rev()
            .find(|&q| d[q..q + 4] == *b"PK\x05\x06" && q + 22 + le(d, q + 20, 2).unwrap_or(0) as usize == d.len())
            .ok_or("no end record")?;
        let end = q..d.len();
        let (mut count, mut size, mut off) = (le(d, q + 10, 2)?, le(d, q + 12, 4)?, le(d, q + 16, 4)?);
        let (mut end64, mut locator) = (None, None);
        let mut cd_end = q;
        if q >= 20 && d[q - 20..q - 16] == *b"PK\x06\x07" {
            let e = usize::try_from(le(d, q - 12, 8)?).map_err(|_| "a zip64 end record too far")?;
            if d.get(e..e + 4) != Some(b"PK\x06\x06") || e + 12 + le(d, e + 4, 8)? as usize != q - 20 || q - 20 < e + 56
            {
                return Err("a zip64 end record not just before its locator".into());
            }
            let end_off = le(d, q + 16, 4)?;
            (count, size, off) = (le(d, e + 32, 8)?, le(d, e + 40, 8)?, le(d, e + 48, 8)?);
            if end_off != 0xffff_ffff && end_off != off {
                return Err("the end record's offset not where the directory is".into());
            }
            (end64, locator, cd_end) = (Some(e..q - 20), Some(q - 20..q), e);
        }
        let (off, size) = (usize::try_from(off).map_err(|_| "offset")?, usize::try_from(size).map_err(|_| "size")?);
        if off.checked_add(size) != Some(cd_end) {
            return Err("the central directory not just before the end records".into());
        }
        let mut entries = Vec::new();
        let mut p = off;
        while p < cd_end {
            if d.get(p..p + 4) != Some(b"PK\x01\x02") {
                return Err("not a central header".into());
            }
            let (n, m, k) = (le(d, p + 28, 2)? as usize, le(d, p + 30, 2)? as usize, le(d, p + 32, 2)? as usize);
            let central = p..p + 46 + n + m + k;
            let clen = central.len();
            if central.end > cd_end {
                return Err("a central header past the directory".into());
            }
            let extra = p + 46 + n..p + 46 + n + m;
            // (usize at 24, csize at 20, offset at 42)
            let f = resolve(d, &[p + 24, p + 20, p + 42], extra)?;
            entries.push(Entry {
                central,
                cfields: [(p + 42, f[2].1), (p + 20, f[1].1)],
                flags: le(d, p + 8, 2)? as u16,
                method: le(d, p + 10, 2)? as u16,
                crc: le(d, p + 16, 4)? as u32,
                usize: f[0].0,
                csize: f[1].0,
                offset: usize::try_from(f[2].0).map_err(|_| "an offset too far")?,
                header: 0..0,
                lfield: (0, None),
                body: 0..0,
                desc: 0..0,
            });
            p += clen;
        }
        if entries.len() as u64 != count {
            return Err("entries not as many as the end record says".into());
        }
        // the local entries tile the start of the file up to the directory
        let mut order: Vec<usize> = (0..entries.len()).collect();
        order.sort_by_key(|&i| entries[i].offset);
        let starts: Vec<usize> = order.iter().map(|&i| entries[i].offset).chain([off]).collect();
        let mut p = 0;
        for (k, &i) in order.iter().enumerate() {
            let e = &mut entries[i];
            if e.offset != p || d.get(p..p + 4) != Some(b"PK\x03\x04") {
                return Err("the local entries not one after another from the start".into());
            }
            let (n, m) = (le(d, p + 26, 2)? as usize, le(d, p + 28, 2)? as usize);
            let hend = p + 30 + n + m;
            // (usize at 22, csize at 18)
            let f = resolve(d, &[p + 22, p + 18], p + 30 + n..hend)?;
            let bend = usize::try_from(e.csize)
                .ok()
                .and_then(|c| hend.checked_add(c))
                .filter(|&b| b <= starts[k + 1])
                .ok_or("a body past its entry")?;
            let flags = le(d, p + 6, 2)? as u16;
            if f[1].0 != e.csize && !(f[1].0 == 0 && flags & 8 != 0) {
                return Err("a local header not of its body's size".into());
            }
            // (a descriptor of 12 to 24 bytes, only where the flags say)
            let dl = starts[k + 1] - bend;
            if dl != 0 && !(flags & 8 != 0 && [12, 16, 20, 24].contains(&dl)) {
                return Err("bytes in no record after a body".into());
            }
            if matches!(dl, 16 | 24) && d[bend..bend + 4] != *b"PK\x07\x08" {
                return Err("a descriptor of no form".into());
            }
            e.header = p..hend;
            e.lfield = (p + 18, f[1].1);
            e.body = hend..bend;
            e.desc = bend..starts[k + 1];
            p = starts[k + 1];
        }
        if p != off && !entries.is_empty() {
            return Err("the local entries not up to the directory".into());
        }
        Ok(Zip { entries, end64, locator, end })
    }

    /// The width of the compressed size field of the data descriptor at `desc`, and where it is.
    fn desc_csize(d: &[u8], desc: &std::ops::Range<usize>) -> Option<(usize, usize)> {
        let sig = matches!(desc.len(), 16 | 24);
        let at = desc.start + if sig { 4 } else { 0 } + 4;
        let _ = d;
        match desc.len() {
            12 | 16 => Some((at, 4)),
            20 | 24 => Some((at, 8)),
            _ => None,
        }
    }
}

/// The data `body` inflates to: its CRC-32 and length.
fn inflated_crc(body: &[u8]) -> Result<(u32, u32), String> {
    let mut crc = flate2::Crc::new();
    let mut dec = flate2::bufread::DeflateDecoder::new(body);
    let mut buf = vec![0u8; 1 << 16];
    loop {
        match fill(&mut dec, &mut buf).map_err(|e| e.to_string())? {
            0 => break,
            n => crc.update(&buf[..n]),
        }
    }
    Ok((crc.sum(), crc.amount()))
}

/// ZIP archives: of the same entries in the same order, their headers the same but for the
/// compressed sizes and offsets, their data (inflated) and stored bodies the same, the end
/// records the same but for the offsets of the directory; every size and offset of R true, each
/// entry's CRC-32 that of its data, and R laid out plainly.
pub(super) fn zip(x: &[u8], r: &[u8]) -> Result<(), String> {
    let a = Zip::parse(x).map_err(|e| format!("the original: {e}"))?;
    let b = Zip::parse(r).map_err(|e| format!("the file rewritten: {e}"))?;
    if a.entries.len() != b.entries.len() {
        return Err("another number of entries".into());
    }
    for (i, (p, q)) in a.entries.iter().zip(&b.entries).enumerate() {
        let what = |s: &str| format!("entry {i}: {s}");
        if masked(x, p.central.clone(), &p.cfields) != masked(r, q.central.clone(), &q.cfields) {
            return Err(what("another central header"));
        }
        if masked(x, p.header.clone(), &[p.lfield]) != masked(r, q.header.clone(), &[q.lfield]) {
            return Err(what("another local header"));
        }
        if q.method != 0 && q.method != 8 || q.method != p.method || q.flags != p.flags {
            return Err(what("another method or flags"));
        }
        let (xb, rb) = (&x[p.body.clone()], &r[q.body.clone()]);
        let (crc, len) = if q.method == 0 {
            if xb != rb {
                return Err(what("another stored body"));
            }
            let mut c = flate2::Crc::new();
            c.update(rb);
            (c.sum(), c.amount())
        } else {
            let (same, a, b) = same_deflate(xb, rb).map_err(|e| what(&e))?;
            if !same {
                return Err(what("other data"));
            }
            if a != xb.len() || b != rb.len() {
                return Err(what("a stream not of its entry's size"));
            }
            inflated_crc(rb).map_err(|e| what(&e))?
        };
        if crc != q.crc || len != q.usize as u32 {
            return Err(what("a CRC-32 or size not of its data"));
        }
        // the descriptors: the same but for the compressed size, which is the body's
        let (xd, rd) = (p.desc.clone(), q.desc.clone());
        if xd.len() != rd.len() {
            return Err(what("descriptors of other forms"));
        }
        if !rd.is_empty() {
            let (at, w) = Zip::desc_csize(r, &rd).ok_or(what("a descriptor of no form"))?;
            let (xat, _) = Zip::desc_csize(x, &xd).ok_or(what("a descriptor of no form"))?;
            if le(r, at, w)? != rb.len() as u64 {
                return Err(what("a descriptor not of its body's size"));
            }
            let mut u = x[xd.clone()].to_vec();
            let mut v = r[rd.clone()].to_vec();
            u[xat - xd.start..xat - xd.start + w].fill(0);
            v[at - rd.start..at - rd.start + w].fill(0);
            if u != v {
                return Err(what("another descriptor"));
            }
        }
    }
    // the end records
    match (&a.end64, &b.end64) {
        (None, None) => {}
        (Some(p), Some(q)) => {
            let off = [(48usize, None)];
            let (u, v) = (masked_at(x, p, &off), masked_at(r, q, &off));
            if u != v {
                return Err("another zip64 end record".into());
            }
            let (l, m) = (a.locator.clone().unwrap(), b.locator.clone().unwrap());
            if masked_8(x, &l) != masked_8(r, &m) {
                return Err("another zip64 locator".into());
            }
        }
        _ => return Err("zip64 records in only one".into()),
    }
    let mut u = x[a.end.clone()].to_vec();
    let mut v = r[b.end.clone()].to_vec();
    u[16..20].fill(0);
    v[16..20].fill(0);
    if u != v {
        return Err("another end record or comment".into());
    }
    Ok(())
}

/// The record `range` of `d` with the 8-byte fields at `fields` zeroed.
fn masked_at(d: &[u8], range: &std::ops::Range<usize>, fields: &[(usize, Option<usize>)]) -> Vec<u8> {
    let mut v = d[range.clone()].to_vec();
    for &(f, _) in fields {
        v[f..f + 8].fill(0);
    }
    v
}

/// The zip64 locator `range` of `d` with its offset zeroed.
fn masked_8(d: &[u8], range: &std::ops::Range<usize>) -> Vec<u8> {
    masked_at(d, range, &[(8, None)])
}

/// SQLite databases (<https://www.sqlite.org/fileformat2.html>): of the same length; the header,
/// the reserved bytes of every page, the pages that are no b-tree, overflow page nor freelist
/// page, the b-tree pages' headers, cell pointers, cells (with their overflow pages' links and
/// payloads) and freeblock headers, and the freelist trunks' links and leaf numbers the same;
/// every other byte the same or zero. The structure is that of X, read here (by no code of the
/// rewriter's).
pub(super) fn sqlite(x: &[u8], r: &[u8]) -> Result<(), String> {
    if x.len() != r.len() {
        return Err("of another length".into());
    }
    let keep = sqlite_kept(x)?;
    // (`keep`: sorted, apart)
    let mut p = 0;
    for k in keep.iter().chain([&(x.len()..x.len())]) {
        if k.start > p
            && r[p..k.start] != x[p..k.start]
            && r[p..k.start].iter().zip(&x[p..k.start]).any(|(&a, &b)| a != 0 && a != b)
        {
            return Err(format!("bytes at {p}..{} changed but to zeros", k.start));
        }
        if r[k.clone()] != x[k.clone()] {
            return Err(format!("bytes kept at {}..{} changed", k.start, k.end));
        }
        p = k.end;
    }
    Ok(())
}

/// The ranges of the SQLite database `x` that a rewriter must not change, in order and apart.
fn sqlite_kept(x: &[u8]) -> Result<Vec<std::ops::Range<usize>>, String> {
    let be = |at: usize, n: usize| x[at..at + n].iter().fold(0usize, |v, &b| v << 8 | b as usize);
    if x.len() < 100 || &x[..16] != b"SQLite format 3\0" {
        return Err("not a database".into());
    }
    let size = match be(16, 2) {
        1 => 65536,
        s if (512..=32768).contains(&s) && s.is_power_of_two() => s,
        _ => return Err("bad page size".into()),
    };
    let usable = size - x[20] as usize;
    if usable < 480 || !x.len().is_multiple_of(size) || x.len() < size {
        return Err("bad usable size or length".into());
    }
    let n = x.len() / size;
    // (page numbers from 1; kind: 0 none, 1 other claimed, tree 2, free 3, overflow 4)
    let mut kind = vec![0u8; n + 1];
    // (ranges within pages, as absolute)
    let mut keep: Vec<std::ops::Range<usize>> = Vec::new();
    keep.push(0..100);
    let at = |p: usize| (p - 1) * size;
    let page_ok = |p: usize| (2..=n).contains(&p);
    let lock = 0x4000_0000 / size + 1;
    if n >= lock {
        kind[lock] = 1;
    }
    if be(52, 4) != 0 {
        let mut p = 2;
        while p <= n {
            kind[p] = 1;
            p += usable / 5 + 1;
        }
    }
    for (p, &k) in kind.iter().enumerate().skip(1) {
        keep.push(at(p) + usable..at(p) + size);
        if k == 1 {
            keep.push(at(p)..at(p) + usable);
        }
    }
    // the freelist
    let (mut t, mut seen) = (be(32, 4), 0);
    while t != 0 {
        if !page_ok(t) || kind[t] != 0 {
            return Err("freelist page misplaced".into());
        }
        kind[t] = 3;
        let k = be(at(t) + 4, 4);
        if k > usable / 4 - 2 {
            return Err("freelist trunk too full".into());
        }
        keep.push(at(t)..at(t) + 8 + 4 * k);
        for i in 0..k {
            let l = be(at(t) + 8 + 4 * i, 4);
            if !page_ok(l) || kind[l] != 0 {
                return Err("freelist leaf misplaced".into());
            }
            kind[l] = 3;
        }
        seen += 1 + k;
        t = be(at(t), 4);
    }
    if seen != be(36, 4) {
        return Err("freelist count differs".into());
    }
    // the b-trees
    let mut chains: Vec<(usize, usize)> = Vec::new();
    for p in 1..=n {
        let h = if p == 1 { 100 } else { 0 };
        if kind[p] != 0 || !matches!(x[at(p) + h], 2 | 5 | 10 | 13) {
            continue;
        }
        kind[p] = 2;
        let (b, ty) = (at(p), x[at(p) + h]);
        let leaf = ty & 8 != 0;
        let count = be(b + h + 3, 2);
        let arr_end = h + if leaf { 8 } else { 12 } + 2 * count;
        if arr_end > usable {
            return Err(format!("page {p}: cell pointers outside"));
        }
        keep.push(b..b + arr_end);
        let lim = |v: usize| (usable - 12) * v / 255 - 23;
        let (max_local, min_local) = (if ty == 13 { usable - 35 } else { lim(64) }, lim(32));
        for i in 0..count {
            let c = be(b + arr_end - 2 * (count - i), 2);
            if c >= usable {
                return Err(format!("page {p}: cell outside"));
            }
            let mut q = b + c + if leaf { 0 } else { 4 };
            let mut total = 0;
            if ty != 5 {
                let (v, e) = sqlite_varint(x, q).ok_or("cell varint")?;
                total = v as usize;
                q = e;
            }
            if ty == 5 || ty == 13 {
                q = sqlite_varint(x, q).ok_or("cell varint")?.1;
            }
            let mut end = q;
            if ty != 5 {
                let local = if total <= max_local {
                    total
                } else if min_local + (total - min_local) % (usable - 4) <= max_local {
                    min_local + (total - min_local) % (usable - 4)
                } else {
                    min_local
                };
                end += local;
                if total > local {
                    if end + 4 > b + usable {
                        return Err(format!("page {p}: cell outside"));
                    }
                    chains.push((be(end, 4), total - local));
                    end += 4;
                }
            }
            if end > b + usable {
                return Err(format!("page {p}: cell outside"));
            }
            keep.push(b + c..end);
        }
        let mut f = be(b + h + 1, 2);
        let mut guard = 0;
        while f != 0 {
            if f + 4 > usable {
                return Err(format!("page {p}: freeblock outside"));
            }
            keep.push(b + f..b + f + 4);
            f = be(b + f, 2);
            guard += 1;
            if guard > usable {
                return Err(format!("page {p}: freeblock loop"));
            }
        }
    }
    // the overflow chains
    for (mut p, mut rest) in chains {
        loop {
            if !page_ok(p) || kind[p] != 0 {
                return Err("overflow page misplaced".into());
            }
            kind[p] = 4;
            if rest <= usable - 4 {
                keep.push(at(p)..at(p) + 4 + rest);
                break;
            }
            keep.push(at(p)..at(p) + usable);
            rest -= usable - 4;
            p = be(at(p), 4);
        }
    }
    // the pages of no structure
    for p in (1..=n).filter(|&p| kind[p] == 0) {
        keep.push(at(p)..at(p) + size);
    }
    keep.retain(|k| k.start < k.end);
    keep.sort_by_key(|k| (k.start, k.end));
    let mut out: Vec<std::ops::Range<usize>> = Vec::with_capacity(keep.len());
    for k in keep {
        match out.last_mut() {
            Some(l) if k.start <= l.end => l.end = l.end.max(k.end),
            _ => out.push(k),
        }
    }
    Ok(out)
}

fn sqlite_varint(d: &[u8], p: usize) -> Option<(u64, usize)> {
    let mut v = 0u64;
    for i in 0..9 {
        let b = *d.get(p + i)?;
        if i == 8 {
            return Some((v << 8 | b as u64, p + 9));
        }
        v = v << 7 | (b & 0x7f) as u64;
        if b & 0x80 == 0 {
            return Some((v, p + i + 1));
        }
    }
    None
}
