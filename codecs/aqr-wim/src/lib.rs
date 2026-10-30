//! WIM files taken apart into their resources' data: the chunks of each LZX resource rebuilt by
//! aqr-lzx's module from their data and record, resources of no compression as they are, the
//! rest (header, chunk tables, lookup table, XML data) kept. The formula is the concatenation
//! of these, so this crate has no module of its own. `formula` is the entry point (see
//! aqr-formula); `wim` reads the file.

pub mod formula;
pub mod wim;
