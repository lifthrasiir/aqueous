//! Packed byte sequences (spec, "Packed byte sequences"): a list of byte sequences as one, for
//! a module taking a number of them that varies between applications as a single input. The
//! number of items, the end of each item (after the header) and the items, the numbers as
//! u64le. aqr-formula's `Node::pack` makes one in a formula; a module [`unpack`]s it.

/// Bytes of the header of `n` items.
pub const fn header_len(n: usize) -> u64 {
    8 * (n as u64 + 1)
}

/// Appends the header of items of `lens` bytes to `out`.
pub fn put_header(out: &mut Vec<u8>, lens: impl ExactSizeIterator<Item = u64>) {
    out.extend_from_slice(&(lens.len() as u64).to_le_bytes());
    let mut end = 0u64;
    for len in lens {
        end += len;
        out.extend_from_slice(&end.to_le_bytes());
    }
}

/// The packed byte sequence of `items`.
pub fn pack<T: AsRef<[u8]>>(items: &[T]) -> Vec<u8> {
    let len = header_len(items.len()) as usize + items.iter().map(|i| i.as_ref().len()).sum::<usize>();
    let mut out = Vec::with_capacity(len);
    put_header(&mut out, items.iter().map(|i| i.as_ref().len() as u64));
    items.iter().for_each(|i| out.extend_from_slice(i.as_ref()));
    out
}

/// The items of `p`, or None where it is not a packed byte sequence.
pub fn unpack(p: &[u8]) -> Option<Vec<&[u8]>> {
    let u64_at = |at: usize| Some(u64::from_le_bytes(*p.get(at..)?.first_chunk()?));
    let n = usize::try_from(u64_at(0)?).ok()?;
    let h = n.checked_add(1)?.checked_mul(8).filter(|&h| h <= p.len())?;
    let (mut items, mut start) = (Vec::with_capacity(n), h);
    for i in 0..n {
        let end = usize::try_from(u64_at(8 * (i + 1))?).ok()?.checked_add(h).filter(|&e| e >= start && e <= p.len())?;
        items.push(&p[start..end]);
        start = end;
    }
    (start == p.len()).then_some(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let items: [&[u8]; 4] = [b"ab", b"", b"cde", b""];
        let p = pack(&items);
        assert_eq!(p.len() as u64, header_len(4) + 5);
        assert_eq!(&p[..16], [4, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(unpack(&p).unwrap(), items);
        assert_eq!(pack::<&[u8]>(&[]), [0; 8]);
        assert_eq!(unpack(&[0; 8]).unwrap(), Vec::<&[u8]>::new());
    }

    #[test]
    fn invalid() {
        let p = pack(&[b"ab", b"cd"]);
        assert_eq!(unpack(&p[..7]), None);
        // too short, too long
        assert_eq!(unpack(&p[..p.len() - 1]), None);
        assert_eq!(unpack(&[&p[..], b"x"].concat()), None);
        // a count past the end, and one that overflows
        let mut q = p.clone();
        q[0] = 5;
        assert_eq!(unpack(&q), None);
        q[..8].copy_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(unpack(&q), None);
        // decreasing ends
        let mut q = p.clone();
        q[8] = 3;
        q[16] = 2;
        assert_eq!(unpack(&q), None);
        // an end past the end
        let mut q = p;
        q[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(unpack(&q), None);
    }
}
