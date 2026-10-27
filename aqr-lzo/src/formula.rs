//! aqr-lzo as a module of entry formulas (see aqr-formula): LZO1X streams rebuilt from their
//! data and records. `FORMAT.md` specifies it. The data is an input of its own, which the
//! formula may recompress in turn.

use aqr_formula::{Apply, Formula, Kind, Module, Node};

use crate::{lzop, recons};

pub use crate::module::{decode, index};

pub const MODULE: Module = Module { name: "lzo-recons", version: 1 };

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
    let splits = aqr_common::pool::par_map(lz.blocks.len(), threads, |i| {
        let b = &lz.blocks[i];
        if b.stored() {
            return None;
        }
        recons::split(&input[b.at..b.at + b.stream], b.data)
            .ok()
            .filter(|sp| sp.used == b.stream && sp.data.len() == b.data)
    });
    if splits.iter().all(Option::is_none) {
        return None;
    }
    // Where each piece of the file goes: the container, the data or the module's output.
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
    for (b, sp) in lz.blocks.iter().zip(splits) {
        let sp = match sp {
            Some(sp) => sp,
            None if b.stored() => {
                keep(&mut pieces, &mut container, &mut at, b.at);
                pieces.push(Piece::Data(data.len(), b.data));
                data.extend_from_slice(&input[b.at..b.at + b.data]);
                at += b.data;
                continue;
            }
            None => continue,
        };
        keep(&mut pieces, &mut container, &mut at, b.at);
        inputs.push((data.len(), b.data));
        pieces.push(Piece::Output(lens.len()));
        lens.push((b.data, sp.record.len(), b.stream));
        data.extend_from_slice(&sp.data);
        records.extend_from_slice(&sp.record);
        at += b.stream;
    }
    keep(&mut pieces, &mut container, &mut at, input.len());
    let mut f = Formula::default();
    f.push("lzo", container);
    f.push("raw", data);
    let record = f.push_as("lzo/record", Kind::Compressed, records);
    let index = f.push("lzo/index", index(&lens.iter().map(|&(d, r, _)| (d, r)).collect::<Vec<_>>()));
    let input_data = Node::concat(inputs.iter().map(|&(a, n)| Node::slice(1, a as u64, n as u64)));
    let a =
        Apply { module: MODULE, resources: vec![], params: vec![], inputs: vec![input_data, record, index], len: 0 };
    let mut outputs: Vec<Option<Node>> =
        f.split(a, lens.iter().map(|&(_, _, s)| s as u64).collect()).into_iter().map(Some).collect();
    f.root = Node::concat(pieces.into_iter().map(|p| match p {
        Piece::Container(a, n) => Node::slice(0, a as u64, n as u64),
        Piece::Data(a, n) => Node::slice(1, a as u64, n as u64),
        Piece::Output(i) => outputs[i].take().unwrap(),
    }));
    Some(f)
}
