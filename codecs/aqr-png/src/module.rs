//! The code of the modules (see `formula`, and FORMAT.md), which needs no aqr-formula, so that
//! aqr-modules builds it into their Wasm modules.

use aqr_common::packed;

use crate::codec::{self, References, Skeleton};
use crate::{Result, corrupt};

/// The module: the file of `len` bytes rebuilt from `inputs`, the skeleton, the index (the
/// length of each image's head, as varints), the heads of the records (all of one kept as it is),
/// the rANS streams and the raw bits of their models (`codec::split_records`). Whole, on threads;
/// [`decode_part`] is the same as a part module.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let (&[], &[sk, index, heads, sts, raws]) = (params, inputs) else { return Err(corrupt()) };
    crc_tables();
    let (index, records) = codec::join_records(index, heads, sts, raws)?;
    Ok(rebuild(sk, &index, &records, None, len, threads, (false, false))?.0)
}

/// The module of a file coded against a reference file: [`decode`] with the reference file as
/// a fourth input.
pub fn decode_against(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let (&[], &[sk, index, records, reference]) = (params, inputs) else { return Err(corrupt()) };
    crc_tables();
    Ok(rebuild(sk, index, records, Some(&codec::references(reference)), len, threads, (false, false))?.0)
}

/// The module of sets: the files rebuilt from `inputs`, the packed skeletons, indexes and
/// records of the files, each against its parent as the parameters give them with its size,
/// one after another in `len` bytes.
pub fn decode_set(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    crc_tables();
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
    let mut sizes = Vec::with_capacity(n);
    for _ in 0..n {
        sizes.push(codec::get_varint(params, &mut at)?);
    }
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
    // before), each copied to its place in the output. Natively the parents keep their references
    // as they are rebuilt (until their children are); in WebAssembly they are made of the
    // parents' files in the output (where a child is rebuilt), so that the instance keeps nothing
    // but the output (what it would keep of the parents' would be holes on its stack)
    let wasm = cfg!(target_family = "wasm");
    let mut depth = vec![0usize; n];
    for i in 0..n {
        depth[i] = parents[i].map_or(0, |p| depth[p] + 1);
    }
    let mut starts = Vec::with_capacity(n);
    sizes.iter().fold(0usize, |at, &s| (starts.push(at), at + s as usize).1);
    let mut all = vec![0u8; len as usize];
    let mut kept: Vec<Option<References>> = Vec::with_capacity(if wasm { 0 } else { n });
    if !wasm {
        kept.extend((0..n).map(|_| None));
    }
    let mut is_parent = vec![false; if wasm { 0 } else { n }];
    if !wasm {
        parents.iter().flatten().for_each(|&p| is_parent[p] = true);
    }
    let mut level = Vec::with_capacity(n);
    for d in 0..=depth.iter().copied().max().unwrap_or(0) {
        level.clear();
        level.extend((0..n).filter(|&i| depth[i] == d));
        let workers = threads.min(level.len()).max(1);
        // (in WebAssembly the images of a file of a task on that task alone: no task spawns tasks)
        let inner = if workers > 1 && wasm { 1 } else { (threads / workers).max(1) };
        let file = |k: usize, all: &[u8], kept: &[Option<References>]| {
            let i = level[k];
            let made;
            let refs = match parents[i] {
                Some(p) if !wasm => kept[p].as_ref(),
                Some(p) => {
                    made = codec::references(&all[starts[p]..][..sizes[p] as usize]);
                    Some(&made)
                }
                None => None,
            };
            let keep = !wasm && is_parent[i];
            rebuild(sks[i], indexes[i], records[i], refs, sizes[i], inner, (keep, wasm && workers > 1))
        };
        if wasm && workers > 1 {
            // (as tasks, each the file and a 0, or a 1)
            let task = |k: usize| match file(k, &all, &kept) {
                Ok((mut f, _)) => (f.push(0), f).1,
                Err(_) => vec![1],
            };
            let mut made = Vec::with_capacity(level.len());
            aqr_common::pool::par_each_apart(
                level.len(),
                workers,
                2,
                task,
                || {},
                |k, b| {
                    let size = sizes[level[k]] as usize;
                    match b.len() == size + 1 && b[size] == 0 {
                        true => {
                            made.push((k, b));
                            Ok(())
                        }
                        false => Err(corrupt()),
                    }
                },
            )?;
            for (k, b) in made {
                all[starts[level[k]]..][..b.len() - 1].copy_from_slice(&b[..b.len() - 1]);
            }
        } else {
            // (one by one where alone, as always in WebAssembly)
            let files = match workers {
                1 => Vec::new(),
                _ => aqr_common::pool::par_map(level.len(), workers, |k| file(k, &all, &kept)),
            };
            let mut files = files.into_iter();
            for k in 0..level.len() {
                let (f, r) = match workers {
                    1 => file(k, &all, &kept)?,
                    _ => files.next().expect("a file")?,
                };
                all[starts[level[k]]..][..f.len()].copy_from_slice(&f);
                if let Some(slot) = kept.get_mut(level[k]) {
                    *slot = r;
                }
            }
            // (those of the depth before are done with)
            level.iter().filter_map(|&i| parents[i]).for_each(|p| {
                if let Some(k) = kept.get_mut(p) {
                    *k = None;
                }
            });
        }
    }
    Ok(all)
}

/// The tables of the chunks' CRC-32 made first: in WebAssembly they take memory as they are
/// first used (`aqr_common::crc32::WASM_TABLES`).
fn crc_tables() {
    aqr_common::crc32::hash(&[]);
}

/// The file of `len` bytes of the skeleton `sk`, the index and the records, against the images
/// of a reference file if given; and, if `keep`, the file's own as a reference file; of room for a
/// byte after it where `tail`.
fn rebuild(
    sk: &[u8],
    index: &[u8],
    records: &[u8],
    refs: Option<&References>,
    len: u64,
    threads: usize,
    (keep, tail): (bool, bool),
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
    let (out, own) = skel.rebuild_with(&recs, threads, refs, keep, tail)?;
    if out.len() as u64 != len {
        return Err(corrupt());
    }
    Ok((out, own))
}
