//! aqr-png as modules of entry formulas (see aqr-formula): [`decode`] on three inputs, the
//! skeleton, the index and the records of the images, and no parameters; or on four, the last
//! the reference file that records may be coded against. And [`decode_set`], of sets of files
//! each coded against an earlier one, of an output per file. See FORMAT.md, *Formula module*
//! and *Sets of files*.

use crate::codec::{self, Parts, Skeleton};
use crate::{Result, corrupt};
use aqr_common::varint;
use aqr_formula::{Apply, Formula, Kind, Module, Node, packed};

pub const MODULE: Module = Module { name: "png-recons", version: 2 };

/// The module of sets of files ([`encode_set`], [`decode_set`]).
pub const SET_MODULE: Module = Module { name: "png-set", version: 1 };

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

/// The parts of `input` for a formula, against `reference` if given (no panic gets out).
fn parts(input: &[u8], threads: usize, reference: Option<&[u8]>) -> Option<Parts> {
    std::panic::catch_unwind(|| codec::formula_parts(input, threads, reference)).ok()?
}

/// The index and the records of `parts`, concatenated.
fn index_records(parts: &Parts) -> (Vec<u8>, Vec<u8>) {
    let mut index = vec![];
    let mut records = Vec::with_capacity(parts.records.iter().map(|r| r.len()).sum());
    for r in &parts.records {
        varint::put(&mut index, r.len() as u64);
        records.extend_from_slice(r);
    }
    (index, records)
}

fn formula(input: &[u8], threads: usize, reference: Option<&[u8]>) -> Option<Formula> {
    let parts = parts(input, threads, reference)?;
    let (index, records) = index_records(&parts);
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
    rebuild(sk, index, records, reference, len, threads)
}

/// The formula of `files`, each coded against its parent where that makes it smaller
/// (`parents[i]`, before it, or none): an application of [`SET_MODULE`] of an output per file,
/// its root the outputs one after another (each for the caller to put where its file is).
/// `None` where a file gives no formula of its own ([`encode`]). `parents` must be as long as
/// `files`, each before its file.
pub fn encode_set(files: &[&[u8]], parents: &[Option<usize>], threads: usize) -> Option<Formula> {
    assert!(files.len() == parents.len() && parents.iter().enumerate().all(|(i, p)| p.is_none_or(|p| p < i)));
    let workers = threads.min(files.len()).max(1);
    let inner = (threads / workers).max(1);
    let parts = codec::par_map(files.len(), workers, |i| parts(files[i], inner, parents[i].map(|p| files[p])));
    let parts: Vec<Parts> = parts.into_iter().collect::<Option<_>>()?;
    let mut params = vec![];
    varint::put(&mut params, files.len() as u64);
    parents.iter().for_each(|p| varint::put(&mut params, p.map_or(0, |p| p as u64 + 1)));
    let mut f = Formula::default();
    let (mut sks, mut indexes, mut records) = (vec![], vec![], vec![]);
    for p in parts {
        let (index, recs) = index_records(&p);
        sks.push(f.push("png/skeleton", p.skeleton));
        indexes.push(f.push("png/index", index));
        records.push(f.push_as("png/records", Kind::Compressed, recs));
    }
    let inputs = vec![Node::pack(sks), Node::pack(indexes), Node::pack(records)];
    let a = Apply { module: SET_MODULE, resources: vec![], params, inputs, len: 0 };
    let outputs = f.share(a, files.iter().map(|d| d.len() as u64).collect());
    f.root = Node::Concat(outputs);
    Some(f)
}

/// The module of sets: the files of `sizes` bytes rebuilt from `inputs`, the packed
/// skeletons, indexes and records of the files, each against the output of its parent as the
/// parameters give them.
pub fn decode_set(params: &[u8], inputs: &[&[u8]], sizes: &[u64], threads: usize) -> Result<Vec<Vec<u8>>> {
    let mut at = 0;
    let n = codec::get_varint(params, &mut at)? as usize;
    if n != sizes.len() {
        return Err(corrupt());
    }
    let mut parents = Vec::with_capacity(n);
    for i in 0..n {
        parents.push(match codec::get_varint(params, &mut at)? {
            0 => None,
            p if p as usize <= i => Some(p as usize - 1),
            _ => return Err(corrupt()),
        });
    }
    let &[sks, indexes, records] = inputs else { return Err(corrupt()) };
    fn unpack(p: &[u8], n: usize) -> Result<Vec<&[u8]>> {
        packed::unpack(p).filter(|items| items.len() == n).ok_or_else(corrupt)
    }
    let (sks, indexes, records) = (unpack(sks, n)?, unpack(indexes, n)?, unpack(records, n)?);
    if at != params.len() {
        return Err(corrupt());
    }
    let mut out: Vec<Vec<u8>> = Vec::with_capacity(n);
    for i in 0..n {
        let reference = parents[i].map(|p| &out[p][..]);
        let file = rebuild(sks[i], indexes[i], records[i], reference, sizes[i], threads)?;
        out.push(file);
    }
    Ok(out)
}

/// The file of `len` bytes of the skeleton `sk`, the index and the records, against the file
/// `reference` if given.
fn rebuild(
    sk: &[u8],
    index: &[u8],
    records: &[u8],
    reference: Option<&[u8]>,
    len: u64,
    threads: usize,
) -> Result<Vec<u8>> {
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
