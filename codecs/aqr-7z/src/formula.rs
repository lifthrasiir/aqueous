//! aqr-7z's formulas (see aqr-formula): a 7z archive whose folders of LZMA or LZMA2 alone are
//! rebuilt from their data by aqr-lzma's modules (`lzma-recons`, `lzma2-recons`), and so is an
//! encoded header of LZMA; the rest of the archive is kept as it is. The data of a folder is the
//! contents of the files it holds, by their kinds by their names (as a stored folder's is), so
//! that whoever takes the formula may recompress them in turn and store them once where the
//! same files are archived along with it.

use std::ops::Range;

use aqr_formula::{Formula, Kind, Node};
use aqr_lzma::formula::{self as lz, LIMIT};
use aqr_lzma::{lzma, lzma2, recons};

use crate::header::{self, Folder, Top};

/// Files' contents of at least this many bytes are slices of their own, apart from the others of
/// their kind next to them: so that the same contents elsewhere are found by their slices.
pub const OWN_MIN: usize = 1 << 10;

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

/// What a folder's packed stream is rebuilt from.
enum Taken {
    /// LZMA: its data and record.
    Lzma { data: Vec<u8>, record: Vec<u8> },
    /// LZMA2: its data, and the lengths of the data and record of each segment, and the records.
    Lzma2 { data: Vec<u8>, lens: Vec<(usize, usize)>, records: Vec<u8> },
    /// Stored: the data is the packed stream.
    Copy,
}

/// The properties and dictionary size of LZMA's coder properties.
fn lzma_props(p: &[u8]) -> Option<(lzma::Props, u32)> {
    let &[b, ref dict @ ..] = p else { return None };
    Some((lzma::Props::from_byte(b)?, u32::from_le_bytes(dict.try_into().ok()?)))
}

/// Splits the packed stream of `f`, a folder of one coder of LZMA, LZMA2 or Copy (of at most
/// [`LIMIT`] bytes of data).
fn take(f: &Folder, packed: &[u8], threads: usize) -> Option<Taken> {
    let c = f.single()?;
    let size = f.size()?;
    if size > LIMIT as u64 {
        return None;
    }
    match &c.id[..] {
        header::COPY => (packed.len() as u64 == size).then_some(Taken::Copy),
        header::LZMA => {
            let (props, dict) = lzma_props(&c.props)?;
            let sp = recons::split(packed, props, dict, Some(size), LIMIT).ok()?;
            (sp.used == packed.len() && sp.data.len() as u64 == size)
                .then_some(Taken::Lzma { data: sp.data, record: sp.record })
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
            (used == packed.len() && data.len() as u64 == size).then_some(Taken::Lzma2 { data, lens, records })
        }
        _ => None,
    }
}

/// The data of `f`'s packed stream (a folder as [`take`] takes it), decoded alone.
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
/// encoded (a folder as [`take`] takes it, of the CRC-32 it is told); and the span and the folder
/// of the encoded header.
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
/// streams is taken (of LZMA, LZMA2 or Copy alone, of [`LIMIT`] bytes of data in all).
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
    let spans = s.pack_spans()?;
    let packs = s.folder_packs()?;
    let jobs: Vec<(usize, Range<usize>)> = (0..s.folders.len())
        .filter_map(|i| {
            let [p] = packs[i].clone().collect::<Vec<_>>()[..] else { return None };
            let r = usize_range(&spans[p])?;
            (r.end <= input.len() && s.folders[i].single().is_some()).then_some((i, r))
        })
        .collect();
    let inner = if jobs.len() == 1 { opt.threads } else { 1 };
    let taken = aqr_common::pool::par_map(jobs.len(), opt.threads, |j| {
        let (i, r) = &jobs[j];
        take(&s.folders[*i], &input[r.clone()], inner)
    });
    // the names of the files of each folder's substreams
    let names: Vec<&[u8]> = h.files.iter().filter(|f| f.stream).map(|f| &f.name[..]).collect();
    let firsts: Vec<usize> = s.subs.iter().scan(0, |a, x| Some(std::mem::replace(a, *a + x.len()))).collect();
    let mut total = 0usize;
    for ((i, r), t) in jobs.into_iter().zip(taken) {
        let Some(t) = t else { continue };
        let data = match t {
            Taken::Lzma { ref data, .. } | Taken::Lzma2 { ref data, .. } => data.len(),
            Taken::Copy => r.len(),
        };
        if total + data > LIMIT {
            continue;
        }
        total += data;
        let mut end = 0;
        let ends: Vec<(Kind, usize)> = s.subs[i]
            .iter()
            .enumerate()
            .map(|(k, &n)| {
                end += n as usize;
                (names.get(firsts[i] + k).map_or(Kind::Unknown, |n| (opt.kind)(n)), end)
            })
            .collect();
        let ends = if ends.is_empty() { vec![(Kind::Unknown, data)] } else { ends };
        let raw = f.streams.len();
        f.push("raw", Vec::new());
        let node = match t {
            Taken::Lzma { data, record } => {
                let data = f.push_by_kind(raw, data, &ends, "7z", opt.own);
                let inputs = vec![data, f.push("lzma/record", record), f.push("lzma/index", Vec::new())];
                Node::apply(lz::MODULE, Vec::new(), inputs, r.len() as u64)
            }
            Taken::Lzma2 { data, lens, records } => {
                let data = f.push_by_kind(raw, data, &ends, "7z", opt.own);
                let inputs = vec![data, f.push("lzma2/record", records), f.push("lzma2/index", lz::index(&lens))];
                Node::apply(lz::LZMA2, Vec::new(), inputs, r.len() as u64)
            }
            Taken::Copy => {
                let node = f.push_by_kind(raw, input[r.clone()].to_vec(), &ends, "7z", opt.own);
                // (the files' contents as they are, in no application)
                f.streams[raw..].iter_mut().for_each(|s| s.content = true);
                node
            }
        };
        pieces.push((r, node));
    }
    if pieces.is_empty() {
        return None;
    }
    // the encoded header, of LZMA or LZMA2 (its data a table, the header's)
    if let Some((span, folder)) = encoded {
        let r = usize_range(&span)?;
        match take(&folder, &input[r.clone()], 1) {
            Some(Taken::Lzma { data, record }) => {
                let inputs = vec![
                    f.push_as("7z/header", Kind::Table, data),
                    f.push("lzma/record", record),
                    f.push("lzma/index", Vec::new()),
                ];
                pieces.push((r.clone(), Node::apply(lz::MODULE, Vec::new(), inputs, r.len() as u64)));
            }
            Some(Taken::Lzma2 { data, lens, records }) => {
                let inputs = vec![
                    f.push_as("7z/header", Kind::Table, data),
                    f.push("lzma2/record", records),
                    f.push("lzma2/index", lz::index(&lens)),
                ];
                pieces.push((r.clone(), Node::apply(lz::LZMA2, Vec::new(), inputs, r.len() as u64)));
            }
            _ => {}
        }
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
