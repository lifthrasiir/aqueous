//! aqr-png as a module of entry formulas (see aqr-formula): [`decode`] on three
//! inputs, the skeleton, the index and the records of the images, and no parameters. See
//! FORMAT.md, *Formula module*.

use crate::container::{self, Skeleton};
use crate::{Result, corrupt};
use aqr_common::varint;
use aqr_formula::{Formula, Module, Node};

pub const MODULE: Module = Module { name: "png-recons", version: 1 };

/// The formula giving `input`, or `None` where the container would store it: not a PNG file
/// that can be parsed, or no image whose record is coded.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    let parts = std::panic::catch_unwind(|| container::formula_parts(input, threads)).ok()??;
    let mut index = vec![];
    let mut records = Vec::with_capacity(parts.records.iter().map(|r| r.len()).sum());
    for r in &parts.records {
        varint::put(&mut index, r.len() as u64);
        records.extend_from_slice(r);
    }
    let mut f = Formula::default();
    let inputs =
        vec![f.push("png/skeleton", parts.skeleton), f.push("png/index", index), f.push("png/records", records)];
    f.root = Node::apply(MODULE, Vec::new(), inputs, input.len() as u64);
    Some(f)
}

/// The module: the file of `len` bytes rebuilt from `inputs`, the skeleton, the index (the
/// length of each image's record, as varints) and the records.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let (&[], &[sk, index, records]) = (params, inputs) else { return Err(corrupt()) };
    let skel = Skeleton::parse(sk)?;
    let (mut at, mut from) = (0, 0);
    let mut recs = Vec::with_capacity(skel.images.len());
    for _ in &skel.images {
        let n = container::get_varint(index, &mut at)? as usize;
        recs.push(container::get_bytes(records, &mut from, n)?);
    }
    if at != index.len() || from != records.len() {
        return Err(corrupt());
    }
    let out = skel.rebuild(&recs, threads)?;
    if out.len() as u64 != len {
        return Err(corrupt());
    }
    Ok(out)
}
