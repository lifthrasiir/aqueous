//! What readers read: a disk's bytes by ranges ([`Image`]), of a plain image (the file's bytes as
//! they are, lent) or of a container's virtual disk (put together from where its blocks are, those
//! not allocated zeros), and a volume within one ([`Sub`]).

use std::borrow::Cow;
use std::ops::Range;

/// Bytes read by ranges. Readers read what they need of it, bounded: of a container, every read
/// puts its bytes together.
pub trait Image: Sync {
    /// Bytes of all of it.
    fn len(&self) -> usize;

    /// The bytes of `r`, where all of it is within.
    fn get(&self, r: Range<usize>) -> Option<Cow<'_, [u8]>>;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The bytes of `r` that are within (cut at the end; none where it starts past it).
    fn cut(&self, r: Range<usize>) -> Cow<'_, [u8]> {
        let end = r.end.min(self.len());
        self.get(r.start.min(end)..end).unwrap_or(Cow::Borrowed(&[]))
    }
}

/// An image of the bytes of a file as they are.
pub struct Plain<'a>(pub &'a [u8]);

impl Image for Plain<'_> {
    fn len(&self) -> usize {
        self.0.len()
    }

    fn get(&self, r: Range<usize>) -> Option<Cow<'_, [u8]>> {
        self.0.get(r).map(Cow::Borrowed)
    }
}

/// Bytes `span` of image `of` (a volume of a disk), cut at its end.
pub struct Sub<'a> {
    pub of: &'a dyn Image,
    pub span: Range<usize>,
}

impl Image for Sub<'_> {
    fn len(&self) -> usize {
        self.span.end.min(self.of.len()).saturating_sub(self.span.start)
    }

    fn get(&self, r: Range<usize>) -> Option<Cow<'_, [u8]>> {
        if r.start > r.end || r.end > self.len() {
            return None;
        }
        self.of.get(self.span.start + r.start..self.span.start + r.end)
    }
}
