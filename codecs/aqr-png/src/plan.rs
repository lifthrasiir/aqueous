//! The memory plans of the modules on WebAssembly ([`aqr_api::stack`]), as [`crate::module`] lays
//! them out: the instance's memory taken block by block in the order the module takes it
//! ([`Sim`]), and each spawn's tasks. What an image takes follows from its record (read here as
//! `codec::rebuild` reads it) and the skeleton's header of it.

use aqr_api::stack::{Sim, block};

use crate::codec::{self, Skeleton, get_bytes, get_varint};
use crate::model::{self, Geometry, Kind};
use crate::part::THREADS;
use crate::png::Ihdr;
use crate::rans::BitR;

/// Bytes of what the modules hold, on WebAssembly (checked there by assertions): a chunk of the
/// skeleton, an image of it, a stream's layout, a block, a span, a pass, an image's reference
/// kept (with its header).
pub mod wasm {
    pub const CHUNK: u64 = 24;
    pub const IMAGE: u64 = 28;
    pub const STREAM: u64 = 28;
    pub const BLOCK: u64 = 1004;
    pub const SPAN: u64 = 8;
    pub const PASS: u64 = 20;
    pub const REFERENCES: u64 = 44;
    /// A payload's length, an item of a batch (a payload kept, or a record and its streams).
    pub const LEN: u64 = 4;
    pub const ITEM: u64 = 36;
    /// A chunk of a file parsed.
    pub const PNG_CHUNK: u64 = 16;
    /// A token, a reference's position.
    pub const TOKEN: u64 = 6;
    pub const POS: u64 = 4;
}

/// What rebuilding an image takes, as its record tells.
struct Image {
    /// Blocks taken in order and kept (but the payload's, which is last), and given back at once
    /// over them.
    kept: Vec<u64>,
    over: u64,
    /// The tokens of the reference kept, where one is.
    refs: Option<u64>,
    /// The tokens of its payload, where it parses ([`model::Reference::count`]).
    tokens: Option<u64>,
}

/// [`Image`] of the record `rec` of an image of `len` bytes (its header `ih`), its reference kept
/// where `keep` (as `codec::image_payload` and `codec::rebuild` take them).
fn image(rec: &[u8], len: usize, ih: &Option<Ihdr>, keep: bool, streamed: bool) -> Option<Image> {
    let mode = *rec.first()?;
    match mode {
        // (as a part module a record kept as it is is streamed: nothing is held)
        0 if streamed => return Some(Image { kept: vec![], over: 0, refs: None, tokens: None }),
        0 => {
            // (kept as it is, and where kept as a reference of a header, parsed: the code
            // tables, then the reference)
            let n = model::Reference::count(&rec[1..], &mut aqr_flate::inflate::Codes::new()).ok().map(|n| n as u64);
            if !(keep && ih.is_some()) {
                return Some(Image { kept: vec![block(len as u64 + 1)], over: 0, refs: None, tokens: n });
            }
            // (its tokens counted, then its payload of room for them, then its reference)
            let [lit, dist] = aqr_flate::inflate::Codes::BYTES;
            let mut kept = vec![block(lit), block(dist), block(len as u64 + 1 + 9 * n.unwrap_or(0))];
            if let Some(n) = n {
                kept.extend([block(wasm::POS * n), block(wasm::TOKEN * n)]);
            }
            return Some(Image { kept, over: 0, refs: n, tokens: n });
        }
        1 | 2 => {}
        _ => return None,
    }
    let g = Geometry::new(ih.as_ref()?)?;
    let rec = &rec[1..];
    let mut kept = vec![block(wasm::PASS * if ih.as_ref()?.interlace == 1 { 7 } else { 1 })];
    let mut at = 0;
    let part = |at: &mut usize| -> Option<&[u8]> {
        let n = get_varint(rec, at).ok()? as usize;
        get_bytes(rec, at, n).ok()
    };
    let layout = part(&mut at)?;
    part(&mut at)?;
    // the layout: its spans, its streams and each one's blocks and trailing bytes, its alternates
    let mut r = BitR::new(layout);
    let kind = if r.bits(1) == 1 { Kind::Bytes } else { Kind::Image };
    let n = r.gamma().ok()? as usize;
    let nstreams = r.gamma().ok()? as usize + 1;
    let (mut streams, mut total, mut most, mut ntok, mut stored) = (vec![], 0u64, 0usize, 0usize, 0usize);
    for _ in 0..nstreams {
        r.bits(16);
        let nblocks = r.gamma().ok()? as usize;
        if nblocks > layout.len() * 8 {
            return None;
        }
        total += nblocks as u64;
        let std_last = r.bits(1) == 1;
        let mut prev = 0;
        for _ in 0..nblocks {
            if !std_last {
                r.bits(1);
            }
            match r.bits(2) {
                0 => {
                    stored = stored.max((r.bits(24) >> 8) as usize);
                }
                1 | 2 => {
                    let count = if r.bits(1) == 1 { prev } else { r.gamma().ok()? as usize };
                    prev = count;
                    (most, ntok) = (most.max(count), ntok + count);
                }
                _ => return None,
            }
        }
        r.bits(8);
        let ntrail = r.gamma().ok()? as usize;
        if ntrail > layout.len() * 8 {
            return None;
        }
        for _ in 0..ntrail {
            r.bits(8);
        }
        streams.push((nblocks as u64, ntrail as u64));
    }
    let nalts = r.gamma().ok()? as u64;
    kept.push(block(wasm::SPAN * total));
    kept.push(block(wasm::STREAM * nstreams as u64));
    for (b, t) in streams {
        kept.push(block(wasm::BLOCK * b));
        kept.push(block(t));
    }
    kept.push(block(4 * nalts));
    // the model's tables: those used, their frequencies, the decoding tables
    let tables = if mode == 2 { model::TABLES_REF } else { model::TABLES };
    let mut lens = [0usize; 3];
    for l in &mut lens {
        *l = get_varint(rec, &mut at).ok()? as usize;
    }
    let tb = get_bytes(rec, &mut at, lens[0]).ok()?;
    let mut r = BitR::new(tb);
    let used: u64 = (0..tables).filter(|_| r.bits(1) == 1).map(|t| model::alphabet(t) as u64).sum();
    let [lut, fc] = crate::rans::Tables::bytes(tables, 256);
    kept.extend([block(tables as u64), block(4 * used), block(lut), block(fc)]);
    let own = keep && !streamed && n <= u32::MAX as usize;
    if streamed {
        // (a piece of each stream of the model, and the room of a block written)
        let writer = (6 * most + 512).max(stored + 16) + aqr_flate::write::TOKEN_SLACK;
        kept.extend([block(crate::part::PIECE as u64), block(crate::part::PIECE as u64), block(writer as u64)]);
    } else {
        // (the payload, then the reference kept: they stay, the rest given back below them)
        kept.push(block(codec::payload_room(len, most, own.then_some(ntok)) as u64));
        if own {
            kept.push(block(wasm::POS * ntok as u64));
            kept.push(block(wasm::TOKEN * ntok as u64));
        }
    }
    // the decoder's windows and a block's tokens
    let win = model::Win::of(&g, n, kind == Kind::Image, 4 * model::WINDOW, model::WINDOW);
    let over = block(win.buf as u64) + block(win.raw as u64) + block(wasm::TOKEN * most as u64);
    Some(Image { kept, over, refs: own.then_some(ntok as u64), tokens: Some(ntok as u64) })
}

/// An image's record, payload's length and header.
type Img<'a> = (&'a [u8], usize, Option<Ihdr>);

/// A file's images, as `codec::Skeleton::rebuild` takes them.
fn images<'a>(sk: &Skeleton, index: &[u8], records: &'a [u8]) -> Option<Vec<Img<'a>>> {
    let (cs, classes) = (&sk.chunks, &sk.classes);
    let (mut at, mut from) = (0, 0);
    let mut out = Vec::with_capacity(sk.images.len());
    for (range, ih) in &sk.images {
        let n = get_varint(index, &mut at).ok()? as usize;
        let skip = if classes[range.start] == codec::FDAT { 4 } else { 0 };
        let len = cs[range.clone()].iter().map(|c| c.len - skip).sum::<usize>();
        out.push((get_bytes(records, &mut from, n).ok()?, len, *ih));
    }
    (at == index.len() && from == records.len()).then_some(out)
}

/// `codec::Skeleton::rebuild` of a file of skeleton `sk`, index `index` and records `records`
/// on `threads` threads, its references kept where `keep`, on the stack of `m`: the skeleton
/// parsed, its records listed, its images' payloads (and references) rebuilt (each a task, of
/// several), the file made. Left on `m` as the module leaves it (the file last).
/// Where `tail`, the file is of room for its references and a byte after it (as a task of
/// `png-set` gives them).
fn file(m: &mut Sim, sk: &[u8], index: &[u8], records: &[u8], threads: usize, keep: bool, tail: bool) -> Option<()> {
    let s = Skeleton::parse(sk).ok()?;
    let n = s.chunks.len() as u64;
    m.take(wasm::CHUNK * n);
    m.take(n);
    m.take(wasm::IMAGE * s.images.len() as u64);
    let imgs = images(&s, index, records)?;
    let k = imgs.len() as u64;
    m.take(8 * k);
    // the payloads, and the references kept
    m.take(12 * k);
    if keep {
        m.take(wasm::REFERENCES * k);
    }
    // (the references kept: each image's tokens, where it has one)
    let mut refs = Vec::with_capacity(imgs.len());
    if threads > 1 && imgs.len() > 1 {
        let infos = imgs.iter().map(|(rec, len, ih)| image(rec, *len, ih, keep, false)).collect::<Option<Vec<_>>>()?;
        let tasks = infos.iter().map(|i| m.task(i.kept.iter().sum::<u64>() + i.over)).collect::<Vec<u64>>();
        // (the tasks' handles; each result kept as it is taken, with its reference)
        m.take(k);
        for ((_, len, ih), i) in imgs.iter().zip(&infos) {
            let t = i.refs.unwrap_or(0);
            m.take(*len as u64 + 9 * t + 1);
            if keep {
                // (the reference got back, reserved exactly; of an image with a header)
                m.take(wasm::POS * t);
                m.take(wasm::TOKEN * t);
                refs.push(ih.map(|_| t));
            }
        }
        m.spawned(0, tasks);
    } else {
        for (rec, len, ih) in &imgs {
            let i = image(rec, *len, ih, keep, false)?;
            for b in &i.kept {
                m.take(*b);
            }
            m.peak(i.over);
            refs.push(i.refs);
        }
    }
    // the file (and its references and a byte after it)
    let total: usize = s.chunks.iter().map(|c| c.len + 12).sum::<usize>() + 8 + s.trailer.len();
    let tail = match tail && keep {
        true => references_bytes(&imgs, &refs) + 1,
        false => tail as u64,
    };
    m.take(total as u64 + tail);
    Some(())
}

/// Bytes of `codec::put_references` of the references of `imgs`, of `refs` tokens each (where
/// it has one).
fn references_bytes(imgs: &[Img], refs: &[Option<u64>]) -> u64 {
    let one = |((_, _, ih), r): (&Img, &Option<u64>)| 1 + 13 * ih.is_some() as u64 + r.map_or(0, |t| 4 + 9 * t);
    4 + imgs.iter().zip(refs).map(one).sum::<u64>()
}

/// The memory plan of `png-recons` ([`crate::module::decode`]): of its initial memory (`initial`
/// bytes), inputs of `inputs` bytes, the skeleton `sk`, the index and the records.
pub fn recons(initial: u64, inputs: &[u64], (sk, index, records): (&[u8], &[u8], &[u8])) -> Option<(u64, u64)> {
    let mut m = Sim::new(initial);
    m.take(aqr_api::stack::whole(0, &[], inputs));
    m.take(aqr_common::crc32::WASM_TABLES);
    file(&mut m, sk, index, records, THREADS, false, false)?;
    let (instance, tasks) = m.plans(1);
    Some((instance, tasks[0]))
}

/// `codec::references` of a file of `chunks` chunks, its images' payloads of these bytes and of
/// these tokens (where they parse, of an image with a header), on the stack of `m` (left as it
/// leaves it, the references last).
fn references(m: &mut Sim, chunks: usize, imgs: &[(usize, Option<u64>)]) {
    m.take(wasm::PNG_CHUNK * chunks as u64);
    m.take(chunks as u64);
    m.take(wasm::IMAGE * imgs.len() as u64);
    let [lit, dist] = aqr_flate::inflate::Codes::BYTES;
    m.take(lit);
    m.take(dist);
    // each image's tokens counted, its payload made at once
    m.take(8 * imgs.len() as u64);
    for (cap, _) in imgs {
        m.peak(block(*cap as u64));
    }
    // the references, each of room for its tokens, its payload made again above it
    m.take(wasm::REFERENCES * imgs.len() as u64);
    for (cap, n) in imgs {
        if let Some(n) = n {
            m.take(wasm::POS * n);
            m.take(wasm::TOKEN * n);
            m.peak(block(*cap as u64));
        }
    }
}

/// [`references`] of the reference file `file`.
fn references_of_file(m: &mut Sim, file: &[u8]) -> Option<()> {
    let Some((chunks, _)) = crate::png::chunks(file) else { return Some(()) };
    let classes = codec::classes(&chunks);
    let imgs = codec::images(&classes, |i| chunks[i].data, |i| chunks[i].kind);
    let mut codes = aqr_flate::inflate::Codes::new();
    let imgs: Vec<(usize, Option<u64>)> = (imgs.iter())
        .map(|(range, ih)| {
            let payload = codec::payload_of(&chunks[range.clone()], classes[range.start]);
            let n = ih.and_then(|_| model::Reference::count(&payload, &mut codes).ok());
            (payload.capacity(), n.map(|n| n as u64))
        })
        .collect();
    references(m, chunks.len(), &imgs);
    Some(())
}

/// [`references`] of the file of skeleton `sk`, index `index` and records `records`: its chunks
/// and images as the skeleton tells them, each image's tokens as its record does.
fn references_of_parts(m: &mut Sim, sk: &[u8], index: &[u8], records: &[u8]) -> Option<()> {
    let s = Skeleton::parse(sk).ok()?;
    let parts = images(&s, index, records)?;
    let mut imgs = Vec::with_capacity(parts.len());
    for ((range, _), (rec, len, ih)) in s.images.iter().zip(&parts) {
        let cap = s.chunks[range.clone()].iter().map(|c| c.len).sum::<usize>();
        let n = image(rec, *len, ih, false, false)?.tokens;
        imgs.push((cap, ih.and(n)));
    }
    references(m, s.chunks.len(), &imgs);
    Some(())
}

/// The memory plan of `png-against` ([`crate::module::decode_against`]): as [`recons`]'s, the
/// references of the reference file `reference` made first.
pub fn against(
    initial: u64,
    inputs: &[u64],
    (sk, index, records, reference): (&[u8], &[u8], &[u8], &[u8]),
) -> Option<(u64, u64)> {
    let mut m = Sim::new(initial);
    m.take(aqr_api::stack::whole(0, &[], inputs));
    m.take(aqr_common::crc32::WASM_TABLES);
    references_of_file(&mut m, reference)?;
    file(&mut m, sk, index, records, THREADS, false, false)?;
    let (instance, tasks) = m.plans(1);
    Some((instance, tasks[0]))
}

/// The memory plan of `png-set` ([`crate::module::decode_set`]): of its initial memory
/// (`initial` bytes), params `params`, inputs of `inputs` bytes and the packed skeletons, indexes
/// and records of the files. The output first; then by depth in the trees, the files of a depth
/// each a task (of several, each giving back the file), or the file in the instance, each made of
/// the references of its parent's file (made of it) and copied to its place, nothing else kept.
pub fn set(
    initial: u64,
    params: &[u8],
    inputs: &[u64],
    (sks, indexes, records): (&[u8], &[u8], &[u8]),
) -> Option<(u64, u64)> {
    let mut at = 0;
    let n = get_varint(params, &mut at).ok()?;
    // (each parameter takes a byte at least)
    if n > params.len() as u64 {
        return None;
    }
    let n = n as usize;
    let mut parents = Vec::with_capacity(n);
    for i in 0..n {
        parents.push(match get_varint(params, &mut at).ok()? {
            0 => None,
            p if usize::try_from(p).is_ok_and(|p| p <= i) => Some(p as usize - 1),
            _ => return None,
        });
    }
    let sizes = (0..n).map(|_| get_varint(params, &mut at).ok()).collect::<Option<Vec<u64>>>()?;
    let unpack = |p| aqr_common::packed::unpack(p).filter(|items| items.len() == n);
    let (sks, indexes, records) = (unpack(sks)?, unpack(indexes)?, unpack(records)?);
    let mut depth = vec![0usize; n];
    for i in 0..n {
        depth[i] = parents[i].map_or(0, |p| depth[p] + 1);
    }
    let mut m = Sim::new(initial);
    m.take(aqr_api::stack::whole(params.len() as u64, &[], inputs));
    m.take(aqr_common::crc32::WASM_TABLES);
    // the parents, the sizes, the unpacked inputs, the depths, the files' places, the output, the
    // files of a depth
    let k = n as u64;
    [8 * k, 8 * k, 8 * k, 8 * k, 8 * k, 4 * k, 4 * k, sizes.iter().try_fold(0u64, |a, &s| a.checked_add(s))?, 4 * k]
        .into_iter()
        .for_each(|b| m.take(b));
    // a file of `threads` threads on the stack of `m`: its parent's references, then the file
    let file = |m: &mut Sim, i: usize, threads: usize, tail: bool| -> Option<()> {
        if let Some(p) = parents[i] {
            references_of_parts(m, sks[p], indexes[p], records[p])?;
        }
        file(m, sks[i], indexes[i], records[i], threads, false, tail)
    };
    for d in 0..=depth.iter().copied().max().unwrap_or(0) {
        let level: Vec<usize> = (0..n).filter(|&i| depth[i] == d).collect();
        let workers = THREADS.min(level.len()).max(1);
        let t = m.top;
        if workers > 1 {
            // (each the made file kept with its index, as it is taken)
            m.take(16 * level.len() as u64);
            let mut tasks = Vec::with_capacity(level.len());
            for &i in &level {
                let mut sub = Sim::new(0);
                file(&mut sub, i, 1, true)?;
                tasks.push(m.task(sub.high));
            }
            m.take(level.len() as u64);
            for &i in &level {
                m.take(sizes[i] + 1);
            }
            m.spawned(0, tasks);
        } else {
            for &i in &level {
                file(&mut m, i, THREADS, false)?;
            }
        }
        m.top = t;
    }
    let (instance, tasks) = m.plans(1);
    Some((instance, tasks[0]))
}

/// The memory plan of `png-recons` as a part module ([`crate::part::run`]): of its initial memory
/// (`initial` bytes), inputs of `inputs` bytes, the skeleton `sk`, the index and the heads of the
/// records: the limit of its instance, and of a task. The inputs are not copied: the skeleton and
/// the index are read whole, a piece of each of the others and of the output taken, and on top of
/// them:
/// - an image at a time (the head of its record, its layout, the model's tables, a piece of each
///   of its streams, the room of a block written, the decoder's windows and a block's tokens), the
///   most of them; or
/// - where the images are small and several (`crate::part::tasks_used`), a batch of them at a
///   time (their records read, then each a task: a copy of the memory as it is, and its payload,
///   layout, tables, pieces, windows and tokens over it).
pub fn part(initial: u64, inputs: &[u64], (sk, index, heads): (&[u8], &[u8], &[u8])) -> Option<(u64, u64)> {
    use crate::part::{BATCH, PIECE};
    /// An image of a batch: its payload kept as it is (its length), or coded (its head past the
    /// mode byte, its streams, the room of its task over a copy of the memory).
    enum Item {
        Kept(u64),
        Coded { head: u64, st: u64, raw: u64, len: u64, own: u64 },
    }
    let s = Skeleton::parse(sk).ok()?;
    let lens = crate::part::payload_lens(&s);
    let tasks = crate::part::tasks_used(&lens);
    let mut m = Sim::new(initial);
    m.take(aqr_common::crc32::WASM_TABLES);
    m.take(inputs[0]);
    m.take(inputs[1]);
    let n = s.chunks.len() as u64;
    m.take(wasm::CHUNK * n);
    m.take(n);
    m.take(wasm::IMAGE * s.images.len() as u64);
    // (the payloads' lengths, the output's piece and the heads', and for tasks those of the others)
    m.take(wasm::LEN * s.images.len() as u64);
    m.take(PIECE as u64);
    m.take(PIECE as u64);
    if tasks {
        m.take(PIECE as u64);
        m.take(PIECE as u64);
    }
    let run_batch = |m: &mut Sim, batch: &mut Vec<Item>| {
        if batch.is_empty() {
            return;
        }
        let t = m.top;
        m.take(wasm::ITEM * THREADS as u64);
        for it in batch.iter() {
            match *it {
                Item::Kept(len) => m.take(len),
                Item::Coded { head, st, raw, .. } => [head, st, raw].into_iter().for_each(|b| m.take(b)),
            }
        }
        // (the list of the images coded, and what each task gives back, joined over it)
        m.take(4 * batch.len() as u64);
        let coded: Vec<(u64, u64)> = (batch.iter())
            .filter_map(|it| if let Item::Coded { len, own, .. } = *it { Some((len, own)) } else { None })
            .collect();
        if coded.len() >= 2 {
            let tasks = coded.iter().map(|&(_, own)| m.task(own)).collect();
            m.spawned(0, tasks);
            m.peak(coded.iter().map(|&(len, _)| block(len + 1)).max().unwrap_or(0));
        } else if let Some(&(_, own)) = coded.first() {
            m.peak(own);
        }
        m.top = t;
        batch.clear();
    };
    let (mut at, mut from) = (0, 0);
    let (mut batch, mut bytes) = (Vec::new(), 0usize);
    for (k, (_, ih)) in s.images.iter().enumerate() {
        let n = get_varint(index, &mut at).ok()? as usize;
        let head = get_bytes(heads, &mut from, n).ok()?;
        let len = lens[k];
        let i = image(head, len, ih, false, true)?;
        let kept = head.first() == Some(&0);
        if !tasks {
            // (the head of the record past its mode byte is held; its payload is not)
            m.peak(if kept { 0 } else { block(n as u64 - 1) } + i.kept.iter().sum::<u64>() + i.over);
            continue;
        }
        if batch.len() >= THREADS || bytes >= BATCH {
            run_batch(&mut m, &mut batch);
            bytes = 0;
        }
        if kept {
            bytes += len;
            batch.push(Item::Kept(len as u64));
            continue;
        }
        let l = crate::part::head_lens(&head[1..]).ok()?;
        bytes += n - 1 + (l[1] + l[2]) as usize;
        // (a task: its payload's room first, then what an image takes but its record)
        let own = block(len as u64 + 1) + i.kept.iter().sum::<u64>() + i.over;
        batch.push(Item::Coded { head: n as u64 - 1, st: l[1], raw: l[2], len: len as u64, own });
    }
    run_batch(&mut m, &mut batch);
    let (instance, tasks) = m.plans(1);
    (at == index.len() && from == heads.len()).then_some((instance, tasks[0]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rans::BitW;

    /// A damaged layout with a trailing-byte count far beyond its size is refused at once.
    #[test]
    fn huge_trailing_count_is_refused() {
        let mut w = BitW::default();
        w.bits(1, 0); // image
        w.gamma(0); // spans
        w.gamma(0); // one stream
        w.bits(16, 0);
        w.gamma(0); // no blocks
        w.bits(1, 1); // std_last
        w.bits(8, 0);
        w.gamma(u32::MAX - 1);
        let layout = w.finish();
        let mut rec = vec![1u8, layout.len() as u8];
        rec.extend_from_slice(&layout);
        rec.push(0); // headers
        let ih = Ihdr { width: 1, height: 1, depth: 8, color: 0, compression: 0, filter: 0, interlace: 0 };
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || tx.send(image(&rec, 1, &Some(ih), false, false).is_none()));
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(1)), Ok(true));
    }
}
