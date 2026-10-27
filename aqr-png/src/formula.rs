//! aqr-png as modules of entry formulas (see aqr-formula): [`decode`] on three inputs, the
//! skeleton, the index and the records of the images, and no parameters; [`decode_against`] on
//! a fourth, the reference file that records may be coded against. And [`decode_set`], of sets
//! of files each coded against an earlier one, the files one after another. See FORMAT.md,
//! *Formula module* and *Sets of files*.

use crate::codec::{self, Parts, References, Skeleton};
use crate::{Result, corrupt};
use aqr_common::varint;
use aqr_formula::{Apply, Formula, Kind, Module, Node, packed};

pub const MODULE: Module = Module { name: "png-recons", version: 3 };

/// The module of a file coded against a reference file ([`encode_against`], [`decode_against`]).
pub const AGAINST_MODULE: Module = Module { name: "png-against", version: 1 };

/// The module of sets of files ([`encode_set`], [`decode_set`]).
pub const SET_MODULE: Module = Module { name: "png-set", version: 2 };

/// The class of the stream that [`encode_against`] puts the reference file in.
pub const REFERENCE: &str = "png/reference";

/// The formula giving `input`, or `None` where there is nothing to code: not a PNG file
/// that can be parsed, or no image whose record is coded.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    formula(input, threads, None)
}

/// [`encode`] with the images coded against those of `reference` (a similar PNG file, such
/// as a variant of the picture) where that makes them smaller. The application is then of
/// [`AGAINST_MODULE`], its last input `reference` itself, a stream of class [`REFERENCE`] for
/// the caller to put a node giving the file in place of.
pub fn encode_against(input: &[u8], reference: &[u8], threads: usize) -> Option<Formula> {
    formula(input, threads, Some(reference))
}

/// The parts of `input` for a formula, against `refs` if given, with the records coded `alone`
/// if known, else coded alone too `also_alone` (no panic gets out).
fn parts(
    input: &[u8],
    threads: usize,
    refs: Option<&References>,
    alone: Option<&[&[u8]]>,
    also_alone: bool,
) -> Option<Parts> {
    std::panic::catch_unwind(|| codec::formula_parts(input, threads, refs, alone, also_alone)).ok()?
}

/// A file's skeleton and the records of its images, as [`encode`] made them.
type Alone<'a> = (&'a [u8], Vec<&'a [u8]>);

/// The skeleton and the records of the images of `f`, a formula of [`encode`].
fn parts_of(f: &Formula) -> Option<Alone<'_>> {
    let stream = |class: &str| f.streams.iter().find(|s| s.class == class).map(|s| &s.data[..]);
    let Node::Apply(a) = &f.root else { return None };
    let (sk, index, records) = (stream("png/skeleton")?, stream("png/index")?, stream("png/records")?);
    if a.module != MODULE || f.streams.len() != 3 {
        return None;
    }
    let (mut at, mut from, mut recs) = (0, 0, vec![]);
    while at < index.len() {
        let n = codec::get_varint(index, &mut at).ok()? as usize;
        recs.push(codec::get_bytes(records, &mut from, n).ok()?);
    }
    (from == records.len()).then_some((sk, recs))
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
    let refs = reference.map(codec::references);
    let parts = parts(input, threads, refs.as_ref(), None, true)?;
    let (index, records) = index_records(&parts);
    let against = parts.records.iter().any(|r| r[0] == 2);
    let mut f = Formula::default();
    // (the records are entropy coded)
    let mut inputs = vec![
        f.push("png/skeleton", parts.skeleton),
        f.push("png/index", index),
        f.push_as("png/records", Kind::Compressed, records),
    ];
    let module = match reference.filter(|_| against) {
        Some(r) => {
            inputs.push(f.push(REFERENCE, r.to_vec()));
            AGAINST_MODULE
        }
        None => MODULE,
    };
    f.root = Node::apply(module, Vec::new(), inputs, input.len() as u64);
    Some(f)
}

/// The module: the file of `len` bytes rebuilt from `inputs`, the skeleton, the index (the
/// length of each image's record, as varints) and the records.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let (&[], &[sk, index, records]) = (params, inputs) else { return Err(corrupt()) };
    Ok(rebuild(sk, index, records, None, len, threads, false)?.0)
}

/// The module of a file coded against a reference file: [`decode`] with the reference file as
/// a fourth input.
pub fn decode_against(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let (&[], &[sk, index, records, reference]) = (params, inputs) else { return Err(corrupt()) };
    Ok(rebuild(sk, index, records, Some(&codec::references(reference)), len, threads, false)?.0)
}

/// The formula of `files`, each coded against its parent (`parents[i]`, before it, or none):
/// an application of [`SET_MODULE`] split into the files ([`Formula::split`]), its root the
/// files one after another (each for the caller to put where its file is). `None` where a file gives no
/// formula of its own ([`encode`]). `parents` must be as long as `files`, each before its
/// file. `alone` (as long, or empty) are the files' formulas of [`encode`] where known, which
/// are not made again: those of the files without a parent are taken as they are, and the
/// images of the others are coded alone where that is smaller than against the parent. Where
/// not known, an image with a parent is coded alone only where it cannot be coded against it.
pub fn encode_set(
    files: &[&[u8]],
    parents: &[Option<usize>],
    alone: &[Option<Formula>],
    threads: usize,
) -> Option<Formula> {
    assert!(files.len() == parents.len() && parents.iter().enumerate().all(|(i, p)| p.is_none_or(|p| p < i)));
    assert!(alone.is_empty() || alone.len() == files.len());
    let workers = threads.min(files.len()).max(1);
    let inner = (threads / workers).max(1);
    // the references of the parents, each once
    let mut is_parent = vec![false; files.len()];
    parents.iter().flatten().for_each(|&p| is_parent[p] = true);
    let refs = codec::par_map(files.len(), workers, |i| is_parent[i].then(|| codec::references(files[i])));
    let alone: Vec<Option<Alone>> = (0..files.len()).map(|i| alone.get(i)?.as_ref().and_then(parts_of)).collect();
    let parts = codec::par_map(files.len(), workers, |i| match (parents[i], &alone[i]) {
        (None, Some(a)) => Some(Parts { skeleton: a.0.to_vec(), records: a.1.iter().map(|r| r.to_vec()).collect() }),
        (p, a) => parts(files[i], inner, p.and_then(|p| refs[p].as_ref()), a.as_ref().map(|a| &a.1[..]), false),
    });
    let parts: Vec<Parts> = parts.into_iter().collect::<Option<_>>()?;
    let mut params = vec![];
    varint::put(&mut params, files.len() as u64);
    parents.iter().for_each(|p| varint::put(&mut params, p.map_or(0, |p| p as u64 + 1)));
    files.iter().for_each(|d| varint::put(&mut params, d.len() as u64));
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
    let outputs = f.split(a, files.iter().map(|d| d.len() as u64).collect());
    f.root = Node::Concat(outputs);
    Some(f)
}

/// The module of sets: the files rebuilt from `inputs`, the packed skeletons, indexes and
/// records of the files, each against its parent as the parameters give them with its size,
/// one after another in `len` bytes.
pub fn decode_set(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let mut at = 0;
    let n = codec::get_varint(params, &mut at)?;
    // (each parameter takes a byte at least)
    if n > params.len() as u64 {
        return Err(corrupt());
    }
    let n = n as usize;
    let mut parents = Vec::with_capacity(n);
    for i in 0..n {
        parents.push(match codec::get_varint(params, &mut at)? {
            0 => None,
            p if p as usize <= i => Some(p as usize - 1),
            _ => return Err(corrupt()),
        });
    }
    let sizes = (0..n).map(|_| codec::get_varint(params, &mut at)).collect::<Result<Vec<u64>>>()?;
    if sizes.iter().try_fold(0u64, |a, &s| a.checked_add(s)) != Some(len) {
        return Err(corrupt());
    }
    let &[sks, indexes, records] = inputs else { return Err(corrupt()) };
    fn unpack(p: &[u8], n: usize) -> Result<Vec<&[u8]>> {
        packed::unpack(p).filter(|items| items.len() == n).ok_or_else(corrupt)
    }
    let (sks, indexes, records) = (unpack(sks, n)?, unpack(indexes, n)?, unpack(records, n)?);
    if at != params.len() {
        return Err(corrupt());
    }
    // by depth in the trees, each depth's files at once against their parents (of the depth
    // before), which keep their references as they are rebuilt (until their children are)
    let mut depth = vec![0usize; n];
    let mut is_parent = vec![false; n];
    for i in 0..n {
        depth[i] = parents[i].map_or(0, |p| depth[p] + 1);
        parents[i].inspect(|&p| is_parent[p] = true);
    }
    let mut out: Vec<Option<Vec<u8>>> = (0..n).map(|_| None).collect();
    let mut refs: Vec<Option<References>> = (0..n).map(|_| None).collect();
    for d in 0..=depth.iter().copied().max().unwrap_or(0) {
        let level: Vec<usize> = (0..n).filter(|&i| depth[i] == d).collect();
        let workers = threads.min(level.len()).max(1);
        let inner = (threads / workers).max(1);
        let files = codec::par_map(level.len(), workers, |k| {
            let (i, r) = (level[k], parents[level[k]].and_then(|p| refs[p].as_ref()));
            rebuild(sks[i], indexes[i], records[i], r, sizes[i], inner, is_parent[i])
        });
        // (those of the depth before are done with)
        level.iter().filter_map(|&i| parents[i]).for_each(|p| refs[p] = None);
        for (&i, f) in level.iter().zip(files) {
            let (f, r) = f?;
            (out[i], refs[i]) = (Some(f), r);
        }
    }
    let mut all = Vec::with_capacity(len as usize);
    out.into_iter().for_each(|f| all.extend_from_slice(&f.unwrap()));
    Ok(all)
}

/// The file of `len` bytes of the skeleton `sk`, the index and the records, against the images
/// of a reference file if given; and, if `keep`, the file's own as a reference file.
fn rebuild(
    sk: &[u8],
    index: &[u8],
    records: &[u8],
    refs: Option<&References>,
    len: u64,
    threads: usize,
    keep: bool,
) -> Result<(Vec<u8>, Option<References>)> {
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
    let (out, own) = skel.rebuild(&recs, threads, refs, keep)?;
    if out.len() as u64 != len {
        return Err(corrupt());
    }
    Ok((out, own))
}
