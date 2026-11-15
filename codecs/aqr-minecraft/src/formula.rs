//! aqr-minecraft as a module of entry formulas (see aqr-formula): a region file made again of its
//! header, its layout, its chunks' data and the bytes kept ([`MODULE`]), each chunk's zlib stream
//! made of its data by aqr-flate's emulation of zlib. `FORMAT.md` specifies it.

use aqr_flate::zlib::{Params, Strategy};
use aqr_formula::{Formula, Kind, Module, Node};

use aqr_common::arith::ArithEncoder;

use crate::layout::{self, CHUNK_HEADER, Chunks, How, Writer};
use crate::model::{self, Coder, Model, Work};
use crate::nbt::{self, BLOCKS, There};
use crate::region::{self, Chunk, HEADER, NONE, ZLIB};
use crate::{Result, corrupt};

pub use crate::module::decode;

/// A region file made of its chunks' data ([`decode`]), a part module.
pub const MODULE: Module = Module::new("mca-recons", 2).with_plan(memory);

/// The memory of the module: its plan ([`crate::module::plan`]), of its header and layout.
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    crate::module::plan(u.initial(), u.params, u.content(0)?, u.content(1)?, u.output())
}

aqr_common::zopt_namespace!("mc", "Minecraft region files (aqr-minecraft)");

aqr_common::zopt! {
    /// The base of [`limit`] (by default that of all formats, `limit-base`).
    pub static LIMIT_BASE: Opt<usize> = Opt::size("mc:limit-base", 0, "bytes the chunks may inflate to, besides limit-ratio per byte")
        .derived(|| aqr_common::zopt::LIMIT_BASE.get(), "limit-base");
    /// The ratio of [`limit`]: 64 (the End's regions inflate to 18 times theirs).
    pub static LIMIT_RATIO: Opt<usize> = Opt::value("mc:limit-ratio", 64, "bytes per byte of the file the chunks may inflate to");
    /// Chunks whose streams the zlib parameters are found of, the first.
    static SAMPLES: Opt<usize> = Opt::value("mc:samples", 4, "chunks whose streams the zlib parameters are found of");
}

/// The most bytes that the chunks of a file of `n` bytes may inflate to, all together: 64 MiB,
/// and 64 times the file by default (the End's regions inflate to 18 times theirs).
pub fn limit(n: usize) -> u64 {
    aqr_common::zopt::limit_of(n, &LIMIT_BASE, &LIMIT_RATIO) as u64
}

/// What became of a chunk: its data and how it is made, of its stream (the bytes after the
/// compression type) where it is rebuilt.
struct Done {
    data: Vec<u8>,
    how: How,
    /// Of an NBT with holes.
    nbt: Option<Nbt>,
}

/// An NBT's holes: whether each candidate is one, their payloads' offsets and lengths, and the
/// skeleton.
struct Nbt {
    holes: Vec<bool>,
    ranges: Vec<(u32, u32)>,
    skeleton: Vec<u8>,
}

/// The holes of the NBT `d` (whether each candidate is one) and its skeleton, or None where it is
/// not an NBT (of [`nbt::parse`]) or has none.
fn holes(d: &mut [u8], secs: &mut Vec<nbt::Section>, frames: &mut Vec<nbt::Frame>) -> Option<Nbt> {
    let mut found = Vec::new();
    nbt::parse(d, &mut There, secs, frames, &mut |c| {
        found.push(c);
        Ok(false)
    })
    .ok()?;
    let holes: Vec<bool> = found
        .iter()
        .map(|c| match c.kind {
            BLOCKS => model::blocks_hole(d, &secs[c.section as usize]),
            _ => !secs[c.section as usize].twice,
        })
        .collect();
    if !holes.contains(&true) {
        return None;
    }
    let ranges: Vec<(u32, u32)> = found.iter().zip(&holes).filter(|(_, h)| **h).map(|(c, _)| (c.at, c.len)).collect();
    let mut skeleton = Vec::with_capacity(d.len());
    let mut at = 0;
    for &(a, len) in &ranges {
        skeleton.extend_from_slice(&d[at..a as usize]);
        at = (a + len) as usize;
    }
    skeleton.extend_from_slice(&d[at..]);
    Some(Nbt { holes, ranges, skeleton })
}

/// The holes of the chunks of a file coded, in order, by one model: each chunk's bytes, or None
/// where the model does not make its data again.
fn code(done: &[Done]) -> Option<Vec<Option<Vec<u8>>>> {
    let (mut m, mut w) = (Model::new(), Work::new());
    let (mut secs, mut frames) = (Vec::with_capacity(nbt::SECTIONS), nbt::frames());
    let mut out = Vec::with_capacity(done.len());
    for d in done {
        let Some(n) = &d.nbt else {
            out.push(None);
            continue;
        };
        // the data with its holes zero, filled as they are coded
        let mut z = d.data.clone();
        for &(at, len) in &n.ranges {
            z[at as usize..(at + len) as usize].fill(0);
        }
        let mut c = ArithEncoder::new(Vec::new());
        let mut j = 0;
        nbt::parse(&mut z, &mut There, &mut secs, &mut frames, &mut |cand| {
            let h = n.holes[j];
            j += 1;
            Ok(c.bit(m.flag(cand.kind), h))
        })
        .ok()?;
        model::chunk(&mut c, &mut m, &mut w, &mut z, &secs, Some(&d.data)).ok()?;
        if z != d.data {
            debug_assert!(false, "holes not made again");
            return None;
        }
        out.push(Some(c.finish()));
    }
    Some(out)
}

/// The data of the zlib stream `z`, which must end where `z` does, of at most `max` bytes.
fn inflate(z: &[u8], max: u64) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut out = Vec::new();
    let mut d = flate2::read::ZlibDecoder::new(z).take(max + 1);
    d.read_to_end(&mut out).ok()?;
    let used = d.into_inner().total_in() as usize;
    (used == z.len() && out.len() as u64 <= max).then_some(out)
}

/// The zlib parameters that zlib's header `h` may be of, the likeliest first: the levels of its
/// flags, of memory levels 8 (the default) and 9, and of the default strategy and the filtered.
fn candidates(h: [u8; 2]) -> Vec<Params> {
    let wbits = (h[0] >> 4) + 8;
    if h[0] & 15 != 8 || !(9..=15).contains(&wbits) {
        return Vec::new();
    }
    let levels: &[u8] = match h[1] >> 6 {
        0 => &[1],
        1 => &[5, 4, 3, 2],
        2 => &[6],
        _ => &[9, 7, 8],
    };
    let mut v = Vec::new();
    for strategy in [Strategy::Default, Strategy::Filtered] {
        for mem in [8, 9] {
            for &level in levels {
                v.push(Params { level, strategy, wbits, mem });
            }
        }
    }
    v
}

/// The zlib stream of a chunk of compression type zlib, after its type.
fn zlib_stream<'a>(d: &'a [u8], c: &Chunk) -> Option<&'a [u8]> {
    let at = c.range().start;
    let len = u32::from_be_bytes(d[at..at + 4].try_into().unwrap()) as usize;
    (d[at + 4] == ZLIB && len > 1).then(|| &d[at + CHUNK_HEADER..at + 4 + len])
}

/// The parameters that make the most streams of `samples` (of their data) again, at least one:
/// of the candidates of their headers, in order, the first that makes all of them.
fn find_params(samples: &[(&[u8], Vec<u8>)]) -> Option<Params> {
    let mut tried = Vec::new();
    let mut best = (0, None);
    for (z, _) in samples {
        for p in candidates(z.get(..2)?.try_into().ok()?) {
            if tried.contains(&p) {
                continue;
            }
            tried.push(p);
            let n = samples.iter().filter(|(z, d)| layout::zlib_onto(d, p, Vec::new()).is_ok_and(|m| m == *z)).count();
            if n > best.0 {
                best = (n, Some(p));
            }
            if n == samples.len() {
                return best.1;
            }
        }
    }
    best.1
}

/// The formula making the region file `input` again, or None where it is not one, or none of
/// its chunks is made of its data. Its chunks' zlib streams are inflated and made again on up to
/// `threads` threads: those that zlib's emulation makes again, of the parameters found for the
/// first, are rebuilt; the rest are kept as they are.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    let chunks = region::parse(input)?;
    let budget = limit(input.len());
    // the parameters, of the first streams
    let samples: Vec<(&[u8], Vec<u8>)> = chunks
        .iter()
        .filter_map(|c| zlib_stream(input, c))
        .take(SAMPLES.get())
        .filter_map(|z| Some((z, inflate(z, budget)?)))
        .collect();
    let params = find_params(&samples);
    let total = std::sync::atomic::AtomicU64::new(0);
    let done: Vec<Done> = aqr_common::pool::par_map(chunks.len(), threads, |i| {
        let mut d = made(input, &chunks[i], params, budget, &total);
        if d.how != How::Kept {
            let (mut secs, mut frames) = (Vec::new(), nbt::frames());
            d.nbt = holes(&mut d.data, &mut secs, &mut frames);
        }
        d
    });
    if done.iter().all(|d| d.how == How::Kept) {
        return None;
    }
    let coded = code(&done)?;
    let mut w = Writer::new(input);
    let mut data = Vec::with_capacity(done.iter().map(|d| d.data.len()).sum());
    let mut holes = Vec::new();
    for ((c, d), z) in chunks.iter().zip(&done).zip(&coded) {
        w.chunk(c, d.how, z.as_ref().map(|z| z.len() as u64));
        match (&d.nbt, z) {
            (Some(n), Some(z)) => {
                data.extend_from_slice(&n.skeleton);
                holes.extend_from_slice(z);
            }
            _ => data.extend_from_slice(&d.data),
        }
    }
    let (layout, kept) = w.finish();
    // The layout must give the file back with the streams as they are: a check of everything but
    // zlib's emulation and the model, which is cheap.
    if !check(input, &layout, &chunks, &done, &kept) {
        return None;
    }
    let p = params.unwrap_or_default();
    let mut f = Formula::default();
    f.push("raw", Vec::new());
    let header = f.push_as("mca/header", Kind::Table, input[..HEADER].to_vec());
    let layout = f.push_as("mca/layout", Kind::Table, layout);
    let data = f.push_as("mca/data", Kind::Table, data);
    let kept = f.push("mca/kept", kept);
    let holes = f.push_as("mca/holes", Kind::Compressed, holes);
    let inputs = vec![header, layout, data, kept, holes];
    f.root = Node::apply(MODULE, layout::params_of(p), inputs, input.len() as u64);
    Some(f)
}

/// What becomes of the chunk `c` of `input`: its data (of zlib's stream, made again of the
/// parameters `params`, or stored), all the chunks' data at most `budget` bytes (`total` so far).
fn made(input: &[u8], c: &Chunk, params: Option<Params>, budget: u64, total: &std::sync::atomic::AtomicU64) -> Done {
    {
        let at = c.range().start;
        let len = u32::from_be_bytes(input[at..at + 4].try_into().unwrap()) as usize;
        let kept = Done { data: Vec::new(), how: How::Kept, nbt: None };
        match input[at + 4] {
            NONE => Done {
                data: input[at + CHUNK_HEADER..at + 4 + len].to_vec(),
                how: How::Stored { n: len as u64 - 1 },
                nbt: None,
            },
            ZLIB => {
                let (Some(p), Some(z)) = (params, zlib_stream(input, c)) else { return kept };
                let Some(data) = inflate(z, budget) else { return kept };
                let sum = total.fetch_add(data.len() as u64, std::sync::atomic::Ordering::Relaxed) + data.len() as u64;
                if sum > budget || layout::zlib_onto(&data, p, Vec::new()).ok().as_deref() != Some(z) {
                    return kept;
                }
                Done { how: How::Rebuilt { n: data.len() as u64, len: z.len() }, data, nbt: None }
            }
            _ => kept,
        }
    }
}

/// Whether the layout makes `input` again, of the streams as they are.
fn check(input: &[u8], layout: &[u8], chunks: &[Chunk], done: &[Done], kept: &[u8]) -> bool {
    struct Original<'a> {
        streams: Vec<&'a [u8]>,
        data: Vec<&'a [u8]>,
        at: usize,
    }
    impl Chunks for Original<'_> {
        fn rebuilt(&mut self, _: u64, _: Option<u64>, mut out: Vec<u8>) -> Result<Vec<u8>> {
            out.extend_from_slice(self.streams.get(self.at).ok_or_else(corrupt)?);
            self.at += 1;
            Ok(out)
        }
        fn stored(&mut self, _: u64, _: Option<u64>, out: &mut dyn aqr_api::stream::Put) -> Result<()> {
            let d = self.data.get(self.at).ok_or_else(corrupt)?;
            self.at += 1;
            out.put(d).ok_or_else(corrupt)
        }
        fn done(&mut self) -> bool {
            self.at == self.streams.len()
        }
    }
    let made: Vec<(&Chunk, &Done)> = chunks.iter().zip(done).filter(|(_, d)| d.how != How::Kept).collect();
    let mut o = Original {
        streams: made.iter().map(|(c, _)| zlib_stream(input, c).unwrap_or(&[])).collect(),
        data: made.iter().map(|(_, d)| &d.data[..]).collect(),
        at: 0,
    };
    let (mut l, mut k) = (aqr_api::stream::Slice(layout), aqr_api::stream::Slice(kept));
    let (mut l, mut k) = (aqr_api::stream::Reader::new(&mut l, 4096), aqr_api::stream::Reader::new(&mut k, 1 << 16));
    let mut out = Vec::with_capacity(input.len());
    layout::build_stream(&input[..HEADER], &mut l, &mut o, &mut k, input.len() as u64, &mut out).is_ok() && out == input
}

/// The output of `a`, an application of [`MODULE`], on `inputs`.
pub fn apply(a: &aqr_formula::Apply, inputs: &[&[u8]], threads: usize) -> Result<Vec<u8>> {
    match a.module {
        m if m == MODULE && a.resources.is_empty() => decode(&a.params, inputs, a.len, threads),
        _ => Err(corrupt()),
    }
}
