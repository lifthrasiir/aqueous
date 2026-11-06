//! aqr-egg's formulas (see aqr-formula): an EGG archive whose blocks are rebuilt from their
//! contents. Stored blocks are their contents; DEFLATE blocks are the outputs of one application
//! of aqr-flate's module (`flate-set`), bzip2 blocks those of one application of aqr-bz2's
//! `bz2-recons`, the LZMA streams of LZMA blocks those of one application of aqr-lzma's
//! `lzma-recons`, AZO blocks those of one application of aqr-azo's `azo-recons`, each split
//! into the blocks. The rest (the headers, encrypted blocks, blocks not rebuilt, the records
//! after the files) is kept as it is. The contents are streams by their kinds by their files'
//! names, so that whoever takes the formula may recompress them in turn and store them once
//! where the same files are archived along with it.

use aqr_bz2::Framing;
use aqr_bz2::module::StreamParams;
use aqr_flate::formula::Derived;
use aqr_formula::{Formula, Kind, Node};
use aqr_lzma::{lzma, recons};

use crate::egg::{self, AZO, BZIP2, Block, DEFLATE, Egg, LZMA, LZMA_HEADER, STORED};

/// Contents of at least this many bytes are slices of their own, apart from the others of their
/// kind next to them: so that the same contents elsewhere are found by their slices.
pub const OWN_MIN: usize = 1 << 10;

/// Bytes from which a DEFLATE block gets threads of its own: two of aqr-flate's segments.
const LARGE: usize = 2 << 20;

#[derive(Debug, Clone)]
pub struct Options {
    pub threads: usize,
    /// The aqr-flate profile of the records.
    pub profile: u16,
    /// With an adaptive profile, whether to try zlib emulation ([`aqr_flate::Options::zlib`]).
    pub zlib: bool,
    /// The most bytes of data that DEFLATE, bzip2 and LZMA blocks may decode to, all together, by
    /// the sizes their headers tell: the blocks past it are kept as they are.
    pub limit: u64,
    /// What a file's contents are by its name: the contents of each [`Kind`] are a stream of their
    /// own, sliced in order. By default all are unknown.
    pub kind: fn(&[u8]) -> Kind,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            threads: 1,
            profile: aqr_flate::DEFAULT_PROFILE,
            zlib: true,
            limit: u64::MAX,
            kind: |_| Kind::Unknown,
        }
    }
}

/// [`Options::limit`] for an archive of `n` bytes on its own: 64 MiB, and 32 times the archive.
pub fn limit(n: usize) -> u64 {
    (64 << 20) + 32 * n as u64
}

/// Contents of blocks one after another, with their kinds and the ends of each.
type Contents = (Vec<u8>, Vec<(Kind, usize)>);

/// What [`encode_with`] made.
pub struct Encoded {
    pub formula: Formula,
    /// Bytes of data decoded from DEFLATE, bzip2 and LZMA blocks (what counts against
    /// [`Options::limit`]).
    pub inflated: u64,
    /// Whether each block, of the files in order, is rebuilt (else kept as it is).
    pub rebuilt: Vec<bool>,
}

/// A block made ready for the formula.
enum Made {
    Stored,
    /// The data and the record.
    Deflated(Vec<u8>, Vec<u8>),
    /// The stream's params and its data.
    Bzip2(StreamParams, Vec<u8>),
    /// The stream's data and record (the stream follows the block's header of LZMA).
    Lzma(Vec<u8>, Vec<u8>),
    /// The AZO stream's data (before its x86 filter) and record.
    Azo(Vec<u8>, Vec<u8>),
}

impl Made {
    /// The method's place: stored, DEFLATE, bzip2, LZMA, AZO.
    fn slot(&self) -> usize {
        match self {
            Made::Stored => 0,
            Made::Deflated(..) => 1,
            Made::Bzip2(..) => 2,
            Made::Lzma(..) => 3,
            Made::Azo(..) => 4,
        }
    }
}

/// The bytes of block `b` that are rebuilt by `m`: all of it, or the stream after its header of
/// LZMA.
fn span(b: &Block, m: &Made) -> std::ops::Range<usize> {
    match m {
        Made::Lzma(..) => b.body.start + LZMA_HEADER..b.body.end,
        _ => b.body.clone(),
    }
}

/// Whether `d` starts with an EGG archive.
pub fn starts(d: &[u8]) -> bool {
    egg::starts(d)
}

/// Bytes of the EGG archive at the start of `d`, to its final end marker (all of `d` where its
/// records run past it).
pub fn extent(d: &[u8]) -> Option<usize> {
    egg::parse(d).map(|a| a.end)
}

/// The formula rebuilding the archive `input`, decoding at most [`limit`] bytes. None where it is
/// not one, or no block is rebuilt.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    let opt = Options { threads, limit: limit(input.len()), ..Options::default() };
    encode_with(input, &opt, &mut |_, _, _| true).map(|e| e.formula)
}

/// The LZMA stream of an LZMA block's data `body` of `size` bytes of data: its data and record.
fn take_lzma(body: &[u8], size: u32, threads: usize) -> Option<Made> {
    let h = body.get(..LZMA_HEADER)?;
    let props = lzma::Props::from_byte(h[4])?;
    let dict = u32::from_le_bytes(h[5..9].try_into().unwrap());
    let s = &body[LZMA_HEADER..];
    // (with an end marker or without)
    [Some(size as u64), None].into_iter().find_map(|known| {
        let sp = recons::split(s, props, dict, known, size as usize, threads).ok()?;
        (sp.used == s.len() && sp.data.len() == size as usize).then_some(Made::Lzma(sp.data, sp.record))
    })
}

/// The kinds of the contents of each block, by the names of their files: the ends of the
/// contents of each kind in the block's data. The block of a solid archive holds the files before
/// it that have none of their own too, where their sizes add up to its size.
fn kinds(a: &Egg, input: &[u8], kind: fn(&[u8]) -> Kind) -> Vec<Vec<(Kind, usize)>> {
    let mut out = Vec::new();
    let mut pending: Vec<usize> = Vec::new();
    for (i, e) in a.entries.iter().enumerate() {
        let k = kind(&input[e.name.clone()]);
        if e.blocks.is_empty() {
            pending.push(i);
            continue;
        }
        for (j, b) in e.blocks.iter().enumerate() {
            let mut ends = Vec::new();
            if j == 0 && a.solid && !pending.is_empty() {
                pending.push(i);
                let sizes: Vec<u64> = pending.iter().map(|&p| a.entries[p].size).collect();
                if sizes.iter().sum::<u64>() == b.size as u64 {
                    let mut at = 0;
                    for (&p, s) in pending.iter().zip(sizes).filter(|(_, s)| *s > 0) {
                        at += s as usize;
                        ends.push((kind(&input[a.entries[p].name.clone()]), at));
                    }
                }
            }
            if ends.is_empty() {
                ends.push((k, b.size as usize));
            }
            out.push(ends);
        }
        pending.clear();
    }
    out
}

/// [`encode`] with `opt`, each bzip2 block taken only where `bz2_pays` tells that its data may pay
/// (as aqr-bz2's `encode_if` asks).
pub fn encode_with(
    input: &[u8],
    opt: &Options,
    bz2_pays: &mut dyn FnMut(usize, &[u8], bool) -> bool,
) -> Option<Encoded> {
    let a = egg::parse(input)?;
    // the blocks of the files in order, whether their files are encrypted
    let blocks: Vec<(&Block, bool)> =
        a.entries.iter().flat_map(|e| e.blocks.iter().map(move |b| (b, e.encrypted))).collect();
    let body = |b: &Block| &input[b.body.clone()];
    // the blocks that may be rebuilt, those decoding within the limit by their headers' sizes
    let mut left = opt.limit;
    let mut made: Vec<Option<Made>> = blocks.iter().map(|_| None).collect();
    let (mut deflated, mut bzip2, mut lzma, mut azo) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (i, &(b, encrypted)) in blocks.iter().enumerate() {
        if encrypted || b.body.is_empty() {
            continue;
        }
        let size = b.size as u64;
        match b.method {
            STORED if b.body.len() as u64 == size => made[i] = Some(Made::Stored),
            m @ (DEFLATE | BZIP2 | LZMA | AZO) if size <= left => {
                left -= size;
                match m {
                    DEFLATE => deflated.push(i),
                    BZIP2 => bzip2.push(i),
                    LZMA => lzma.push(i),
                    _ => azo.push(i),
                }
            }
            _ => {}
        }
    }
    let fopt = aqr_flate::Options { zlib: opt.zlib, ..aqr_flate::Options::for_profile(opt.profile).ok()? };
    // (the large ones first, each on all threads, the small ones side by side on one each)
    let mut order = deflated.clone();
    order.sort_by_key(|&i| std::cmp::Reverse(blocks[i].0.body.len()));
    let done = aqr_common::pool::par_map(order.len(), opt.threads, |k| {
        let b = blocks[order[k]].0;
        let threads = if b.body.len() >= LARGE { opt.threads } else { 1 };
        let o = aqr_flate::Options { threads, max_data: b.size as usize, ..fopt.clone() };
        let (data, record) = aqr_flate::deconstruct_with(body(b), &o).ok()?;
        // (a block kept costs nothing more than itself, its header kept anyway)
        (record.len() < b.body.len()).then_some(Made::Deflated(data, record))
    });
    for (i, m) in order.into_iter().zip(done) {
        made[i] = m;
    }
    for i in bzip2 {
        let b = blocks[i].0;
        made[i] = aqr_bz2::formula::take_whole(body(b), Framing::Bzip2, opt.threads, b.size as usize, bz2_pays)
            .map(|(p, d)| Made::Bzip2(p, d));
    }
    for i in lzma {
        let b = blocks[i].0;
        made[i] = take_lzma(body(b), b.size, opt.threads);
    }
    // (each on all threads, by its blocks of 512 KiB)
    for i in azo {
        let b = blocks[i].0;
        let sp = aqr_azo::formula::take(body(b), b.size as u64, b.size as usize, opt.threads);
        made[i] = sp.map(|sp| Made::Azo(sp.data, sp.record));
    }
    // (the contents cut by the files' sizes: the data as long as the header says)
    for (m, (b, _)) in made.iter_mut().zip(&blocks) {
        let len = match m {
            Some(Made::Deflated(d, _) | Made::Bzip2(_, d) | Made::Lzma(d, _) | Made::Azo(d, _)) => d.len(),
            _ => continue,
        };
        if len != b.size as usize {
            *m = None;
        }
    }
    if made.iter().all(Option::is_none) {
        return None;
    }

    let kinds = kinds(&a, input, opt.kind);
    let mut f = Formula::default();
    f.push("egg", Vec::new());
    // the contents of each method's blocks with their kinds and ends; the records and their
    // lengths of DEFLATE and LZMA; the bzip2 streams' params
    let mut contents: [Contents; 5] = Default::default();
    let (mut flate_records, mut flate_lens) = (Vec::new(), Vec::new());
    let (mut lzma_records, mut lzma_lens) = (Vec::new(), Vec::new());
    let (mut azo_records, mut azo_lens) = (Vec::new(), Vec::new());
    let mut params = Vec::new();
    let mut inflated = 0;
    for (i, (m, ends)) in made.iter_mut().zip(&kinds).enumerate() {
        let Some(m) = m else { continue };
        let slot = m.slot();
        let data = match m {
            Made::Stored => None,
            Made::Deflated(data, record) => {
                flate_lens.push((data.len(), record.len()));
                flate_records.extend_from_slice(&std::mem::take(record));
                Some(std::mem::take(data))
            }
            Made::Bzip2(p, data) => {
                params.push(std::mem::take(p));
                Some(std::mem::take(data))
            }
            Made::Lzma(data, record) => {
                lzma_lens.push((data.len(), record.len()));
                lzma_records.extend_from_slice(&std::mem::take(record));
                Some(std::mem::take(data))
            }
            Made::Azo(data, record) => {
                azo_lens.push((data.len(), record.len()));
                azo_records.extend_from_slice(&std::mem::take(record));
                Some(std::mem::take(data))
            }
        };
        let (out, out_ends) = &mut contents[slot];
        let start = out.len();
        match data {
            Some(d) => {
                inflated += d.len() as u64;
                out.extend_from_slice(&d);
            }
            None => out.extend_from_slice(body(blocks[i].0)),
        }
        out_ends.extend(ends.iter().map(|&(k, e)| (k, start + e)));
    }
    let sizes = |slot: usize| -> Vec<u64> {
        blocks
            .iter()
            .zip(&made)
            .filter_map(|((b, _), m)| m.as_ref().filter(|m| m.slot() == slot).map(|m| span(b, m).len() as u64))
            .collect()
    };
    let by_kind = |f: &mut Formula, (data, ends): Contents| {
        let raw = f.streams.len();
        f.push("raw", Vec::new());
        f.push_by_kind(raw, data, &ends, "egg", OWN_MIN)
    };
    let [stored, flate, bz2, lz, az] = contents;
    // each method's nodes, one for each block in order
    let mut nodes: [std::vec::IntoIter<Node>; 5] = Default::default();
    let stored_sizes = sizes(0);
    if !stored_sizes.is_empty() {
        nodes[0] = by_kind(&mut f, stored).cut(&stored_sizes).into_iter();
    }
    let flate_sizes = sizes(1);
    if !flate_sizes.is_empty() {
        let records = f.push_as("flate/record", Kind::Compressed, flate_records);
        let index = match aqr_flate::formula::index(&flate_lens) {
            i if i.is_empty() => Node::Concat(Vec::new()),
            i => f.push("flate/index", i),
        };
        let data = by_kind(&mut f, flate);
        let len = flate_sizes.iter().sum();
        let a = aqr_flate::formula::apply(opt.profile, data, records, index, len, Derived::None).ok()?;
        nodes[1] = outputs(&mut f, a, flate_sizes).into_iter();
    }
    let bz2_sizes = sizes(2);
    if !bz2_sizes.is_empty() {
        let data = by_kind(&mut f, bz2);
        let a = aqr_bz2::formula::apply_framed(Framing::Bzip2, &params, data, bz2_sizes.iter().sum());
        nodes[2] = outputs(&mut f, a, bz2_sizes).into_iter();
    }
    let lzma_sizes = sizes(3);
    if !lzma_sizes.is_empty() {
        let data = by_kind(&mut f, lz);
        let records = f.push("lzma/record", lzma_records);
        let index = f.push("lzma/index", aqr_lzma::formula::index(&lzma_lens));
        let a = aqr_formula::Apply {
            module: aqr_lzma::formula::MODULE,
            resources: vec![],
            params: Vec::new(),
            inputs: vec![data, records, index],
            len: lzma_sizes.iter().sum(),
            memory_limit: aqr_formula::NO_LIMIT,
            task_limits: Vec::new(),
        };
        nodes[3] = outputs(&mut f, a, lzma_sizes).into_iter();
    }
    let azo_sizes = sizes(4);
    if !azo_sizes.is_empty() {
        let data = by_kind(&mut f, az);
        let records = f.push_as("azo/record", Kind::Compressed, azo_records);
        let index = f.push("azo/index", aqr_azo::formula::index(&azo_lens));
        let a = aqr_azo::formula::apply(data, records, index, azo_sizes.iter().sum());
        nodes[4] = outputs(&mut f, a, azo_sizes).into_iter();
    }
    // the root: the bytes kept and the blocks rebuilt in turn
    let (mut container, mut root, mut at) = (Vec::new(), Vec::new(), 0);
    for ((b, _), m) in blocks.iter().zip(&made) {
        let Some(m) = m else { continue };
        let s = span(b, m);
        root.push(Node::slice(0, container.len() as u64, (s.start - at) as u64));
        container.extend_from_slice(&input[at..s.start]);
        root.push(nodes[m.slot()].next().expect("a node for each block"));
        at = s.end;
    }
    root.push(Node::slice(0, container.len() as u64, (input.len() - at) as u64));
    container.extend_from_slice(&input[at..]);
    f.streams[0].data = container.into();
    f.root = Node::concat(root);
    let rebuilt = made.iter().map(Option::is_some).collect();
    Some(Encoded { formula: f, inflated, rebuilt })
}

/// The outputs of `a`, one of each of `sizes`.
fn outputs(f: &mut Formula, a: aqr_formula::Apply, sizes: Vec<u64>) -> Vec<Node> {
    match sizes.len() {
        1 => vec![Node::Apply(Box::new(a))],
        _ => f.split(a, sizes),
    }
}

/// The output of `a`, an application of aqr-flate's module, aqr-bz2's `bz2-recons` or aqr-lzma's
/// `lzma-recons`, on `inputs`.
pub fn apply(a: &aqr_formula::Apply, inputs: &[&[u8]], threads: usize) -> Result<Vec<u8>, String> {
    let res: Vec<&[u8]> = a.resources.iter().map(|r| &r[..]).collect();
    match a.module {
        m if m == aqr_bz2::formula::MODULE && res.is_empty() => {
            aqr_bz2::formula::decode(&a.params, inputs, a.len, threads).map_err(|e| e.to_string())
        }
        m if m == aqr_lzma::formula::MODULE && res.is_empty() => {
            aqr_lzma::formula::decode(&a.params, inputs, a.len, threads).map_err(|e| e.to_string())
        }
        m if m == aqr_azo::formula::MODULE && res.is_empty() => {
            aqr_azo::formula::decode(&a.params, inputs, a.len, threads).map_err(|e| e.to_string())
        }
        m if m == aqr_flate::formula::MODULE => {
            aqr_flate::formula::decode(&a.params, &res, inputs, a.len, threads).map_err(|e| e.to_string())
        }
        m => Err(format!("module {m:?} not of aqr-egg")),
    }
}
