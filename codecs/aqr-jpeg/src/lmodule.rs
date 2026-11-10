//! The code of the module `jpeg-lossless-recons` (see `lformula`, and FORMAT.md), which needs no
//! aqr-formula, so that aqr-modules builds it into its Wasm module.
//!
//! The file comes back from four inputs: `cut`, the file without the entropy-coded data of the
//! lossless JPEGs coded here; `info`, where they go and how they are coded; `lens` and `model`,
//! the chunks of the arithmetic-coded samples and their lengths. A *job* is a run of streams
//! coded one after the other with one model and one arithmetic stream (small tiles learn from
//! one another), or one stream cut in bands of rows, each its own chunk.

use aqr_common::arith::ArithDecoder;

use crate::jpeg::{Error, err};
use crate::lmodel::{self, Model};
use crate::lossless::{self, Header};
use crate::model::get_varint;
use crate::par::prelude::*;

/// Most samples of a stream that are taken (a corrupt header would ask for gigabytes).
pub const MAX_SAMPLES: usize = 1 << 30;

/// Most bytes of the headers of a stream, from its SOI to the EOI after its SOS: the part module
/// holds them while it parses them.
pub const HEADER_MAX: usize = 1 << 16;

/// Most bytes between a group's streams (the part module holds them while the group's tiles are
/// decoded as tasks), and, at the head of a group, for it to run with others.
pub const GAP_MAX: usize = 1 << 12;

/// Most bytes of a group of streams that the part module holds for its tasks: its gaps, the
/// headers, and a record of each ([`script_bytes`]).
pub const SCRIPT_JOB: usize = 1 << 17;

/// Bytes the part module holds of a stream of a group: the lengths of its gap, its headers and
/// its data, the flag, and the gap and the headers.
pub fn script_bytes(gap: usize, header: usize, len: usize) -> usize {
    use aqr_common::varint::len as vl;
    vl(gap as u64) + vl(header as u64) + vl(len as u64) + 1 + gap + header
}

/// The record of a stream in the info: where its SOI is in a cut of `cut_len` bytes, the bytes of
/// its entropy-coded data, and its flags (0 or 1).
pub fn stream_record(info: &[u8], at: &mut usize, cut_len: u64) -> Result<(usize, usize, u64), Error> {
    let soi = small(get_varint(info, at)?, cut_len as usize)?;
    let len = small(get_varint(info, at)?, usize::MAX >> 1)?;
    let flags = get_varint(info, at)?;
    if flags > 1 {
        return bad("flags");
    }
    Ok((soi, len, flags))
}

/// A lossless JPEG of the file: where its headers start in the `cut`, how many bytes of
/// entropy-coded data go after them, and the headers.
pub struct Stream {
    pub len: usize,
    pub header: Header,
}

/// Streams `first..first + count` coded together, in `bands` bands where it is one stream.
pub struct Job {
    pub first: usize,
    pub count: usize,
    pub bands: usize,
    /// the index of its first chunk
    pub chunk: usize,
}

pub struct Layout {
    pub streams: Vec<Stream>,
    pub jobs: Vec<Job>,
}

fn bad<T>(what: &str) -> Result<T, Error> {
    err(format!("bad {what}"))
}

fn small(v: u64, max: usize) -> Result<usize, Error> {
    usize::try_from(v).ok().filter(|&n| n <= max).ok_or(Error("bad number".into()))
}

impl Layout {
    /// The streams and jobs that `info` tells of, with the headers parsed from the `cut`.
    pub fn parse(cut: &[u8], info: &[u8]) -> Result<Layout, Error> {
        let mut at = 0;
        // (each job, and each stream, takes a few bytes of the info)
        let njobs = small(get_varint(info, &mut at)?, info.len())?;
        let mut streams: Vec<Stream> = Vec::new();
        let mut jobs: Vec<Job> = Vec::with_capacity(njobs);
        let mut chunk = 0usize;
        for _ in 0..njobs {
            let count = small(get_varint(info, &mut at)?, info.len())?;
            let bands = small(get_varint(info, &mut at)?, 1 << 16)?;
            if count == 0 || bands == 0 || (bands > 1 && count > 1) {
                return bad("job");
            }
            jobs.push(Job { first: streams.len(), count, bands, chunk });
            chunk += bands;
            let group = bands == 1;
            let mut script = 0;
            for k in 0..count {
                let (soi, len, flags) = stream_record(info, &mut at, cut.len() as u64)?;
                let mut header = lossless::parse(cut, soi)?;
                // the entropy-coded data is not in the cut: the EOI follows the scan's header
                if header.end != header.start || header.samples() > MAX_SAMPLES || header.start + 2 - soi > HEADER_MAX {
                    return bad("stream");
                }
                // (the cut goes on after the EOI)
                let prev = streams.last().map(|p| p.header.start + 2);
                if prev.is_some_and(|p| soi < p) {
                    return bad("order of the streams");
                }
                if group {
                    // (what the part module holds of a group while its tasks decode it)
                    let gap = if k == 0 { 0 } else { soi - prev.unwrap_or(0) };
                    if gap > GAP_MAX {
                        return bad("gap in a group");
                    }
                    script += script_bytes(gap, header.start + 2 - soi, len);
                    if script > SCRIPT_JOB {
                        return bad("group");
                    }
                }
                header.last_code = flags == 1;
                streams.push(Stream { len, header });
            }
        }
        if at != info.len() {
            return bad("jobs");
        }
        Ok(Layout { streams, jobs })
    }

    /// Chunks of the arithmetic-coded samples.
    pub fn chunks(&self) -> usize {
        self.jobs.last().map_or(0, |j| j.chunk + j.bands)
    }

    /// Bytes of the entropy-coded data of the streams of `job`.
    fn data(&self, job: &Job) -> usize {
        self.streams[job.first..job.first + job.count].iter().map(|s| s.len).sum()
    }
}

/// The chunks of `model` that `lens` tells the lengths of, `n` of them.
pub fn split_chunks<'a>(lens: &[u8], model: &'a [u8], n: usize) -> Result<Vec<&'a [u8]>, Error> {
    let (mut at, mut pos) = (0, 0);
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let len = small(get_varint(lens, &mut at)?, model.len() - pos)?;
        out.push(&model[pos..pos + len]);
        pos += len;
    }
    if at != lens.len() || pos != model.len() {
        return bad("chunk lengths");
    }
    Ok(out)
}

/// The module: the file of `len` bytes from the four `inputs` (`cut`, `info`, `lens`, `model`),
/// no params, on up to `threads` threads (0: rayon's pool as it is, the global one outside any
/// other).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>, Error> {
    if !params.is_empty() {
        return bad("parameters");
    }
    let &[cut, info, lens, model] = inputs else { return bad("inputs") };
    let len = usize::try_from(len).map_err(|_| Error("bad length".into()))?;
    let r = crate::module::in_pool(threads, || {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(cut, info, lens, model, len)))
    });
    r.map_err(|_| Error("internal panic".into()))?
}

fn run(cut: &[u8], info: &[u8], lens: &[u8], model: &[u8], len: usize) -> Result<Vec<u8>, Error> {
    let lay = Layout::parse(cut, info)?;
    let total = lay.streams.iter().try_fold(cut.len(), |t, s| t.checked_add(s.len));
    if total != Some(len) {
        return bad("length");
    }
    let chunks = split_chunks(lens, model, lay.chunks())?;
    let mut out = Vec::with_capacity(len);
    let mut prev = 0;
    // the jobs a batch at a time, the batch as wide as the threads (a single job in WebAssembly,
    // where nothing runs at once and the blocks of a job are freed in the order they were made)
    let batch = crate::par::current_num_threads().max(1);
    for jobs in lay.jobs.chunks(batch) {
        let done: Vec<Result<Vec<u8>, Error>> =
            jobs.par_iter().map(|j| run_job(&lay, j, &chunks[j.chunk..j.chunk + j.bands])).collect();
        for (job, data) in jobs.iter().zip(done) {
            let data = data?;
            let mut at = 0;
            for s in &lay.streams[job.first..job.first + job.count] {
                out.extend_from_slice(&cut[prev..s.header.start]);
                prev = s.header.start;
                out.extend_from_slice(&data[at..at + s.len]);
                at += s.len;
            }
        }
    }
    out.extend_from_slice(&cut[prev..]);
    Ok(out)
}

/// The entropy-coded data of the streams of `job`, one after the other.
fn run_job(lay: &Layout, job: &Job, chunks: &[&[u8]]) -> Result<Vec<u8>, Error> {
    let streams = &lay.streams[job.first..job.first + job.count];
    let mut buf = Vec::with_capacity(lay.data(job));

    let zeros = |h: &Header| -> Vec<Vec<u16>> { (0..h.comps.len()).map(|i| vec![0u16; h.samples_of(i)]).collect() };
    if job.bands > 1 {
        let h = &streams[0].header;
        let mut planes = zeros(h);
        let mut full: Vec<&mut [u16]> = planes.iter_mut().map(|p| &mut p[..]).collect();
        lmodel::decode_bands_into(h, chunks, &mut full)?;
        lossless::encode_into(h, &views(&planes), &mut buf)?;
    } else {
        let comps = streams[0].header.comps.len();
        if streams.iter().any(|s| s.header.comps.len() != comps) {
            return bad("job of streams of different components");
        }
        let mut model = Model::new(comps);
        let mut d = ArithDecoder::new(chunks[0]);
        for s in streams {
            let mut planes = zeros(&s.header);
            let mut full: Vec<&mut [u16]> = planes.iter_mut().map(|p| &mut p[..]).collect();
            lmodel::code_frame(&mut d, &mut model, &s.header, &mut full)?;
            lossless::encode_into(&s.header, &views(&planes), &mut buf)?;
        }
        if d.overrun() {
            return err("data cut off");
        }
    }
    if buf.len() != lay.data(job) {
        return bad("length of the entropy-coded data");
    }
    Ok(buf)
}

fn views(planes: &[Vec<u16>]) -> Vec<&[u16]> {
    planes.iter().map(|p| &p[..]).collect()
}
