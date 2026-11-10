//! `jpeg-lossless-recons` as a part module (see `lmodule` for what it is and FORMAT.md): its
//! inputs taken and its output put in order through windows ([`WINDOW`]), so that what it holds
//! is a chunk of the arithmetic-coded samples, the rows of a frame and the model, whatever the
//! length of the file.
//!
//! The file is put as it comes: the cut up to a stream's headers, which are read marker by
//! marker, the entropy-coded data of the stream a row of MCUs at a time as the rows are
//! decoded, and so on to the end of the cut.

use aqr_api::stream::{In, Pull, Put, Reader, Slice, Written};
use aqr_common::arith::ArithDecoder;

use crate::jpeg::{Error, err};
use crate::lmodel::{self, Frame, Model};
use crate::lmodule::{self, HEADER_MAX};
use crate::lossless::{self, Emitter, Header, Rows};

/// Pages of the windows of the module as a part module ([`decode_part`]): of each input and of
/// the output.
pub const WINDOW: u32 = 16;

/// Bytes of a piece of the cut and the model that are read at once, and of the info and lens.
const PIECE: usize = 1 << 16;
const SMALL: usize = 1 << 12;

fn bad<T>(what: &str) -> Result<T, Error> {
    err(format!("bad {what}"))
}

/// The module as a part module (its entry point, of `aqr_api::entry!`): what [`decode`] gives,
/// its inputs taken and its output put in order through windows.
pub fn decode_part(params_len: u64, lens: [u64; 4], size: u64) {
    aqr_api::finish(part(params_len, lens, size));
}

fn part(params_len: u64, lens: [u64; 4], size: u64) -> Result<(), Error> {
    if params_len != 0 {
        return bad("parameters");
    }
    let (mut cut, mut info, mut ls, mut model) =
        (In::<0>::new(lens[0]), In::<1>::new(lens[1]), In::<2>::new(lens[2]), In::<3>::new(lens[3]));
    let mut out = Written::new(size);
    run([&mut cut, &mut info, &mut ls, &mut model], lens, &mut out, size)
}

/// The module on slices, whole: the file of `len` bytes (for tests, and what the part module's
/// plan is measured against).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>, Error> {
    if !params.is_empty() {
        return bad("parameters");
    }
    let &[cut, info, lens, model] = inputs else { return bad("inputs") };
    let mut out = Vec::new();
    let (mut c, mut i, mut l, mut m) = (Slice(cut), Slice(info), Slice(lens), Slice(model));
    let sizes = [cut.len() as u64, info.len() as u64, lens.len() as u64, model.len() as u64];
    run([&mut c, &mut i, &mut l, &mut m], sizes, &mut out, len)?;
    Ok(out)
}

/// The output counted, and refused past its size.
struct Sink<'a> {
    out: &'a mut dyn Put,
    total: u64,
    size: u64,
}

impl Sink<'_> {
    fn put(&mut self, d: &[u8]) -> Result<(), Error> {
        self.total += d.len() as u64;
        if self.total > self.size {
            return bad("length");
        }
        self.out.put(d).ok_or(Error("bad length".into()))
    }
}

/// What is read of the inputs, and what is held while a stream is.
struct State<'a> {
    cut: Reader<'a>,
    info: Reader<'a>,
    lens: Reader<'a>,
    model: Reader<'a>,
    /// the bytes of the model not yet taken, and the cut's length
    model_left: u64,
    cut_len: u64,
    /// the headers of the stream being read, from its SOI to the EOI after the scan's header
    hb: Vec<u8>,
}

fn num(r: &mut Reader) -> Result<u64, Error> {
    r.num().map_err(|_| Error("bad number".into()))
}

impl State<'_> {
    /// Puts the cut up to `to` (a position in it), as it was read from.
    fn copy_to(&mut self, to: u64, sink: &mut Sink) -> Result<(), Error> {
        let n = to.checked_sub(self.cut.taken()).ok_or(Error("bad order of the streams".into()))?;
        self.cut.each(n, |p| sink.put(p).ok()).ok_or(Error("bad cut".into()))
    }

    /// Reads the headers of the stream at `soi` into `hb`: marker by marker up to its SOS and the
    /// two bytes after, and parses them.
    fn read_header(&mut self, last_code: bool) -> Result<Header, Error> {
        let hb = &mut self.hb;
        hb.clear();
        let cut = &mut self.cut;
        let mut byte = |hb: &mut Vec<u8>| -> Result<u8, Error> {
            let b = cut.byte().ok_or(Error("bad cut".into()))?;
            if hb.len() == HEADER_MAX {
                return bad("headers");
            }
            hb.push(b);
            Ok(b)
        };
        if byte(hb)? != 0xFF || byte(hb)? != 0xD8 {
            return bad("stream");
        }
        loop {
            if byte(hb)? != 0xFF {
                return bad("marker");
            }
            let mut m = byte(hb)?;
            while m == 0xFF {
                m = byte(hb)?;
            }
            match m {
                0x01 => continue,
                0xD8 | 0xD0..=0xD7 | 0xD9 | 0x00 => return bad("marker"),
                _ => {}
            }
            let (hi, lo) = (byte(hb)? as usize, byte(hb)? as usize);
            let seg = (hi << 8 | lo).checked_sub(2).ok_or(Error("bad segment".into()))?;
            for _ in 0..seg {
                byte(hb)?;
            }
            if m == 0xDA {
                // (the EOI that follows)
                byte(hb)?;
                byte(hb)?;
                break;
            }
        }
        let mut h = lossless::parse(hb, 0)?;
        if h.end != h.start || h.start + 2 != hb.len() || h.samples() > lmodule::MAX_SAMPLES {
            return bad("stream");
        }
        h.last_code = last_code;
        Ok(h)
    }

    /// The next stream of the job: the cut up to its headers put, and the headers (put but the
    /// EOI after them, which is left in `hb`), and the bytes of its data.
    fn next_stream(&mut self, sink: &mut Sink) -> Result<(Header, usize), Error> {
        let soi = num(&mut self.info)?;
        let len = num(&mut self.info)?;
        let flags = num(&mut self.info)?;
        if soi > self.cut_len || flags > 1 {
            return bad("stream");
        }
        self.copy_to(soi, sink)?;
        let h = self.read_header(flags == 1)?;
        sink.put(&self.hb[..h.start])?;
        Ok((h, usize::try_from(len).map_err(|_| Error("bad length".into()))?))
    }

    /// The next chunk of the model.
    fn chunk(&mut self) -> Result<Vec<u8>, Error> {
        let len = num(&mut self.lens)?;
        if len > self.model_left {
            return bad("chunk");
        }
        self.model_left -= len;
        let mut chunk = vec![0u8; len as usize];
        self.model.exact(&mut chunk).ok_or(Error("bad model".into()))?;
        Ok(chunk)
    }
}

/// The last rows of a plane held for the entropy-coded data of a frame, which has rows of bands
/// decoded apart: those of the MCU row and the one above.
struct Held {
    ring: Vec<Vec<u16>>,
    v: Vec<usize>,
    w: Vec<usize>,
}

impl Held {
    fn new(h: &Header) -> Held {
        let w: Vec<usize> = (0..h.comps.len()).map(|i| h.plane(i).0).collect();
        let v: Vec<usize> = h.comps.iter().map(|c| c.v).collect();
        Held { ring: (0..w.len()).map(|i| vec![0; (v[i] + 1) * w[i]]).collect(), v, w }
    }

    fn set(&mut self, ci: usize, y: usize, row: &[u16]) {
        let at = (y % (self.v[ci] + 1)) * self.w[ci];
        self.ring[ci][at..at + self.w[ci]].copy_from_slice(row);
    }
}

impl Rows for Held {
    fn row(&self, ci: usize, y: usize) -> &[u16] {
        let at = (y % (self.v[ci] + 1)) * self.w[ci];
        &self.ring[ci][at..at + self.w[ci]]
    }
}

/// The module on its four streams (`lens` their lengths), the file of `size` bytes put to `out`.
pub fn run(inputs: [&mut dyn Pull; 4], lens: [u64; 4], out: &mut dyn Put, size: u64) -> Result<(), Error> {
    let [cut, info, ls, model] = inputs;
    let mut st = State {
        cut: Reader::new(cut, PIECE),
        info: Reader::new(info, SMALL),
        lens: Reader::new(ls, SMALL),
        model: Reader::new(model, PIECE),
        model_left: lens[3],
        cut_len: lens[0],
        hb: Vec::with_capacity(HEADER_MAX),
    };
    let mut sink = Sink { out, total: 0, size };
    let njobs = num(&mut st.info)?;
    for _ in 0..njobs {
        let count = num(&mut st.info)?;
        let bands = num(&mut st.info)?;
        if count == 0 || bands == 0 || (bands > 1 && count > 1) || bands > 1 << 16 {
            return bad("job");
        }
        if bands == 1 {
            group(&mut st, &mut sink, count)?;
        } else {
            banded(&mut st, &mut sink, bands as usize)?;
        }
    }
    // the rest of the cut
    let cut_len = st.cut_len;
    st.copy_to(cut_len, &mut sink)?;
    if sink.total != size || !st.info.done() || !st.lens.done() || st.model_left != 0 {
        return bad("end of the streams");
    }
    Ok(())
}

/// A job of `count` streams, one chunk and one model for them.
fn group(st: &mut State, sink: &mut Sink, count: u64) -> Result<(), Error> {
    let (mut h, mut len) = st.next_stream(sink)?;
    let comps = h.comps.len();
    let chunk = st.chunk()?;
    let mut d = ArithDecoder::new(&chunk);
    let mut model = Model::new(comps);
    for i in 0..count {
        if i > 0 {
            (h, len) = st.next_stream(sink)?;
            if h.comps.len() != comps {
                return bad("job of streams of different components");
            }
        }
        let mut frame = Frame::new(&h);
        let mut emitter = Emitter::new(&h);
        let mut row = Vec::with_capacity(Emitter::row_room(&h));
        let mut emitted = 0;
        for r in 0..h.mcus().1 {
            frame.code_row(&mut d, &mut model, r)?;
            row.clear();
            emitter.mcu_row(&frame, r, &mut row)?;
            emitted += row.len();
            sink.put(&row)?;
        }
        if emitted != len {
            return bad("length of the entropy-coded data");
        }
        sink.put(&st.hb[h.start..])?;
    }
    if d.overrun() {
        return err("data cut off");
    }
    Ok(())
}

/// A job of one stream in `bands` bands, a chunk and a model for each.
fn banded(st: &mut State, sink: &mut Sink, bands: usize) -> Result<(), Error> {
    let (h, len) = st.next_stream(sink)?;
    let rows = lmodel::band_rows(h.mcus().1, bands);
    if rows.len() != bands {
        return bad("bands");
    }
    let mut held = Held::new(&h);
    let mut emitter = Emitter::new(&h);
    let mut row = Vec::with_capacity(Emitter::row_room(&h));
    let mut emitted = 0;
    for &(r0, r1) in &rows {
        let chunk = st.chunk()?;
        let mut d = ArithDecoder::new(&chunk);
        let mut model = Model::new(h.comps.len());
        let bh = lmodel::band_header(&h, r0, r1);
        let mut frame = Frame::new(&bh);
        for r in 0..r1 - r0 {
            frame.code_row(&mut d, &mut model, r)?;
            for ci in 0..h.comps.len() {
                let v = h.comps[ci].v;
                for y in r * v..(r + 1) * v {
                    held.set(ci, r0 * v + y, frame.row(ci, y));
                }
            }
            row.clear();
            emitter.mcu_row(&held, r0 + r, &mut row)?;
            emitted += row.len();
            sink.put(&row)?;
        }
        if d.overrun() {
            return err("data cut off");
        }
    }
    if emitted != len {
        return bad("length of the entropy-coded data");
    }
    sink.put(&st.hb[h.start..])?;
    Ok(())
}

/// The memory plan of the module as a part module ([`aqr_api::stack`]): what [`decode_part`]
/// takes beyond its initial memory (`initial` bytes) at most, for the `cut`, `info` and `lens`
/// given (`None` where they are not so): the pieces of the readers and the headers, and the most
/// a job takes at a moment (its chunk and model, and the largest frame's rows). Nothing of it
/// follows the lengths of the file, but the width of its frames and the size of its chunks.
pub fn plan(initial: u64, cut: &[u8], info: &[u8], lens: &[u8]) -> Option<u64> {
    use crate::model::get_varint;
    use aqr_api::stack::{Sim, block};
    let n = |k: usize| k as u64;
    let mut sim = Sim::new(initial);
    for size in [PIECE, SMALL, SMALL, PIECE, HEADER_MAX] {
        sim.take(n(size));
    }
    let (mut at, mut la) = (0, 0);
    let njobs = get_varint(info, &mut at).ok()?;
    for _ in 0..njobs {
        let count = get_varint(info, &mut at).ok()?;
        let bands = get_varint(info, &mut at).ok()?;
        if count == 0 || bands == 0 || (bands > 1 && count > 1) || bands > 1 << 16 {
            return None;
        }
        let mut headers = Vec::new();
        for _ in 0..count {
            let (soi, _, flags) = lmodule::stream_record(info, &mut at, cut.len() as u64).ok()?;
            let mut h = lossless::parse(cut, soi).ok()?;
            if h.end != h.start || h.start + 2 - soi > HEADER_MAX || h.samples() > lmodule::MAX_SAMPLES {
                return None;
            }
            h.last_code = flags == 1;
            headers.push(h);
        }
        let mut chunks = Vec::new();
        for _ in 0..bands {
            chunks.push(usize::try_from(get_varint(lens, &mut la).ok()?).ok()?);
        }
        let h0 = &headers[0];
        let model: u64 = Model::bytes(h0.comps.len()).iter().map(|&b| block(b)).sum();
        // a frame, its emitter and the room for a row of its data
        let frame = |h: &Header| Frame::bytes(h) + Emitter::bytes() + block(n(Emitter::row_room(h)));
        let job = if bands == 1 {
            block(n(chunks[0])) + model + headers.iter().map(frame).max()?
        } else {
            // the rows of the bands, the rows held for the data, and a band's chunk, model and frame
            let nc = n(h0.comps.len());
            let held = block(4 * nc)
                + block(4 * nc)
                + block(12 * nc)
                + (0..h0.comps.len()).map(|i| block(2 * n((h0.comps[i].v + 1) * h0.plane(i).0))).sum::<u64>();
            let most = chunks.iter().map(|&c| block(n(c))).max()?;
            block(8 * bands)
                + held
                + Emitter::bytes()
                + block(n(Emitter::row_room(h0)))
                + most
                + model
                + Frame::bytes(h0)
        };
        sim.peak(job);
    }
    Some(sim.plan())
}
