//! ALZ recompression: an archive of ALZip's is rebuilt bit for bit from the contents of its
//! entries, so that the contents are stored instead: DEFLATE bodies by aqr-flate, bzip2 bodies
//! (ALZip's framing of libbzip2's compressor) by aqr-bz2. `formula` is the entry point (see
//! aqr-formula); `alz` reads the archive. `FORMAT.md` describes the format and the formula.
//!
//! The crate has no module of its own: the formula puts the bodies rebuilt between the bytes it
//! keeps (the headers, and whatever is not rebuilt).

pub mod alz;
pub mod formula;
