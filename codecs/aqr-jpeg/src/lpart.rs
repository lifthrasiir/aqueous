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
use crate::lmodule::{self, GAP_MAX, HEADER_MAX};
use crate::lossless::{self, Emitter, Header, Raw, Rows};

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
    /// a stream's record, and a job's, read before they were taken
    ahead: Option<(u64, u64, u64)>,
    job_ahead: Option<(u64, u64)>,
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
        let (soi, len, flags) = self.record()?;
        if soi > self.cut_len {
            return bad("stream");
        }
        self.copy_to(soi, sink)?;
        let h = self.read_header(flags == 1)?;
        sink.put(&self.hb[..h.start])?;
        let len = usize::try_from(len).map_err(|_| Error("bad length".into()))?;
        if !lmodule::data_fits(&h, len) {
            return bad("length");
        }
        Ok((h, len))
    }

    /// The next stream's record in the info: its SOI, the bytes of its data, its flags.
    fn record(&mut self) -> Result<(u64, u64, u64), Error> {
        if let Some(r) = self.ahead.take() {
            return Ok(r);
        }
        let (soi, len, flags) = (num(&mut self.info)?, num(&mut self.info)?, num(&mut self.info)?);
        if flags > 1 {
            return bad("stream");
        }
        Ok((soi, len, flags))
    }

    /// [`State::record`], left to be taken.
    fn peek(&mut self) -> Result<(u64, u64, u64), Error> {
        let r = self.record()?;
        self.ahead = Some(r);
        Ok(r)
    }

    /// The next job: its streams and bands.
    fn job(&mut self) -> Result<(u64, u64), Error> {
        if let Some(j) = self.job_ahead.take() {
            return Ok(j);
        }
        let (count, bands) = (num(&mut self.info)?, num(&mut self.info)?);
        if count == 0 || bands == 0 || (bands > 1 && count > 1) || bands > 1 << 16 {
            return bad("job");
        }
        Ok((count, bands))
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

    /// A row of 16-bit samples as bytes, little-endian, the row `y` of component `ci`.
    fn set_bytes(&mut self, ci: usize, y: usize, bytes: &[u8]) {
        let at = (y % (self.v[ci] + 1)) * self.w[ci];
        let (pairs, _) = bytes.as_chunks::<2>();
        for (s, b) in self.ring[ci][at..at + self.w[ci]].iter_mut().zip(pairs) {
            *s = u16::from_le_bytes(*b);
        }
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
        ahead: None,
        job_ahead: None,
    };
    let mut sink = Sink { out, total: 0, size };
    let njobs = num(&mut st.info)?;
    let mut done = 0;
    while done < njobs {
        let (count, bands) = st.job()?;
        if bands == 1 {
            done += groups(&mut st, &mut sink, count, njobs - done)?;
        } else {
            banded(&mut st, &mut sink, bands as usize)?;
            done += 1;
        }
    }
    // the rest of the cut
    let cut_len = st.cut_len;
    st.copy_to(cut_len, &mut sink)?;
    if sink.total != size
        || st.ahead.is_some()
        || st.job_ahead.is_some()
        || !st.info.done()
        || !st.lens.done()
        || st.model_left != 0
    {
        return bad("end of the streams");
    }
    Ok(())
}

/// What a task of a group has: where its streams are in the script, how many, and the bytes of
/// their data.
type Place = [usize; 3];

/// The record of a stream in a script (see [`groups`]): the gap before its headers, the headers,
/// the bytes of its data, and the flag.
struct Rec<'a> {
    gap: &'a [u8],
    header: &'a [u8],
    len: usize,
    last_code: bool,
}

fn rec<'a>(script: &'a [u8], at: &mut usize) -> Result<Rec<'a>, Error> {
    use crate::model::get_varint;
    let mut n =
        || -> Result<usize, Error> { usize::try_from(get_varint(script, at)?).map_err(|_| Error("bad number".into())) };
    let (gap, header, len) = (n()?, n()?, n()?);
    let flag = *script.get(*at).ok_or(Error("bad script".into()))?;
    *at += 1;
    let end = at.checked_add(gap).and_then(|e| e.checked_add(header)).filter(|&e| e <= script.len());
    let end = end.ok_or(Error("bad script".into()))?;
    let r = Rec { gap: &script[*at..*at + gap], header: &script[*at + gap..end], len, last_code: flag == 1 };
    *at = end;
    Ok(r)
}

/// The entropy-coded data of the streams of a group, one after the other (the task of a job).
fn group_task(script: &[u8], place: Place, chunk: &[u8]) -> Result<Vec<u8>, Error> {
    let [mut at, count, total] = place;
    let mut out = Vec::with_capacity(total);
    let mut d = ArithDecoder::new(chunk);
    let mut model: Option<(usize, Model)> = None;
    for _ in 0..count {
        let r = rec(script, &mut at)?;
        let mut h = lossless::parse(r.header, 0)?;
        if h.end != h.start || h.start + 2 != r.header.len() || h.samples() > lmodule::MAX_SAMPLES {
            return bad("stream");
        }
        h.last_code = r.last_code;
        let comps = h.comps.len();
        let (c, model) = model.get_or_insert_with(|| (comps, Model::new(comps)));
        if *c != comps {
            return bad("job of streams of different components");
        }
        let mut frame = Frame::new(&h);
        let mut emitter = Emitter::new(&h);
        let mut row = Vec::with_capacity(Emitter::row_room(&h));
        let start = out.len();
        for y in 0..h.mcus().1 {
            frame.code_row(&mut d, model, y)?;
            row.clear();
            emitter.mcu_row(&frame, y, &mut row)?;
            if out.len() + row.len() > total {
                return bad("length of the entropy-coded data");
            }
            out.extend_from_slice(&row);
        }
        if out.len() - start != r.len {
            return bad("length of the entropy-coded data");
        }
    }
    if d.overrun() {
        return err("data cut off");
    }
    Ok(out)
}

/// Most bytes of the script ([`groups`]): the jobs of a round, whose room is [`SCRIPT_JOB`] each
/// when they are read, and the temporary gap.
pub const SCRIPT_MAX: usize = 4 * lmodule::SCRIPT_JOB;
const SCRIPT_CAP: usize = SCRIPT_MAX + 2 * lmodule::GAP_MAX;

/// The jobs of streams coded together, `left` jobs being left, the first of `count` streams (jobs
/// of one chunk and one model each), as many as follow it that are groups. A round of them
/// at a time is read into a script: for each stream a record ([`rec`]) with its gap in the
/// cut and its headers, and its chunk; they are decoded as tasks, each its streams one after the
/// other, and put in order, each stream's gap, headers and data. A round ends before a job that
/// is [`ROUND`] jobs on, or comes after a gap of more than [`GAP_MAX`] (the first stream of
/// a round has its gap put at once), or whose script would not fit. Returns the jobs it took.
fn groups(st: &mut State, sink: &mut Sink, count: u64, mut left: u64) -> Result<u64, Error> {
    use crate::lmodule::{SCRIPT_JOB, script_bytes};
    let all = left;
    let mut script: Vec<u8> = Vec::with_capacity(SCRIPT_CAP);
    let mut places: Vec<Place> = Vec::with_capacity(ROUND);
    let mut next = Some(count);
    while next.is_some() {
        script.clear();
        places.clear();
        let mut chunks: Vec<Vec<u8>> = Vec::with_capacity(ROUND);
        while let Some(count) = next {
            let (soi, _, _) = st.peek()?;
            let gap = soi.checked_sub(st.cut.taken()).ok_or(Error("bad order of the streams".into()))?;
            let first = places.is_empty();
            if !first && (gap > GAP_MAX as u64 || places.len() == ROUND || script.len() + SCRIPT_JOB > SCRIPT_MAX) {
                break;
            }
            let job_at = script.len();
            let mut data = 0usize;
            for k in 0..count {
                let (soi, len, flags) = st.record()?;
                let gap = soi.checked_sub(st.cut.taken()).ok_or(Error("bad order of the streams".into()))?;
                if soi > st.cut_len || (k > 0 && gap > GAP_MAX as u64) {
                    return bad("stream");
                }
                let len = usize::try_from(len).map_err(|_| Error("bad length".into()))?;
                // (the first stream of the round has its gap put now)
                let gap = if first && k == 0 {
                    st.copy_to(soi, sink)?;
                    0
                } else {
                    gap as usize
                };
                let at = script.len();
                st.cut
                    .each(gap as u64, |p| {
                        script.extend_from_slice(p);
                        Some(())
                    })
                    .ok_or(Error("bad cut".into()))?;
                let h = st.read_header(flags == 1)?;
                if !lmodule::data_fits(&h, len) {
                    return bad("length");
                }
                let hlen = st.hb.len();
                if script.len() - job_at + script_bytes(gap, hlen, len) - gap > SCRIPT_JOB {
                    return bad("group");
                }
                // (the lengths before the gap)
                let mut meta = [0u8; 32];
                let mut m = 0;
                for v in [gap, hlen, len] {
                    let mut v = v as u64;
                    while v >= 0x80 {
                        meta[m] = v as u8 | 0x80;
                        m += 1;
                        v >>= 7;
                    }
                    meta[m] = v as u8;
                    m += 1;
                }
                meta[m] = flags as u8;
                m += 1;
                script.resize(script.len() + m, 0);
                script.copy_within(at..at + gap, at + m);
                script[at..at + m].copy_from_slice(&meta[..m]);
                script.extend_from_slice(&st.hb);
                data = data.checked_add(len).ok_or(Error("bad length".into()))?;
            }
            places.push([job_at, count as usize, data]);
            chunks.push(st.chunk()?);
            left -= 1;
            next = None;
            if left > 0 {
                let (c, bands) = st.job()?;
                if bands == 1 {
                    next = Some(c);
                } else {
                    st.job_ahead = Some((c, bands));
                }
            }
        }
        let task = |i: usize| group_task(&script, places[i], &chunks[i]).unwrap_or_default();
        aqr_common::pool::par_each_apart(
            places.len(),
            THREADS,
            2,
            task,
            || (),
            |i, bytes| {
                let [mut at, count, total] = places[i];
                if bytes.len() != total {
                    return bad("group");
                }
                let mut off = 0;
                for _ in 0..count {
                    let r = rec(&script, &mut at)?;
                    let split = r.header.len() - 2;
                    sink.put(r.gap)?;
                    sink.put(&r.header[..split])?;
                    sink.put(&bytes[off..off + r.len])?;
                    sink.put(&r.header[split..])?;
                    off += r.len;
                }
                Ok(())
            },
        )?;
    }
    Ok(all - left)
}

/// Bands decoded at once, as tasks, and the task slots of the module.
pub const THREADS: usize = 8;
/// Bands read and spawned together, a round: more than run at once, so that the next bands are
/// decoded while the rows of the first are put.
pub const ROUND: usize = 16;

/// Bytes of the samples of an MCU row of a frame of `h`: for each component its rows of the MCU row,
/// 16 bits to a sample, little-endian.
fn mcu_row_bytes(h: &Header) -> usize {
    (0..h.comps.len()).map(|ci| 2 * h.comps[ci].v * h.plane(ci).0).sum()
}

/// Bytes [`band_task`] gives for a band of `rows` MCU rows whose rows after the first take `bits`
/// bits: the samples of its first MCU row, those bits, and the samples of its last MCU row (if it is
/// another).
fn band_bytes(h: &Header, rows: usize, bits: u64) -> usize {
    let row = mcu_row_bytes(h);
    row + bits.div_ceil(8) as usize + if rows > 1 { row } else { 0 }
}

/// Appends the samples of the MCU row `r` of `frame` to `out`, as [`mcu_row_bytes`] counts.
fn push_row(out: &mut Vec<u8>, h: &Header, frame: &Frame, r: usize) {
    for ci in 0..h.comps.len() {
        let v = h.comps[ci].v;
        for y in r * v..(r + 1) * v {
            for s in frame.row(ci, y) {
                out.extend_from_slice(&s.to_le_bytes());
            }
        }
    }
}

/// The band `rows` of the frame of `h` decoded from its `chunk` (the task of a band), which gives
/// the samples of its first MCU row, the Huffman codes of the rows after the first as raw bits
/// (`bits` of them: the frame has no restart intervals, so they do not depend on what comes before
/// the band), and the samples of its last MCU row. Err where the chunk is cut off or the bits are
/// not so many.
fn band_task(h: &Header, rows: (usize, usize), chunk: &[u8], bits: u64) -> Result<Vec<u8>, Error> {
    let (r0, r1) = rows;
    let mut out = Vec::with_capacity(band_bytes(h, r1 - r0, bits));
    let mut d = ArithDecoder::new(chunk);
    let mut model = Model::new(h.comps.len());
    let bh = lmodel::band_header(h, r0, r1);
    let mut frame = Frame::new(&bh);
    let emitter = Emitter::new(&bh);
    frame.code_row(&mut d, &mut model, 0)?;
    push_row(&mut out, h, &frame, 0);
    if r1 - r0 > 1 {
        let mut raw = Raw::new(&mut out);
        for r in 1..r1 - r0 {
            frame.code_row(&mut d, &mut model, r)?;
            emitter.bits(&frame, r, &mut raw)?;
        }
        raw.finish();
        if raw.bits != bits {
            return bad("bits of a band");
        }
        push_row(&mut out, h, &frame, r1 - r0 - 1);
    } else if bits != 0 {
        return bad("bits of a band");
    }
    if d.overrun() {
        return err("data cut off");
    }
    Ok(out)
}

/// A job of one stream in `bands` bands, a chunk and a model for each: [`ROUND`] bands at a
/// time are read (their chunks), decoded as tasks, and put in order: the codes of a band's first
/// MCU row from its samples (and the row above, the last of the band before), then its other codes
/// as the task made them, shifted to follow, and the samples of its last row held.
fn banded(st: &mut State, sink: &mut Sink, bands: usize) -> Result<(), Error> {
    let (h, len) = st.next_stream(sink)?;
    if h.restart != 0 {
        return bad("bands of a frame with restart intervals");
    }
    let rows = lmodel::band_rows(h.mcus().1, bands);
    if rows.len() != bands {
        return bad("bands");
    }
    let mut bits: Vec<u64> = Vec::with_capacity(bands);
    for _ in 0..bands {
        let b = num(&mut st.info)?;
        // (the codes of a band are some of the stream's data)
        if b / 8 > len as u64 {
            return bad("bits of a band");
        }
        bits.push(b);
    }
    let mut held = Held::new(&h);
    let mut emitter = Emitter::new(&h);
    let room = Emitter::row_room(&h);
    let mut row = Vec::with_capacity(room);
    let mut emitted = 0;
    let mut first = 0;
    let my = h.mcus().1;
    while first < bands {
        let n = (bands - first).min(ROUND);
        let mut chunks: Vec<Vec<u8>> = Vec::with_capacity(ROUND);
        for _ in 0..n {
            chunks.push(st.chunk()?);
        }
        let task = |i: usize| band_task(&h, rows[first + i], &chunks[i], bits[first + i]).unwrap_or_default();
        // (in WebAssembly as tasks even alone, as the plan has them)
        aqr_common::pool::par_each_apart(
            n,
            THREADS,
            2,
            task,
            || (),
            |i, bytes| {
                let (r0, r1) = rows[first + i];
                let nbits = bits[first + i];
                if bytes.is_empty() || bytes.len() != band_bytes(&h, r1 - r0, nbits) {
                    return bad("band");
                }
                let rb = mcu_row_bytes(&h);
                // (the samples of an MCU row of the bytes, held at the row `r`)
                let hold = |held: &mut Held, from: &[u8], r: usize| {
                    let mut at = 0;
                    for ci in 0..h.comps.len() {
                        let (v, w) = (h.comps[ci].v, h.plane(ci).0);
                        for y in r * v..(r + 1) * v {
                            held.set_bytes(ci, y, &from[at..at + 2 * w]);
                            at += 2 * w;
                        }
                    }
                };
                hold(&mut held, &bytes[..rb], r0);
                row.clear();
                emitter.mcu_row(&held, r0, &mut row)?;
                emitted += row.len();
                sink.put(&row)?;
                if r1 - r0 > 1 {
                    let data = &bytes[rb..bytes.len() - rb];
                    emitter.append(data, nbits, r1 == my, &mut row, (room - 4) / 2, |p| {
                        emitted += p.len();
                        sink.put(p)
                    })?;
                    hold(&mut held, &bytes[bytes.len() - rb..], r1 - 1);
                }
                Ok(())
            },
        )?;
        first += n;
    }
    if emitted != len {
        return bad("length of the entropy-coded data");
    }
    sink.put(&st.hb[h.start..])?;
    Ok(())
}

/// The memory plan of the module as a part module ([`aqr_api::stack`]): what [`decode_part`]
/// takes beyond its initial memory (`initial` bytes) at most, and what a task of its class
/// takes, for the `cut`, `info` and `lens` given (`None` where they are not so): the pieces of the
/// readers and the headers, and the most a job takes at a moment (its chunks and model, and the
/// largest frame's rows). Nothing of it follows the lengths of the file, but the width of its
/// frames and the size of its chunks.
pub fn plan(initial: u64, cut: &[u8], info: &[u8], lens: &[u8]) -> Option<(u64, u64)> {
    use crate::model::get_varint;
    use aqr_api::stack::{Sim, block};
    let n = |k: usize| k as u64;
    let mut sim = Sim::new(initial);
    for size in [PIECE, SMALL, SMALL, PIECE, HEADER_MAX] {
        sim.take(n(size));
    }
    let (mut at, mut la) = (0, 0);
    let njobs = get_varint(info, &mut at).ok()?;
    // the jobs: their streams (where each starts, the bytes of its data, its headers) and chunks
    struct Job {
        bands: u64,
        streams: Vec<(usize, usize, Header)>,
        chunks: Vec<usize>,
        /// the bits of a band's rows after the first
        bits: Vec<u64>,
    }
    let mut jobs = Vec::new();
    for _ in 0..njobs {
        let count = get_varint(info, &mut at).ok()?;
        let bands = get_varint(info, &mut at).ok()?;
        if count == 0 || bands == 0 || (bands > 1 && count > 1) || bands > 1 << 16 {
            return None;
        }
        let mut streams = Vec::new();
        for _ in 0..count {
            let (soi, len, flags) = lmodule::stream_record(info, &mut at, cut.len() as u64).ok()?;
            let mut h = lossless::parse(cut, soi).ok()?;
            if h.end != h.start
                || h.start + 2 - soi > HEADER_MAX
                || h.samples() > lmodule::MAX_SAMPLES
                || !lmodule::data_fits(&h, len)
            {
                return None;
            }
            h.last_code = flags == 1;
            streams.push((soi, len, h));
        }
        let mut chunks = Vec::new();
        for _ in 0..bands {
            chunks.push(usize::try_from(get_varint(lens, &mut la).ok()?).ok()?);
        }
        if streams.windows(2).any(|w| w[1].0 < w[0].2.start + 2) {
            return None;
        }
        let mut bits = Vec::new();
        if bands > 1 {
            if streams[0].2.restart != 0 {
                return None;
            }
            for _ in 0..bands {
                let b = get_varint(info, &mut at).ok()?;
                if b / 8 > streams[0].1 as u64 {
                    return None;
                }
                bits.push(b);
            }
        }
        jobs.push(Job { bands, streams, chunks, bits });
    }
    // a frame, its emitter and the room for a row of its data
    let frame = |h: &Header| Frame::bytes(h) + Emitter::bytes() + block(n(Emitter::row_room(h)));
    let model_of = |h: &Header| -> u64 { Model::bytes(h.comps.len()).iter().map(|&b| block(b)).sum() };
    // (where the cut has been read to)
    let mut taken = 0usize;
    let mut j = 0;
    while j < jobs.len() {
        let job = &jobs[j];
        let (_, _, h0) = &job.streams[0];
        if job.bands == 1 {
            // groups, a round of them at a time: the script, the places, then for each round the
            // list of chunks and the chunks
            let top = sim.top;
            sim.take(n(SCRIPT_CAP));
            sim.take(12 * n(ROUND));
            while j < jobs.len() && jobs[j].bands == 1 {
                let t = sim.top;
                sim.take(12 * n(ROUND));
                let (mut script, mut round) = (0usize, Vec::new());
                while j < jobs.len() && jobs[j].bands == 1 {
                    let job = &jobs[j];
                    let gap = job.streams[0].0.checked_sub(taken)?;
                    if !round.is_empty()
                        && (gap > GAP_MAX || round.len() == ROUND || script + lmodule::SCRIPT_JOB > SCRIPT_MAX)
                    {
                        break;
                    }
                    let job_at = script;
                    for (k, (soi, len, h)) in job.streams.iter().enumerate() {
                        let gap = soi.checked_sub(taken)?;
                        if k > 0 && gap > GAP_MAX {
                            return None;
                        }
                        let gap = if round.is_empty() && k == 0 { 0 } else { gap };
                        let hlen = h.start + 2 - soi;
                        let bytes = lmodule::script_bytes(gap, hlen, *len);
                        if script - job_at + bytes - gap > lmodule::SCRIPT_JOB {
                            return None;
                        }
                        script += bytes;
                        taken = h.start + 2;
                    }
                    sim.take(n(job.chunks[0]));
                    round.push(j);
                    j += 1;
                }
                // a task: its output, its model, and the frame of the largest stream
                let own = |&j: &usize| -> Option<(u64, u64)> {
                    let job = &jobs[j];
                    let out: usize = job.streams.iter().map(|s| s.1).sum();
                    let most = job.streams.iter().map(|s| frame(&s.2)).max()?;
                    Some((block(n(out)) + model_of(&job.streams[0].2) + most, block(n(out))))
                };
                let owns: Vec<(u64, u64)> = round.iter().map(own).collect::<Option<_>>()?;
                if round.len() >= 2 {
                    sim.spawned(0, owns.iter().map(|o| sim.task(o.0)).collect());
                    sim.peak(block(n(round.len())) + owns.iter().map(|o| o.1).max()?);
                } else {
                    sim.peak(owns[0].0);
                }
                sim.top = t;
            }
            sim.top = top;
            continue;
        }
        j += 1;
        taken = h0.start + 2;
        let chunks = &job.chunks;
        let bands = job.bands;
        let model = model_of(h0);
        // the rows of the bands, and the rows held for the data
        let nc = n(h0.comps.len());
        let held = block(4 * nc)
            + block(4 * nc)
            + block(12 * nc)
            + (0..h0.comps.len()).map(|i| block(2 * n((h0.comps[i].v + 1) * h0.plane(i).0))).sum::<u64>();
        let rows = lmodel::band_rows(h0.mcus().1, bands as usize);
        let top = sim.top;
        sim.take(block(8 * bands) + block(8 * bands) + held + Emitter::bytes() + block(n(Emitter::row_room(h0))));
        for (round, chunk) in rows.chunks(ROUND).zip(chunks.chunks(ROUND)) {
            let t = sim.top;
            // the list of chunks, and the chunks, which the tasks have a copy of
            sim.take(block(12 * n(ROUND)));
            chunk.iter().for_each(|&c| sim.take(n(c)));
            // a task: its model, its frame and the rows it gives, over that copy
            let first = rows.iter().position(|r| r == &round[0])?;
            let size = |i: usize| block(n(band_bytes(h0, round[i].1 - round[i].0, job.bits[first + i])));
            let own = |i: usize| {
                model + Frame::bytes(&lmodel::band_header(h0, round[i].0, round[i].1)) + Emitter::bytes() + size(i)
            };
            if round.len() >= 2 {
                let tasks = (0..round.len()).map(|i| sim.task(own(i))).collect();
                sim.spawned(0, tasks);
                // (what the parent holds of the tasks, and a result it has joined)
                sim.peak(block(n(round.len())) + (0..round.len()).map(size).max()?);
            } else {
                sim.peak(own(0));
            }
            sim.top = t;
        }
        sim.top = top;
    }
    let (instance, tasks) = sim.plans(1);
    Some((instance, tasks[0]))
}
