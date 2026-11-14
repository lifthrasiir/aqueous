//! aqr-minecraft as a module of entry formulas (see aqr-formula): a region file made again of its
//! header, its layout, its chunks' data and the bytes kept ([`MODULE`]), each chunk's zlib stream
//! made of its data by aqr-flate's emulation of zlib. `FORMAT.md` specifies it.

use aqr_flate::zlib::{Params, Strategy};
use aqr_formula::{Formula, Kind, Module, Node};

use crate::layout::{self, CHUNK_HEADER, Chunks, How, Writer};
use crate::region::{self, Chunk, HEADER, NONE, ZLIB};
use crate::{Result, corrupt};

pub use crate::module::decode;

/// A region file made of its chunks' data ([`decode`]), a part module.
pub const MODULE: Module = Module::new("mca-recons", 1).with_plan(memory);

/// The memory of the module: its plan ([`crate::module::plan`]), of its header and layout.
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    crate::module::plan(u.initial(), u.params, u.content(0)?, u.content(1)?, u.output())
}

/// The most bytes that the chunks of a file of `n` bytes may inflate to, all together: 64 MiB,
/// and 64 times the file (the End's regions inflate to 18 times theirs).
pub fn limit(n: usize) -> u64 {
    (64 << 20) + 64 * n as u64
}

/// Chunks whose streams the zlib parameters are found of, the first.
const SAMPLES: usize = 4;

/// What became of a chunk: its data and how it is made, of its stream (the bytes after the
/// compression type) where it is rebuilt.
struct Done {
    data: Vec<u8>,
    how: How,
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
        .take(SAMPLES)
        .filter_map(|z| Some((z, inflate(z, budget)?)))
        .collect();
    let params = find_params(&samples);
    let total = std::sync::atomic::AtomicU64::new(0);
    let done: Vec<Done> = aqr_common::pool::par_map(chunks.len(), threads, |i| {
        let c = &chunks[i];
        let at = c.range().start;
        let len = u32::from_be_bytes(input[at..at + 4].try_into().unwrap()) as usize;
        let kept = Done { data: Vec::new(), how: How::Kept };
        match input[at + 4] {
            NONE => {
                Done { data: input[at + CHUNK_HEADER..at + 4 + len].to_vec(), how: How::Stored { n: len as u64 - 1 } }
            }
            ZLIB => {
                let (Some(p), Some(z)) = (params, zlib_stream(input, c)) else { return kept };
                let Some(data) = inflate(z, budget) else { return kept };
                let sum = total.fetch_add(data.len() as u64, std::sync::atomic::Ordering::Relaxed) + data.len() as u64;
                if sum > budget || layout::zlib_onto(&data, p, Vec::new()).ok().as_deref() != Some(z) {
                    return kept;
                }
                Done { how: How::Rebuilt { n: data.len() as u64, len: z.len() }, data }
            }
            _ => kept,
        }
    });
    if done.iter().all(|d| d.how == How::Kept) {
        return None;
    }
    let mut w = Writer::new(input);
    let mut data = Vec::with_capacity(done.iter().map(|d| d.data.len()).sum());
    for (c, d) in chunks.iter().zip(&done) {
        w.chunk(c, d.how);
        data.extend_from_slice(&d.data);
    }
    let (layout, kept) = w.finish();
    // The layout must give the file back with the streams as they are: a check of everything but
    // zlib's emulation, which is cheap.
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
    f.root = Node::apply(MODULE, layout::params_of(p), vec![header, layout, data, kept], input.len() as u64);
    Some(f)
}

/// Whether the layout makes `input` again, of the streams as they are.
fn check(input: &[u8], layout: &[u8], chunks: &[Chunk], done: &[Done], kept: &[u8]) -> bool {
    struct Original<'a> {
        streams: Vec<&'a [u8]>,
        data: Vec<&'a [u8]>,
        at: usize,
    }
    impl Chunks for Original<'_> {
        fn rebuilt(&mut self, _: u64, mut out: Vec<u8>) -> Result<Vec<u8>> {
            out.extend_from_slice(self.streams.get(self.at).ok_or_else(corrupt)?);
            self.at += 1;
            Ok(out)
        }
        fn stored(&mut self, _: u64, out: &mut dyn aqr_api::stream::Put) -> Result<()> {
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
