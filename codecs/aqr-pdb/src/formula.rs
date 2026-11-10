//! aqr-pdb as modules of entry formulas (see aqr-formula): a PDB file rebuilt from its streams
//! (`pdb-msf`), the modules' symbol records from theirs (`pdb-symbols`; FORMAT.md), which the
//! archive compresses.

use std::ops::Range;

use aqr_formula::{Apply, Formula, Kind, Module, Node};

use crate::{dbi, msf, symbols, types};

pub use crate::module::{msf as decode, symbols as decode_symbols, types as decode_types};

pub const MODULE: Module = Module::new("pdb-msf", 1).with_plan(memory);
pub const SYMBOLS: Module = Module::new("pdb-symbols", 2).with_plan(symbols_memory);
pub const TYPES: Module = Module::new("pdb-types", 1).with_plan(types_memory);

/// Files of modules' symbols shorter than this in all are left as they are: the modules' blocks
/// (about 14 KB) would cost more than the symbols coded save, and the container alone saves
/// nothing (Windows' public PDBs, mostly public symbols and section contributions, archive 4 to
/// 13% larger through it).
const MIN_SYMBOLS: usize = 512 << 10;

/// TPI and IPI streams of less than this in all are left as they are, for the same reason.
const MIN_TYPES: usize = 64 << 10;

/// The memory of [`MODULE`]: its initial memory, and its plan ([`crate::module::msf_plan`]).
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    Some(u.initial() + crate::module::msf_plan(u.inputs, u.content(1)?, u.output())?)
}

/// The memory of [`SYMBOLS`]: its initial memory, and its plan ([`crate::module::symbols_plan`]).
fn symbols_memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    Some(u.initial() + crate::module::symbols_plan(u.params, u.inputs, u.content(0)?, u.content(4)?, u.output())?)
}

/// The memory of [`TYPES`]: its initial memory, and its plan ([`crate::types::plan`]).
fn types_memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    Some(crate::types::plan(u.initial()))
}

/// Whether `d` starts like an MSF file.
pub fn starts(d: &[u8]) -> bool {
    msf::starts(d)
}

/// The length of the MSF file at the start of `d`, by its superblock.
pub fn extent(d: &[u8]) -> Option<usize> {
    msf::extent(d)
}

/// The formula of PDB file `input`, or `None` where it is not one this crate takes (of modules'
/// symbols too short for the modules to pay). It is the same whatever `threads` is (one is
/// used).
pub fn encode(input: &[u8], _threads: usize) -> Option<Formula> {
    let ([streams, layout, rest], spans) = msf::encode_streams(input)?;
    let mut f = Formula::default();
    f.push_content("pdb/streams", Kind::Unknown, Vec::new());
    // the streams the modules make, in the order they are in the file
    let mut made: Vec<(Range<usize>, Node)> = Vec::new();
    if let Some((subs, outputs)) = symbols_apply(&mut f, &streams, &spans) {
        made.extend(subs.into_iter().zip(outputs));
    }
    made.extend(types_apply(&mut f, &streams, &spans));
    if made.is_empty() {
        return None;
    }
    made.sort_by_key(|m| m.0.start);
    if made.windows(2).any(|w| w[0].0.end > w[1].0.start) {
        return None;
    }
    // the streams but those, which the outputs fill
    let (mut kept, mut parts, mut at) = (Vec::new(), Vec::new(), 0);
    for (r, out) in made {
        parts.push(Node::slice(0, kept.len() as u64, (r.start - at) as u64));
        kept.extend_from_slice(&streams[at..r.start]);
        parts.push(out);
        at = r.end;
    }
    parts.push(Node::slice(0, kept.len() as u64, (streams.len() - at) as u64));
    kept.extend_from_slice(&streams[at..]);
    f.streams[0].data = kept.into();
    let inputs = vec![Node::concat(parts), f.push_as("pdb/layout", Kind::Table, layout), f.push("pdb/rest", rest)];
    f.root = Node::apply(MODULE, Vec::new(), inputs, input.len() as u64);
    Some(f)
}

/// The TPI and IPI streams among `streams` (the streams one after another, each where `spans`
/// tells) through [`TYPES`], with their hash streams: where each is, and the node that makes it.
/// None are made where the types are fewer than [`MIN_TYPES`] bytes in all.
fn types_apply(f: &mut Formula, streams: &[u8], spans: &[Option<Range<usize>>]) -> Vec<(Range<usize>, Node)> {
    let Some(e) = types::encode(streams, spans) else { return Vec::new() };
    if e.places.iter().map(|p| p.bytes.len()).sum::<usize>() < MIN_TYPES {
        return Vec::new();
    }
    let inputs = types::STREAM_NAMES
        .iter()
        .zip(&e.streams)
        .map(|(name, data)| {
            let kind = if *name == "name" { Kind::Text } else { Kind::Table };
            f.push_as(format!("pdb-types/{name}"), kind, data.clone())
        })
        .collect();
    let a = Apply {
        module: TYPES,
        resources: vec![],
        params: e.params,
        inputs,
        len: 0,
        memory_limit: aqr_formula::NO_LIMIT,
        task_limits: Vec::new(),
    };
    let outputs = f.share(a, e.sizes.to_vec());
    e.places
        .into_iter()
        .map(|p| {
            let nodes: Vec<Node> = p.outputs.iter().map(|&k| outputs[k].clone()).collect();
            let node = if nodes.len() == 1 { nodes.into_iter().next().unwrap() } else { Node::concat(nodes) };
            (p.bytes, node)
        })
        .collect()
}

/// The modules' streams among `streams` (the streams one after another, each where `spans`
/// tells) through [`SYMBOLS`]: where each is, and the outputs of the application.
fn symbols_apply(
    f: &mut Formula,
    streams: &[u8],
    spans: &[Option<Range<usize>>],
) -> Option<(Vec<Range<usize>>, Vec<Node>)> {
    let stream = |k: usize| spans.get(k).cloned().flatten();
    let mods = dbi::modules(&streams[stream(3)?])?;
    // the modules' streams, each whole, and what the DBI stream tells of it
    let mut subs: Vec<(Range<usize>, dbi::Module)> = mods
        .into_iter()
        .filter_map(|m| stream(m.stream).filter(|r| m.symbols <= r.len() && !r.is_empty()).map(|r| (r, m)))
        .collect();
    subs.sort_by_key(|s| s.0.start);
    subs.dedup_by(|b, a| b.0.start < a.0.end);
    if subs.iter().map(|s| s.1.symbols).sum::<usize>() < MIN_SYMBOLS {
        return None;
    }
    let parts: Vec<(&[u8], dbi::Module)> = subs.iter().map(|(r, m)| (&streams[r.clone()], *m)).collect();
    let (params, st) = symbols::encode(&parts)?;
    let subs: Vec<Range<usize>> = subs.into_iter().map(|s| s.0).collect();
    let inputs = symbols::INPUTS
        .iter()
        .map(|(name, which)| {
            let packed = crate::pack(&which.iter().map(|&k| &st[k][..]).collect::<Vec<_>>());
            // (what follows the symbols, their lines most, is scanned as content)
            match *name {
                "lines" => f.push_content(format!("pdb-symbols/{name}"), Kind::Unknown, packed),
                _ => f.push_as(format!("pdb-symbols/{name}"), Kind::Table, packed),
            }
        })
        .collect();
    let a = Apply {
        module: SYMBOLS,
        resources: vec![],
        params,
        inputs,
        len: 0,
        memory_limit: aqr_formula::NO_LIMIT,
        task_limits: Vec::new(),
    };
    let outputs = f.split(a, subs.iter().map(|r| r.len() as u64).collect());
    Some((subs, outputs))
}
