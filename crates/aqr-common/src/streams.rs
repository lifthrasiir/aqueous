//! Streams of data and records one after another, as the records of a parse (aqr-lzo, aqr-azo,
//! aqr-lha, aqr-lzma, aqr-rar) keep them: the data of every stream in one input, their records
//! in another, and an index of the two lengths of each in a third.

use crate::varint;

/// The index of streams of data and records of `lens` bytes: for each, the two lengths as
/// varints; empty for a single stream.
pub fn index(lens: &[(usize, usize)]) -> Vec<u8> {
    let mut out = Vec::new();
    if lens.len() > 1 {
        for &(d, r) in lens {
            varint::put(&mut out, d as u64);
            varint::put(&mut out, r as u64);
        }
    }
    out
}

/// The streams that `index` cuts into ([`cut`]): a varint each of their two lengths, one where
/// it is empty.
pub fn streams(index: &[u8]) -> usize {
    match index.is_empty() {
        true => 1,
        false => count(index) / 2,
    }
}

/// The varints of `index`: its bytes that end one (of the high bit clear).
fn count(index: &[u8]) -> usize {
    index.iter().filter(|&&b| b < 0x80).count()
}

/// The data and records of streams, each its own.
pub type Streams<'a> = Vec<(&'a [u8], &'a [u8])>;

/// The data and records of each stream that `index` cuts `data` and `records` into; the error
/// of `corrupt` where the index does not cut them whole. (Generic over the error, so that each
/// module has it as it had its own: a function of aqr-common's own came to more bytes.)
pub fn cut<'a, E>(data: &'a [u8], records: &'a [u8], index: &[u8], corrupt: impl Fn() -> E) -> Result<Streams<'a>, E> {
    if index.is_empty() {
        return Ok(vec![(data, records)]);
    }
    let (mut at, mut d, mut r) = (0, data, records);
    // (reserved for them all: the varints counted, two a stream)
    let mut out = Vec::with_capacity(count(index) / 2);
    while at < index.len() {
        let mut take = |from: &mut &'a [u8]| {
            let n = varint::get(index, &mut at).map_err(|_| corrupt())?;
            let n = usize::try_from(n).ok().filter(|&n| n <= from.len()).ok_or_else(&corrupt)?;
            let (a, b) = from.split_at(n);
            *from = b;
            Ok(a)
        };
        out.push((take(&mut d)?, take(&mut r)?));
    }
    if !d.is_empty() || !r.is_empty() {
        return Err(corrupt());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cut_by_index() {
        let (data, records) = (b"aaabbbbb", b"xyyy");
        let cut = |i: &[u8]| cut(data, records, i, || ());
        assert_eq!(cut(&index(&[(8, 4)])), Ok(vec![(&data[..], &records[..])]));
        let i = index(&[(3, 1), (5, 3)]);
        assert_eq!(cut(&i), Ok(vec![(&b"aaa"[..], &b"x"[..]), (&b"bbbbb"[..], &b"yyy"[..])]));
        assert_eq!(cut(&index(&[(3, 1), (4, 3)])), Err(()), "data left");
        assert_eq!(cut(&index(&[(3, 1), (6, 3)])), Err(()), "past the data");
        assert_eq!(cut(&i[..3]), Err(()), "an index cut short");
    }
}
