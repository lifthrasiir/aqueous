//! What the LZ parsers and their records share (aqr-lzo, aqr-azo, aqr-lha, aqr-bvx, aqr-lzma,
//! aqr-rar, aqr-lzx): the length of a match.

/// The length of the common prefix of `d[a..]` and `d[b..]`, `a < b`, up to the end of `d`.
#[inline]
pub fn lcp(d: &[u8], a: usize, b: usize) -> usize {
    lcp_up(d, a, b, usize::MAX)
}

/// [`lcp`] up to `cap` bytes.
#[inline]
pub fn lcp_up(d: &[u8], a: usize, b: usize, cap: usize) -> usize {
    let n = cap.min(d.len() - b);
    let mut i = 0;
    while i + 8 <= n {
        let x = u64::from_le_bytes(d[a + i..a + i + 8].try_into().unwrap())
            ^ u64::from_le_bytes(d[b + i..b + i + 8].try_into().unwrap());
        if x != 0 {
            return i + (x.trailing_zeros() / 8) as usize;
        }
        i += 8;
    }
    while i < n && d[a + i] == d[b + i] {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_prefixes() {
        let d = b"abcdefghijk_abcdefghijk_abcdefgh!";
        assert_eq!(lcp(d, 0, 12), 20);
        assert_eq!(lcp(d, 12, 24), 8);
        assert_eq!(lcp_up(d, 0, 12, 5), 5);
        assert_eq!(lcp_up(d, 0, 12, 100), 20);
        assert_eq!(lcp(d, 0, 1), 0);
        assert_eq!(lcp(b"aaaa", 0, 1), 3);
    }
}
