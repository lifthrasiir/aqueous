//! The data of a stream seen through a window: all of it where it is at hand, else the part
//! of it that the walk of the record reaches ([`Window::fed`]), read in as the walk goes. A
//! match finder and a model look at the bytes from a dictionary's reach before the position to
//! [`LOOK`] bytes after it, so that is all a window holds, besides some slack.

use aqr_common::lz::lcp_up;

use crate::stream::Pull;

/// Bytes after the position that a window holds (or all that are left of the data): a stored
/// chunk of LZMA2 (65536 bytes) and the longest match (273) to look at, and the bytes the match
/// finder hashes ahead.
pub const LOOK: usize = (1 << 16) + 1024;

/// Bytes that a window reads from its feed at a time at most.
const FILL: usize = 1 << 16;

/// The base of a window is a multiple of this, so that a position's low bits (the literal and
/// position contexts of LZMA, 4 bits at most) are the same in the window.
const ALIGN: usize = 4096;

/// Data of `end` bytes, of which the bytes `d` are at hand, from the position `base`.
#[derive(Clone, Copy)]
pub struct View<'a> {
    pub d: &'a [u8],
    pub base: usize,
    pub end: usize,
}

impl<'a> View<'a> {
    /// All of `d`.
    pub fn whole(d: &'a [u8]) -> Self {
        View { d, base: 0, end: d.len() }
    }

    /// The byte at position `p`.
    #[inline(always)]
    pub fn at(&self, p: usize) -> u8 {
        self.d[p - self.base]
    }

    /// The bytes from position `p` to `q`.
    #[inline(always)]
    pub fn get(&self, p: usize, q: usize) -> &'a [u8] {
        &self.d[p - self.base..q - self.base]
    }

    /// The length of the common prefix of the data at positions `a` and `b`, at most `cap`.
    #[inline(always)]
    pub fn lcp(&self, a: usize, b: usize, cap: usize) -> usize {
        lcp_up(self.d, a - self.base, b - self.base, cap)
    }
}

/// A window on the data of a stream of `total` bytes.
pub struct Window<'a> {
    /// All of the data, where it is at hand.
    whole: &'a [u8],
    /// The bytes held from `base` (a window fed by `feed`), in room for `cap` of them.
    buf: Vec<u8>,
    base: usize,
    total: usize,
    reach: usize,
    cap: usize,
    feed: Option<&'a mut dyn Pull>,
}

/// Bytes of the room of a window over data of `total` bytes that a match finder reaches `reach`
/// back in: the reach, the bytes after the position and the alignment of the base, and slack
/// (an eighth of the reach, a piece at least) so that the window is moved once for each
/// slack's worth of bytes. All the data where it is shorter.
pub fn room(total: usize, reach: usize) -> usize {
    total.min(reach.saturating_add(LOOK + ALIGN).saturating_add((reach / 8).max(FILL)))
}

impl<'a> Window<'a> {
    /// A window on all of `d`, at hand.
    pub fn whole(d: &'a [u8]) -> Self {
        Window { whole: d, buf: Vec::new(), base: 0, total: d.len(), reach: 0, cap: 0, feed: None }
    }

    /// A window on `total` bytes that `feed` gives in order, of matches at most `reach` back.
    pub fn fed(feed: &'a mut dyn Pull, total: usize, reach: usize) -> Self {
        let cap = room(total, reach);
        Window { whole: &[], buf: Vec::with_capacity(cap), base: 0, total, reach, cap, feed: Some(feed) }
    }

    pub fn total(&self) -> usize {
        self.total
    }

    pub fn view(&self) -> View<'_> {
        match self.feed {
            None => View::whole(self.whole),
            Some(_) => View { d: &self.buf, base: self.base, end: self.total },
        }
    }

    /// Reads on until the window holds the bytes up to [`LOOK`] after `pos` (or the end of the
    /// data), and from `pos - reach` (dropping those before). None where the feed ends before.
    #[inline(always)]
    pub fn ensure(&mut self, pos: usize) -> Option<()> {
        if self.feed.is_none() || self.base + self.buf.len() >= (pos + LOOK).min(self.total) {
            return Some(());
        }
        self.fill(pos)
    }

    #[cold]
    fn fill(&mut self, pos: usize) -> Option<()> {
        let want = (pos + LOOK).min(self.total);
        while self.base + self.buf.len() < want {
            if self.buf.len() == self.cap {
                let base = pos.saturating_sub(self.reach) / ALIGN * ALIGN;
                if base <= self.base {
                    return None;
                }
                self.buf.drain(..base - self.base);
                self.base = base;
            }
            let old = self.buf.len();
            let k = (self.cap - old).min(FILL).min(self.total - self.base - old);
            self.buf.resize(old + k, 0);
            let n = self.feed.as_deref_mut()?.pull(&mut self.buf[old..]);
            self.buf.truncate(old + n);
            if n == 0 {
                return None;
            }
        }
        Some(())
    }

    /// Reads (and drops) what the walk did not reach of the data, so that the feed is at the end
    /// of it. None where the feed ends before.
    pub fn finish(&mut self) -> Option<()> {
        let Some(feed) = self.feed.as_deref_mut() else { return Some(()) };
        let mut left = self.total - (self.base + self.buf.len());
        let mut junk = [0u8; 256];
        while left > 0 {
            let n = feed.pull(&mut junk[..left.min(256)]);
            if n == 0 {
                return None;
            }
            left -= n;
        }
        self.buf.clear();
        self.base = self.total;
        Some(())
    }
}
