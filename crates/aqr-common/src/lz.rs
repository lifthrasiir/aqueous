//! What the LZ parsers and their records share (aqr-lzo, aqr-azo, aqr-lha, aqr-bvx, aqr-lzma,
//! aqr-rar, aqr-lzx): the length of a match.

/// The length of the common prefix of `d[a..]` and `d[b..]`, `a < b`, up to the end of `d`.
#[inline]
pub fn lcp(d: &[u8], a: usize, b: usize) -> usize {
    lcp_up(d, a, b, usize::MAX)
}

/// [`lcp`] up to `cap` bytes.
// (inlined natively, where its callers' loops are hot; called in Wasm, whose modules grow by
// some kilobytes each otherwise)
#[cfg_attr(not(target_arch = "wasm32"), inline)]
#[cfg_attr(target_arch = "wasm32", inline(never))]
pub fn lcp_up(d: &[u8], a: usize, b: usize, cap: usize) -> usize {
    let n = cap.min(d.len() - b);
    // (both sides cut to `n` bytes first, so that the words within them are not checked again)
    let (x, y) = (&d[a..a + n], &d[b..b + n]);
    let mut i = 0;
    for (p, q) in x.chunks_exact(8).zip(y.chunks_exact(8)) {
        let w = u64::from_le_bytes(p.try_into().unwrap()) ^ u64::from_le_bytes(q.try_into().unwrap());
        if w != 0 {
            return i + (w.trailing_zeros() / 8) as usize;
        }
        i += 8;
    }
    while i < n && x[i] == y[i] {
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
