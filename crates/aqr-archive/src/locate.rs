//! Locating an archive in a file (spec, "Locating an archive"): the archive at the end of the
//! file found from its footer, or the first one in the file found by reading its blocks, or the
//! one at the end of a PE image before its certificate table (a self-extracting executable signed
//! after it was made), and each trimmed where a footer follows one of its metadata blocks (what
//! appending to the archive leaves until it commits, spec, "Appending to an archive").

use std::ops::Range;

use super::{FOOTER_MAGIC, HEADER_MAGIC, OVERLAY_FOOTER_MAGIC, OVERLAY_HEADER_MAGIC, decoded, envelope, leb};

/// Where the archive in `file` is (spec, "locate an archive"), or none: the whole of an
/// encrypted archive.
pub fn locate(file: &[u8]) -> Option<Range<usize>> {
    if envelope::is_encrypted(file) {
        return Some(0..file.len());
    }
    let back = from_end(file);
    if let Some(footer) = header_at(file, 0) {
        if back == Some(0) {
            return Some(0..trim(file));
        }
        let e = forward(file, footer)?;
        return Some(0..footer_end(file, e)?);
    }
    if let Some(h) = back {
        return Some(h..h + trim(&file[h..]));
    }
    // (the step 5: before the certificate table that ends a PE image, past the zeros, up to 7,
    // that pad it to 8 bytes; the footer's own last bytes may be zeros too)
    let c = pe_certificate_table(file)?.start;
    let zeros = file[..c].iter().rev().take(7).take_while(|&&b| b == 0).count();
    (0..=zeros).find_map(|k| {
        let f = &file[..c - k];
        let h = from_end(f)?;
        Some(h..h + trim(&f[h..]))
    })
}

/// The certificate table of PE image `file` (spec, "PE certificate table": Authenticode's
/// signatures, the data directory 4, of a file offset), where it ends the file.
pub fn pe_certificate_table(file: &[u8]) -> Option<Range<usize>> {
    let u32_at = |o: usize| file.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize);
    if !file.starts_with(b"MZ") {
        return None;
    }
    let p = u32_at(60)?;
    if file.get(p..p + 4)? != b"PE\0\0" {
        return None;
    }
    let o = p + 24;
    let (dirs, count) = match u16_at(file, o)? {
        0x10b => (o + 96, u32_at(o + 92)?),
        0x20b => (o + 112, u32_at(o + 108)?),
        _ => return None,
    };
    let (c, s) = (u32_at(dirs + 32)?, u32_at(dirs + 36)?);
    (count >= 5 && s > 0 && c.checked_add(s)? == file.len()).then_some(c..file.len())
}

/// The footer magic matching the header magic at `at` of `d`, where there is one there.
fn header_at(d: &[u8], at: usize) -> Option<[u8; 6]> {
    match d.get(at..at + 6)? {
        h if h == HEADER_MAGIC => Some(FOOTER_MAGIC),
        h if h == OVERLAY_HEADER_MAGIC => Some(OVERLAY_FOOTER_MAGIC),
        _ => None,
    }
}

/// Whether `d` has a footer magic at `at`.
fn is_footer(d: &[u8], at: usize) -> bool {
    d.get(at..at + 6).is_some_and(|m| m == FOOTER_MAGIC || m == OVERLAY_FOOTER_MAGIC)
}

fn u16_at(d: &[u8], at: usize) -> Option<usize> {
    d.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize)
}

/// The end of the footer at `e` of `d`, where `d` holds it whole.
fn footer_end(d: &[u8], e: usize) -> Option<usize> {
    let end = e + u16_at(d, e + 6)? + 10;
    (end <= d.len()).then_some(end)
}

/// Where the archive ending at the end of `file` starts (its header magic), as the footer there
/// tells (the step 2).
fn from_end(file: &[u8]) -> Option<usize> {
    let n = u16_at(file, file.len().checked_sub(2)?)?;
    let f = file.len().checked_sub(n + 10)?;
    if !is_footer(file, f) || u16_at(file, f + 6)? != n {
        return None;
    }
    let mut at = f + 8;
    let s = usize::try_from(leb(file, &mut at).ok()?).ok()?;
    let h = f.checked_sub(s)?.checked_sub(6)?;
    (header_at(file, h)? == file[f..f + 6]).then_some(h)
}

/// The footer reached by reading the blocks of `file` from its header magic on (the step 3.2):
/// where it starts, of `footer` matching the header.
fn forward(file: &[u8], footer: [u8; 6]) -> Option<usize> {
    let mut at = HEADER_MAGIC.len();
    loop {
        if is_footer(file, at) {
            return (file[at..at + 6] == footer).then_some(at);
        }
        at = frame_at(file, at)?.2;
    }
}

/// The type of the block of `d` at `at` by the generic structure (spec, "Block"), and where it
/// ends (after its padding), where it is whole.
pub fn block_at(d: &[u8], at: usize) -> Option<(u64, usize)> {
    frame_at(d, at).map(|(ty, _, end)| (ty, end))
}

/// The block of `d` at `at` by the generic structure, where it is whole: its type, its data and
/// where it ends (after its padding).
pub(crate) fn frame_at(d: &[u8], at: usize) -> Option<(u64, Range<usize>, usize)> {
    let mut p = at;
    let ty = leb(d, &mut p).ok()?;
    if p - at != aqr_common::varint::len(ty) {
        return None;
    }
    let width = 1 << (ty % 4);
    let size = d.get(p..p + width)?;
    p += width;
    let size = usize::try_from(u64::from_le_bytes(std::array::from_fn(|i| size.get(i).copied().unwrap_or(0)))).ok()?;
    let data = p..p.checked_add(size).filter(|&e| e <= d.len())?;
    let mut end = data.end;
    if (end - at) % 2 == 1 {
        end += 1;
        if end > d.len() {
            return None;
        }
    }
    Some((ty, data, end))
}

/// Whether block type `ty` is a metadata block's.
pub(super) fn metadata(ty: u64) -> bool {
    (0x10..=0x17).contains(&ty)
}

/// Of the located archive `a` (a header magic, and a footer matching it at its end): where its
/// footer is, and where its last metadata block is (none where it has none).
pub fn footer_of(a: &[u8]) -> Option<(Range<usize>, Option<usize>)> {
    header_at(a, 0)?;
    let n = u16_at(a, a.len().checked_sub(2)?)?;
    let f = a.len().checked_sub(n + 10)?;
    if !is_footer(a, f) || u16_at(a, f + 6)? != n {
        return None;
    }
    let mut at = f + 8;
    let _size_hint = leb(a, &mut at).ok()?;
    let back = usize::try_from(leb(a, &mut at).ok()?).ok()?;
    let metadata = match back {
        0 => None,
        b => Some((f + 8).checked_sub(b)?),
    };
    Some((f..a.len(), metadata))
}

/// The length of the located archive `a` (a header magic, and a footer matching it at its
/// end), trimmed where a footer other than the last follows one of its metadata blocks (spec,
/// "trim a located archive").
pub fn trim(a: &[u8]) -> usize {
    trimmed(a).unwrap_or(a.len())
}

fn trimmed(a: &[u8]) -> Option<usize> {
    let n = u16_at(a, a.len().checked_sub(2)?)?;
    let f = a.len().checked_sub(n + 10)?;
    let mut at = f + 8;
    let _size_hint = leb(a, &mut at).ok()?;
    let back = usize::try_from(leb(a, &mut at).ok()?).ok()?;
    if back == 0 {
        return None;
    }
    let last = (f + 8).checked_sub(back)?;
    let (ty, data, end) = frame_at(a, last)?;
    if !metadata(ty) {
        return None;
    }
    // (the metadata blocks back to the first, each by the offset at the start of its decoded
    // data; ever earlier, so the walk ends)
    let mut blocks = vec![end];
    let (mut at, mut data, mut ty) = (last, data, ty);
    while let Some(prev) = decoded(&a[data.clone()], ty & 0x04 != 0, true).ok().and_then(|d| {
        let mut p = 0;
        usize::try_from(leb(&d, &mut p).ok()?).ok()
    }) && prev > 0
        && let Some(p) = at.checked_sub(prev)
        && let Some((t, d, end)) = frame_at(a, p)
        && metadata(t)
    {
        blocks.push(end);
        (at, data, ty) = (p, d, t);
    }
    for mut e in blocks.into_iter().rev() {
        while let Some((0..=3, _, end)) = frame_at(a, e) {
            e = end;
        }
        if e != f && a.get(e..e + 6) == a.get(f..f + 6) {
            let end = e + u16_at(a, e + 6)? + 10;
            if end <= f {
                return Some(end);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::natives;
    use crate::{ContentAt, Encoder, Entry, EntryNode, Extra, Leaf, Level, SegmentWriter, Tree, open};
    use aqr_formula::Formula;

    /// An archive of a file of `data` alone.
    pub(crate) fn one(data: &[u8]) -> Vec<u8> {
        let enc = Encoder::new(Level::Zstd(3));
        let mut f = Formula::default();
        let s = f.push("a", data.to_vec());
        let mut w = SegmentWriter::new();
        let mut out = SegmentWriter::header().to_vec();
        let b = w.batch(&enc, &f, &[Leaf { outputs: vec![s], sums: vec![None] }]).unwrap();
        w.commit(&b);
        out.extend_from_slice(b.blocks());
        let mut t = Tree::default();
        t.entries.push(Entry { parent: None, name: b"a".to_vec(), node: EntryNode::File(0), extra: Extra::default() });
        let contents = [ContentAt { leaf: b.leaf_at[0], output: 0, size: data.len() as u64 }];
        out.extend_from_slice(&w.finish(&enc, &t, &contents).unwrap().0);
        out
    }

    #[test]
    fn located() {
        let a = one(b"hello");
        assert_eq!(locate(&a), Some(0..a.len()));
        // leading bytes, trailing bytes, both (not found), nothing
        let lead = [b"stub!".as_slice(), &a].concat();
        assert_eq!(locate(&lead), Some(5..lead.len()));
        let trail = [&a, b"tail".as_slice()].concat();
        assert_eq!(locate(&trail), Some(0..a.len()));
        assert_eq!(locate(&[b"stub".as_slice(), &a, b"tail"].concat()), None);
        assert_eq!(locate(b"nothing here"), None);
        assert_eq!(locate(&a[..a.len() - 1]), None);
        // two archives one after the other: the first, by its blocks
        assert_eq!(locate(&[&a, &a[..]].concat()), Some(0..a.len()));
        assert!(open(&lead[locate(&lead).unwrap()], &natives()).is_ok());
        assert_eq!(trim(&a), a.len());
        let (f, m) = footer_of(&a).unwrap();
        assert!(f.end == a.len() && f.len() < 30 && m.is_some_and(|m| m < f.start));
        assert_eq!(footer_of(&lead), None);
    }

    /// A PE image of PE32 or PE32+ with `body` after its headers and a certificate table of `n`
    /// bytes at `body`'s end padded to 8 bytes by zeros (as `signtool` signs it).
    fn signed(plus: bool, body: &[u8], n: usize) -> Vec<u8> {
        let mut f = vec![0u8; 0x200];
        f[..2].copy_from_slice(b"MZ");
        f[60..64].copy_from_slice(&0x80u32.to_le_bytes());
        f[0x80..0x84].copy_from_slice(b"PE\0\0");
        let o = 0x80 + 24;
        let (dirs, count) = if plus { (o + 112, o + 108) } else { (o + 96, o + 92) };
        f[o..o + 2].copy_from_slice(&(if plus { 0x20bu16 } else { 0x10b }).to_le_bytes());
        f[count..count + 4].copy_from_slice(&16u32.to_le_bytes());
        f.extend_from_slice(body);
        f.resize(f.len().next_multiple_of(8), 0);
        let c = f.len();
        f[dirs + 32..dirs + 36].copy_from_slice(&(c as u32).to_le_bytes());
        f[dirs + 36..dirs + 40].copy_from_slice(&(n as u32).to_le_bytes());
        f.extend(std::iter::repeat_n(0xa5, n));
        f
    }

    #[test]
    fn located_before_a_signature() {
        let a = one(b"hello, signed");
        for plus in [false, true] {
            // (each length of the padding before the table)
            for stub in 0..8 {
                let body = [&vec![1u8; stub][..], &a].concat();
                let f = signed(plus, &body, 24);
                let at = 0x200 + stub;
                assert_eq!(locate(&f), Some(at..at + a.len()), "{plus} {stub}");
                assert_eq!(pe_certificate_table(&f), Some(f.len() - 24..f.len()));
            }
        }
        // a footer ending in zeros (its padding's own): found by fewer zeros taken off
        let mut z = a.clone();
        let tail = z.len() - 1;
        if z[tail] == 0 {
            let f = signed(true, &z, 16);
            assert!(locate(&f).is_some());
        }
        z.extend_from_slice(&[1, 2, 3]);
        let f = signed(true, &z, 16);
        assert_eq!(locate(&f), None, "(bytes not of the padding after the archive)");
        // a table not ending the file, or of a directory not there: none
        let mut f = signed(true, &a, 16);
        f.push(0);
        assert_eq!(locate(&f), None);
        let mut f = signed(false, &a, 16);
        f[0x80 + 24 + 92..0x80 + 24 + 96].copy_from_slice(&4u32.to_le_bytes());
        assert_eq!((locate(&f), pe_certificate_table(&f)), (None, None));
    }
}
