//! aqr-lzo as a module of entry formulas (see aqr-formula): LZO1X streams rebuilt from their
//! data and records. `FORMAT.md` specifies it. The data is an input of its own, which the
//! formula may recompress in turn.

use aqr_formula::{Apply, Formula, Kind, Module, Node};

use crate::{lzop, recons};

pub use crate::module::{decode, index};

pub const MODULE: Module = Module::new("lzo-recons", 1).with_plan(memory).with_contents(&[0]);

/// The memory of the module: its initial memory, and its plan ([`crate::module::plan`]).
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let &[len] = u.outputs else { return None };
    Some(u.initial() + crate::module::plan(u.inputs, u.content(2)?, len))
}

/// Whether `d` starts like an lzop file whose structure holds ([`lzop::parse`]).
pub fn starts(d: &[u8]) -> bool {
    d.starts_with(&lzop::MAGIC) && lzop::parse(d).is_some()
}

/// Bytes of the lzop file at the start of `d`; None where its structure does not hold.
pub fn extent(d: &[u8]) -> Option<usize> {
    lzop::parse(d).map(|f| f.len)
}

/// The formula rebuilding `input`, an lzop file, or None where it is not one or none of its
/// streams splits. A stream that does not split stays as it is, and so does what follows the
/// file. Streams are split on up to `threads` threads.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    let lz = lzop::parse(input)?;
    let blocks: Vec<Block> = lz
        .blocks
        .iter()
        .map(|b| match b.stored() {
            true => Block { at: b.at, len: b.data, data: Some(b.data), stored: true },
            false => Block { at: b.at, len: b.stream, data: Some(b.data), stored: false },
        })
        .collect();
    encode_blocks(input, &blocks, threads, false)
}

/// A piece of an input [`encode_blocks`] takes: an LZO1X stream (of `data` bytes of data where
/// it is known), or data stored as it is.
#[derive(Clone, Copy, Debug)]
pub struct Block {
    pub at: usize,
    pub len: usize,
    pub data: Option<usize>,
    pub stored: bool,
}

/// The formula rebuilding `input` from the LZO1X streams of `blocks` split (those that do not
/// split kept as they are, or with `contents` stored, and the bytes between the blocks kept),
/// or None where none splits and none is stored. The data, the streams' and the stored blocks', is the contents of the
/// formula, a slice for each block; `contents` marks its stream so where no stream splits
/// (else the module's input does: the lzop file's formula, as it was). Blocks are in order,
/// apart.
pub fn encode_blocks(input: &[u8], blocks: &[Block], threads: usize, contents: bool) -> Option<Formula> {
    let splits = aqr_common::pool::par_map(blocks.len(), threads, |i| {
        let b = &blocks[i];
        if b.stored {
            return None;
        }
        // (where the data's length is unknown, LZO1X's most: 255 bytes of a match a byte)
        let limit = b.data.unwrap_or(b.len.saturating_mul(256).max(4096));
        recons::split(&input[b.at..b.at + b.len], limit)
            .ok()
            .filter(|sp| sp.used == b.len && b.data.is_none_or(|n| sp.data.len() == n))
    });
    if splits.iter().all(Option::is_none) && !(contents && blocks.iter().any(|b| b.stored)) {
        return None;
    }
    // Where each piece of the input goes: the container, the data or the module's output.
    enum Piece {
        Container(usize, usize),
        Data(usize, usize),
        Output(usize),
    }
    let (mut container, mut data, mut records, mut lens) = (vec![], vec![], vec![], vec![]);
    let mut pieces = vec![];
    let mut inputs = vec![];
    let mut at = 0;
    // The bytes from `at` to `to` kept in the container.
    let keep = |pieces: &mut Vec<Piece>, container: &mut Vec<u8>, at: &mut usize, to: usize| {
        pieces.push(Piece::Container(container.len(), to - *at));
        container.extend_from_slice(&input[*at..to]);
        *at = to;
    };
    for (b, sp) in blocks.iter().zip(splits) {
        let sp = match sp {
            Some(sp) => sp,
            // (with `contents`, a stream that does not split is data stored as it is: a member
            // stored where its directory tells of no codec)
            None if b.stored || contents => {
                keep(&mut pieces, &mut container, &mut at, b.at);
                pieces.push(Piece::Data(data.len(), b.len));
                data.extend_from_slice(&input[b.at..b.at + b.len]);
                at += b.len;
                continue;
            }
            None => continue,
        };
        keep(&mut pieces, &mut container, &mut at, b.at);
        inputs.push((data.len(), sp.data.len()));
        pieces.push(Piece::Output(lens.len()));
        lens.push((sp.data.len(), sp.record.len(), b.len));
        data.extend_from_slice(&sp.data);
        records.extend_from_slice(&sp.record);
        at += b.len;
    }
    keep(&mut pieces, &mut container, &mut at, input.len());
    let mut f = Formula::default();
    f.push("lzo", container);
    match contents {
        true => f.push_content("raw", Kind::Unknown, data),
        false => f.push("raw", data),
    };
    let mut outputs: Vec<Option<Node>> = Vec::new();
    if !lens.is_empty() {
        let record = f.push_as("lzo/record", Kind::Compressed, records);
        let index = f.push("lzo/index", index(&lens.iter().map(|&(d, r, _)| (d, r)).collect::<Vec<_>>()));
        let input_data = Node::concat(inputs.iter().map(|&(a, n)| Node::slice(1, a as u64, n as u64)));
        let a = Apply {
            module: MODULE,
            resources: vec![],
            params: vec![],
            inputs: vec![input_data, record, index],
            len: 0,
            memory_limit: aqr_formula::NO_LIMIT,
            task_limits: Vec::new(),
        };
        outputs = f.split(a, lens.iter().map(|&(_, _, s)| s as u64).collect()).into_iter().map(Some).collect();
    }
    f.root = Node::concat(pieces.into_iter().map(|p| match p {
        Piece::Container(a, n) => Node::slice(0, a as u64, n as u64),
        Piece::Data(a, n) => Node::slice(1, a as u64, n as u64),
        Piece::Output(i) => outputs[i].take().unwrap(),
    }));
    Some(f)
}
