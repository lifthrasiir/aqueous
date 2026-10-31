//! EGG recompression: an archive of ALZip's EGG format is rebuilt bit for bit from the contents
//! of its files, so that the contents are stored instead: DEFLATE blocks by aqr-flate, bzip2
//! blocks by aqr-bz2, LZMA blocks by aqr-lzma, AZO blocks by aqr-azo. `formula` is the entry
//! point (see aqr-formula); `egg` reads the archive. `FORMAT.md` describes the format and the
//! formula.
//!
//! The crate has no module of its own: the formula puts the blocks rebuilt between the bytes it
//! keeps (the headers, and whatever is not rebuilt).

pub mod egg;
pub mod formula;
