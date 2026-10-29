//! The base relocations of a PE image (`.reloc`, data directory 5) as the RVAs of their entries,
//! each as its difference from the one before, and their types: the blocks' headers, one for
//! each page, and their padding follow from the entries (`exe-recons/pe-reloc`, FORMAT.md §R).

use alloc::vec::Vec;

/// Bytes of a block's header: the page's RVA and the block's size (`le32` each).
#[cfg(feature = "encode")]
const HEADER: usize = 8;

/// The streams of the module: the RVAs' differences (varints) and the types (a byte each).
pub const STREAM_NAMES: [&str; 2] = ["RVAS", "TYPES"];

/// The relocations `d` as the streams of [`STREAM_NAMES`], their blocks' sizes multiples of
/// `1 << align`; `None` where they are not as [`decode`] makes them.
#[cfg(feature = "encode")]
pub fn encode(d: &[u8], align: u8) -> Option<[Vec<u8>; 2]> {
    let unit = 1usize << align;
    let (mut rvas, mut types) = (Vec::new(), Vec::new());
    let (mut at, mut last) = (0, 0u32);
    while at < d.len() {
        let page = crate::util::to_u32(d.get(at..at + 4)?);
        let size = crate::util::to_u32(d.get(at + 4..at + 8)?) as usize;
        let block = d.get(at + HEADER..at.checked_add(size)?)?.as_chunks::<2>().0;
        // the entries but the padding: the fewest that a block of this size holds, the rest
        // zeros
        let n = (0..=block.len())
            .find(|&n| (HEADER + 2 * n).next_multiple_of(unit) == size && block[n..].iter().all(|e| *e == [0, 0]))?;
        for e in &block[..n] {
            let e = u16::from_le_bytes(*e);
            let rva = page.wrapping_add(e as u32 & 0xfff);
            put_varint(&mut rvas, rva.wrapping_sub(last));
            types.push((e >> 12) as u8);
            last = rva;
        }
        at += size;
    }
    (decode(&rvas, &types, align, d.len()).as_deref() == Some(d)).then_some([rvas, types])
}

/// The relocations, `len` bytes, from the streams `rvas` and `types`; `None` where they are not
/// valid (FORMAT.md §R4).
pub fn decode(rvas: &[u8], types: &[u8], align: u8, len: usize) -> Option<Vec<u8>> {
    let unit = 1usize << align;
    let mut out = Vec::with_capacity(len);
    let (mut rva, mut pos) = (0u32, 0);
    let mut start = None::<(usize, u32)>;
    let close = |out: &mut Vec<u8>, start: usize| {
        out.resize((out.len()).next_multiple_of(unit), 0);
        let size = (out.len() - start) as u32;
        out[start + 4..start + 8].copy_from_slice(&size.to_le_bytes());
    };
    for &t in types {
        let (v, n) = get_varint(rvas.get(pos..)?)?;
        pos += n;
        rva = rva.wrapping_add(v);
        if t > 15 {
            return None;
        }
        let page = rva & !0xfff;
        if start.is_none_or(|(_, p)| p != page) {
            if let Some((s, _)) = start {
                close(&mut out, s);
            }
            start = Some((out.len(), page));
            out.extend_from_slice(&page.to_le_bytes());
            out.extend_from_slice(&[0; 4]);
        }
        out.extend_from_slice(&((t as u16) << 12 | (rva & 0xfff) as u16).to_le_bytes());
        if out.len() > len {
            return None;
        }
    }
    if let Some((s, _)) = start {
        close(&mut out, s);
    }
    (pos == rvas.len() && out.len() == len).then_some(out)
}

#[cfg(feature = "encode")]
fn put_varint(out: &mut Vec<u8>, mut v: u32) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// A varint below 2^32 of at most 5 bytes, and its length.
fn get_varint(d: &[u8]) -> Option<(u32, usize)> {
    let mut v = 0u64;
    for (i, &b) in d.iter().take(5).enumerate() {
        v |= ((b & 0x7f) as u64) << (7 * i);
        if b < 0x80 {
            return u32::try_from(v).ok().map(|v| (v, i + 1));
        }
    }
    None
}

#[cfg(all(test, feature = "encode"))]
mod tests {
    use super::*;

    /// Blocks of `(page, entries)`, padded with zero entries to multiples of `1 << align`.
    fn blocks(b: &[(u32, &[u16])], align: u8) -> Vec<u8> {
        let mut d = Vec::new();
        for &(page, entries) in b {
            let start = d.len();
            d.extend_from_slice(&page.to_le_bytes());
            d.extend_from_slice(&[0; 4]);
            entries.iter().for_each(|e| d.extend_from_slice(&e.to_le_bytes()));
            d.resize(d.len().next_multiple_of(1 << align), 0);
            let size = (d.len() - start) as u32;
            d[start + 4..start + 8].copy_from_slice(&size.to_le_bytes());
        }
        d
    }

    #[test]
    fn round_trip() {
        let pages: &[(u32, &[u16])] =
            &[(0x1000, &[0x3004, 0x3010, 0x3ffc]), (0x3000, &[0x3000]), (0x2000, &[0xa008, 0x3100, 0x3080, 0x3090])];
        for align in [2, 3] {
            let d = blocks(pages, align);
            let [rvas, types] = encode(&d, align).unwrap();
            assert_eq!(types, [3, 3, 3, 3, 10, 3, 3, 3]);
            assert_eq!(rvas[..3], [0x84, 0x20, 0x0c]);
            assert_eq!(decode(&rvas, &types, align, d.len()), Some(d));
        }
        assert_eq!(encode(&[], 2), Some([Vec::new(), Vec::new()]));
    }

    #[test]
    fn not_canonical() {
        // a page twice in a row, an empty block, a page not at 4 KiB, bytes past the last block
        // (padding other than zeros, or more than asked, is entries of type 0)
        let page_twice = blocks(&[(0x1000, &[0x3004]), (0x1000, &[0x3008])], 2);
        let mut trailing = blocks(&[(0x1000, &[0x3004, 0x3008])], 2);
        trailing.extend_from_slice(&[0, 0]);
        for d in [page_twice, blocks(&[(0x1000, &[])], 2), blocks(&[(0x1800, &[0x3004])], 2), trailing] {
            assert_eq!(encode(&d, 2), None, "{d:x?}");
        }
    }

    #[test]
    fn padded_more() {
        let d = blocks(&[(0x1000, &[0x3004, 0x3008])], 3);
        assert_eq!(encode(&d, 3).unwrap()[1], [3, 3]);
        assert_eq!(encode(&d, 2).unwrap()[1], [3, 3, 0]);
    }

    #[test]
    fn invalid() {
        assert_eq!(decode(&[4], &[16], 2, 12), None);
        assert_eq!(decode(&[4, 4], &[3], 2, 12), None);
        assert_eq!(decode(&[0x80, 0x80, 0x80, 0x80, 0x10], &[3], 2, 12), None);
    }
}
