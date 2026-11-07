//! The memory plans of the modules on WebAssembly ([`aqr_api::stack`]), as [`crate::module`] lays
//! them out: the instance's memory taken block by block in the order the module takes it
//! ([`Sim`]), and each spawn's tasks. What an image takes follows from its record (read here as
//! `codec::rebuild` reads it) and the skeleton's header of it.

use aqr_api::stack::{Sim, block};

use crate::codec::{self, Skeleton, get_bytes, get_varint};
use crate::model::{self, Geometry, Kind};
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
    /// A token, a reference's position.
    pub const TOKEN: u64 = 6;
    pub const POS: u64 = 4;
}

/// Threads the modules are given (their task slots).
pub const THREADS: usize = 8;

/// What rebuilding an image takes, as its record tells.
struct Image {
    /// Blocks taken in order and kept (but the payload's, which is last), and given back at once
    /// over them.
    kept: Vec<u64>,
    over: u64,
    /// The tokens of the reference kept.
    refs: u64,
}

/// [`Image`] of the record `rec` of an image of `len` bytes (its header `ih`), its reference kept
/// where `keep` (as `codec::image_payload` and `codec::rebuild` take them).
fn image(rec: &[u8], len: usize, ih: &Option<Ihdr>, keep: bool) -> Option<Image> {
    let mode = *rec.first()?;
    match mode {
        0 => return Some(Image { kept: vec![block(len as u64 + 1)], over: 0, refs: 0 }),
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
    let (mut streams, mut total, mut most, mut ntok) = (vec![], 0u64, 0usize, 0usize);
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
                    r.bits(24);
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
    // (the payload, then the reference kept: they stay, the rest given back below them)
    let own = keep && n <= u32::MAX as usize;
    kept.push(block(codec::payload_room(len, most, own.then_some(ntok)) as u64));
    if own {
        kept.push(block(wasm::POS * ntok as u64));
        kept.push(block(wasm::TOKEN * ntok as u64));
    }
    // the decoder's windows and a block's tokens
    let win = model::Win::of(&g, n, kind == Kind::Image, 4 * model::WINDOW, model::WINDOW);
    let over = block(win.buf as u64) + block(win.raw as u64) + block(wasm::TOKEN * most as u64);
    Some(Image { kept, over, refs: if own { ntok as u64 } else { 0 } })
}

/// A file's images, as `codec::Skeleton::rebuild` takes them: each image's record, payload's length
/// and header.
/// An image's record, payload's length and header.
type Img<'a> = (&'a [u8], usize, Option<Ihdr>);

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
fn file(m: &mut Sim, sk: &[u8], index: &[u8], records: &[u8], threads: usize, keep: bool) -> Option<()> {
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
    if threads > 1 && imgs.len() > 1 {
        let tasks = imgs
            .iter()
            .map(|(rec, len, ih)| image(rec, *len, ih, keep).map(|i| m.task(i.kept.iter().sum::<u64>() + i.over)))
            .collect::<Option<Vec<u64>>>()?;
        // (the tasks' handles; each result kept as it is taken, with its reference)
        m.take(k);
        for (rec, len, ih) in &imgs {
            let i = image(rec, *len, ih, keep)?;
            m.take(*len as u64 + 9 * i.refs + 1);
            if keep {
                // (the reference got back, reserved exactly)
                m.take(wasm::POS * i.refs);
                m.take(wasm::TOKEN * i.refs);
            }
        }
        m.spawned(tasks);
    } else {
        for (rec, len, ih) in &imgs {
            let i = image(rec, *len, ih, keep)?;
            for b in &i.kept {
                m.take(*b);
            }
            m.peak(i.over);
        }
    }
    // the file
    let total: usize = s.chunks.iter().map(|c| c.len + 12).sum::<usize>() + 8 + s.trailer.len();
    m.take(total as u64);
    Some(())
}

/// The memory plan of `png-recons` ([`crate::module::decode`]): of its initial memory (`initial`
/// bytes), inputs of `inputs` bytes, the skeleton `sk`, the index and the records.
pub fn recons(initial: u64, inputs: &[u64], (sk, index, records): (&[u8], &[u8], &[u8])) -> Option<u64> {
    let mut m = Sim::new(initial);
    m.take(aqr_api::stack::whole(0, &[], inputs));
    m.take(aqr_common::crc32::WASM_TABLES);
    file(&mut m, sk, index, records, THREADS, false)?;
    Some(m.plan(THREADS))
}
