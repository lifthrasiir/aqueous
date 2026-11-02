//! aqr-pdb as modules of entry formulas (see aqr-formula): a PDB file rebuilt from its streams
//! (`pdb-msf`), the modules' symbol records from theirs (`pdb-symbols`; FORMAT.md), which the
//! archive compresses.

use std::ops::Range;

use aqr_formula::{Apply, Formula, Kind, Module, Node};

use crate::{dbi, msf, symbols};

pub use crate::module::{msf as decode, symbols as decode_symbols};

pub const MODULE: Module = Module::new("pdb-msf", 1).with_memory(memory);
pub const SYMBOLS: Module = Module::new("pdb-symbols", 1).with_memory(symbols_memory);

/// Files of modules' symbols shorter than this in all are left as they are: the modules' blocks
/// (about 14 KB) would cost more than the symbols coded save, and the container alone saves
/// nothing (Windows' public PDBs, mostly public symbols and section contributions, archive 4 to
/// 13% larger through it).
const MIN_SYMBOLS: usize = 512 << 10;

/// The memory of [`MODULE`]: the output; the coverage of each block (4 bytes each, of the
/// output's blocks, of 512 bytes at least); the streams' lists of blocks (about 32 bytes each
/// and 4 bytes a block) and the directory (4 bytes a stream and a block), each stream and
/// block a byte of the layout at least.
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let layout = *u.inputs.get(1)?;
    Some(u.whole(u.output() / 128 + 48 * layout, 4))
}

/// The memory of [`SYMBOLS`]: the output, and the scopes open (16 bytes each, each a record
/// at least, each 2 bytes of the kinds in the first input at least).
fn symbols_memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let head = *u.inputs.first()?;
    Some(u.whole(8 * head + (16 << 10), 2))
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
    // the streams but the symbols, which the outputs fill
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

/// The modules' symbols among `streams` (the streams one after another, each where `spans`
/// tells) through [`SYMBOLS`]: where each module's are, and the outputs of the application.
fn symbols_apply(
    f: &mut Formula,
    streams: &[u8],
    spans: &[Option<Range<usize>>],
) -> Option<(Vec<Range<usize>>, Vec<Node>)> {
    let stream = |k: usize| spans.get(k).cloned().flatten();
    let mods = dbi::modules(&streams[stream(3)?])?;
    let mut subs: Vec<Range<usize>> = mods
        .into_iter()
        .filter_map(|(k, n)| stream(k).filter(|r| n <= r.len()).map(|r| r.start..r.start + n))
        .filter(|r| !r.is_empty())
        .collect();
    subs.sort_by_key(|r| r.start);
    subs.dedup_by(|b, a| b.start < a.end);
    if subs.iter().map(|r| r.len()).sum::<usize>() < MIN_SYMBOLS {
        return None;
    }
    let parts: Vec<&[u8]> = subs.iter().map(|r| &streams[r.clone()]).collect();
    let st = symbols::encode(&parts)?;
    let inputs = symbols::INPUTS
        .iter()
        .map(|(name, which)| {
            let packed = crate::pack(&which.iter().map(|&k| &st[k][..]).collect::<Vec<_>>());
            f.push_as(format!("pdb-symbols/{name}"), Kind::Table, packed)
        })
        .collect();
    let a = Apply {
        module: SYMBOLS,
        resources: vec![],
        params: vec![],
        inputs,
        len: 0,
        memory_limit: aqr_formula::NO_LIMIT,
    };
    let outputs = f.split(a, subs.iter().map(|r| r.len() as u64).collect());
    Some((subs, outputs))
}
