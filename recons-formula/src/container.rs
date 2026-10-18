//! A container for a formula on its own: the formula's description and its streams as one.
//!
//! ```text
//! magic        69 04 5f ae 69 5a ("aQRfrmla" in base64)
//! length       u16 LE, bytes of the description; 0 is reserved
//! description  ops of a stack machine, each pushing one node
//! stream       the rest: the formula's streams one after another
//! ```
//!
//! Every number in the description is LEB128. The ops, by their first number:
//!
//! - 0, slice: `0 start len` pushes `len` bytes of the stream from `start`.
//! - 1, concat: `1 n` pops `n` nodes and pushes their concatenation (the deepest first).
//! - 2-127 are reserved.
//! - 128 and up, apply: `id plen params[plen] n len` pops `n` nodes and pushes the module
//!   numbered `id` applied to them (the deepest first) with `params`, making `len` bytes. Module
//!   numbers are given by the user of the container, so that the one-byte ops stay apart.
//!
//! The description must leave exactly one node, the formula's output.

use recons_common::varint;

use crate::{Error, Formula, Module, Node, Result, Stream};

pub const MAGIC: [u8; 6] = [0x69, 0x04, 0x5f, 0xae, 0x69, 0x5a];

const SLICE: u64 = 0;
const CONCAT: u64 = 1;
/// The first module number.
pub const FIRST_MODULE: u64 = 0x80;

/// `f` in the container, its modules numbered by `ids`. Fails on modules without a number
/// (or one below [`FIRST_MODULE`]), slices out of their stream, and descriptions over 65535
/// bytes.
pub fn encode(f: &Formula, ids: &[(u64, Module)]) -> Result<Vec<u8>> {
    let mut bases = Vec::with_capacity(f.streams.len());
    let mut total = 0u64;
    for s in &f.streams {
        bases.push(total);
        total += s.data.len() as u64;
    }
    let mut desc = Vec::new();
    describe(f, &f.root, &bases, ids, &mut desc)?;
    let len = u16::try_from(desc.len()).map_err(|_| Error::new(format!("description of {} bytes", desc.len())))?;
    let mut out = Vec::with_capacity(MAGIC.len() + 2 + desc.len() + total as usize);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&desc);
    f.streams.iter().for_each(|s| out.extend_from_slice(&s.data));
    Ok(out)
}

fn describe(f: &Formula, n: &Node, bases: &[u64], ids: &[(u64, Module)], out: &mut Vec<u8>) -> Result<()> {
    match n {
        &Node::Slice { stream, start, len } => {
            let size = f.streams.get(stream).map(|s| s.data.len() as u64);
            if size.is_none_or(|size| start.checked_add(len).is_none_or(|end| end > size)) {
                return Err(Error::new(format!("slice [{start}, +{len}) out of stream #{stream}")));
            }
            varint::put(out, SLICE);
            varint::put(out, bases[stream] + start);
            varint::put(out, len);
        }
        Node::Concat(ns) => {
            ns.iter().try_for_each(|n| describe(f, n, bases, ids, out))?;
            varint::put(out, CONCAT);
            varint::put(out, ns.len() as u64);
        }
        Node::Apply(a) => {
            let id = module_id(ids, a.module)?;
            a.inputs.iter().try_for_each(|n| describe(f, n, bases, ids, out))?;
            varint::put(out, id);
            varint::put(out, a.params.len() as u64);
            out.extend_from_slice(&a.params);
            varint::put(out, a.inputs.len() as u64);
            varint::put(out, a.len);
        }
    }
    Ok(())
}

fn module_id(ids: &[(u64, Module)], m: Module) -> Result<u64> {
    ids.iter()
        .find(|&&(_, i)| i == m)
        .map(|&(id, _)| id)
        .filter(|&id| id >= FIRST_MODULE)
        .ok_or_else(|| Error::new(format!("no number for {m}")))
}

/// The most bytes of description [`encode`] takes.
pub const MAX_DESCRIPTION: usize = u16::MAX as usize;

/// Bytes of the description of node `n` at most, wherever the streams of its formula are among
/// others (up to 32 GiB of them): the bounds of formulas put together ([`Formula::embed`]) add
/// up, with the nodes around them. Fails on modules without a number.
pub fn description_bound(n: &Node, ids: &[(u64, Module)]) -> Result<usize> {
    Ok(match n {
        // offsets of up to 35 bits
        &Node::Slice { start, len, .. } => 1 + varint::len(start).max(5) + varint::len(len),
        Node::Concat(ns) => {
            ns.iter().map(|n| description_bound(n, ids)).sum::<Result<usize>>()? + 1 + varint::len(ns.len() as u64)
        }
        Node::Apply(a) => {
            let inputs = a.inputs.iter().map(|n| description_bound(n, ids)).sum::<Result<usize>>()?;
            let params = varint::len(a.params.len() as u64) + a.params.len();
            inputs
                + varint::len(module_id(ids, a.module)?)
                + params
                + varint::len(a.inputs.len() as u64)
                + varint::len(a.len)
        }
    })
}

/// The formula in container `c`, with a single stream of class `class`; modules are numbered
/// by `ids`. The structure is kept as described (concatenations are not flattened), so it
/// encodes back to `c`. Slices are checked against the stream; nothing is evaluated.
pub fn decode(c: &[u8], ids: &[(u64, Module)], class: &'static str) -> Result<Formula> {
    let corrupt = |what: &str| Error::new(format!("corrupt formula container: {what}"));
    if c.get(..MAGIC.len()) != Some(&MAGIC) {
        return Err(corrupt("bad magic"));
    }
    let len = c.get(6..8).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize).ok_or_else(|| corrupt("truncated"))?;
    if len == 0 {
        return Err(corrupt("reserved description length 0"));
    }
    let desc = c.get(8..8 + len).ok_or_else(|| corrupt("truncated description"))?;
    let data = &c[8 + len..];

    let mut at = 0;
    let num = |at: &mut usize| varint::get(desc, at).map_err(|e| corrupt(e.0));
    let mut stack: Vec<Node> = Vec::new();
    // The top `n` nodes, the deepest first.
    let pop = |stack: &mut Vec<Node>, n: u64| {
        let n = usize::try_from(n).ok().filter(|&n| n <= stack.len()).ok_or_else(|| corrupt("stack underflow"))?;
        Ok::<_, Error>(stack.split_off(stack.len() - n))
    };
    while at < desc.len() {
        let node = match num(&mut at)? {
            SLICE => {
                let (start, len) = (num(&mut at)?, num(&mut at)?);
                if start.checked_add(len).is_none_or(|end| end > data.len() as u64) {
                    return Err(corrupt("slice out of the stream"));
                }
                Node::slice(0, start, len)
            }
            CONCAT => {
                let n = num(&mut at)?;
                Node::Concat(pop(&mut stack, n)?)
            }
            id if id >= FIRST_MODULE => {
                let module =
                    ids.iter().find(|&&(i, _)| i == id).map(|&(_, m)| m).ok_or_else(|| corrupt("unknown module"))?;
                let plen = num(&mut at)?;
                let params = usize::try_from(plen)
                    .ok()
                    .and_then(|plen| desc.get(at..at.checked_add(plen)?))
                    .ok_or_else(|| corrupt("truncated params"))?
                    .to_vec();
                at += params.len();
                let (n, len) = (num(&mut at)?, num(&mut at)?);
                Node::apply(module, params, pop(&mut stack, n)?, len)
            }
            _ => return Err(corrupt("reserved op")),
        };
        stack.push(node);
    }
    let root = stack.pop().filter(|_| stack.is_empty()).ok_or_else(|| corrupt("not exactly one node left"))?;
    Ok(Formula { streams: vec![Stream { class: class.into(), data: data.to_vec() }], root })
}

#[cfg(test)]
mod tests {
    use super::*;

    const REV: Module = Module { name: "rev", version: 1 };
    const IDS: &[(u64, Module)] = &[(0x80, REV)];

    fn sample() -> Formula {
        let mut f = Formula::default();
        let a = f.push("a", b"hello".to_vec());
        let b = f.push("b", b"xyz".to_vec());
        f.root =
            Node::concat([Node::slice(0, 0, 2), Node::apply(REV, vec![1, 2, 3], vec![a, b], 8), Node::slice(1, 1, 2)]);
        f
    }

    #[test]
    fn layout() {
        let c = encode(&sample(), IDS).unwrap();
        #[rustfmt::skip]
        let desc = [
            0, 0, 2,
            0, 0, 5,
            0, 5, 3,
            0x80, 1, 3, 1, 2, 3, 2, 8,
            0, 6, 2,
            1, 3,
        ];
        assert_eq!(c[..6], MAGIC);
        assert_eq!(c[6..8], (desc.len() as u16).to_le_bytes());
        assert_eq!(c[8..8 + desc.len()], desc);
        assert_eq!(&c[8 + desc.len()..], b"helloxyz");
    }

    #[test]
    fn bound() {
        let f = sample();
        let desc =
            |f: &Formula| encode(f, IDS).unwrap().len() - 8 - f.streams.iter().map(|s| s.data.len()).sum::<usize>();
        let b = description_bound(&f.root, IDS).unwrap();
        assert!(b >= desc(&f), "{b}");
        // Of formulas put together: the parts add up.
        let mut g = Formula::raw("big", vec![0; 1 << 20]);
        let parts = [f.clone(), f.clone(), f]
            .into_iter()
            .map(|f| {
                let b = description_bound(&f.root, IDS).unwrap();
                (g.embed(f), b)
            })
            .collect::<Vec<_>>();
        let sum: usize = parts.iter().map(|p| p.1).sum();
        g.root = Node::Concat(std::iter::once(g.root.clone()).chain(parts.into_iter().map(|p| p.0)).collect());
        let b = description_bound(&g.root, IDS).unwrap();
        assert_eq!(b, sum + description_bound(&Node::slice(0, 0, 1 << 20), IDS).unwrap() + 2);
        assert!(b >= desc(&g));
        assert!(description_bound(&g.root, &[]).is_err());
    }

    #[test]
    fn roundtrip() {
        let f = sample();
        let c = encode(&f, IDS).unwrap();
        let g = decode(&c, IDS, "data").unwrap();
        assert_eq!(g.streams.len(), 1);
        assert_eq!(g.len(), f.len());
        assert_eq!(encode(&g, IDS).unwrap(), c);
        // Big numbers take several bytes; an empty formula is still one op.
        let mut f = Formula::raw("a", vec![7; 300]);
        f.root = Node::concat([Node::slice(0, 200, 100), Node::slice(0, 0, 150)]);
        let c = encode(&f, IDS).unwrap();
        assert_eq!(encode(&decode(&c, IDS, "data").unwrap(), IDS).unwrap(), c);
        let c = encode(&Formula::default(), IDS).unwrap();
        assert_eq!(c[6..], [2, 0, 1, 0]);
        assert_eq!(decode(&c, IDS, "data").unwrap().root, Node::Concat(vec![]));
    }

    #[test]
    fn encode_errors() {
        let f = sample();
        assert!(encode(&f, &[]).is_err());
        assert!(encode(&f, &[(5, REV)]).is_err());
        let mut f = Formula::raw("a", vec![0; 4]);
        f.root = Node::slice(0, 2, 3);
        assert!(encode(&f, IDS).is_err());
        f.root = Node::Concat((0..30000).map(|_| Node::slice(0, 1, 1)).collect());
        assert!(encode(&f, IDS).is_err());
    }

    #[test]
    fn decode_errors() {
        let c = |desc: &[u8], data: &[u8]| [&MAGIC[..], &(desc.len() as u16).to_le_bytes(), desc, data].concat();
        assert!(decode(&c(&[0, 0, 2], b"ab"), IDS, "d").is_ok());
        for (bad, why) in [
            (c(&[0, 0, 3], b"ab"), "slice out of the stream"),
            (c(&[0, 0, 1, 0, 1, 1], b"ab"), "two nodes left"),
            (c(&[1, 1], b""), "underflow"),
            (c(&[2], b""), "reserved op"),
            (c(&[0x81, 1, 0, 0, 0], b""), "unknown module"),
            (c(&[0x80, 1, 5, 0, 0], b""), "truncated params"),
            (c(&[0, 0], b""), "truncated op"),
            (c(&[0x80, 0x80], b""), "truncated number"),
            (c(&[], b"ab"), "reserved length"),
            (c(&[0, 0, 2], b"ab")[..9].to_vec(), "truncated description"),
            ([&[0x69, 0x04, 0x5f, 0xae, 0x69, 0x5b][..], &[1, 0, 1, 0]].concat(), "bad magic"),
        ] {
            assert!(decode(&bad, IDS, "d").is_err(), "{why}");
        }
    }
}
