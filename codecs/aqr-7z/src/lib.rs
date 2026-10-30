//! 7z archives rebuilt bit for bit (`formula`): their folders of LZMA, LZMA2 or Copy alone from
//! the contents of the files they hold, by aqr-lzma's modules, and an encoded header likewise;
//! the rest of the archive (the signature header, other folders, the header) kept as it is.
//! `header` parses the archive; `write` writes small ones, for tests. There is no module of its
//! own.

pub mod formula;
pub mod header;
pub mod write;
