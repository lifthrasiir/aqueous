//! aqr-png as a module of entry formulas (see aqr-formula): [`decode`] on three
//! inputs, the skeleton, the index and the records of the images, and no parameters; or on
//! four, the last the reference file that records may be coded against. See FORMAT.md,
//! *Formula module*.

use crate::codec::{self, Skeleton};
use crate::{Result, corrupt};
use aqr_common::varint;
use aqr_formula::{Formula, Kind, Module, Node};

pub const MODULE: Module = Module { name: "png-recons", version: 2 };

/// The class of the stream that [`encode_against`] puts the reference file in.
pub const REFERENCE: &str = "png/reference";

/// The formula giving `input`, or `None` where there is nothing to code: not a PNG file
/// that can be parsed, or no image whose record is coded.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    formula(input, threads, None)
}

/// [`encode`] with the images coded against those of `reference` (a similar PNG file, such
/// as a variant of the picture) where that makes them smaller. The application's last input
/// is then `reference` itself, a stream of class [`REFERENCE`] for the caller to put a node
/// giving the file in place of.
pub fn encode_against(input: &[u8], reference: &[u8], threads: usize) -> Option<Formula> {
    formula(input, threads, Some(reference))
}

fn formula(input: &[u8], threads: usize, reference: Option<&[u8]>) -> Option<Formula> {
    let parts = std::panic::catch_unwind(|| codec::formula_parts(input, threads, reference)).ok()??;
    let mut index = vec![];
    let mut records = Vec::with_capacity(parts.records.iter().map(|r| r.len()).sum());
    for r in &parts.records {
        varint::put(&mut index, r.len() as u64);
        records.extend_from_slice(r);
    }
    let against = parts.records.iter().any(|r| r[0] == 2);
    let mut f = Formula::default();
    // (the records are entropy coded)
    let mut inputs = vec![
        f.push("png/skeleton", parts.skeleton),
        f.push("png/index", index),
        f.push_as("png/records", Kind::Compressed, records),
    ];
    if let Some(r) = reference.filter(|_| against) {
        inputs.push(f.push(REFERENCE, r.to_vec()));
    }
    f.root = Node::apply(MODULE, Vec::new(), inputs, input.len() as u64);
    Some(f)
}

/// The module: the file of `len` bytes rebuilt from `inputs`, the skeleton, the index (the
/// length of each image's record, as varints) and the records, and the reference file if
/// there is a fourth.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let (sk, index, records, reference) = match (params, inputs) {
        (&[], &[sk, index, records]) => (sk, index, records, None),
        (&[], &[sk, index, records, r]) => (sk, index, records, Some(r)),
        _ => return Err(corrupt()),
    };
    let skel = Skeleton::parse(sk)?;
    let (mut at, mut from) = (0, 0);
    let mut recs = Vec::with_capacity(skel.images.len());
    for _ in &skel.images {
        let n = codec::get_varint(index, &mut at)? as usize;
        recs.push(codec::get_bytes(records, &mut from, n)?);
    }
    if at != index.len() || from != records.len() {
        return Err(corrupt());
    }
    let out = skel.rebuild(&recs, threads, reference)?;
    if out.len() as u64 != len {
        return Err(corrupt());
    }
    Ok(out)
}
