//! aqr-lha's formulas (see aqr-formula): an LHA archive whose files' packed data is rebuilt from
//! their contents. Stored files (`-lh0-`, `-lz4-`, `-pm0-`) are their contents; the streams of
//! `-lh4-` to `-lh7-` are the outputs of one application of the module `lha-recons` (`FORMAT.md`),
//! split into the streams. The rest (the headers, the data of other methods, the bytes after the
//! headers) is kept as it is. The contents are streams by their kinds by their names, so that
//! whoever takes the formula may recompress them in turn and store them once where the same
//! files are archived along with it.

use aqr_formula::{Apply, Formula, Kind, Module, Node};

use crate::lha::{self, Entry};
use crate::lzh::Method;
use crate::recons;

pub use crate::module::{decode, index};

pub const MODULE: Module = Module::new("lha-recons", 1).with_memory(memory);

/// The memory of the module, the streams written onto the output one after another: the inputs
/// and the output, and a stream's work ([`recons::memory`]) of the largest stream (by the index,
/// else all the data).
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let &[data, ..] = u.inputs else { return None };
    // (the index's lengths of data, every other varint)
    let largest = (|| {
        let i = u.contents.input(2).filter(|i| !i.is_empty())?;
        let (mut at, mut most) = (0, 0u64);
        while at < i.len() {
            most = most.max(aqr_common::varint::get(i, &mut at).ok()?);
            aqr_common::varint::get(i, &mut at).ok()?;
        }
        Some(most)
    })()
    .unwrap_or(data);
    Some(u.whole(recons::memory(largest), 16))
}

/// The application of [`MODULE`] rebuilding streams of `len` bytes in all from their data
/// `data`, their records `records` and the index of their lengths `index` ([`index`]).
pub fn apply_recons(data: Node, records: Node, index: Node, len: u64) -> Apply {
    Apply {
        module: MODULE,
        resources: vec![],
        params: vec![],
        inputs: vec![data, records, index],
        len,
        memory_limit: aqr_formula::NO_LIMIT,
    }
}

/// Contents of at least this many bytes are slices of their own, apart from the others of their
/// kind next to them: so that the same contents elsewhere are found by their slices.
pub const OWN_MIN: usize = 1 << 10;

#[derive(Debug, Clone)]
pub struct Options {
    pub threads: usize,
    /// The most bytes of data that the streams may decode to, all together, by the sizes their
    /// headers tell: the streams past it are kept as they are.
    pub limit: u64,
    /// What a file's contents are by its name: the contents of each [`Kind`] are a stream of their
    /// own, sliced in order. By default all are unknown.
    pub kind: fn(&[u8]) -> Kind,
}

impl Default for Options {
    fn default() -> Options {
        Options { threads: 1, limit: u64::MAX, kind: |_| Kind::Unknown }
    }
}

/// [`Options::limit`] for an archive of `n` bytes on its own: 64 MiB, and 32 times the archive.
pub fn limit(n: usize) -> u64 {
    (64 << 20) + 32 * n as u64
}

/// What [`encode_with`] made.
pub struct Encoded {
    pub formula: Formula,
    /// Bytes of data decoded from streams (what counts against [`Options::limit`]).
    pub inflated: u64,
    /// Whether each entry's data is rebuilt (else kept as it is).
    pub rebuilt: Vec<bool>,
}

/// An entry's data made ready for the formula.
enum Made {
    Stored,
    /// The data and the record.
    Split(Vec<u8>, Vec<u8>),
}

/// Whether `d` starts with an LHA archive.
pub fn starts(d: &[u8]) -> bool {
    lha::starts(d)
}

/// Bytes of the LHA archive at the start of `d`, to its end.
pub fn extent(d: &[u8]) -> Option<usize> {
    lha::parse(d).map(|a| a.end)
}

/// The formula rebuilding the archive `input`, decoding at most [`limit`] bytes. None where it is
/// not one, or no file is rebuilt.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    let opt = Options { threads, limit: limit(input.len()), ..Options::default() };
    encode_with(input, &opt).map(|e| e.formula)
}

/// Whether `e` is stored: its data its contents.
fn stored(e: &Entry) -> bool {
    matches!(&e.method, b"-lh0-" | b"-lz4-" | b"-pm0-") && e.data.len() as u64 == e.size
}

/// [`encode`] with `opt`.
pub fn encode_with(input: &[u8], opt: &Options) -> Option<Encoded> {
    let a = lha::parse(input)?;
    let data = |e: &Entry| &input[e.data.clone()];
    // the streams that may be split, those decoding within the limit by their headers' sizes
    let mut left = opt.limit;
    let mut made: Vec<Option<Made>> = a.entries.iter().map(|_| None).collect();
    let mut streams = Vec::new();
    for (i, e) in a.entries.iter().enumerate() {
        if e.data.is_empty() {
            continue;
        }
        if stored(e) {
            made[i] = Some(Made::Stored);
        } else if Method::of(&e.method).is_some() && e.size <= left && usize::try_from(e.size).is_ok() {
            left -= e.size;
            streams.push(i);
        }
    }
    // (the large ones first, side by side on one thread each)
    streams.sort_by_key(|&i| std::cmp::Reverse(a.entries[i].data.len()));
    let done = aqr_common::pool::par_map(streams.len(), opt.threads, |k| {
        let e = &a.entries[streams[k]];
        let m = Method::of(&e.method)?;
        let sp = recons::split(data(e), e.size as usize, m).ok()?;
        // (a stream kept costs nothing more than itself, its header kept anyway)
        (sp.record.len() < e.data.len()).then_some(Made::Split(sp.data, sp.record))
    });
    for (i, m) in streams.into_iter().zip(done) {
        made[i] = m;
    }
    if made.iter().all(Option::is_none) {
        return None;
    }

    let mut f = Formula::default();
    f.push("lha", Vec::new());
    // the contents of stored files and of streams with their kinds and ends; the records and
    // their lengths
    let mut kept = (Vec::new(), Vec::new());
    let mut split = (Vec::new(), Vec::new());
    let (mut records, mut lens) = (Vec::new(), Vec::new());
    let mut inflated = 0;
    for (e, m) in a.entries.iter().zip(&mut made) {
        let kind = (opt.kind)(&input[e.name.clone()]);
        let put = |(data, ends): &mut (Vec<u8>, Vec<(Kind, usize)>), d: &[u8]| {
            data.extend_from_slice(d);
            ends.push((kind, data.len()));
        };
        match m {
            None => {}
            Some(Made::Stored) => put(&mut kept, data(e)),
            Some(Made::Split(d, record)) => {
                inflated += d.len() as u64;
                lens.push((d.len(), record.len()));
                put(&mut split, &std::mem::take(d));
                records.extend_from_slice(&std::mem::take(record));
            }
        }
    }
    let sizes = |split: bool| -> Vec<u64> {
        a.entries
            .iter()
            .zip(&made)
            .filter(|(_, m)| match m {
                Some(Made::Split(..)) => split,
                Some(Made::Stored) => !split,
                None => false,
            })
            .map(|(e, _)| e.data.len() as u64)
            .collect()
    };
    let by_kind = |f: &mut Formula, (data, ends): (Vec<u8>, Vec<(Kind, usize)>)| {
        let raw = f.streams.len();
        f.push("raw", Vec::new());
        f.push_by_kind(raw, data, &ends, "lha", OWN_MIN)
    };
    let kept_sizes = sizes(false);
    let kept_nodes = match kept_sizes.is_empty() {
        true => Vec::new(),
        false => cut(by_kind(&mut f, kept), &kept_sizes),
    };
    let split_sizes = sizes(true);
    let split_nodes = match split_sizes.is_empty() {
        true => Vec::new(),
        false => {
            let records = f.push_as("lha/record", Kind::Compressed, records);
            let index = match index(&lens) {
                i if i.is_empty() => Node::Concat(Vec::new()),
                i => f.push("lha/index", i),
            };
            let data = by_kind(&mut f, split);
            let a = apply_recons(data, records, index, split_sizes.iter().sum());
            match split_sizes.len() {
                1 => vec![Node::Apply(Box::new(a))],
                _ => f.split(a, split_sizes),
            }
        }
    };
    let (mut kept_nodes, mut split_nodes) = (kept_nodes.into_iter(), split_nodes.into_iter());
    // the root: the bytes kept and the data rebuilt in turn
    let (mut container, mut root, mut at) = (Vec::new(), Vec::new(), 0);
    for (e, m) in a.entries.iter().zip(&made) {
        let node = match m {
            None => continue,
            Some(Made::Stored) => kept_nodes.next(),
            Some(Made::Split(..)) => split_nodes.next(),
        };
        root.push(Node::slice(0, container.len() as u64, (e.data.start - at) as u64));
        container.extend_from_slice(&input[at..e.data.start]);
        root.push(node.expect("a node for each entry"));
        at = e.data.end;
    }
    root.push(Node::slice(0, container.len() as u64, (input.len() - at) as u64));
    container.extend_from_slice(&input[at..]);
    f.streams[0].data = container.into();
    f.root = Node::concat(root);
    let rebuilt = made.iter().map(Option::is_some).collect();
    Some(Encoded { formula: f, inflated, rebuilt })
}

/// `n`, a slice or a concatenation of slices, cut into nodes of `sizes`.
fn cut(n: Node, sizes: &[u64]) -> Vec<Node> {
    let mut slices = Vec::new();
    match n {
        Node::Concat(ns) => slices.extend(ns),
        n => slices.push(n),
    }
    let mut slices = slices.into_iter().map(|s| match s {
        Node::Slice { stream, start, len } => (stream, start, len),
        _ => unreachable!("slices of contents"),
    });
    let mut cur = slices.next();
    sizes
        .iter()
        .map(|&size| {
            let (mut parts, mut want) = (Vec::new(), size);
            while want > 0 {
                let (s, a, l) = cur.expect("as many bytes as the sizes");
                let k = l.min(want);
                parts.push(Node::slice(s, a, k));
                want -= k;
                cur = if k == l { slices.next() } else { Some((s, a + k, l - k)) };
            }
            Node::concat(parts)
        })
        .collect()
}

/// The output of `a`, an application of [`MODULE`], on `inputs`.
pub fn apply(a: &Apply, inputs: &[&[u8]], threads: usize) -> Result<Vec<u8>, String> {
    match a.module {
        m if m == MODULE && a.resources.is_empty() => {
            decode(&a.params, inputs, a.len, threads).map_err(|e| e.to_string())
        }
        m => Err(format!("module {m:?} not of aqr-lha")),
    }
}
