//! The base relocations of a PE image (`.reloc`, data directory 5) as the RVAs of their entries,
//! each as its difference from the one before, and their types: the blocks' headers, one for
//! each page, and their padding follow from the entries (`exe-recons/pe-reloc`, FORMAT.md §R).
//! Entries may be predicted (version 2): RVAs that another module gives (the x86 decoder's
//! absolute fields, FORMAT.md §5.7), of a type the parameters tell; the streams then hold the
//! predictions that are wrong, by their indices, and the entries not predicted.

use alloc::vec::Vec;

/// Bytes of a block's header: the page's RVA and the block's size (`le32` each).
#[cfg(feature = "encode")]
const HEADER: usize = 8;

/// The streams of the module: the RVAs' differences (varints) and the types (a byte each).
pub const STREAM_NAMES: [&str; 2] = ["RVAS", "TYPES"];

/// The stream of the wrong predictions' indices (varints), after [`STREAM_NAMES`]'s.
pub const WRONG: &str = "WRONG";

/// The entries of the relocations `d`, (RVA, type) each, in order, their blocks' sizes
/// multiples of `1 << align`; `None` where they are not as [`decode`] makes them.
#[cfg(feature = "encode")]
fn entries(d: &[u8], align: u8) -> Option<Vec<(u32, u8)>> {
    let unit = 1usize << align;
    let (mut out, mut at) = (Vec::new(), 0);
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
            out.push((page.wrapping_add(e as u32 & 0xfff), (e >> 12) as u8));
        }
        at += size;
    }
    Some(out)
}

/// The RVAs of the entries of the relocations `d` (as [`encode`] takes them), in order.
#[cfg(feature = "encode")]
pub fn rvas(d: &[u8], align: u8) -> Option<Vec<u32>> {
    Some(entries(d, align)?.into_iter().map(|e| e.0).collect())
}

/// The relocations `d` as the streams of [`STREAM_NAMES`], their blocks' sizes multiples of
/// `1 << align`; `None` where they are not as [`decode`] makes them.
#[cfg(feature = "encode")]
pub fn encode(d: &[u8], align: u8) -> Option<[Vec<u8>; 2]> {
    let [rvas, types, _] = encode_predicted(d, align, &[], 0)?;
    Some([rvas, types])
}

/// The relocations `d` as the streams of [`STREAM_NAMES`] and [`WRONG`], of the predictions
/// `pred` (RVAs, ascending) of type `t`: the entries not predicted, and the indices of the
/// predictions that are not entries of that type. `None` where they are not as
/// [`decode_predicted`] makes them (as [`encode`]; and where predicted, where the entries are
/// not in ascending order).
#[cfg(feature = "encode")]
pub fn encode_predicted(d: &[u8], align: u8, pred: &[u32], t: u8) -> Option<[Vec<u8>; 3]> {
    let all = entries(d, align)?;
    let (mut rvas, mut types, mut wrong) = (Vec::new(), Vec::new(), Vec::new());
    let mut listed = |(rva, ty): (u32, u8), last: &mut u32| {
        crate::util::put_varint(&mut rvas, rva.wrapping_sub(*last).into());
        types.push(ty);
        *last = rva;
    };
    let mut last = 0u32;
    if pred.is_empty() {
        all.iter().for_each(|&e| listed(e, &mut last));
    } else {
        if !all.windows(2).all(|w| w[0].0 < w[1].0) || !pred.windows(2).all(|w| w[0] < w[1]) {
            return None;
        }
        // (both ascending: merged)
        let (mut j, mut next) = (0, 0usize);
        for (i, &p) in pred.iter().enumerate() {
            while j < all.len() && all[j].0 < p {
                listed(all[j], &mut last);
                j += 1;
            }
            if j < all.len() && all[j] == (p, t) {
                j += 1;
            } else {
                crate::util::put_varint(&mut wrong, (i - next) as u64);
                next = i + 1;
            }
        }
        all[j..].iter().for_each(|&e| listed(e, &mut last));
    }
    let mut it = pred.iter().copied();
    let back = decode_merged([&rvas, &types, &wrong], &mut || Some(it.next()), t, align, d.len(), 0);
    (back.as_deref() == Some(d)).then_some([rvas, types, wrong])
}

/// The predictions `pred` as their input holds them (`le32` each).
#[cfg(feature = "encode")]
pub fn pred_bytes(pred: &[u32]) -> Vec<u8> {
    pred.iter().flat_map(|p| p.to_le_bytes()).collect()
}

/// The relocations, `len` bytes, from the streams `rvas` and `types`; `None` where they are not
/// valid (FORMAT.md §R4).
pub fn decode(rvas: &[u8], types: &[u8], align: u8, len: usize) -> Option<Vec<u8>> {
    decode_predicted([rvas, types, &[]], &[], 0, align, len)
}

/// The relocations, `len` bytes, from the streams of [`STREAM_NAMES`] and [`WRONG`], the
/// predictions `pred` (`le32` RVAs) of type `t`; `None` where they are not valid (FORMAT.md §R4).
pub fn decode_predicted(streams: [&[u8]; 3], pred: &[u8], t: u8, align: u8, len: usize) -> Option<Vec<u8>> {
    if !pred.len().is_multiple_of(4) {
        return None;
    }
    let mut it = pred.as_chunks::<4>().0.iter().map(|p| u32::from_le_bytes(*p));
    decode_merged(streams, &mut || Some(it.next()), t, align, len, 0)
}

/// The fields of a section of data that are predictions (FORMAT.md §R3): those of `size` bytes
/// (4 or 8) at RVAs that are multiples of `size`, whose values less `base` (wrapping) are from
/// `lo` to `hi`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Data {
    pub base: u64,
    pub lo: u64,
    pub hi: u64,
    pub size: usize,
}

/// The predictions of `data` (as [`Data`]): the pieces' bytes one after another, of `pieces`
/// (`le32` RVA and `le32` length each, their lengths `data`'s), in order.
pub fn data_predictions<'a>(f: Data, pieces: &'a [[u8; 8]], data: &'a [u8]) -> impl Iterator<Item = u32> + 'a {
    let mut at = 0usize;
    pieces.iter().flat_map(move |p| {
        let (rva, len) = piece(p);
        let b = data.get(at..at + len as usize).unwrap_or(&[]);
        at += len as usize;
        let first = (f.size - rva as usize % f.size) % f.size;
        (first..b.len().saturating_sub(f.size - 1)).step_by(f.size).filter_map(move |o| {
            let mut v = [0u8; 8];
            v[..f.size].copy_from_slice(&b[o..o + f.size]);
            (f.lo..=f.hi).contains(&u64::from_le_bytes(v).wrapping_sub(f.base)).then_some(rva.wrapping_add(o as u32))
        })
    })
}

/// The RVA and the length of a piece of data ([`data_predictions`]).
pub fn piece(p: &[u8; 8]) -> (u32, u32) {
    (u32::from_le_bytes(p[..4].try_into().unwrap()), u32::from_le_bytes(p[4..].try_into().unwrap()))
}

/// The predictions `a` and `b` merged by RVA, ascending where each is: `Some(None)` once both are
/// used up, `None` for one RVA in both.
pub fn merged(a: impl Iterator<Item = u32>, b: impl Iterator<Item = u32>) -> impl FnMut() -> Option<Option<u32>> {
    let (mut a, mut b) = (a.peekable(), b.peekable());
    move || match (a.peek(), b.peek()) {
        (Some(x), Some(y)) if x == y => None,
        (Some(x), Some(y)) if x < y => Some(a.next()),
        (Some(_), Some(_)) | (None, _) => Some(b.next()),
        (Some(_), None) => Some(a.next()),
    }
}

/// [`decode_predicted`] of the predictions `next_pred` gives (`Some(None)` at their end, `None`
/// where they are not valid), with room for `more` bytes after them.
pub fn decode_merged(
    streams: [&[u8]; 3],
    next_pred: &mut dyn FnMut() -> Option<Option<u32>>,
    t: u8,
    align: u8,
    len: usize,
    more: usize,
) -> Option<Vec<u8>> {
    let [mut rvas, mut types, mut wrong] = streams;
    if t > 15 {
        return None;
    }
    let unit = 1usize << align;
    let mut out = crate::util::room(len.checked_add(more)?)?;
    let mut start = None::<(usize, u32)>;
    let close = |out: &mut Vec<u8>, start: usize| {
        out.resize((out.len()).next_multiple_of(unit), 0);
        let size = (out.len() - start) as u32;
        out[start + 4..start + 8].copy_from_slice(&size.to_le_bytes());
    };
    // the entries listed, and the predictions right, merged by RVA
    let mut last = 0u32;
    let mut next_listed = |rvas: &mut &[u8], types: &mut &[u8]| -> Option<Option<(u32, u8)>> {
        let Some((&ty, rest)) = types.split_first() else { return Some(None) };
        *types = rest;
        let (v, n) = get_varint(rvas)?;
        *rvas = &rvas[n..];
        last = last.wrapping_add(v);
        (ty <= 15).then_some(Some((last, ty)))
    };
    let mut i = 0usize;
    let mut next_wrong = match wrong.is_empty() {
        true => None,
        false => Some(take_varint(&mut wrong)?),
    };
    let mut next_pred = || -> Option<Option<u32>> {
        loop {
            let Some(p) = next_pred()? else { return Some(None) };
            i += 1;
            let i = i - 1;
            if next_wrong != Some(i as u64) {
                return Some(Some(p));
            }
            next_wrong = match wrong.is_empty() {
                true => None,
                false => Some((i as u64 + 1).checked_add(take_varint(&mut wrong)?)?),
            };
        }
    };
    let (mut l, mut p) = (next_listed(&mut rvas, &mut types)?, next_pred()?);
    loop {
        let (rva, ty) = match (l, p) {
            (None, None) => break,
            (Some(e), None) => {
                l = next_listed(&mut rvas, &mut types)?;
                e
            }
            (Some(e), Some(q)) if e.0 < q => {
                l = next_listed(&mut rvas, &mut types)?;
                e
            }
            (Some(e), Some(q)) if e.0 == q => return None,
            (_, Some(q)) => {
                p = next_pred()?;
                (q, t)
            }
        };
        let page = rva & !0xfff;
        if start.is_none_or(|(_, p)| p != page) {
            if let Some((s, _)) = start {
                close(&mut out, s);
            }
            start = Some((out.len(), page));
            out.extend_from_slice(&page.to_le_bytes());
            out.extend_from_slice(&[0; 4]);
        }
        out.extend_from_slice(&((ty as u16) << 12 | (rva & 0xfff) as u16).to_le_bytes());
        if out.len() > len {
            return None;
        }
    }
    if let Some((s, _)) = start {
        close(&mut out, s);
    }
    // (every stream read to its end, every wrong index of a prediction)
    (rvas.is_empty() && next_wrong.is_none() && out.len() == len).then_some(out)
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

/// [`get_varint`] taken off the front of `d`.
fn take_varint(d: &mut &[u8]) -> Option<u64> {
    let (v, n) = get_varint(d)?;
    *d = &d[n..];
    Some(v as u64)
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
    fn predicted() {
        let pages: &[(u32, &[u16])] =
            &[(0x1000, &[0x3004, 0x3010, 0x3ffc]), (0x2000, &[0x3080, 0x3090, 0xa100]), (0x5000, &[0x3000])];
        let d = blocks(pages, 2);
        // right, wrong (no entry, an entry of another type), and one missed
        let pred = [0x1004, 0x1008, 0x1010, 0x1ffc, 0x2080, 0x2090, 0x2100];
        let [rvas, types, wrong] = encode_predicted(&d, 2, &pred, 3).unwrap();
        assert_eq!(types, [10, 3], "the entries not predicted");
        assert_eq!(wrong, [1, 4], "of indices 1 and 6");
        let p = pred_bytes(&pred);
        assert_eq!(decode_predicted([&rvas, &types, &wrong], &p, 3, 2, d.len()), Some(d.clone()));
        // with none predicted, as version 1
        let [r1, t1, w1] = encode_predicted(&d, 2, &[], 3).unwrap();
        assert!(w1.is_empty());
        assert_eq!([r1.clone(), t1.clone()], encode(&d, 2).unwrap());
        assert_eq!(decode(&r1, &t1, 2, d.len()), Some(d.clone()));
        // all predicted, all right
        let all = [0x1004, 0x1010, 0x1ffc, 0x2080, 0x2090, 0x5000];
        let [r, t, w] = encode_predicted(&d, 2, &all, 3).unwrap();
        assert_eq!((t.len(), w.len()), (1, 0));
        assert_eq!(decode_predicted([&r, &t, &w], &pred_bytes(&all), 3, 2, d.len()), Some(d.clone()));
        // damaged: a wrong index past the predictions, one more, a prediction twice, a type
        let run = |w: &[u8], p: &[u8], t: u8| decode_predicted([&rvas, &types, w], p, t, 2, d.len());
        assert_eq!(run(&[1, 9], &p, 3), None);
        assert_eq!(run(&[1, 4, 0], &p, 3), None);
        assert_eq!(run(&wrong, &p[..p.len() - 1], 3), None);
        assert_eq!(run(&wrong, &p, 16), None);
        assert_ne!(run(&[2, 3], &p, 3), Some(d.clone()));
        // entries out of order are not predicted
        let mixed = blocks(&[(0x2000, &[0x3080]), (0x1000, &[0x3004])], 2);
        assert!(encode_predicted(&mixed, 2, &[0x1004], 3).is_none());
        assert!(encode_predicted(&mixed, 2, &[], 3).is_some());
        // a prediction the same as an entry listed is no valid input
        assert_eq!(decode_predicted([&[0x84, 0x20], &[3], &[]], &pred_bytes(&[0x1004]), 3, 2, 12), None);
    }

    #[test]
    fn data_predicted() {
        let f = Data { base: 0x40_0000, lo: 0x1000, hi: 0x8000, size: 4 };
        // two pieces: at 0x2002 (its first aligned word at 0x2004) and at 0x3000
        let mut data = vec![0u8; 2];
        for v in [0x40_1000u32, 7, 0x40_9000, 0x40_8000] {
            data.extend(v.to_le_bytes());
        }
        data.extend([0x00, 0x10, 0x40]); // a word cut by the end of the piece
        let n0 = data.len() as u32;
        for v in [0x40_2000u32, 0x3f_f000] {
            data.extend(v.to_le_bytes());
        }
        let piece = |rva: u32, n: u32| {
            let mut r = [0u8; 8];
            r[..4].copy_from_slice(&rva.to_le_bytes());
            r[4..].copy_from_slice(&n.to_le_bytes());
            r
        };
        let pieces = [piece(0x2002, n0), piece(0x3000, data.len() as u32 - n0)];
        let got: Vec<u32> = data_predictions(f, &pieces, &data).collect();
        assert_eq!(got, [0x2002 + 2, 0x2002 + 14, 0x3000]);
        // of 8 bytes, aligned to 8
        let f8 = Data { size: 8, ..f };
        let mut d8 = vec![0u8; 8];
        d8.extend(0x40_1000u64.to_le_bytes());
        let p8 = [piece(0x5000, d8.len() as u32)];
        assert_eq!(data_predictions(f8, &p8, &d8).collect::<Vec<_>>(), [0x5008]);
        // merged with others by RVA; one RVA in both is no valid input
        let mut m = merged([0x1000u32, 0x200c].into_iter(), got.iter().copied());
        let all: Vec<_> = std::iter::from_fn(|| m().unwrap()).collect();
        assert_eq!(all, [0x1000, 0x2004, 0x200c, 0x2010, 0x3000]);
        let mut m = merged([0x2004u32].into_iter(), got.iter().copied());
        assert_eq!(m(), None);
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
