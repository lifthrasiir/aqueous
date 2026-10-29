//! Output made in order ([`Out`]): into a vector, or compared with the bytes it should be
//! ([`Check`]), so that an encoder checks that what it stores gives its input back without
//! making a copy of the input.

use std::ops::Range;

/// Where output goes, byte after byte; what is out so far may be read back.
pub trait Out {
    /// Bytes out so far.
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn put(&mut self, d: &[u8]);

    /// Puts bytes `r` of what is out so far again.
    fn put_within(&mut self, r: Range<usize>);

    /// Bytes `r` of what is out so far.
    fn out(&self, r: Range<usize>) -> &[u8];
}

impl Out for Vec<u8> {
    fn len(&self) -> usize {
        Vec::len(self)
    }

    fn put(&mut self, d: &[u8]) {
        self.extend_from_slice(d);
    }

    fn put_within(&mut self, r: Range<usize>) {
        self.extend_from_within(r);
    }

    fn out(&self, r: Range<usize>) -> &[u8] {
        &self[r]
    }
}

/// Output compared with `want` as it is made, rather than kept: what is out so far is read back
/// from `want` (all the same while [`Check::same`]).
pub struct Check<'a> {
    want: &'a [u8],
    at: usize,
    same: bool,
}

impl<'a> Check<'a> {
    pub fn new(want: &'a [u8]) -> Check<'a> {
        Check { want, at: 0, same: true }
    }

    /// Whether the output is all of `want`.
    pub fn same(&self) -> bool {
        self.same && self.at == self.want.len()
    }
}

impl Out for Check<'_> {
    fn len(&self) -> usize {
        self.at
    }

    fn put(&mut self, d: &[u8]) {
        let end = self.at.saturating_add(d.len());
        self.same &= self.want.get(self.at..end) == Some(d);
        self.at = end;
    }

    fn put_within(&mut self, r: Range<usize>) {
        let want = self.want;
        self.put(want.get(r).unwrap_or(&[]));
    }

    fn out(&self, r: Range<usize>) -> &[u8] {
        // (where the output is not the same, any bytes do)
        self.want.get(r).unwrap_or(&[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check() {
        let want = b"abcabcx";
        let made = |c: &mut dyn Out| {
            c.put(b"abc");
            c.put_within(0..3);
            c.put(b"x");
        };
        let mut v = Vec::new();
        made(&mut v);
        assert_eq!(v, want);
        let mut c = Check::new(want);
        made(&mut c);
        assert!(c.same());
        for other in [&b"abcabc"[..], b"abcabcxy", b"abdabcx", b""] {
            let mut c = Check::new(other);
            made(&mut c);
            assert!(!c.same(), "{other:?}");
        }
        let mut c = Check::new(b"ab");
        c.put_within(5..9);
        assert!(!c.same() && c.out(0..9).is_empty());
    }
}
