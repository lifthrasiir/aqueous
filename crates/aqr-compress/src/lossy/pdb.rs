//! PDB files (MSF 7.00: blocks of a fixed size, a directory of the streams' blocks): `slack`, the
//! bytes no reader of the container takes zeroed in place: the blocks that the active free block
//! map marks free and nothing uses, the bytes of each stream's last block past its size, those of
//! the directory's last block past its size and of the block map past its entries. The
//! superblock, both free block maps (the inactive one too), the directory, the block map and the
//! streams' data are kept. The file's length stays.
//!
//! The container must be consistent, or the file is kept as it is: no block used twice (by
//! streams, the directory, its map, or as the superblock or a map), none out of the file, none
//! in use marked free (but stream 0's, the previous directory, which writers free), a directory that parses exactly.

use crate::Options;

const MAGIC: &[u8; 32] = b"Microsoft C/C++ MSF 7.00\r\n\x1aDS\0\0\0";

fn u32le(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(*d.get(at..)?.first_chunk()?))
}

pub(super) fn slack(d: &[u8], _: &Options) -> Result<Option<Vec<u8>>, String> {
    if d.len() < 56 || !d.starts_with(MAGIC) {
        return Err("not MSF 7.00".into());
    }
    let f = |k: usize| u32le(d, 32 + 4 * k).unwrap();
    let (bs, fpm, nb, dsz, map) = (f(0) as usize, f(1) as usize, f(2) as usize, f(3) as usize, f(5) as usize);
    if !matches!(bs, 512 | 1024 | 2048 | 4096) {
        return Err("unknown block size".into());
    }
    if !(1..=2).contains(&fpm) {
        return Err("free block map is not block 1 or 2".into());
    }
    if nb == 0 || nb.checked_mul(bs) != Some(d.len()) {
        return Err("block count disagrees with the length".into());
    }
    let nd = dsz.div_ceil(bs);
    if dsz < 4 || 4 * nd > bs {
        return Err("bad directory size".into());
    }
    // (per block: 0 free to zero, 1 claimed; the superblock and the maps are claimed)
    let mut claim = vec![false; nb];
    claim[0] = true;
    for k in (0..nb).step_by(bs) {
        for b in [k + 1, k + 2] {
            if let Some(c) = claim.get_mut(b) {
                *c = true;
            }
        }
    }
    let mut take = |b: usize| -> Result<(), String> {
        match claim.get_mut(b) {
            None => Err("block out of the file".into()),
            Some(c) if *c => Err("block used twice".into()),
            Some(c) => {
                *c = true;
                Ok(())
            }
        }
    };
    let mut used = Vec::new();
    take(map)?;
    used.push(map);
    let blk = |b: usize| &d[b * bs..(b + 1) * bs];
    let mut dir = Vec::with_capacity(nd * bs);
    let mut dir_blocks = Vec::with_capacity(nd);
    for k in 0..nd {
        let b = u32le(blk(map), 4 * k).unwrap() as usize;
        take(b)?;
        used.push(b);
        dir_blocks.push(b);
        dir.extend_from_slice(blk(b));
    }
    dir.truncate(dsz);
    let ns = u32le(&dir, 0).unwrap() as usize;
    let table = ns.checked_mul(4).and_then(|n| n.checked_add(4)).filter(|&n| n <= dsz).ok_or("bad stream count")?;
    let mut tails: Vec<(usize, usize)> = Vec::new(); // (block, bytes kept)
    let mut at = table;
    for s in 0..ns {
        let size = u32le(&dir, 4 + 4 * s).unwrap();
        let n = if size == u32::MAX { 0 } else { (size as usize).div_ceil(bs) };
        let end =
            n.checked_mul(4).and_then(|m| m.checked_add(at)).filter(|&e| e <= dsz).ok_or("directory too short")?;
        let mut last = 0;
        for k in 0..n {
            let b = u32le(&dir, at + 4 * k).unwrap() as usize;
            take(b)?;
            // (stream 0 is the previous directory, which the free block map frees)
            if s != 0 {
                used.push(b);
            }
            last = b;
        }
        if n > 0 && !(size as usize).is_multiple_of(bs) {
            tails.push((last, size as usize % bs));
        }
        at = end;
    }
    if at != dsz {
        return Err("directory has trailing bytes".into());
    }
    // the active free block map: a set bit is a free block
    let free = |b: usize| -> bool {
        let m = b / bs * bs + fpm;
        m < nb && blk(m)[b % bs / 8] >> (b % 8) & 1 != 0
    };
    if used.iter().any(|&b| free(b)) {
        return Err("block in use is marked free".into());
    }
    let mut out = d.to_vec();
    for b in (0..nb).filter(|&b| !claim[b] && free(b)) {
        out[b * bs..(b + 1) * bs].fill(0);
    }
    for (b, keep) in tails {
        out[b * bs + keep..(b + 1) * bs].fill(0);
    }
    let tail = dsz % bs;
    if tail != 0 {
        let b = *dir_blocks.last().unwrap();
        out[b * bs + tail..(b + 1) * bs].fill(0);
    }
    out[map * bs + 4 * nd..(map + 1) * bs].fill(0);
    Ok((out != d).then_some(out))
}
