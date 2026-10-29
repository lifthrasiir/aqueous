//! aqr-lzma as a module of entry formulas (see aqr-formula): LZMA streams rebuilt from their
//! data and records. `FORMAT.md` specifies it. The data is an input of its own, which the
//! formula may recompress in turn (a tar archive's members, which other files may hold).

use aqr_formula::{Formula, Module, Node};

use crate::{alone, recons};

pub use crate::module::{decode, index};

pub const MODULE: Module = Module::new("lzma-recons", 1);

/// The most data a stream is decoded to.
const LIMIT: usize = 1 << 31;

/// Whether `d` starts like a `.lzma` file: a header of valid properties, a dictionary size as
/// encoders write it (2^n or 3 × 2^n bytes, 4 KiB at least), a data size that is unknown or below
/// 2^48, and a stream that decodes for its first 1 KiB ([`crate::lzma::plausible`]). Only for the
/// start of a file.
pub fn starts(d: &[u8]) -> bool {
    let Some(h) = alone::parse(d) else { return false };
    let dict = h.dict;
    dict >= 1 << 12
        && (dict.is_power_of_two() || dict % 3 == 0 && (dict / 3).is_power_of_two())
        && h.size.is_none_or(|s| s < 1 << 48)
        && crate::lzma::plausible(&d[alone::HEADER..], h.props, 1 << 10)
}

/// The data of the `.lzma` file `d` (as far as its stream goes), or None where it does not
/// decode: what [`encode`]'s formula holds as data, for telling what it may share with others.
pub fn data(d: &[u8]) -> Option<Vec<u8>> {
    let h = alone::parse(d)?;
    crate::lzma::decode(&d[alone::HEADER..], h.props, h.size, LIMIT).ok().map(|dec| dec.data)
}

/// The formula rebuilding `input`, a `.lzma` file, or None where it is not one or its stream
/// does not decode. What follows the stream is kept as it is.
pub fn encode(input: &[u8], _threads: usize) -> Option<Formula> {
    if !starts(input) {
        return None;
    }
    let h = alone::parse(input)?;
    let s = &input[alone::HEADER..];
    let sp = recons::split(s, h.props, h.dict, h.size, LIMIT).ok()?;
    let rest = s.len() - sp.used;
    let mut f = Formula::default();
    f.push("lzma", [&input[..alone::HEADER], &s[sp.used..]].concat());
    let data = f.push("raw", sp.data);
    let record = f.push("lzma/record", sp.record);
    let index = f.push("lzma/index", Vec::new());
    f.root = Node::concat([
        Node::slice(0, 0, alone::HEADER as u64),
        Node::apply(MODULE, Vec::new(), vec![data, record, index], sp.used as u64),
        Node::slice(0, alone::HEADER as u64, rest as u64),
    ]);
    Some(f)
}
