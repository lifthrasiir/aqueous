//! aqr-dwarf as modules of entry formulas (see aqr-formula): sections of DWARF rebuilt from
//! their streams (FORMAT.md), which the archive compresses.

use aqr_formula::{Formula, Kind, Module, Node};

pub use crate::module::{apple as decode_apple, info as decode_info};

pub const APPLE: Module = Module::new("dwarf-apple", 1).with_memory(apple_memory);
pub const INFO: Module = Module::new("dwarf-info", 1).with_memory(info_memory);

/// The memory of [`APPLE`]: the output, and of each entry (each a byte of the strings' deltas
/// at least) its hash, bucket, string and records (16 bytes), its end and its place in the
/// order, twice as it is sorted (12 bytes); each DIE offset (each a byte of its stream at least,
/// 4 bytes); the counts of the sort, of 2^16 and of the buckets (4 bytes each, the buckets at
/// most the output's words).
fn apple_memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let (entries, dies) = (*u.inputs.get(1)?, *u.inputs.get(3)?);
    Some(u.whole(28 * entries + 4 * dies + (4 << 16) + u.output(), 6))
}

/// The memory of [`INFO`]: the output; the inputs unpacked (borrowed); the offsets of the
/// strings of `.debug_str` (4 bytes each); the abbreviation tables (their slots and attributes at
/// most `info::budget`, 32 bytes each); the start of each DIE (4 bytes each, each a byte of the
/// codes at least) and the references not resolved (32 bytes each, each a byte of the references
/// at least).
fn info_memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let (abbrev, strs, die) = (*u.inputs.first()?, *u.inputs.get(1)?, *u.inputs.get(2)?);
    let tables = 32 * crate::info::budget(abbrev as usize) as u64;
    let strings = u.content(1).map_or(strs, |s| s.iter().filter(|&&c| c == 0).count() as u64 + 1);
    // (the codes and the references, by the lengths at the head of their input)
    let (codes, refs) = match u.content(2).and_then(|d| crate::unpack(d, 3).ok()) {
        Some(s) => (s[0].len() as u64, s[2].len() as u64),
        None => (die, die),
    };
    Some(u.whole(4 * strings + tables + 4 * codes + 32 * refs + (16 << 10), 6))
}

/// The application of [`APPLE`] to the table `d`, whose strings are `.debug_str` `strs` (the
/// node `str_node` makes), its streams added to `f`; `None` where it is not one the module
/// rebuilds.
pub fn apple(f: &mut Formula, d: &[u8], strs: &[u8], str_node: impl FnOnce(&mut Formula) -> Node) -> Option<Node> {
    let (params, st) = crate::apple::encode(d, strs)?;
    let mut inputs = vec![str_node(f)];
    for (s, name) in st.into_iter().zip(crate::apple::STREAM_NAMES) {
        inputs.push(f.push_as(format!("dwarf-apple/{name}"), Kind::Table, s));
    }
    Some(Node::apply(APPLE, params, inputs, d.len() as u64))
}

/// The application of [`INFO`] to `.debug_info` `d` of the abbreviations `abbrev` and the
/// strings `strs` (the nodes `nodes` makes of them), its streams added to `f`; `None` where it
/// is not rebuilt as it is.
pub fn info(
    f: &mut Formula,
    d: &[u8],
    abbrev: &[u8],
    strs: &[u8],
    nodes: impl FnOnce(&mut Formula) -> (Node, Node),
) -> Option<Node> {
    let st = crate::info::encode(d, abbrev, strs)?;
    let (a, s) = nodes(f);
    let mut inputs = vec![a, s];
    for (name, which) in crate::info::INPUTS {
        let parts: Vec<&[u8]> = which.iter().map(|&k| &st[k][..]).collect();
        inputs.push(f.push_as(format!("dwarf-info/{name}"), Kind::Table, crate::pack(&parts)));
    }
    Some(Node::apply(INFO, Vec::new(), inputs, d.len() as u64))
}
