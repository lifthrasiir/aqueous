//! aqr-png as modules of entry formulas (see aqr-formula): [`decode`] on three inputs, the
//! skeleton, the index and the records of the images, and no parameters; [`decode_against`] on
//! a fourth, the reference file that records may be coded against. And [`decode_set`], of sets
//! of files each coded against an earlier one, the files one after another. See FORMAT.md,
//! *Formula module* and *Sets of files*.

use crate::codec::{self, Parts, References};
use crate::model;
use aqr_common::varint;
use aqr_formula::{Apply, Formula, Kind, Module, Node};

pub use crate::module::{decode, decode_against, decode_set};

pub const MODULE: Module = Module::new("png-recons", 3).with_memory(memory);

/// The module of a file coded against a reference file ([`encode_against`], [`decode_against`]).
pub const AGAINST_MODULE: Module = Module::new("png-against", 1).with_memory(memory_against);

/// The module of sets of files ([`encode_set`], [`decode_set`]).
pub const SET_MODULE: Module = Module::new("png-set", 2).with_memory(memory_set);

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
    let a =
        Apply { module: SET_MODULE, resources: vec![], params, inputs, len: 0, memory_limit: aqr_formula::NO_LIMIT };
    let outputs = f.split(a, files.iter().map(|d| d.len() as u64).collect());
    f.root = Node::Concat(outputs);
    Some(f)
}

// The modules' memory. The decoders are rebuilt here as sums of bytes on 32 bits: the file's
// skeleton and the images' records are read as `codec::Skeleton` and `codec::rebuild` do.

/// The memory of `png-recons`: [`rebuild`] of the file in the memory as it is started.
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let (&[], Some([sk, index, records])) = (u.params, u.all_contents()) else { return None };
    let f = File::parse(sk, index, records, false)?;
    Some(slack(rebuild(&f, u.base(), THREADS, u.output()).peak))
}

/// The memory of `png-against`: the same after the reference file's images are parsed
/// ([`references_memory`]), whose references the tasks copy.
fn memory_against(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let (&[], Some([sk, index, records, reference])) = (u.params, u.all_contents()) else { return None };
    let f = File::parse(sk, index, records, false)?;
    Some(slack(rebuild(&f, u.base() + references_memory(reference)?, THREADS, u.output()).peak))
}

/// The memory of `png-set`: the files by depth in the trees, the files of a depth as tasks
/// (`workers` at once, each with the threads left), the references the next depth needs held
/// with the files done, and the files made one in `len`.
///
/// Per depth the instance holds its parameters' vectors (128 bytes a file), the files done and
/// the references kept, then the results of the depth (the bytes of a task, three times as they
/// are made, and the files and references as they are got); each task is a copy of the memory
/// it spawns from (the high-water mark of all that came before) and what [`rebuild`] adds.
fn memory_set(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let [sks, indexes, records] = u.all_contents()?;
    let mut at = 0;
    let n = codec::get_varint(u.params, &mut at).ok()?;
    if n > u.params.len() as u64 {
        return None;
    }
    let n = n as usize;
    let mut parents = Vec::with_capacity(n);
    for i in 0..n {
        parents.push(match codec::get_varint(u.params, &mut at).ok()? {
            0 => None,
            p if p as usize <= i => Some(p as usize - 1),
            _ => return None,
        });
    }
    let sizes = (0..n).map(|_| codec::get_varint(u.params, &mut at).ok()).collect::<Option<Vec<u64>>>()?;
    let unpack = |p| aqr_common::packed::unpack(p).filter(|items| items.len() == n);
    let (sks, indexes, records) = (unpack(sks)?, unpack(indexes)?, unpack(records)?);
    let mut is_parent = vec![false; n];
    let mut depth = vec![0usize; n];
    for i in 0..n {
        depth[i] = parents[i].map_or(0, |p| depth[p] + 1);
        parents[i].inspect(|&p| is_parent[p] = true);
    }
    let files: Vec<File> =
        (0..n).map(|i| File::parse(sks[i], indexes[i], records[i], is_parent[i])).collect::<Option<_>>()?;
    let live0 = u.base() + 128 * n as u64;
    // the high-water mark of the instance, the files done and the references kept
    let (mut hw, mut done, mut refs) = (live0, 0u64, 0u64);
    let mut peak = live0;
    for d in 0..=depth.iter().copied().max().unwrap_or(0) {
        let level: Vec<usize> = (0..n).filter(|&i| depth[i] == d).collect();
        let workers = THREADS.min(level.len()).max(1);
        let inner = (THREADS / workers).max(1);
        let spawn = workers > 1 && level.len() > 1;
        // (the references of the depth before are held until the depth is done)
        let held = live0 + done + refs;
        hw = hw.max(held);
        let (mut made, mut kept, mut wire) = (0u64, 0u64, 0u64);
        let mut tasks = vec![];
        let mut after = hw;
        for &i in &level {
            let r = rebuild(&files[i], hw, inner, sizes[i]);
            // the result of the file: ok, the file and the references (the images', each an
            // option of a header and of a reference)
            let bytes = 2 + 8 + sizes[i] + 1 + 8 + r.wire_refs;
            made += sizes[i];
            kept += r.refs;
            wire = wire.max(bytes);
            // (live in the task as it puts the result: the file, the references and the bytes,
            // three times)
            // (r.hw has the file, made past the rest)
            tasks.push(if spawn { r.peak.max(r.hw + 3 * bytes) } else { r.peak });
            after = after.max(r.hw);
        }
        let spawner = held + made + kept + wire + 32 * level.len() as u64;
        if spawn {
            peak = peak.max(spawner + top(&mut tasks, 8));
            hw = hw.max(spawner);
        } else {
            peak = peak.max(tasks.iter().copied().max().unwrap_or(0));
            hw = hw.max(after);
        }
        done += made;
        refs = kept;
    }
    // (the files joined: the one output, made once all are held)
    let end = hw + u.output();
    Some(slack(peak.max(end)))
}

/// Threads the modules are given (their task slots).
const THREADS: usize = 8;

/// The sum of the `k` largest of `v`.
fn top(v: &mut [u64], k: usize) -> u64 {
    v.sort_unstable_by(|a, b| b.cmp(a));
    v.iter().take(k).sum()
}

/// Bytes of the allocator's slack: pages of the large blocks and the small ones, and what the
/// tasks make as they are spawned.
fn slack(peak: u64) -> u64 {
    peak + 16 * aqr_formula::PAGE
}

/// `n` bytes, and a page for the allocator (as it grows the memory for a large block).
fn lg(n: u64) -> u64 {
    n + if n >= aqr_formula::PAGE { aqr_formula::PAGE } else { 0 }
}

/// The bytes of a vector made by pushes, up to the end, with the old block held as it grows.
fn grown(n: u64) -> u64 {
    3 * n
}

/// Bytes of a dynamic block kept in a stream's blocks: the header's vectors (the lengths, and
/// at most as many code length operations of two bytes) and the allocator's.
const HEADER: u64 = 320 + 640 + 32;
/// Bytes of a block (`Block` with its `DynHeader`) on 32 bits.
const BLOCK: u64 = 80;
/// Bytes of the tables of a model (the lookup of 4 KiB and the frequencies' 1 KiB, a table
/// each, and the frequencies read, 256 words a table and its header, held as they are made).
const fn player(tables: usize) -> u64 {
    tables as u64 * (4096 + 1024 + 1024 + 16)
}
/// Bytes a token takes in memory, and in a task's result.
const TOKEN: u64 = 6;
const TOKEN_WIRE: u64 = 5;
/// What the parser of a stream ([`scan`]) takes besides the tokens, by block (the frequencies
/// of 1280 bytes, the block and its header, each vector as it grows) and its tables.
const PARSE_BLOCK: u64 = 3 * 1280 + 3 * BLOCK + HEADER;
const PARSE: u64 = 64 << 10;

/// The tokens of an image's payload as [`model::Reference::parse`] finds them, and the most
/// blocks of a stream.
struct Scan {
    tokens: u64,
    blocks: u64,
}

/// [`Scan`] of `payload` (a payload [`model::Reference::parse`] takes: the same streams).
fn scan(payload: &[u8]) -> Option<Scan> {
    use aqr_flate::inflate::{self, Sink};
    struct Count(u64);
    impl Sink for Count {
        fn lit(&mut self, _: u8) {
            self.0 += 1;
        }
        fn mat(&mut self, _: u16, _: u16, _: bool) {
            self.0 += 1;
        }
        fn stored(&mut self, _: &[u8]) {}
    }
    if payload.len() < 2 {
        return None;
    }
    let (mut o, mut s) = (0, Scan { tokens: 0, blocks: 0 });
    let mut first = true;
    loop {
        let mut c = Count(0);
        match inflate::parse_head(&payload[o + 2..], usize::MAX, &mut c) {
            Ok(l) => {
                s.tokens += c.0;
                s.blocks = s.blocks.max(l.stream.blocks.len() as u64);
                // (where the next zlib stream would start, if one plausibly does)
                let next = o + 2 + l.end + 4;
                let header = |b: &[u8]| {
                    b.len() >= 2
                        && b[0] & 0x0f == 8
                        && b[0] >> 4 <= 7
                        && b[1] & 0x20 == 0
                        && (b[0] as u16 * 256 + b[1] as u16).is_multiple_of(31)
                };
                if next + 2 < payload.len() && header(&payload[next..]) {
                    o = next;
                    first = false;
                    continue;
                }
                return Some(s);
            }
            Err(_) if first => return None,
            Err(_) => return Some(s),
        }
    }
}

/// The bytes of a reference of `tokens` tokens made from a payload of `len` bytes
/// ([`model::Reference::parse`]): its capacity, two vectors of that many 4-byte positions and
/// tokens, from a quarter more than the payload's bytes and doubled as they run out; and
/// the most it takes as it is made.
fn reference_bytes(len: u64, tokens: u64) -> (u64, u64) {
    let n0 = len + len / 4;
    let mut cap = n0.max(1);
    let mut grew = false;
    while cap < tokens {
        cap *= 2;
        grew = true;
    }
    let live = (4 + TOKEN) * cap;
    // (the old block held as the new is made)
    (live, if grew { live + live / 2 } else { live })
}

/// The memory of `codec::references` of `file`, a high-water mark over what the module has
/// before: the chunks, their classes and the images, then image by image the payload's copy and
/// the parse on top of the references made.
fn references_memory(file: &[u8]) -> Option<u64> {
    let Some((chunks, _)) = crate::png::chunks(file) else { return Some(0) };
    let k = chunks.len() as u64;
    // the chunks, the classes and the images (each of 28 bytes) by pushes
    let imgs = chunks.iter().filter(|c| matches!(&c.kind, b"IDAT" | b"fdAT")).count() as u64;
    let fixed = grown(16 * k) + k + grown(28 * (imgs + 1));
    let mut made = 0u64;
    let mut peak = fixed;
    let mut at = 0;
    while at < chunks.len() {
        let kind = chunks[at].kind;
        let start = at;
        if !matches!(&kind, b"IDAT" | b"fdAT") {
            at += 1;
            continue;
        }
        // (the images are runs of chunks of a class: IDAT, and fdAT with a sequence number)
        while at < chunks.len() && chunks[at].kind == kind && (kind == *b"IDAT" || chunks[at].data.len() >= 4) {
            at += 1;
        }
        if at == start {
            at += 1;
            continue;
        }
        let skip = if kind == *b"fdAT" { 4 } else { 0 };
        let payload: Vec<u8> = chunks[start..at].iter().flat_map(|c| c.data[skip..].iter().copied()).collect();
        let len = payload.len() as u64;
        let (live, transient, blocks) = match scan(&payload) {
            Some(s) => {
                let (live, transient) = reference_bytes(len, s.tokens);
                (live, transient, s.blocks)
            }
            // (the vectors of the parse that fails)
            None => (0, if len >= 2 { (4 + TOKEN) * (len + len / 4) } else { 0 }, 1 << 10),
        };
        peak = peak.max(fixed + made + lg(len) + transient + blocks * PARSE_BLOCK + PARSE);
        made += live + 32;
    }
    Some(peak.max(fixed + made))
}

/// The sizes of a PNG file's skeleton, read as the module reads it.
struct File {
    /// Bytes of the skeleton's vectors (the chunks, the classes, the images by pushes) and the
    /// records and the results of the images.
    skeleton: u64,
    images: Vec<Image>,
    /// Whether the images' references are kept.
    keep: bool,
}

/// What an image takes, with its payload of `len` bytes.
struct Image {
    len: u64,
    /// The bytes of the payload as it is made (a coded image's has room for the longest codes
    /// ahead), at the most the image takes besides it while it is made, and the reference kept
    /// (if the file's is) with its tokens.
    capacity: u64,
    work: u64,
    own: u64,
    /// The tokens of the reference in a task's result.
    wire: u64,
}

impl File {
    /// `keep`: the file's images' references are kept.
    fn parse(sk: &[u8], index: &[u8], records: &[u8], keep: bool) -> Option<File> {
        let skel = codec::Skeleton::parse(sk).ok()?;
        let mut k = 0;
        let nchunks = codec::get_varint(sk, &mut k).ok()? as usize;
        let mut lens = Vec::with_capacity(nchunks.min(1 << 16));
        for _ in 0..nchunks {
            let _kind = codec::get_bytes(sk, &mut k, 4).ok()?;
            let len = codec::get_varint(sk, &mut k).ok()? as usize;
            let flags = *sk.get(k)?;
            k += 1;
            if flags & 0x80 != 0 {
                k += 4;
            }
            lens.push((len, flags & 0x7f));
            match flags & 0x7f {
                0 => k += len,
                2 => k += 4,
                _ => (),
            }
        }
        let (mut at, mut from) = (0, 0);
        let mut images = vec![];
        let nimgs = skel.images.len() as u64;
        for (range, ih) in &skel.images {
            let n = codec::get_varint(index, &mut at).ok()? as usize;
            let rec = codec::get_bytes(records, &mut from, n).ok()?;
            let skip = if lens[range.start].1 == 2 { 4 } else { 0 };
            let len = lens[range.clone()].iter().map(|c| (c.0 - skip) as u64).sum::<u64>();
            images.push(Image::new(rec, len, ih.as_ref(), keep)?);
        }
        let chunks = nchunks as u64;
        let skeleton = grown(24 * chunks.min(1 << 16)).max(24 * chunks) + chunks + grown(28 * nimgs) + 200 * nimgs;
        if at != index.len() || from != records.len() {
            return None;
        }
        Some(File { skeleton, images, keep })
    }
}

impl Image {
    fn new(rec: &[u8], len: u64, ih: Option<&crate::png::Ihdr>, keep: bool) -> Option<Image> {
        use crate::model::{Geometry, Kind};
        let (&mode, body) = rec.split_first()?;
        if mode == 0 {
            // kept as it is (a reference parsed of it, if it is kept)
            if rec.len() as u64 != len + 1 {
                return None;
            }
            let (mut live, mut work, mut wire) = (0, 0, 0);
            if keep && ih.is_some() {
                match scan(body) {
                    Some(s) => {
                        let (l, transient) = reference_bytes(len, s.tokens);
                        (live, work, wire) = (l, transient + s.blocks * PARSE_BLOCK + PARSE, s.tokens);
                    }
                    // (the vectors of the parse that fails)
                    None if len >= 2 => work = (4 + TOKEN) * (len + len / 4) + PARSE,
                    None => (),
                }
            }
            return Some(Image { len, capacity: len, work: lg(len) + work, own: live, wire });
        }
        if mode > 2 {
            return None;
        }
        let g = Geometry::new(ih?)?;
        let mut at = 0;
        let part = |at: &mut usize| -> Option<&[u8]> {
            let n = codec::get_varint(body, at).ok()? as usize;
            codec::get_bytes(body, at, n).ok()
        };
        let layout = part(&mut at)?;
        let _headers = part(&mut at)?;
        let mut r = crate::rans::BitR::new(layout);
        let kind = if r.bits(1) == 1 { Kind::Bytes } else { Kind::Image };
        let n = r.gamma().ok()? as u64;
        let nstreams = r.gamma().ok()? as u64 + 1;
        if nstreams > layout.len() as u64 {
            return None;
        }
        let (mut blocks, mut dynamic, mut trailing) = (0u64, 0u64, 0u64);
        let (mut spans, mut ntok, mut most) = (0u64, 0u64, 0u64);
        for _ in 0..nstreams {
            r.bits(16);
            let nblocks = r.gamma().ok()? as u64;
            if nblocks > layout.len() as u64 * 8 {
                return None;
            }
            let std_last = r.bits(1) == 1;
            let mut prev = 0u64;
            for _ in 0..nblocks {
                if !std_last {
                    r.bits(1);
                }
                match r.bits(2) {
                    0 => {
                        r.bits(24);
                    }
                    t @ (1 | 2) => {
                        let count = if r.bits(1) == 1 { prev } else { r.gamma().ok()? as u64 };
                        prev = count;
                        ntok += count;
                        most = most.max(count);
                        dynamic += (t == 2) as u64;
                    }
                    _ => return None,
                }
                spans += 1;
                blocks += 1;
                if r.overrun() {
                    return None;
                }
            }
            r.bits(8);
            let ntrail = r.gamma().ok()? as u64;
            if ntrail > layout.len() as u64 * 8 {
                return None;
            }
            trailing += ntrail;
            for _ in 0..ntrail {
                r.bits(8);
            }
        }
        let nalts = r.gamma().ok()? as u64;
        if nalts > layout.len() as u64 * 8 || ntok > n {
            return None;
        }
        let tables = if mode == 2 { model::TABLES_REF } else { model::TABLES };
        let (buf, raw) = window(&g, n, matches!(kind, Kind::Image));
        let capacity = len + 6 * most + 64;
        // the streams' layout, kept in the blocks of the writer: the blocks, the headers of the
        // dynamic ones, the trailing bytes; the spans by pushes; the alts; the tables
        // and their frequencies; the writer's payload; the data and the raw windows; the
        // longest block's tokens; and the reference being made
        let own = if keep { (4 + TOKEN) * ntok } else { 0 };
        let work = 40 * nstreams
            + BLOCK * blocks
            + HEADER * dynamic
            + trailing
            + grown(8 * spans)
            + 8 * nalts
            + player(tables)
            + lg(capacity)
            + lg(buf)
            + lg(raw)
            + lg(TOKEN * most)
            + lg(own);
        Some(Image { len, capacity, work, own, wire: if keep { ntok } else { 0 } })
    }

    /// Bytes of the result of its task: ok, the payload, and the reference if kept.
    fn bytes(&self) -> u64 {
        2 + 8 + self.len + 1 + if self.own > 0 { 16 + (4 + TOKEN_WIRE) * self.wire } else { 0 }
    }
}

/// The buffers of the data and the raw rows for `n` bytes of data of `g`: whole, or the windows
/// of `model::Win::new` (the sizes of Wasm, where they are not set by the environment).
fn window(g: &model::Geometry, n: u64, image: bool) -> (u64, u64) {
    let w = model::Win::of(g, usize::try_from(n).unwrap_or(usize::MAX), image, 4 * model::WINDOW, model::WINDOW);
    (w.buf as u64, w.raw as u64)
}

/// What [`rebuild`] makes of a file.
struct Rebuilt {
    /// The most the memory of the instance and its tasks take (the entry's included).
    peak: u64,
    /// The instance's memory after, which a task spawned later copies.
    hw: u64,
    /// The references kept, and their bytes in the result of a task.
    refs: u64,
    wire_refs: u64,
}

/// `codec::Skeleton::rebuild` of `file` into `total` bytes, run by an instance whose memory
/// is `h0` as it starts, given `threads`.
///
/// With tasks (more than one image and thread) each image is a task, `min(8, images)` at once:
/// a copy of the memory (`h0` and the skeleton's vectors) and what the image takes
/// ([`Image::work`]), or the bytes of its result put (grown as they are, three times: the
/// payload, the old block and the new) if more. The instance holds the results got (payloads
/// and references) and one result's bytes. Without, the images one after another, each payload
/// held. Then the payloads are put in the file, made at its size.
fn rebuild(file: &File, h0: u64, threads: usize, total: u64) -> Rebuilt {
    let hw1 = h0 + file.skeleton;
    let images = &file.images;
    let spawn = threads > 1 && images.len() > 1;
    let keep = file.keep;
    let refs: u64 = images.iter().map(|i| i.own).sum::<u64>() + 32 * images.len() as u64;
    let wire_refs = images.iter().map(|i| 15 + if i.own > 0 { 17 + 9 * i.wire } else { 0 }).sum::<u64>();
    let (mut peak, mut hw);
    if spawn {
        let mut tasks: Vec<u64> = images.iter().map(|i| hw1 + i.work + 3 * i.bytes()).collect();
        let held: u64 = images.iter().map(|i| i.len + i.own).sum();
        let one = images.iter().map(|i| i.bytes()).max().unwrap_or(0);
        hw = hw1 + held + one;
        peak = hw + top(&mut tasks, 8);
    } else {
        let mut before = 0;
        hw = hw1;
        for i in images {
            hw = hw.max(hw1 + before + i.work.max(lg(i.capacity) + i.own));
            before += i.capacity + i.own;
        }
        peak = hw;
    }
    // (the file is made past the high-water mark, as the blocks freed need not fit it)
    let reassembly = hw + lg(total);
    peak = peak.max(reassembly);
    hw = reassembly;
    Rebuilt { peak, hw, refs: refs * keep as u64, wire_refs }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::codec::common;

    fn stream<'a>(f: &'a Formula, class: &str) -> &'a [u8] {
        &f.streams.iter().find(|s| s.class == class).expect("a stream").data
    }

    /// The estimates exist for what the encoder makes (frames as tasks, a reference), and
    /// cover at least the inputs and the output.
    #[test]
    fn memory_covers_inputs_and_output() {
        let file = common::apng(&[(64, 48), (32, 32), (64, 48)], 1);
        let f = encode(&file, 1).expect("a formula");
        let (sk, index, records) = (stream(&f, "png/skeleton"), stream(&f, "png/index"), stream(&f, "png/records"));
        let u = aqr_formula::Usage {
            params: &[],
            resources: &[],
            inputs: &[sk.len() as u64, index.len() as u64, records.len() as u64],
            outputs: &[file.len() as u64],
            contents: &vec![sk, index, records],
            initial_pages: 17,
        };
        let est = memory(&u).expect("an estimate");
        assert!(est > u.base() + 2 * file.len() as u64, "{est}");
        // a variant of the file, against it
        let other = common::apng(&[(64, 48), (32, 32), (64, 48)], 2);
        let f = encode_against(&other, &file, 1).expect("a formula");
        let class = if f.streams.iter().any(|s| s.class == REFERENCE) { REFERENCE } else { return };
        let (sk, index, records) = (stream(&f, "png/skeleton"), stream(&f, "png/index"), stream(&f, "png/records"));
        let reference = stream(&f, class);
        let u = aqr_formula::Usage {
            params: &[],
            resources: &[],
            inputs: &[sk.len() as u64, index.len() as u64, records.len() as u64, reference.len() as u64],
            outputs: &[other.len() as u64],
            contents: &vec![sk, index, records, reference],
            initial_pages: 17,
        };
        let against = memory_against(&u).expect("an estimate");
        assert!(against > u.base() + 2 * other.len() as u64, "{against}");
    }
}
