//! aqr-ogg as a module of entry formulas (see aqr-formula): an Ogg file rebuilt from the layout
//! record of its pages and its streams' packets (FORMAT.md). A Vorbis stream's audio packets
//! are rebuilt in turn by aqr-vorbis's module, from the stream's setup header and the record of
//! their values.

use std::collections::HashMap;

use aqr_formula::{Formula, Kind, Module, Node, Usage};
use aqr_vorbis::setup::{Ident, Setup};

use crate::layout::{self, CHUNK, Layout, Packets, Predictor, Stream, Walk};
use crate::page::{self, BOS, Page};

pub use crate::module::decode;

pub const MODULE: Module = Module::new("ogg-recons", 1).with_memory(memory);

/// The classes of the layout record, of the packets of streams no recompressor takes, and of
/// the header packets of Vorbis streams before their setup headers.
pub const LAYOUT: &str = "ogg/layout";
pub const PACKETS: &str = "ogg/packets";
pub const HEADERS: &str = "ogg/headers";

/// The memory of the module (wasm32 sizes): the inputs and the file; each stream's packets
/// copied, their ends (as they grow, twice their count), lacing values and durations; the
/// layout's pages (24 bytes each) and their streams.
fn memory(u: &Usage<'_>) -> Option<u64> {
    let [layout, packets] = u.all_contents::<2>()?;
    let l = Layout::read(layout, u.output()).ok()?;
    let (mut n, mut at) = (0u64, 0);
    while let Some(len) = packets.get(at..at + 2) {
        let len = u16::from_le_bytes([len[0], len[1]]) as usize;
        n += (len < CHUNK) as u64;
        at += 2 + len;
    }
    let bytes = packets.len() as u64;
    let pages = l.pages.len() as u64;
    let extra = bytes + 8 * n + (bytes / 255 + n) + 8 * n + 28 * pages + 64 * l.streams.len() as u64;
    Some(u.whole(extra, 4 * l.streams.len() as u64 + 2))
}

/// Whether `d` starts with an Ogg page.
pub fn starts(d: &[u8]) -> bool {
    Page::at(d, 0).is_some()
}

/// The bytes of the pages one after another at the start of `d`.
pub fn extent(d: &[u8]) -> Option<usize> {
    page::pages(d).last().map(Page::end)
}

/// The predictor of a stream of `p`: Vorbis where its first packet is an identification header
/// and its third a setup header that aqr-vorbis parses, Opus where its first is `OpusHead`.
fn predictor(p: &Packets) -> (Predictor, Option<Setup>) {
    if p.is_empty() {
        return (Predictor::None, None);
    }
    if let Some(ident) = vorbis_ident(p.get(0))
        && p.len() >= 3
        && let Ok(s) = Setup::parse(p.get(2), ident)
    {
        return (Predictor::Vorbis(s.modes.iter().map(|m| m.blockflag).collect()), Some(s));
    }
    if p.get(0).starts_with(b"OpusHead") {
        return (Predictor::Opus, None);
    }
    (Predictor::None, None)
}

/// The channels and block sizes of a Vorbis identification header.
fn vorbis_ident(id: &[u8]) -> Option<Ident> {
    if id.len() < 30 || &id[..7] != b"\x01vorbis" || id[11] == 0 {
        return None;
    }
    let (short, long) = (id[28] & 15, id[28] >> 4);
    if !(6..=13).contains(&short) || !(6..=13).contains(&long) || short > long {
        return None;
    }
    Some(Ident { channels: id[11] as usize, blocksizes: [1 << short, 1 << long] })
}

/// Audio packets of a Vorbis stream are given to aqr-vorbis in pieces, each coded apart, on
/// threads and of tables of its own: a piece starts at the first packet at or past each multiple
/// of so many bytes of the packets framed (as aqr-lzma's parts do), so that the pieces are the
/// same whatever the threads, and their grid divides the archive's blocks (16 MiB shared, 128
/// MiB of a stream). aqr-vorbis's encoder takes about 35 bytes a byte; of the 12 Vorbis files of
/// FORMAT.md §6, the records grow by 0.15% of the files in pieces of 1 MiB, 0.05% of 2 MiB and
/// 0.02% of 4 MiB over the streams whole.
const PIECE: usize = 2 << 20;

/// A Vorbis stream's audio packets framed, and the pieces of them for aqr-vorbis: the packets
/// up to one of a chunk or more (framed as aqr-vorbis's are not), in pieces of [`PIECE`].
struct Audio {
    framed: Vec<u8>,
    pieces: Vec<std::ops::Range<usize>>,
}

impl Audio {
    /// The audio of stream `p`, or None where its setup header is a chunk or more.
    fn of(p: &Packets) -> Option<Audio> {
        if p.len() < 3 || p.get(2).len() >= CHUNK {
            return None;
        }
        let mut framed = Vec::with_capacity(p.bytes.len() - p.ends[2] + 2 * p.len());
        let (mut pieces, mut start, mut end) = (Vec::new(), 0, None);
        for i in 3..p.len() {
            let q = p.get(i);
            if q.len() >= CHUNK && end.is_none() {
                end = Some(framed.len());
            }
            layout::frame(&mut framed, q);
            if end.is_none() && framed.len() / PIECE > start / PIECE {
                pieces.push(start..framed.len());
                start = framed.len();
            }
        }
        let end = end.unwrap_or(framed.len());
        if start < end {
            pieces.push(start..end);
        }
        Some(Audio { framed, pieces })
    }
}

/// The node of a Vorbis stream's packets framed: its first two headers and the setup header's
/// length, the setup header (a stream aqr-vorbis's nodes take too), and the audio packets of
/// each piece as far as aqr-vorbis takes them (`records`: the bytes each takes and its record),
/// the rest as it is.
fn vorbis(f: &mut Formula, s: &Setup, p: &Packets, audio: &Audio, records: Vec<Option<(usize, Vec<u8>)>>) -> Node {
    let setup = p.get(2);
    let mut head = Vec::new();
    layout::frame(&mut head, p.get(0));
    layout::frame(&mut head, p.get(1));
    head.extend_from_slice(&(setup.len() as u16).to_le_bytes());
    let node = f.push("vorbis/setup", setup.to_vec());
    let mut parts = vec![f.push(HEADERS, head), node.clone()];
    let raw = |f: &mut Formula, d: &[u8]| f.push_as(PACKETS, Kind::Compressed, d.to_vec());
    let mut at = 0;
    for (r, record) in audio.pieces.iter().zip(records) {
        if let Some((used, record)) = record {
            let inputs = vec![node.clone(), f.push("vorbis/record", record)];
            let params = aqr_vorbis::module::params(s.ident);
            parts.push(Node::apply(aqr_vorbis::formula::MODULE, params, inputs, used as u64));
            at = r.start + used;
        }
        if at < r.end {
            parts.push(raw(f, &audio.framed[at..r.end]));
        }
        at = r.end;
    }
    if at < audio.framed.len() {
        parts.push(raw(f, &audio.framed[at..]));
    }
    Node::concat(parts)
}

/// The formula of the Ogg file at the start of `input`, or None where it does not start with
/// a page or none of its streams' packets are recompressed (aqr-vorbis takes none): the layout
/// of the others' pages is no smaller than the pages' headers once compressed, and their
/// packets framed are larger than as laced. What follows its last page is kept as it is. The
/// same whatever `threads` is.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    encode_any(input, threads).filter(|(_, taken)| *taken).map(|(f, _)| f)
}

/// [`encode`] of any Ogg file, and whether any of its streams' packets are recompressed.
pub fn encode_any(input: &[u8], threads: usize) -> Option<(Formula, bool)> {
    let pages = page::pages(input);
    let end = pages.last()?.end();
    // the streams: a page of the first page's flag, or of a serial number of none before, starts
    // one; the others are of the last stream of their serial number
    let mut of = Vec::with_capacity(pages.len());
    let (mut serials, mut lacing, mut bodies) = (Vec::new(), Vec::<Vec<u8>>::new(), Vec::<Vec<u8>>::new());
    let mut last = HashMap::new();
    for p in &pages {
        let s = match last.get(&p.serial) {
            Some(&s) if p.kind & BOS == 0 => s,
            _ => {
                serials.push(p.serial);
                lacing.push(Vec::new());
                bodies.push(Vec::new());
                last.insert(p.serial, serials.len() - 1);
                serials.len() - 1
            }
        };
        of.push(s);
        lacing[s].extend_from_slice(&input[p.lacing.clone()]);
        bodies[s].extend_from_slice(&input[p.body.clone()]);
    }
    let mut packets = Vec::with_capacity(serials.len());
    let mut opens = Vec::with_capacity(serials.len());
    for (values, body) in lacing.iter().zip(bodies) {
        let mut p = Packets { bytes: body, ends: Vec::new() };
        let mut at = 0;
        for &v in values {
            at += v as usize;
            if v < 255 {
                p.ends.push(at);
            }
        }
        let open = values.last() == Some(&255);
        if open {
            p.ends.push(at);
        }
        packets.push(p);
        opens.push(open);
    }
    let (predictors, setups): (Vec<_>, Vec<_>) = packets.iter().map(predictor).unzip();
    let durations =
        predictors.iter().zip(&packets).map(|(pr, p)| pr.durations(p)).collect::<Result<Vec<_>, _>>().ok()?;
    debug_assert!(packets.iter().zip(&opens).zip(&lacing).all(|((p, &o), l)| p.lacing(o).ok().as_ref() == Some(l)));

    let mut layout = Layout::default();
    let mut w = Walk::new(&packets, &lacing, &durations, &of);
    for (p, &s) in pages.iter().zip(&of) {
        let step = w.page(s, p.lacing.len()).ok()?;
        w.settle(s, &step, p.seq, p.granule);
        layout.pages.push(layout::Page {
            stream: s,
            segments: p.lacing.len() as u8,
            kind: p.kind ^ step.kind,
            seq: p.seq.wrapping_sub(step.seq),
            granule: p.granule.wrapping_sub(step.granule),
        });
    }
    // the Vorbis streams' audio, and the records of all their pieces, made apart
    let audio: Vec<Option<Audio>> =
        packets.iter().zip(&setups).map(|(p, s)| s.as_ref().and_then(|_| Audio::of(p))).collect();
    let pieces: Vec<(usize, std::ops::Range<usize>)> = audio
        .iter()
        .enumerate()
        .flat_map(|(i, a)| a.iter().flat_map(move |a| a.pieces.iter().map(move |r| (i, r.clone()))))
        .collect();
    let records = aqr_common::pool::par_map(pieces.len(), threads, |k| {
        let (i, r) = &pieces[k];
        let s = setups[*i].as_ref().unwrap();
        aqr_vorbis::codec::encode(s, &audio[*i].as_ref().unwrap().framed[r.clone()]).filter(|r| r.0 > 0)
    });
    let mut records = records.into_iter();
    let mut f = Formula::default();
    let mut taken = false;
    let mut nodes = Vec::with_capacity(packets.len());
    for (i, p) in packets.iter().enumerate() {
        let node = match (&setups[i], &audio[i]) {
            (Some(s), Some(a)) => {
                let records: Vec<_> = records.by_ref().take(a.pieces.len()).collect();
                taken |= records.iter().any(Option::is_some);
                vorbis(&mut f, s, p, a, records)
            }
            _ => {
                let kind = if predictors[i] == Predictor::None { Kind::Unknown } else { Kind::Compressed };
                f.push_as(PACKETS, kind, p.framed())
            }
        };
        let predictor = predictors[i].clone();
        layout.streams.push(Stream { serial: serials[i], bytes: node.len(), open: opens[i], predictor });
        nodes.push(node);
    }
    let inputs = vec![f.push(LAYOUT, layout.write()), Node::concat(nodes)];
    let ogg = Node::apply(MODULE, Vec::new(), inputs, end as u64);
    f.root = if end < input.len() { Node::concat([ogg, f.push("ogg/tail", input[end..].to_vec())]) } else { ogg };
    Some((f, taken))
}
