//! aqr-pdb as modules of entry formulas (see aqr-formula): a PDB file rebuilt from its streams
//! (`pdb-msf`), the modules' symbol records from theirs (`pdb-symbols`; FORMAT.md), which the
//! archive compresses.

use std::ops::Range;

use aqr_formula::{Apply, Formula, Kind, Module, Node};

use crate::{dbi, msf, symbols};

pub use crate::module::{msf as decode, symbols as decode_symbols};

pub const MODULE: Module = Module::new("pdb-msf", 1).with_plan(memory);
pub const SYMBOLS: Module = Module::new("pdb-symbols", 1).with_memory(symbols_memory);

/// Files of modules' symbols shorter than this in all are left as they are: the modules' blocks
/// (about 14 KB) would cost more than the symbols coded save, and the container alone saves
/// nothing (Windows' public PDBs, mostly public symbols and section contributions, archive 4 to
/// 13% larger through it).
const MIN_SYMBOLS: usize = 512 << 10;

/// The memory of [`MSF`]: its initial memory, and its plan ([`crate::module::msf_plan`]).
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    Some(u.initial() + crate::module::msf_plan(u.inputs, u.content(1)?, u.output())?)
}

/// The memory of [`SYMBOLS`]: the output; the scopes open (16 bytes each, each a record at
/// least, each 2 bytes of the kinds in the first input at least); the inlinees' last files and
/// lines (about 32 bytes each in a hash map, each an entry of inlinee lines at least, each 4
/// bytes of the C13 lines' input at least).
fn symbols_memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let (head, c13) = (*u.inputs.first()?, *u.inputs.get(4)?);
    Some(u.whole(8 * head + 8 * c13 + (16 << 10), 3))
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
    let (subs, outputs) = symbols_apply(&mut f, &streams, &spans)?;
    // the streams but the modules', which the outputs fill
    let (mut kept, mut parts, mut at) = (Vec::new(), Vec::new(), 0);
    for (r, out) in subs.into_iter().zip(outputs) {
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
    let st = symbols::encode(&parts)?;
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
        params: vec![],
        inputs,
        len: 0,
        memory_limit: aqr_formula::NO_LIMIT,
        task_limits: Vec::new(),
    };
    let outputs = f.split(a, subs.iter().map(|r| r.len() as u64).collect());
    Some((subs, outputs))
}
