//! The code of the modules (see `formula`, and FORMAT.md), which needs no aqr-formula, so that
//! aqr-modules builds it into their Wasm modules.

use aqr_common::packed;

use crate::codec::{self, References, Skeleton};
use crate::{Result, corrupt};

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
        // (as tasks on WebAssembly)
        let files = aqr_common::pool::par_map_apart(level.len(), workers, |k| {
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
