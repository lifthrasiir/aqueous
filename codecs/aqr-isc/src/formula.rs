//! aqr-isc's formula (see aqr-formula): an InstallShield cabinet whose chunks are rebuilt from
//! the contents of the files they hold. The chunks (independent raw DEFLATE streams) are the
//! outputs of one application of aqr-flate's module (`flate-set`), split; the rest (the headers,
//! the chunks' lengths, chunks not rebuilt) is kept as it is. The files' data, the chunks'
//! data one after another up to a chunk that holds less than a full one, are contents of their
//! own, to recompress in turn (the executables among them) and to store once where the same
//! files are archived along with the cabinet.

use aqr_flate::formula::Item;
use aqr_formula::{Formula, Kind, Node};

use crate::isc;

/// Bytes of the data of a full chunk; one with less ends its file.
pub const CHUNK: usize = 32768;

/// The most bytes of data a chunk may decode to.
const MAX_CHUNK: usize = 1 << 16;

/// Files of at least this many bytes are contents of their own; the smaller ones share one.
pub const OWN_MIN: usize = 1 << 12;

#[derive(Debug, Clone)]
pub struct Options {
    pub threads: usize,
    /// The aqr-flate profile of the records.
    pub profile: u16,
    /// With an adaptive profile, whether to try zlib emulation ([`aqr_flate::Options::zlib`]).
    pub zlib: bool,
    /// The most bytes of data that the chunks may decode to, all together: the chunks past it are
    /// kept as they are.
    pub limit: u64,
}

impl Default for Options {
    fn default() -> Options {
        Options { threads: 1, profile: aqr_flate::DEFAULT_PROFILE, zlib: true, limit: u64::MAX }
    }
}

/// [`Options::limit`] for a cabinet of `n` bytes on its own: 64 MiB, and 32 times the cabinet.
pub fn limit(n: usize) -> u64 {
    (64 << 20) + 32 * n as u64
}

/// What [`encode_with`] made.
pub struct Encoded {
    pub formula: Formula,
    /// Bytes of data decoded from the chunks (what counts against [`Options::limit`]).
    pub inflated: u64,
    /// The number of chunks rebuilt, and of all.
    pub rebuilt: usize,
    pub chunks: usize,
}

/// Whether `d` starts with a cabinet of data.
pub fn starts(d: &[u8]) -> bool {
    isc::starts(d)
}

/// The formula rebuilding the cabinet `input`, decoding at most [`limit`] bytes. None where it is
/// not one, or no chunk is rebuilt.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    encode_with(input, &Options { threads, limit: limit(input.len()), ..Options::default() }).map(|e| e.formula)
}

/// [`encode`] with `opt`.
pub fn encode_with(input: &[u8], opt: &Options) -> Option<Encoded> {
    let cab = isc::parse(input)?;
    let fopt = aqr_flate::Options { zlib: opt.zlib, ..aqr_flate::Options::for_profile(opt.profile).ok()? };
    // the chunks within the limit by their most data, each side by side on a thread
    let take = usize::try_from(opt.limit / CHUNK as u64).unwrap_or(usize::MAX).min(cab.chunks.len());
    let done = aqr_common::pool::par_map(take, opt.threads, |k| {
        let body = &input[cab.chunks[k].clone()];
        let o = aqr_flate::Options { threads: 1, max_data: MAX_CHUNK, ..fopt.clone() };
        let (data, record) = aqr_flate::deconstruct_with(body, &o).ok()?;
        // (a chunk kept costs nothing more than itself)
        (record.len() < body.len()).then_some((data, record))
    });
    if done.iter().all(Option::is_none) {
        return None;
    }

    let mut f = Formula::default();
    f.push("isc", Vec::new());
    // the records and the files that are too small to be contents of their own: streams made
    // first so that the nodes of the chunks' data and records can name them
    let (records_at, small_at) = (f.streams.len(), f.streams.len() + 1);
    f.push_as("flate/record", Kind::Compressed, Vec::new());
    f.push("isc/small", Vec::new());
    let (mut records, mut small) = (Vec::new(), Vec::new());
    let mut items: Vec<Item> = Vec::new();
    let mut inflated = 0u64;
    // the file being read: its data, and for each of its chunks (by index among those rebuilt)
    // where in it
    let mut file: (Vec<u8>, Vec<(usize, usize, usize)>) = Default::default();
    let mut rebuilt = Vec::new();
    let flush = |f: &mut Formula,
                 items: &mut Vec<Item>,
                 small: &mut Vec<u8>,
                 file: &mut (Vec<u8>, Vec<(usize, usize, usize)>),
                 profile: u16| {
        let (data, parts) = std::mem::take(file);
        if parts.is_empty() {
            return;
        }
        let (stream, base) = match data.len() >= OWN_MIN {
            true => (f.streams.len(), 0),
            false => (small_at, small.len()),
        };
        match data.len() >= OWN_MIN {
            true => {
                f.push_content("isc", Kind::Unknown, data);
            }
            false => small.extend_from_slice(&data),
        }
        for (k, at, len) in parts {
            items[k].data = (Node::slice(stream, (base + at) as u64, len as u64), len);
            items[k].profile = profile;
        }
    };
    for (k, d) in done.iter().enumerate() {
        match d {
            Some((data, record)) => {
                let (at, len) = (file.0.len(), data.len());
                file.0.extend_from_slice(data);
                file.1.push((items.len(), at, len));
                items.push(Item {
                    profile: opt.profile,
                    data: (Node::Concat(Vec::new()), 0),
                    record: (Node::slice(records_at, records.len() as u64, record.len() as u64), record.len()),
                    len: cab.chunks[k].len() as u64,
                });
                records.extend_from_slice(record);
                inflated += len as u64;
                rebuilt.push(k);
                if len < CHUNK {
                    flush(&mut f, &mut items, &mut small, &mut file, opt.profile);
                }
            }
            None => flush(&mut f, &mut items, &mut small, &mut file, opt.profile),
        }
    }
    flush(&mut f, &mut items, &mut small, &mut file, opt.profile);
    f.streams[records_at].data = records.into();
    f.streams[small_at].data = small.into();
    let nodes = aqr_flate::formula::streams(&mut f, items).ok()?;

    // the root: the bytes kept and the chunks rebuilt in turn
    let (mut container, mut root, mut at) = (Vec::new(), Vec::new(), 0);
    for (&k, node) in rebuilt.iter().zip(nodes) {
        let body = &cab.chunks[k];
        root.push(Node::slice(0, container.len() as u64, (body.start - at) as u64));
        container.extend_from_slice(&input[at..body.start]);
        root.push(node);
        at = body.end;
    }
    root.push(Node::slice(0, container.len() as u64, (input.len() - at) as u64));
    container.extend_from_slice(&input[at..]);
    f.streams[0].data = container.into();
    f.root = Node::concat(root);
    Some(Encoded { formula: f, inflated, rebuilt: rebuilt.len(), chunks: cab.chunks.len() })
}
