//! aqr-mszip's formulas (see aqr-formula): a CAB file whose MSZIP and stored folders are rebuilt
//! from their data. An MSZIP folder's blocks are one DEFLATE stream joined (`mszip`), which
//! aqr-flate's module (`flate-set`, one application for all of them) rebuilds from the data and
//! a record; [`MODULE`] cuts the blocks back from it, the data blocks' headers kept between
//! them, and takes a stored folder's blocks from its data. The rest of the file (the header,
//! folders of other methods, the files, a signature) is kept as it is. The data of a folder is
//! the contents of its files by their kinds by their names, so that whoever takes the formula may
//! recompress them in turn and store them once where the same files are archived along with it.

use aqr_flate::formula::Derived;
use aqr_formula::{Apply, Formula, Kind, Node};

use crate::cab::{self, Cab, Folder};
use crate::module::Block;
use crate::mszip::{self, BitW};

pub use crate::module::{decode, index};

pub const MODULE: aqr_formula::Module = aqr_formula::Module::new("mszip-recons", 1).with_memory(memory);

/// The memory of the module: its inputs and its output, nothing else of size.
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    Some(u.whole(0, 1))
}

/// Files' contents of at least this many bytes are slices of their own, apart from the others of
/// their kind next to them: so that the same contents elsewhere are found by their slices.
pub const OWN_MIN: usize = 1 << 10;

#[derive(Debug, Clone)]
pub struct Options {
    pub threads: usize,
    /// The aqr-flate profile of the records.
    pub profile: u16,
    /// With an adaptive profile, whether to try zlib emulation ([`aqr_flate::Options::zlib`]).
    pub zlib: bool,
    /// The most bytes of data that the MSZIP folders may inflate to, all together: the folders
    /// past it are kept as they are.
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

/// [`Options::limit`] for a file of `n` bytes on its own: 64 MiB, and 32 times the file.
pub fn limit(n: usize) -> u64 {
    (64 << 20) + 32 * n as u64
}

/// What [`encode_with`] made.
pub struct Encoded {
    pub formula: Formula,
    /// Bytes of data inflated from MSZIP folders (what counts against [`Options::limit`]).
    pub inflated: u64,
}

/// A folder made ready for the formula.
enum Made {
    /// The data, its record, the DEFLATE blocks of each data block, and the bytes of the joined
    /// stream.
    Mszip {
        data: Vec<u8>,
        record: Vec<u8>,
        blocks: Vec<usize>,
        joined: usize,
    },
    Stored {
        data: Vec<u8>,
    },
}

impl Made {
    fn data(&self) -> &[u8] {
        match self {
            Made::Mszip { data, .. } | Made::Stored { data } => data,
        }
    }
}

/// Whether the data blocks of `f` follow each other, each of some data.
fn contiguous(f: &Folder) -> bool {
    f.blocks.windows(2).all(|w| w[0].payload.end == w[1].start) && f.blocks.iter().all(|b| b.uncomp > 0)
}

/// Folder `i` of `c` made ready, of at most `max` bytes of data.
fn make(input: &[u8], c: &Cab, i: usize, max: u64, opt: &Options) -> Option<Made> {
    let f = &c.folders[i];
    if f.blocks.is_empty() || !contiguous(f) || c.continued(i) {
        return None;
    }
    let payloads: Vec<&[u8]> = f.blocks.iter().map(|b| &input[b.payload.clone()]).collect();
    match f.method() {
        cab::NONE => {
            if f.blocks.iter().any(|b| b.payload.len() != b.uncomp) {
                return None;
            }
            Some(Made::Stored { data: payloads.concat() })
        }
        cab::MSZIP if f.size() as u64 <= max => {
            let mut w = BitW::new(Vec::with_capacity(f.span()?.len()));
            let joined = mszip::join(&payloads, &mut w).ok()?;
            if joined.iter().zip(&f.blocks).any(|(j, b)| j.data != b.uncomp) {
                return None;
            }
            let fopt = aqr_flate::Options {
                zlib: opt.zlib,
                threads: opt.threads,
                max_data: f.size(),
                ..aqr_flate::Options::for_profile(opt.profile).ok()?
            };
            let (data, record) = aqr_flate::deconstruct_with(&w.out, &fopt).ok()?;
            let blocks = joined.iter().map(|j| j.blocks).collect();
            (data.len() == f.size()).then_some(Made::Mszip { data, record, blocks, joined: w.out.len() })
        }
        _ => None,
    }
}

/// Whether `d` starts with a CAB file's header.
pub fn starts(d: &[u8]) -> bool {
    cab::starts(d)
}

/// The bytes of the CAB file at the start of `d`: the cabinet, and a signature after it.
pub fn extent(d: &[u8]) -> Option<usize> {
    cab::parse(d).map(|c| c.extent())
}

/// The formula rebuilding the CAB file `input`, inflating at most [`limit`] bytes. None where it
/// is not one, or no folder is taken.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    encode_with(input, &Options { threads, limit: limit(input.len()), ..Options::default() }).map(|e| e.formula)
}

/// [`encode`] with `opt`.
pub fn encode_with(input: &[u8], opt: &Options) -> Option<Encoded> {
    let c = cab::parse(input)?;
    // (the folders side by side, each stream's segments on the workers free)
    let mut made = aqr_common::pool::par_map(c.folders.len(), opt.threads, |i| make(input, &c, i, opt.limit, opt));
    // the folders within the limit, of spans one after another
    let (mut inflated, mut at) = (0u64, 0usize);
    for (m, f) in made.iter_mut().zip(&c.folders) {
        let Some(span) = f.span() else { continue };
        if m.is_none() {
            continue;
        }
        let mszip = matches!(m, Some(Made::Mszip { .. }));
        if span.start < at || (mszip && inflated + f.size() as u64 > opt.limit) {
            *m = None;
            continue;
        }
        at = span.end;
        inflated += if mszip { f.size() as u64 } else { 0 };
    }
    if made.iter().all(Option::is_none) {
        return None;
    }
    let mut f = Formula::default();
    f.push("cab", Vec::new());
    // the files' contents of each folder taken by their kinds, MSZIP's and stored apart
    let (mut flate_data, mut flate_ends, mut raw_data, mut raw_ends) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let (mut records, mut lens, mut joined_len) = (Vec::new(), Vec::new(), 0u64);
    let (mut blocks, mut kept, mut pieces) = (Vec::new(), Vec::new(), Vec::new());
    for (i, m) in made.into_iter().enumerate() {
        let Some(m) = m else { continue };
        let folder = &c.folders[i];
        let (data, ends) = match m {
            Made::Mszip { .. } => (&mut flate_data, &mut flate_ends),
            Made::Stored { .. } => (&mut raw_data, &mut raw_ends),
        };
        let base = data.len();
        file_ends(&c, i, m.data().len(), opt.kind).into_iter().for_each(|(k, e)| ends.push((k, base + e)));
        data.extend_from_slice(m.data());
        for (k, b) in folder.blocks.iter().enumerate() {
            kept.extend_from_slice(&input[b.start..b.payload.start]);
            let k_len = b.payload.start - b.start;
            blocks.push(match &m {
                Made::Mszip { blocks, .. } => Block::Mszip { kept: k_len, blocks: blocks[k] },
                Made::Stored { .. } => Block::Raw { kept: k_len, len: b.uncomp },
            });
        }
        if let Made::Mszip { data, record, joined, .. } = &m {
            lens.push((data.len(), record.len()));
            records.extend_from_slice(record);
            joined_len += *joined as u64;
        }
        pieces.push(folder.span().unwrap());
    }
    let joined = match lens.is_empty() {
        true => Node::Concat(Vec::new()),
        false => {
            let records = f.push_as("flate/record", Kind::Compressed, records);
            let index = match aqr_flate::formula::index(&lens) {
                i if i.is_empty() => Node::Concat(Vec::new()),
                i => f.push("flate/index", i),
            };
            let raw = f.streams.len();
            f.push("raw", Vec::new());
            let data = f.push_by_kind(raw, flate_data, &flate_ends, "cab", OWN_MIN);
            let a = aqr_flate::formula::apply(opt.profile, data, records, index, joined_len, Derived::None).ok()?;
            Node::Apply(Box::new(a))
        }
    };
    let raw = match raw_data.is_empty() {
        true => Node::Concat(Vec::new()),
        false => {
            let raw = f.streams.len();
            f.push("raw", Vec::new());
            f.push_by_kind(raw, raw_data, &raw_ends, "cab", OWN_MIN)
        }
    };
    let kept = f.push_as("cab/data-headers", Kind::Table, kept);
    let index = f.push("mszip/index", index(&blocks));
    let sizes: Vec<u64> = pieces.iter().map(|p| p.len() as u64).collect();
    let a = Apply {
        module: MODULE,
        resources: vec![],
        params: vec![],
        inputs: vec![joined, raw, kept, index],
        len: sizes.iter().sum(),
        memory_limit: aqr_formula::NO_LIMIT,
    };
    let nodes = match sizes.len() {
        1 => vec![Node::Apply(Box::new(a))],
        _ => f.split(a, sizes),
    };
    let mut container = Vec::new();
    let mut root = Vec::with_capacity(2 * pieces.len() + 1);
    let mut at = 0;
    for (r, node) in pieces.into_iter().zip(nodes) {
        root.push(Node::slice(0, container.len() as u64, (r.start - at) as u64));
        container.extend_from_slice(&input[at..r.start]);
        root.push(node);
        at = r.end;
    }
    root.push(Node::slice(0, container.len() as u64, (input.len() - at) as u64));
    container.extend_from_slice(&input[at..]);
    f.streams[0].data = container.into();
    f.root = Node::concat(root);
    Some(Encoded { formula: f, inflated })
}

/// The kinds of the contents of the files of folder `i` of `c` and their ends in its `len` bytes
/// of data, gaps between them unknown; one unknown piece where they overlap or run past the data.
fn file_ends(c: &Cab, i: usize, len: usize, kind: fn(&[u8]) -> Kind) -> Vec<(Kind, usize)> {
    let mut files: Vec<&cab::File> = c.files_of(i).collect();
    files.sort_by_key(|f| f.offset);
    let mut ends = Vec::with_capacity(files.len() + 1);
    let mut at = 0;
    for f in files {
        let (start, end) = (f.offset as usize, f.offset as usize + f.size as usize);
        if start < at || end > len {
            return vec![(Kind::Unknown, len)];
        }
        if start > at {
            ends.push((Kind::Unknown, start));
        }
        ends.push((kind(&f.name), end));
        at = end;
    }
    if at < len || ends.is_empty() {
        ends.push((Kind::Unknown, len));
    }
    ends
}

/// The output of `a`, an application of [`MODULE`] or of aqr-flate's, on `inputs`.
pub fn apply(a: &Apply, inputs: &[&[u8]], threads: usize) -> crate::Result<Vec<u8>> {
    let res: Vec<&[u8]> = a.resources.iter().map(|r| &r[..]).collect();
    match a.module {
        m if m == MODULE && res.is_empty() => decode(&a.params, inputs, a.len),
        m if m == aqr_flate::formula::MODULE => aqr_flate::formula::decode(&a.params, &res, inputs, a.len, threads)
            .map_err(|_| crate::Error("DEFLATE stream not rebuilt")),
        _ => Err(crate::corrupt()),
    }
}
