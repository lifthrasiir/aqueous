//! The lossless JPEGs (SOF3) of a file, as a module of entry formulas (see aqr-formula, FORMAT.md):
//! raw photographs in CR2, DNG and the like, whose pixels are Huffman-coded differences. The file
//! without those is the first input, and the pixels come back from [`crate::lmodel`]'s model.
//! Streams are found by their SOI wherever they are, so the container is not parsed.

use aqr_common::arith::ArithEncoder;
use aqr_common::varint;
use aqr_formula::{Formula, Kind, Module, Node};

use crate::lmodel::{self, Model};
use crate::lmodule::{self, MAX_SAMPLES};
use crate::lossless::{self, Header, Samples};
use crate::par::prelude::*;

pub use crate::lmodule::decode;

pub const MODULE: Module = Module::new("jpeg-lossless-recons", 1).with_plan(memory);

/// The memory of the module: its plan ([`crate::lmodule::plan`]).
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let inputs: [&[u8]; 4] = u.all_contents()?;
    lmodule::plan(u.initial(), &inputs, u.output())
}

/// A stream no longer than this many samples is coded with the ones next to it, with one model.
const GROUP: usize = 1 << 22;
/// A stream of at least this many samples is cut in bands, as many as it has times
/// [`BAND`] samples, at most [`BANDS`].
const BAND_MIN: usize = 1 << 21;
const BAND: usize = 1 << 20;
const BANDS: usize = 16;

/// A stream that is coded: its place in the file, its headers and its samples.
struct Coded {
    soi: usize,
    header: Header,
    samples: Samples,
}

/// The formula for `input`; None where it holds no lossless JPEG that is coded, or it would not
/// be smaller. `threads` bounds the parallelism (0: rayon's pool as it is).
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    crate::module::in_pool(threads, || {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| encode_at(input))).ok().flatten()
    })
}

/// The stream of `h` at `soi`, if its data is reproduced exactly from its samples.
fn check(input: &[u8], soi: usize, mut header: Header) -> Option<Coded> {
    let data = &input[header.start..header.end];
    if header.samples() > MAX_SAMPLES {
        return None;
    }
    let samples = lossless::decode(&header, data).ok()?;
    // (a table that lists a symbol twice: an encoder writes the first code or the last)
    let same = |h: &Header| lossless::encode(h, &samples).is_ok_and(|b| b == data);
    if !same(&header) {
        header.last_code = true;
        if !same(&header) {
            return None;
        }
    }
    Some(Coded { soi, header, samples })
}

/// The jobs: (first stream, streams, bands).
fn jobs(coded: &[Coded]) -> Vec<(usize, usize, usize)> {
    let mut jobs: Vec<(usize, usize, usize)> = Vec::new();
    // samples of the job being grown, if it is a group
    let mut open = 0;
    for (i, c) in coded.iter().enumerate() {
        let n = c.header.samples();
        if n >= BAND_MIN {
            let rows = c.header.mcus().1;
            let bands = lmodel::band_rows(rows, (n / BAND).clamp(1, BANDS)).len();
            jobs.push((i, 1, bands));
            open = 0;
            continue;
        }
        match jobs.last_mut() {
            Some(j) if open > 0 && open + n <= GROUP && coded[j.0].header.comps.len() == c.header.comps.len() => {
                j.1 += 1;
                open += n;
            }
            _ => {
                jobs.push((i, 1, 1));
                open = n;
            }
        }
    }
    jobs
}

/// The chunks of a job: one for streams coded together, one for each band.
fn code_job(coded: &mut [Coded], bands: usize) -> Vec<Vec<u8>> {
    if bands > 1 {
        let c = &mut coded[0];
        let mut full: Vec<&mut [u16]> = c.samples.planes.iter_mut().map(|p| &mut p[..]).collect();
        return lmodel::encode_bands_of(&c.header, &mut full, bands);
    }
    let mut model = Model::new(coded[0].header.comps.len());
    let mut e = ArithEncoder::new(Vec::new());
    for c in coded {
        let mut full: Vec<&mut [u16]> = c.samples.planes.iter_mut().map(|p| &mut p[..]).collect();
        lmodel::code_frame(&mut e, &mut model, &c.header, &mut full).expect("encoding does not fail");
    }
    vec![e.finish()]
}

fn encode_at(input: &[u8]) -> Option<Formula> {
    let found = lossless::find(input);
    if found.is_empty() {
        return None;
    }
    let mut coded: Vec<Coded> = found.into_par_iter().filter_map(|(soi, h)| check(input, soi, h)).collect();
    if coded.is_empty() {
        return None;
    }
    let jobs = jobs(&coded);
    // the chunks of each job, the jobs at once
    let mut parts: Vec<&mut [Coded]> = Vec::with_capacity(jobs.len());
    let mut rest = &mut coded[..];
    for &(_, count, _) in &jobs {
        let (job, tail) = rest.split_at_mut(count);
        parts.push(job);
        rest = tail;
    }
    let chunks: Vec<Vec<Vec<u8>>> =
        parts.into_par_iter().zip(jobs.par_iter()).map(|(job, &(_, _, bands))| code_job(job, bands)).collect();
    // the file without the entropy-coded data, and what the module needs to put it back
    let mut cut = Vec::with_capacity(input.len());
    let mut info = Vec::new();
    varint::put(&mut info, coded.len() as u64);
    let (mut prev, mut removed) = (0, 0);
    for c in &coded {
        cut.extend_from_slice(&input[prev..c.header.start]);
        prev = c.header.end;
        varint::put(&mut info, (c.soi - removed) as u64);
        removed += c.header.end - c.header.start;
        varint::put(&mut info, (c.header.end - c.header.start) as u64);
        varint::put(&mut info, c.header.last_code as u64);
    }
    cut.extend_from_slice(&input[prev..]);
    varint::put(&mut info, jobs.len() as u64);
    for &(_, count, bands) in &jobs {
        varint::put(&mut info, count as u64);
        varint::put(&mut info, bands as u64);
    }
    let (mut lens, mut model) = (Vec::new(), Vec::new());
    for chunk in chunks.iter().flatten() {
        varint::put(&mut lens, chunk.len() as u64);
        model.extend_from_slice(chunk);
    }
    let mut f = Formula::default();
    let cut = f.push_content("ljpeg/cut", Kind::Unknown, cut);
    let info = f.push("ljpeg/info", info);
    let lens = f.push("ljpeg/lens", lens);
    let model = f.push_as("ljpeg/model", Kind::Compressed, model);
    f.root = Node::apply(MODULE, Vec::new(), vec![cut, info, lens, model], input.len() as u64);
    let size: usize = f.streams.iter().map(|s| s.data.len()).sum();
    (size < input.len()).then_some(f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lossless::tests::jpeg;

    /// The file back from the formula's four streams, as the module makes it.
    fn back(f: &Formula, len: usize) -> Vec<u8> {
        let inputs: Vec<&[u8]> = f.streams.iter().map(|s| &s.data[..]).collect();
        decode(&[], &inputs, len as u64, 0).unwrap()
    }

    /// Streams of several shapes among other data: tiles that share a model, and one cut in bands.
    #[test]
    fn roundtrip_in_a_container() {
        let mut file = b"II*\0\x08\0\0\0 a container ".to_vec();
        for (k, (w, h)) in [(40, 30), (40, 30), (50, 20)].into_iter().enumerate() {
            file.extend_from_slice(&jpeg(14, w, h, &[(1, 1), (1, 1)], 1 + k as u8, 0, 0, k as u32));
            file.extend_from_slice(b" between ");
        }
        // one big enough to be cut in bands, among the others' components
        file.extend_from_slice(&jpeg(12, 2400, 1000, &[(1, 1)], 6, 0, 0, 9));
        file.extend_from_slice(&jpeg(12, 40, 30, &[(2, 1), (1, 1), (1, 1)], 4, 0, 0, 10));
        file.extend_from_slice(b" and the end");
        let f = encode(&file, 0).expect("coded");
        assert_eq!(back(&f, file.len()), file);
        // (the streams are all in the cut, as jobs: a group of the first two, then the others)
        let lay = lmodule::Layout::parse(&f.streams[0].data, &f.streams[1].data).unwrap();
        assert_eq!(lay.streams.len(), 5);
        assert_eq!(lay.jobs.iter().map(|j| (j.count, j.bands)).collect::<Vec<_>>(), [(3, 1), (1, 2), (1, 1)]);
    }

    #[test]
    fn nothing_to_code() {
        assert!(encode(b"II*\0 no jpeg here", 0).is_none());
        assert!(encode(&[0xFF, 0xD8, 0xFF, 0xD9], 0).is_none());
    }

    #[test]
    fn damaged_inputs_are_errors() {
        let file = jpeg(14, 60, 40, &[(1, 1), (1, 1)], 1, 0, 0, 3);
        let f = encode(&file, 0).expect("coded");
        let ins: Vec<Vec<u8>> = f.streams.iter().map(|s| s.data.to_vec()).collect();
        let run = |ins: &[Vec<u8>], len: usize| {
            let r: Vec<&[u8]> = ins.iter().map(|v| &v[..]).collect();
            decode(&[], &r, len as u64, 0)
        };
        assert_eq!(run(&ins, file.len()).unwrap(), file);
        assert!(run(&ins, file.len() + 1).is_err());
        for k in 0..4 {
            let mut bad = ins.clone();
            let half = bad[k].len() / 2;
            bad[k].truncate(half);
            let _ = run(&bad, file.len());
            let mut bad = ins.clone();
            bad[k].iter_mut().for_each(|b| *b ^= 0x5A);
            let _ = run(&bad, file.len());
        }
        assert!(decode(&[1], &ins.iter().map(|v| &v[..]).collect::<Vec<_>>(), file.len() as u64, 0).is_err());
    }
}
