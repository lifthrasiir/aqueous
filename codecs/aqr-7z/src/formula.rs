//! aqr-7z's formulas (see aqr-formula): a 7z archive whose folders are rebuilt from their data:
//! their coders of LZMA or LZMA2 by aqr-lzma's modules (`lzma-recons`, `lzma2-recons`), the
//! filters over them (the branch converters, Delta, BCJ2) by aqr-bcj's, which apply them to the
//! data before them; an encoded header of LZMA likewise. The rest of the archive is kept as it
//! is. The data of a folder is the contents of the files it holds, by their kinds by their names
//! (as a stored folder's is), so that whoever takes the formula may recompress them in turn and
//! store them once where the same files are archived along with it.

use std::ops::Range;

use aqr_bcj::bcj2;
use aqr_bcj::formula::{self as bcj, Filter};
use aqr_formula::{Apply, Formula, Kind, Node};
use aqr_lzma::formula::{self as lz, LIMIT};
use aqr_lzma::{lzma, lzma2, recons};

use crate::header::{self, Coder, Folder, Streams, Top};

/// Files' contents of at least this many bytes are slices of their own, apart from the others of
/// their kind next to them: so that the same contents elsewhere are found by their slices.
pub const OWN_MIN: usize = 1 << 10;

/// The id of BCJ2's coder.
const BCJ2: &[u8] = &[3, 3, 1, 0x1b];

#[derive(Debug, Clone)]
pub struct Options {
    pub threads: usize,
    /// What a file's contents are by its name: the contents of each [`Kind`] are a stream of their
    /// own, sliced in order. By default all are unknown.
    pub kind: fn(&[u8]) -> Kind,
    /// Contents of at least this many bytes are slices of their own; by default [`OWN_MIN`].
    pub own: usize,
}

impl Default for Options {
    fn default() -> Options {
        Options { threads: 1, kind: |_| Kind::Unknown, own: OWN_MIN }
    }
}

/// How a coder's packed stream is rebuilt from its data.
enum Rebuild {
    /// LZMA: its record.
    Lzma(Vec<u8>),
    /// LZMA2: the lengths of the data and record of each segment, and the records.
    Lzma2(Vec<(usize, usize)>, Vec<u8>),
    /// Stored: the data is the packed stream.
    Copy,
}

/// The properties and dictionary size of LZMA's coder properties.
fn lzma_props(p: &[u8]) -> Option<(lzma::Props, u32)> {
    let &[b, ref dict @ ..] = p else { return None };
    Some((lzma::Props::from_byte(b)?, u32::from_le_bytes(dict.try_into().ok()?)))
}

/// Splits `packed`, the stream of coder `c` of LZMA, LZMA2 or Copy, of `size` bytes of data (at
/// most [`LIMIT`]): its data, and how it is rebuilt.
fn take(c: &Coder, size: u64, packed: &[u8], threads: usize) -> Option<(Vec<u8>, Rebuild)> {
    if c.ins != 1 || c.outs != 1 || size > LIMIT as u64 {
        return None;
    }
    match &c.id[..] {
        header::COPY => (packed.len() as u64 == size).then(|| (packed.to_vec(), Rebuild::Copy)),
        header::LZMA => {
            let (props, dict) = lzma_props(&c.props)?;
            let sp = recons::split(packed, props, dict, Some(size), LIMIT).ok()?;
            (sp.used == packed.len() && sp.data.len() as u64 == size).then_some((sp.data, Rebuild::Lzma(sp.record)))
        }
        header::LZMA2 => {
            let dict = lzma2::dict_size(*c.props.first()?)?;
            let sps = lzma2::split(packed, dict, LIMIT, threads).ok()?;
            let (mut data, mut records, mut lens) = (Vec::with_capacity(size as usize), Vec::new(), Vec::new());
            let mut used = 0;
            for x in sps {
                lens.push((x.data.len(), x.record.len()));
                data.extend_from_slice(&x.data);
                records.extend_from_slice(&x.record);
                used += x.used;
            }
            (used == packed.len() && data.len() as u64 == size).then_some((data, Rebuild::Lzma2(lens, records)))
        }
        _ => None,
    }
}

/// The node of a packed stream rebuilt from its data `data` (of `len` bytes packed).
fn rebuild(f: &mut Formula, how: Rebuild, data: Node, len: u64) -> Node {
    match how {
        Rebuild::Lzma(record) => {
            let inputs = vec![data, f.push("lzma/record", record), f.push("lzma/index", Vec::new())];
            Node::apply(lz::MODULE, Vec::new(), inputs, len)
        }
        Rebuild::Lzma2(lens, records) => {
            let inputs = vec![data, f.push("lzma2/record", records), f.push("lzma2/index", lz::index(&lens))];
            Node::apply(lz::LZMA2, Vec::new(), inputs, len)
        }
        Rebuild::Copy => data,
    }
}

/// The data of `f`'s packed stream (a folder of one coder of LZMA, LZMA2 or Copy), decoded alone.
fn unpack(f: &Folder, packed: &[u8]) -> Option<Vec<u8>> {
    let c = f.single()?;
    let size = f.size()?;
    if size > LIMIT as u64 {
        return None;
    }
    let data = match &c.id[..] {
        header::COPY => packed.to_vec(),
        header::LZMA => {
            let (props, _) = lzma_props(&c.props)?;
            lzma::decode(packed, props, Some(size), LIMIT).ok()?.data
        }
        header::LZMA2 => lzma2::decode(packed, LIMIT).ok()?.segments.into_iter().flat_map(|s| s.data).collect(),
        _ => return None,
    };
    (data.len() as u64 == size).then_some(data)
}

/// A coder of LZMA, LZMA2 or Copy reading a packed stream: the coder, the size of its output, and
/// the folder's packed stream it reads.
type Part = (usize, u64, usize);

/// The shapes of folders that are rebuilt.
enum Shape {
    /// Filters (the outermost first, as encoding applies them) over a coder.
    Chain(Vec<Filter>, Part),
    /// BCJ2 over coders of its main, call and jump streams; the packed stream of its range coder's.
    Bcj2([Part; 3], usize),
}

/// The part reading what input `i` of `f` takes.
fn part(f: &Folder, i: u64) -> Option<Part> {
    let o = f.source(i)?.ok()?;
    let c = f.coder_of_out(o)?;
    let coder = &f.coders[c];
    let (base, _) = f.bases(c);
    let compressor = [header::COPY, header::LZMA, header::LZMA2].contains(&&coder.id[..]);
    let pack = f.source(base)?.err()?;
    (compressor && coder.ins == 1 && coder.outs == 1).then_some((c, *f.sizes.get(o as usize)?, pack))
}

fn shape(f: &Folder) -> Option<Shape> {
    let mut o = f.output()?;
    let mut filters = Vec::new();
    loop {
        let c = f.coder_of_out(o)?;
        let coder = &f.coders[c];
        let (base, _) = f.bases(c);
        if coder.id == BCJ2 {
            if coder.ins != 4 || coder.outs != 1 || !filters.is_empty() {
                return None;
            }
            let parts = [part(f, base)?, part(f, base + 1)?, part(f, base + 2)?];
            return Some(Shape::Bcj2(parts, f.source(base + 3)?.err()?));
        }
        if coder.ins != 1 || coder.outs != 1 || f.sizes[o as usize] != f.size()? {
            return None;
        }
        match f.source(base)? {
            Err(pack) if [header::COPY, header::LZMA, header::LZMA2].contains(&&coder.id[..]) => {
                return Some(Shape::Chain(filters, (c, f.sizes[o as usize], pack)));
            }
            Ok(next) => {
                filters.push(Filter::from_7z(&coder.id, &coder.props)?);
                o = next;
            }
            Err(_) => return None,
        }
    }
}

/// A packed stream rebuilt: its index among the archive's, the bytes of its data, and how.
struct Packed {
    pack: usize,
    data_len: usize,
    how: Rebuild,
}

/// A folder made ready for its formula.
enum Made {
    /// The files' data, filtered for a packed stream.
    Chain { data: Vec<u8>, filters: Vec<Filter>, packed: Packed },
    /// The files' data, BCJ2's three streams' packed streams of it (with the range coder's), and
    /// that of the range coder.
    Bcj2 { data: Vec<u8>, parts: [Packed; 3], rc: usize },
    /// Packed streams rebuilt from data of their own, not the files' (filters that do not
    /// give back what they filtered).
    Apart(Vec<(Vec<u8>, Packed)>),
}

impl Made {
    fn data_len(&self) -> usize {
        match self {
            Made::Chain { data, .. } | Made::Bcj2 { data, .. } => data.len(),
            Made::Apart(ps) => ps.iter().map(|p| p.0.len()).sum(),
        }
    }
}

/// Makes folder `i` of `s` ready, its packed streams of `packs` (by the archive's indices,
/// `input`'s bytes).
fn make(s: &Streams, i: usize, packs: &[Range<usize>], input: &[u8], threads: usize) -> Option<Made> {
    let f = &s.folders[i];
    let first = s.folder_packs()?[i].start;
    let taken = |(c, size, pack): Part| {
        let (data, how) = take(&f.coders[c], size, &input[packs[first + pack].clone()], threads)?;
        Some((data, Packed { pack: first + pack, data_len: size as usize, how }))
    };
    match shape(f)? {
        Shape::Chain(filters, p) => {
            let (x, packed) = taken(p)?;
            if filters.is_empty() {
                return Some(Made::Chain { data: x, filters, packed });
            }
            match bcj::unfilter(&filters, &x) {
                Some(data) => Some(Made::Chain { data, filters, packed }),
                None => Some(Made::Apart(vec![(x, packed)])),
            }
        }
        Shape::Bcj2(parts, rc) => {
            let [m, c, j] = parts.map(taken);
            let (m, c, j) = (m?, c?, j?);
            let rc = first + rc;
            let streams = bcj2::Streams { main: m.0, call: c.0, jump: j.0 };
            let size = f.size()? as usize;
            let data = bcj2::decode(&streams, &input[packs[rc].clone()], size)
                .ok()
                .filter(|d| bcj2::encode(d, &input[packs[rc].clone()]).ok().as_ref() == Some(&streams));
            match data {
                Some(data) => Some(Made::Bcj2 { data, parts: [m.1, c.1, j.1], rc }),
                None => Some(Made::Apart(vec![(streams.main, m.1), (streams.call, c.1), (streams.jump, j.1)])),
            }
        }
    }
}

fn usize_range(r: &Range<u64>) -> Option<Range<usize>> {
    Some(usize::try_from(r.start).ok()?..usize::try_from(r.end).ok()?)
}

/// Whether `d` starts with a 7z archive's signature header.
pub fn starts(d: &[u8]) -> bool {
    header::signature(d).is_some()
}

/// The bytes of the 7z archive at the start of `d`: up to the end of its header.
pub fn extent(d: &[u8]) -> Option<usize> {
    header::signature(d).and_then(|(h, _)| usize::try_from(h.end).ok())
}

/// The span of an encoded header's packed stream, and its folder.
type Encoded = (Range<u64>, Folder);

/// The header of the archive whose bytes `read` gives by their span, decoded where it is
/// encoded (a folder of one coder of LZMA, LZMA2 or Copy, of the CRC-32 it is told); and the
/// span and the folder of the encoded header.
fn header_of(read: &mut dyn FnMut(Range<u64>) -> Option<Vec<u8>>) -> Option<(header::Header, Option<Encoded>)> {
    let (span, crc) = header::signature(&read(0..header::SIGNATURE as u64)?)?;
    let bytes = read(span)?;
    if aqr_common::crc32::hash(&bytes) != crc {
        return None;
    }
    match header::parse(&bytes)? {
        Top::Plain(h) => Some((h, None)),
        Top::Encoded(s) => {
            let [folder] = &s.folders[..] else { return None };
            let [packs] = &s.pack_spans()?[..] else { return None };
            let data = unpack(folder, &read(packs.clone())?)?;
            if folder.crc.is_some_and(|c| c != aqr_common::crc32::hash(&data)) {
                return None;
            }
            match header::parse(&data)? {
                Top::Plain(h) => Some((h, Some((packs.clone(), folder.clone())))),
                Top::Encoded(_) => None,
            }
        }
    }
}

/// The CRC-32 and size of the files' contents in the 7z archive whose bytes `read` gives by
/// their span (the signature header, the header, and the packed stream of an encoded header),
/// of those whose CRC-32 it tells.
pub fn entry_sums(read: &mut dyn FnMut(Range<u64>) -> Option<Vec<u8>>) -> Vec<(u32, u64)> {
    let Some((h, _)) = header_of(read) else { return Vec::new() };
    let sizes = h.main.subs.iter().flatten();
    sizes.zip(&h.main.sub_crcs).filter_map(|(&n, c)| Some(((*c)?, n))).collect()
}

/// The formula rebuilding archive `input`. None where it is not one, or no folder of its main
/// streams is rebuilt (of [`LIMIT`] bytes of data in all).
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    encode_with(input, &Options { threads, ..Options::default() })
}

/// [`encode`] with `opt`.
pub fn encode_with(input: &[u8], opt: &Options) -> Option<Formula> {
    let mut read = |r: Range<u64>| input.get(usize_range(&r)?).map(<[u8]>::to_vec);
    let (h, encoded) = header_of(&mut read)?;
    let mut f = Formula::default();
    f.push("7z", Vec::new());
    // the packed streams rebuilt, by their spans
    let mut pieces: Vec<(Range<usize>, Node)> = Vec::new();
    let s = &h.main;
    let packs: Vec<Range<usize>> = s.pack_spans()?.iter().map(usize_range).collect::<Option<_>>()?;
    if packs.last().is_some_and(|p| p.end > input.len()) {
        return None;
    }
    let inner = if s.folders.len() == 1 { opt.threads } else { 1 };
    let made = aqr_common::pool::par_map(s.folders.len(), opt.threads, |i| make(s, i, &packs, input, inner));
    // the names of the files of each folder's substreams
    let names: Vec<&[u8]> = h.files.iter().filter(|f| f.stream).map(|f| &f.name[..]).collect();
    let firsts: Vec<usize> = s.subs.iter().scan(0, |a, x| Some(std::mem::replace(a, *a + x.len()))).collect();
    let mut total = 0usize;
    for (i, m) in made.into_iter().enumerate() {
        let Some(m) = m else { continue };
        if total + m.data_len() > LIMIT {
            continue;
        }
        total += m.data_len();
        // the files' data cut by their kinds
        let files = |f: &mut Formula, data: Vec<u8>| {
            let mut end = 0;
            let mut ends: Vec<(Kind, usize)> = s.subs[i]
                .iter()
                .enumerate()
                .map(|(k, &n)| {
                    end += n as usize;
                    (names.get(firsts[i] + k).map_or(Kind::Unknown, |n| (opt.kind)(n)), end)
                })
                .collect();
            if ends.is_empty() {
                ends.push((Kind::Unknown, data.len()));
            }
            let raw = f.streams.len();
            f.push("raw", Vec::new());
            (raw, f.push_by_kind(raw, data, &ends, "7z", opt.own))
        };
        match m {
            Made::Chain { data, filters, packed } => {
                let len = data.len() as u64;
                let copy = filters.is_empty() && matches!(packed.how, Rebuild::Copy);
                let (raw, node) = files(&mut f, data);
                if copy {
                    // (the files' contents as they are, in no application)
                    f.streams[raw..].iter_mut().for_each(|s| s.content = true);
                }
                let node =
                    rebuild(&mut f, packed.how, bcj::apply(&filters, node, len), packs[packed.pack].len() as u64);
                pieces.push((packs[packed.pack].clone(), node));
            }
            Made::Bcj2 { data, parts, rc } => {
                let len = data.len() as u64;
                let (_, node) = files(&mut f, data);
                let rc_node = f.push_as("bcj2/rc", Kind::Compressed, input[packs[rc].clone()].to_vec());
                let a = Apply {
                    module: bcj::BCJ2,
                    resources: vec![],
                    params: vec![],
                    inputs: vec![node, rc_node.clone()],
                    len,
                    memory_limit: aqr_formula::NO_LIMIT,
                };
                let outputs = f.split(a, parts.iter().map(|p| p.data_len as u64).collect());
                for (p, o) in parts.into_iter().zip(outputs) {
                    let node = rebuild(&mut f, p.how, o, packs[p.pack].len() as u64);
                    pieces.push((packs[p.pack].clone(), node));
                }
                pieces.push((packs[rc].clone(), rc_node));
            }
            Made::Apart(ps) => {
                for (data, p) in ps {
                    // (a stored one is kept as it is)
                    if matches!(p.how, Rebuild::Copy) {
                        continue;
                    }
                    let data = f.push("raw", data);
                    pieces.push((packs[p.pack].clone(), rebuild(&mut f, p.how, data, packs[p.pack].len() as u64)));
                }
            }
        }
    }
    if pieces.is_empty() {
        return None;
    }
    // the encoded header, of LZMA or LZMA2 (its data a table, the header's)
    if let Some((span, folder)) = encoded
        && let Some(c) = folder.single()
        && let Some(r) = usize_range(&span)
        && let Some((data, how)) = take(c, folder.size()?, &input[r.clone()], 1)
        && !matches!(how, Rebuild::Copy)
    {
        let data = f.push_as("7z/header", Kind::Table, data);
        pieces.push((r.clone(), rebuild(&mut f, how, data, r.len() as u64)));
    }
    pieces.sort_by_key(|p| p.0.start);
    let mut container = Vec::new();
    let mut nodes = Vec::with_capacity(2 * pieces.len() + 1);
    let mut at = 0;
    for (r, node) in pieces {
        if r.start < at {
            return None;
        }
        nodes.push(Node::slice(0, container.len() as u64, (r.start - at) as u64));
        container.extend_from_slice(&input[at..r.start]);
        nodes.push(node);
        at = r.end;
    }
    nodes.push(Node::slice(0, container.len() as u64, (input.len() - at) as u64));
    container.extend_from_slice(&input[at..]);
    f.streams[0].data = container.into();
    f.root = Node::concat(nodes);
    Some(f)
}
