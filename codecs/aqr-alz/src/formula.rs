//! aqr-alz's formulas (see aqr-formula): an ALZ archive whose entries' bodies are rebuilt from
//! their contents. Stored bodies are their contents; DEFLATE bodies are the outputs of one
//! application of aqr-flate's module (`flate-set`), bzip2 bodies those of one application of
//! aqr-bz2's `bz2-alz-recons`, each split into the bodies. The rest (the headers, encrypted
//! bodies, bodies not rebuilt, the records after the entries) is kept as it is. The contents are
//! streams by their kinds by their names, so that whoever takes the formula may recompress them
//! in turn and store them once where the same files are archived along with it.

use aqr_bz2::module::StreamParams;
use aqr_flate::formula::Derived;
use aqr_formula::{Formula, Kind, Node};

use crate::alz::{self, BZIP2, DEFLATE, Entry, STORED};

/// Contents of at least this many bytes are slices of their own, apart from the others of their
/// kind next to them: so that the same contents elsewhere are found by their slices.
pub const OWN_MIN: usize = 1 << 10;

/// Bytes from which a DEFLATE body gets threads of its own: two of aqr-flate's segments.
const LARGE: usize = 2 << 20;

#[derive(Debug, Clone)]
pub struct Options {
    pub threads: usize,
    /// The aqr-flate profile of the records.
    pub profile: u16,
    /// With an adaptive profile, whether to try zlib emulation ([`aqr_flate::Options::zlib`]).
    pub zlib: bool,
    /// The most bytes of data that DEFLATE and bzip2 bodies may decode to, all together, by the
    /// sizes their headers tell: the bodies past it are kept as they are.
    pub limit: u64,
    /// What an entry's contents are by its name: the contents of each [`Kind`] are a stream of
    /// their own, sliced in order. By default all are unknown.
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

/// What [`encode_with`] made.
pub struct Encoded {
    pub formula: Formula,
    /// Bytes of data decoded from DEFLATE and bzip2 bodies (what counts against
    /// [`Options::limit`]).
    pub inflated: u64,
    /// Whether each entry's body is rebuilt (else kept as it is).
    pub rebuilt: Vec<bool>,
}

/// A body made ready for the formula.
enum Made {
    Stored,
    /// The data and the record.
    Deflated(Vec<u8>, Vec<u8>),
    /// The stream's params and its data.
    Bzip2(StreamParams, Vec<u8>),
}

/// Whether `d` starts with an ALZ archive.
pub fn starts(d: &[u8]) -> bool {
    alz::starts(d)
}

/// Bytes of the ALZ archive at the start of `d`, to its end record (all of `d` where its records
/// run past it).
pub fn extent(d: &[u8]) -> Option<usize> {
    alz::parse(d).map(|a| a.end)
}

/// The formula rebuilding the archive `input`, decoding at most [`limit`] bytes. None where it is
/// not one, or no body is rebuilt.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    let opt = Options { threads, limit: limit(input.len()), ..Options::default() };
    encode_with(input, &opt, &mut |_, _, _| true).map(|e| e.formula)
}

/// [`encode`] with `opt`, each bzip2 body taken only where `bz2_pays` tells that its data may pay
/// (as aqr-bz2's `encode_if` asks).
pub fn encode_with(
    input: &[u8],
    opt: &Options,
    bz2_pays: &mut dyn FnMut(usize, &[u8], bool) -> bool,
) -> Option<Encoded> {
    let a = alz::parse(input)?;
    let body = |e: &Entry| &input[e.body.clone()];
    // the bodies that may be rebuilt, those decoding within the limit by their headers' sizes
    let mut left = opt.limit;
    let mut made: Vec<Option<Made>> = a.entries.iter().map(|_| None).collect();
    let mut deflated = Vec::new();
    let mut bzip2 = Vec::new();
    for (i, e) in a.entries.iter().enumerate() {
        if e.encrypted || e.body.is_empty() {
            continue;
        }
        match e.method {
            Some(STORED) if e.body.len() as u64 == e.size => made[i] = Some(Made::Stored),
            Some(m @ (DEFLATE | BZIP2)) if e.size <= left => {
                left -= e.size;
                if m == DEFLATE { deflated.push(i) } else { bzip2.push(i) }
            }
            _ => {}
        }
    }
    let fopt = aqr_flate::Options { zlib: opt.zlib, ..aqr_flate::Options::for_profile(opt.profile).ok()? };
    // (the large ones first, each on all threads, the small ones side by side on one each)
    let mut order = deflated.clone();
    order.sort_by_key(|&i| std::cmp::Reverse(a.entries[i].body.len()));
    let done = aqr_common::pool::par_map(order.len(), opt.threads, |k| {
        let e = &a.entries[order[k]];
        let threads = if e.body.len() >= LARGE { opt.threads } else { 1 };
        let max_data = usize::try_from(e.size).ok()?;
        let o = aqr_flate::Options { threads, max_data, ..fopt.clone() };
        let (data, record) = aqr_flate::deconstruct_with(body(e), &o).ok()?;
        // (a body kept costs nothing more than itself, its header kept anyway)
        (record.len() < e.body.len()).then_some(Made::Deflated(data, record))
    });
    for (i, m) in order.into_iter().zip(done) {
        made[i] = m;
    }
    for i in bzip2 {
        let e = &a.entries[i];
        let max_data = usize::try_from(e.size).ok()?;
        made[i] = aqr_bz2::formula::take_alz(body(e), opt.threads, max_data, bz2_pays).map(|(p, d)| Made::Bzip2(p, d));
    }
    if made.iter().all(Option::is_none) {
        return None;
    }

    let mut f = Formula::default();
    f.push("alz", Vec::new());
    // the contents of each method's bodies with their kinds and ends; the records and their
    // lengths; the bzip2 streams' params
    let mut stored = (Vec::new(), Vec::new());
    let mut flate = (Vec::new(), Vec::new());
    let mut bz2 = (Vec::new(), Vec::new());
    let (mut records, mut lens, mut params) = (Vec::new(), Vec::new(), Vec::new());
    let mut inflated = 0;
    for (e, m) in a.entries.iter().zip(&mut made) {
        let kind = (opt.kind)(&input[e.name.clone()]);
        let put = |(data, ends): &mut (Vec<u8>, Vec<(Kind, usize)>), d: &[u8]| {
            data.extend_from_slice(d);
            ends.push((kind, data.len()));
        };
        match m {
            None => {}
            Some(Made::Stored) => put(&mut stored, body(e)),
            Some(Made::Deflated(data, record)) => {
                inflated += data.len() as u64;
                lens.push((data.len(), record.len()));
                put(&mut flate, &std::mem::take(data));
                records.extend_from_slice(&std::mem::take(record));
            }
            Some(Made::Bzip2(p, data)) => {
                inflated += data.len() as u64;
                put(&mut bz2, &std::mem::take(data));
                params.push(std::mem::take(p));
            }
        }
    }
    let sizes = |method: fn(&Made) -> bool| -> Vec<u64> {
        a.entries
            .iter()
            .zip(&made)
            .filter(|(_, m)| m.as_ref().is_some_and(method))
            .map(|(e, _)| e.body.len() as u64)
            .collect()
    };
    let by_kind = |f: &mut Formula, (data, ends): (Vec<u8>, Vec<(Kind, usize)>)| {
        let raw = f.streams.len();
        f.push("raw", Vec::new());
        f.push_by_kind(raw, data, &ends, "alz", OWN_MIN)
    };
    // each method's nodes, one for each body in order
    let stored_sizes = sizes(|m| matches!(m, Made::Stored));
    let stored_nodes = match stored_sizes.is_empty() {
        true => Vec::new(),
        false => cut(by_kind(&mut f, stored), &stored_sizes),
    };
    let flate_sizes = sizes(|m| matches!(m, Made::Deflated(..)));
    let flate_nodes = match flate_sizes.is_empty() {
        true => Vec::new(),
        false => {
            let records = f.push_as("flate/record", Kind::Compressed, records);
            let index = match aqr_flate::formula::index(&lens) {
                i if i.is_empty() => Node::Concat(Vec::new()),
                i => f.push("flate/index", i),
            };
            let data = by_kind(&mut f, flate);
            let len = flate_sizes.iter().sum();
            let a = aqr_flate::formula::apply(opt.profile, data, records, index, len, Derived::None).ok()?;
            outputs(&mut f, a, flate_sizes)
        }
    };
    let bz2_sizes = sizes(|m| matches!(m, Made::Bzip2(..)));
    let bz2_nodes = match bz2_sizes.is_empty() {
        true => Vec::new(),
        false => {
            let data = by_kind(&mut f, bz2);
            let a = aqr_bz2::formula::apply_alz(&params, data, bz2_sizes.iter().sum());
            outputs(&mut f, a, bz2_sizes)
        }
    };
    let (mut stored_nodes, mut flate_nodes, mut bz2_nodes) =
        (stored_nodes.into_iter(), flate_nodes.into_iter(), bz2_nodes.into_iter());
    // the root: the bytes kept and the bodies rebuilt in turn
    let (mut container, mut root, mut at) = (Vec::new(), Vec::new(), 0);
    for (e, m) in a.entries.iter().zip(&made) {
        let node = match m {
            None => continue,
            Some(Made::Stored) => stored_nodes.next(),
            Some(Made::Deflated(..)) => flate_nodes.next(),
            Some(Made::Bzip2(..)) => bz2_nodes.next(),
        };
        root.push(Node::slice(0, container.len() as u64, (e.body.start - at) as u64));
        container.extend_from_slice(&input[at..e.body.start]);
        root.push(node.expect("a node for each body"));
        at = e.body.end;
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

/// The output of `a`, an application of aqr-flate's module or of aqr-bz2's `bz2-alz-recons`, on
/// `inputs`.
pub fn apply(a: &aqr_formula::Apply, inputs: &[&[u8]], threads: usize) -> Result<Vec<u8>, String> {
    let res: Vec<&[u8]> = a.resources.iter().map(|r| &r[..]).collect();
    match a.module {
        m if m == aqr_bz2::formula::MODULE_ALZ && res.is_empty() => {
            aqr_bz2::formula::decode_alz(&a.params, inputs, a.len, threads).map_err(|e| e.to_string())
        }
        m if m == aqr_flate::formula::MODULE => {
            aqr_flate::formula::decode(&a.params, &res, inputs, a.len, threads).map_err(|e| e.to_string())
        }
        m => Err(format!("module {m:?} not of aqr-alz")),
    }
}
