//! aqr-dwarf as modules of entry formulas (see aqr-formula): sections of DWARF rebuilt from
//! their streams (FORMAT.md), which the archive compresses.

use aqr_formula::{Formula, Kind, Module, Node};

pub use crate::module::apple as decode_apple;

pub const APPLE: Module = Module::new("dwarf-apple", 1).with_memory(apple_memory);

/// The memory of [`APPLE`]: the output, and of each entry (each a byte of the strings' deltas
/// at least) its hash, bucket, string and records (16 bytes), its end and its place in the
/// order, twice as it is sorted (12 bytes); each DIE offset (each a byte of its stream at least,
/// 4 bytes); the counts of the sort, of 2^16 and of the buckets (4 bytes each, the buckets at
/// most the output's words).
fn apple_memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let (entries, dies) = (*u.inputs.get(1)?, *u.inputs.get(3)?);
    Some(u.whole(28 * entries + 4 * dies + (4 << 16) + u.output(), 6))
}

/// The application of [`APPLE`] to the table `d`, whose strings are `.debug_str` `strs` (the
/// node `str_node`), its streams added to `f`; `None` where it is not one the module rebuilds.
pub fn apple(f: &mut Formula, d: &[u8], strs: &[u8], str_node: Node) -> Option<Node> {
    let (params, st) = crate::apple::encode(d, strs)?;
    let mut inputs = vec![str_node];
    for (s, name) in st.into_iter().zip(crate::apple::STREAM_NAMES) {
        inputs.push(f.push_as(format!("dwarf-apple/{name}"), Kind::Table, s));
    }
    Some(Node::apply(APPLE, params, inputs, d.len() as u64))
}
