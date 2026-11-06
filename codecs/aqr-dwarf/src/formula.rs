//! aqr-dwarf as modules of entry formulas (see aqr-formula): sections of DWARF rebuilt from
//! their streams (FORMAT.md), which the archive compresses.

use aqr_formula::{Formula, Kind, Module, Node};

pub use crate::module::{apple as decode_apple, info as decode_info};

pub const APPLE: Module = Module::new("dwarf-apple", 1).with_plan(apple_memory);
pub const INFO: Module = Module::new("dwarf-info", 2).with_plan(info_memory);

/// The contents of the inputs of `u`.
fn contents<'a>(u: &aqr_formula::Usage<'a>) -> Option<Vec<&'a [u8]>> {
    (0..u.inputs.len()).map(|k| u.content(k)).collect()
}

/// The memory of [`APPLE`]: its initial memory, and its plan ([`crate::module::apple_plan`]).
fn apple_memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    Some(u.initial() + crate::module::apple_plan(u.params, &contents(u)?, u.output())?)
}

/// The memory of [`INFO`]: its initial memory, and its plan ([`crate::module::info_plan`]).
fn info_memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    Some(u.initial() + crate::module::info_plan(u.params, &contents(u)?, u.output())?)
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
    let (params, st) = crate::info::encode(d, abbrev, strs)?;
    let (a, s) = nodes(f);
    let mut inputs = vec![a, s];
    for (name, which) in crate::info::INPUTS {
        let parts: Vec<&[u8]> = which.iter().map(|&k| &st[k][..]).collect();
        inputs.push(f.push_as(format!("dwarf-info/{name}"), Kind::Table, crate::pack(&parts)));
    }
    Some(Node::apply(INFO, params, inputs, d.len() as u64))
}
