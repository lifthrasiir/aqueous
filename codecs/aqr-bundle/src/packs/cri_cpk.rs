//! CRI's CPK archives (`.cpk`, CRI FileMajik / CRIWARE), little endian header, big-endian
//! `@UTF` tables ([`super::cri_utf`]). UNVERIFIED on real files: written from public
//! descriptions of the format and tested only on packs the tests write.
//!
//! The file starts `CPK `, 12 bytes (flags, the table's size, ...) and the `@UTF` table of the
//! header at 0x10 (possibly masked), one row: `ContentOffset`, `TocOffset`, `ItocOffset`,
//! `Align`, ... Each further table (`TOC `, `ITOC`, `ETOC`, `GTOC`) is at its offset with its
//! own 16-byte header (magic, flags, size, ...) and its `@UTF` table after it.
//!
//! The TOC's rows are the files: `DirName`, `FileName`, `FileSize`, `ExtractSize`,
//! `FileOffset`, `ID`. A file's data starts at `FileOffset` plus the TOC's offset where that
//! is before `ContentOffset` (the usual case: the contents follow the TOC), else plus
//! `ContentOffset`: which of the two is not certain from the descriptions, so the base is
//! picked that puts a file at `ContentOffset` and more files within the archive, none of them
//! before `ContentOffset`.
//! A file whose `FileSize` is under its `ExtractSize` is compressed by CRILAYLA: a file of its
//! extents with `Compressed::Opaque { len: ExtractSize }`.
//!
//! An archive of an ITOC alone (files by ID, no names) is read as `<ID>.<ext>`: the files in
//! the order of the IDs from `ContentOffset`, each `FileSize` long and the next aligned up to
//! `Align`; the sizes are in the nested tables `DataL` (IDs under 65536) and `DataH`. ETOC
//! (times) and GTOC (groups) hold no data; the headers, tables and padding are the rest.

use super::{cri_awb, cri_utf, within};
use aqr_disk::{Compressed, File};

pub fn starts(d: &[u8]) -> bool {
    d.starts_with(b"CPK ")
}

/// The `@UTF` table of the section `magic` at `at`.
fn section<'a>(d: &'a [u8], at: u64, magic: &[u8; 4]) -> Option<cri_utf::Table<'a>> {
    let at = usize::try_from(at).ok()?;
    if d.get(at..at.checked_add(4)?)? != magic {
        return None;
    }
    cri_utf::parse(d.get(at.checked_add(16)?..)?)
}

pub fn files(d: &[u8]) -> Option<Vec<File>> {
    if !starts(d) {
        return None;
    }
    let head = cri_utf::parse(d.get(0x10..)?)?;
    let num = |n: &str| head.num(0, n).unwrap_or(0);
    let (content, toc, itoc) = (num("ContentOffset"), num("TocOffset"), num("ItocOffset"));
    let files = if toc != 0 { toc_files(d, toc, content) } else { None };
    let files = match files {
        Some(f) if !f.is_empty() => f,
        _ if itoc != 0 => itoc_files(d, itoc, content, num("Align"))?,
        _ => return None,
    };
    (!files.is_empty()).then_some(files)
}

fn toc_files(d: &[u8], toc: u64, content: u64) -> Option<Vec<File>> {
    let t = section(d, toc, b"TOC ")?;
    let entries: Vec<_> = (0..t.rows.len())
        .filter_map(|r| {
            let (size, extract) = (t.num(r, "FileSize")?, t.num(r, "ExtractSize").unwrap_or(0));
            let mut name = Vec::new();
            if let Some(dir) = t.str(r, "DirName").filter(|s| !s.is_empty()) {
                name.extend_from_slice(dir);
                name.push(b'/');
            }
            name.extend_from_slice(t.str(r, "FileName")?);
            Some((name, t.num(r, "FileOffset")?, size, extract))
        })
        .collect();
    let mut bases = vec![if toc < content || content == 0 { toc } else { content }];
    for b in [content, toc] {
        if !bases.contains(&b) {
            bases.push(b);
        }
    }
    // Files start at or after ContentOffset; of the bases the one with a file starting there
    // (the contents' first) and then the one with more files in the archive is taken.
    let place = |base: u64| -> Vec<File> {
        entries
            .iter()
            .filter_map(|(name, off, size, extract)| {
                let r = within(d, base.checked_add(*off)?, *size)?;
                if (r.start as u64) < content {
                    return None;
                }
                let mut f = super::file(name.clone(), r);
                if *size < *extract {
                    f.compressed = Some(Compressed::Opaque { len: *extract });
                }
                Some(f)
            })
            .collect()
    };
    let mut best: Vec<File> = Vec::new();
    let mut best_score = (false, 0);
    for b in bases {
        let p = place(b);
        let score = (p.iter().any(|f| f.extents[0].start as u64 == content), p.len());
        if score > best_score {
            (best, best_score) = (p, score);
        }
    }
    Some(best)
}

fn itoc_files(d: &[u8], itoc: u64, content: u64, align: u64) -> Option<Vec<File>> {
    let t = section(d, itoc, b"ITOC")?;
    let mut items: Vec<(u64, u64, u64)> = Vec::new();
    for col in ["DataL", "DataH"] {
        let Some(r) = t.data(0, col).filter(|r| !r.is_empty()) else { continue };
        let Some(sub) = cri_utf::parse(&t.bytes[r]) else { continue };
        for i in 0..sub.rows.len() {
            let (Some(id), Some(size)) = (sub.num(i, "ID"), sub.num(i, "FileSize")) else { continue };
            items.push((id, size, sub.num(i, "ExtractSize").unwrap_or(0)));
        }
    }
    items.sort_by_key(|i| i.0);
    let align = align.max(1);
    let mut pos = content;
    let mut out = Vec::new();
    for (id, size, extract) in items {
        if let Some(r) = within(d, pos, size) {
            let ext = if size < extract { "bin" } else { cri_awb::ext(&d[r.clone()]) };
            let mut f = super::file(format!("{id}.{ext}").into_bytes(), r);
            if size < extract {
                f.compressed = Some(Compressed::Opaque { len: extract });
            }
            out.push(f);
        }
        pos = pos.checked_add(size)?.div_ceil(align).checked_mul(align)?;
    }
    Some(out)
}
