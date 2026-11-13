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

/// The class of the container of [`encode_blocks`]'s formula with `contents`: the blocks are
/// members of a file of their own, which a recompressor makes apart.
pub const MEMBERS: &str = "lzo/members";

/// Bytes of streams [`encode_blocks`] splits at once.
const WINDOW: usize = 32 << 20;

/// Bytes of data and streams an application of [`encode_blocks`]'s with `contents` takes at
/// most (but a stream of more): its data is one content.
const GROUP: usize = 32 << 20;

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
/// or None where none splits and none is stored. With `contents`, the streams' data is one
/// content (the module's input, one slice) and the stored blocks' data a stream of contents, a
/// slice for each; without, the formula is the lzop file's as it was. Blocks are in order,
/// apart.
pub fn encode_blocks(input: &[u8], blocks: &[Block], threads: usize, contents: bool) -> Option<Formula> {
    // (split a window of blocks at a time, of [`WINDOW`] bytes of streams: all of a game's data
    // file's split at once took 1.5 GB besides their data)
    let split_one = |b: &Block| -> Option<recons::Split> {
        if b.stored {
            return None;
        }
        // (where the data's length is unknown, LZO1X's most: 255 bytes of a match a byte)
        let limit = b.data.unwrap_or(b.len.saturating_mul(256).max(4096));
        recons::split(&input[b.at..b.at + b.len], limit)
            .ok()
            .filter(|sp| sp.used == b.len && b.data.is_none_or(|n| sp.data.len() == n))
    };
    let mut windows = Vec::new();
    let mut i = 0;
    while i < blocks.len() {
        let (mut j, mut bytes) = (i, 0);
        while j < blocks.len() && (j == i || bytes + blocks[j].len <= WINDOW) {
            bytes += blocks[j].len;
            j += 1;
        }
        windows.push(i..j);
        i = j;
    }
    let split_window =
        |w: &std::ops::Range<usize>| aqr_common::pool::par_map(w.len(), threads, |k| split_one(&blocks[w.start + k]));
    // (whether any splits: the first window that has one, kept for the walk below)
    let mut first: Option<(usize, Vec<Option<recons::Split>>)> = None;
    for (n, w) in windows.iter().enumerate() {
        let sp = split_window(w);
        if sp.iter().any(Option::is_some) {
            first = Some((n, sp));
            break;
        }
    }
    if first.is_none() && !(contents && blocks.iter().any(|b| b.stored)) {
        return None;
    }
    // Where each piece of the input goes: the container, the data or the module's output.
    enum Piece {
        Container(usize, usize),
        Data(usize, usize),
        Stored(usize, usize),
        Output(usize),
    }
    let (mut container, mut data, mut records, mut lens) = (vec![], vec![], vec![], vec![]);
    // (with `contents`, the stored blocks' data apart from the streams': the streams' data is
    // then one content, scanned once, not a content of each stream: thousands of them took
    // most of the time of a game's data file)
    let mut stored: Vec<u8> = vec![];
    let mut pieces = vec![];
    let mut inputs = vec![];
    let mut at = 0;
    // The bytes from `at` to `to` kept in the container.
    let keep = |pieces: &mut Vec<Piece>, container: &mut Vec<u8>, at: &mut usize, to: usize| {
        pieces.push(Piece::Container(container.len(), to - *at));
        container.extend_from_slice(&input[*at..to]);
        *at = to;
    };
    // the windows in order: those before the first with a split have none
    let (first_n, first_sp) = first.unwrap_or((windows.len(), Vec::new()));
    let mut first_sp = Some(first_sp);
    let splits = windows.iter().enumerate().flat_map(|(n, w)| {
        let sp: Vec<Option<recons::Split>> = match n.cmp(&first_n) {
            std::cmp::Ordering::Less => w.clone().map(|_| None).collect(),
            std::cmp::Ordering::Equal => first_sp.take().unwrap(),
            std::cmp::Ordering::Greater => split_window(w),
        };
        sp.into_iter()
    });
    for (b, sp) in blocks.iter().zip(splits) {
        let sp = match sp {
            Some(sp) => sp,
            // (with `contents`, a stream that does not split is data stored as it is: a member
            // stored where its directory tells of no codec)
            None if contents => {
                keep(&mut pieces, &mut container, &mut at, b.at);
                pieces.push(Piece::Stored(stored.len(), b.len));
                stored.extend_from_slice(&input[b.at..b.at + b.len]);
                at += b.len;
                continue;
            }
            None if b.stored => {
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
    // (the container of a file whose blocks are members of their own: [`MEMBERS`])
    f.push(if contents { MEMBERS } else { "lzo" }, container);
    match contents {
        true => {
            f.push_content("raw", Kind::Unknown, data);
            f.push_content("raw", Kind::Unknown, stored);
        }
        false => _ = f.push("raw", data),
    };
    let mut outputs: Vec<Option<Node>> = Vec::new();
    let apply = |inputs: Vec<Node>| Apply {
        module: MODULE,
        resources: vec![],
        params: vec![],
        inputs,
        len: 0,
        memory_limit: aqr_formula::NO_LIMIT,
        task_limits: Vec::new(),
    };
    if !lens.is_empty() && !contents {
        let record = f.push_as("lzo/record", Kind::Compressed, records);
        let index = f.push("lzo/index", index(&lens.iter().map(|&(d, r, _)| (d, r)).collect::<Vec<_>>()));
        let input_data = Node::concat(inputs.iter().map(|&(a, n)| Node::slice(1, a as u64, n as u64)));
        let a = apply(vec![input_data, record, index]);
        outputs = f.split(a, lens.iter().map(|&(_, _, s)| s as u64).collect()).into_iter().map(Some).collect();
    } else if !lens.is_empty() {
        // applications of streams one after another, [`GROUP`] bytes of their data and streams
        // at most (one of all of a game's data file of 50 MB took 274 MiB, over the bound),
        // the data of each one content (one of each stream cost each a content's work)
        let rec = f.push_as("lzo/record", Kind::Compressed, records);
        let Node::Slice { stream: rec_stream, .. } = rec else { unreachable!() };
        let (mut i, mut r_at) = (0, 0u64);
        while i < lens.len() {
            let mut j = i;
            let mut bytes = 0;
            while j < lens.len() && (j == i || bytes + lens[j].0 + lens[j].2 <= GROUP) {
                bytes += lens[j].0 + lens[j].2;
                j += 1;
            }
            let r_len: u64 = lens[i..j].iter().map(|l| l.1 as u64).sum();
            let index = f.push("lzo/index", index(&lens[i..j].iter().map(|&(d, r, _)| (d, r)).collect::<Vec<_>>()));
            let (d_at, d_len) = (inputs[i].0 as u64, lens[i..j].iter().map(|l| l.0 as u64).sum());
            let a = apply(vec![Node::slice(1, d_at, d_len), Node::slice(rec_stream, r_at, r_len), index]);
            outputs.extend(f.split(a, lens[i..j].iter().map(|&(_, _, s)| s as u64).collect()).into_iter().map(Some));
            r_at += r_len;
            i = j;
        }
    }
    f.root = Node::concat(pieces.into_iter().map(|p| match p {
        Piece::Container(a, n) => Node::slice(0, a as u64, n as u64),
        Piece::Data(a, n) => Node::slice(1, a as u64, n as u64),
        Piece::Stored(a, n) => Node::slice(2, a as u64, n as u64),
        Piece::Output(i) => outputs[i].take().unwrap(),
    }));
    Some(f)
}
