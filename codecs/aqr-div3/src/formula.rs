//! aqr-div3 as a module of entry formulas (see aqr-formula): an AVI file whose video is DivX 3, rebuilt from its
//! frames coded by the model, the rest of its bytes (the skeleton), the audio's bytes (as they are, or from aqr-ac3's
//! module for AC-3) and the places of the frames and audio chunks (the layout). `FORMAT.md` specifies them.
use std::ops::Range;
use std::vec;
use std::vec::Vec;

use aqr_common::varint;
use aqr_formula::{Formula, Module, Node};

use crate::module::FRAME_MAX;
use crate::{Codec, Decoder, Encoder, Error, Result, SliceSource, corrupt};

/// The bytes of frames a chunk of the coded frames holds, about: each chunk has a model of its own, so that they are coded
/// and decoded at once.
pub const CHUNK: usize = 16 << 20;


/// The classes of the streams.
pub const CODED: &str = "div3/frames";
pub const SKELETON: &str = "div3/skeleton";
pub const LAYOUT: &str = "div3/layout";
pub const AUDIO: &str = "div3/audio";

/// The module `div3-recons`: params the width and height as two 16-bit numbers; inputs the coded frames, the skeleton,
/// the layout and the audio.
pub const MODULE: Module = Module::new("div3-recons", 2).with_plan(memory);

fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let &[_, _, _, _] = u.inputs else { return None };
    Some(crate::module::plan(u.initial()))
}

/// The video of an AVI file: its size, where its frames are, and where its audio chunks are.
pub struct Video {
    pub width: u16,
    pub height: u16,
    pub frames: Vec<Range<usize>>,
    pub audio: Vec<Range<usize>>,
    /// Whether the audio stream is AC-3 (format tag 0x2000).
    pub ac3: bool,
}

fn u32le(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

/// The streams of an AVI file's header list: the index of the first DivX 3 video stream with its width and height, and
/// the index of the first audio stream with whether it is AC-3.
struct Streams {
    video: Option<(usize, u16, u16)>,
    audio: Option<(usize, bool)>,
}

fn streams(d: &[u8]) -> Option<Streams> {
    if d.len() < 12 || &d[..4] != b"RIFF" || &d[8..12] != b"AVI " {
        return None;
    }
    let mut out = Streams { video: None, audio: None };
    let mut index = 0usize;
    let mut pos = d.windows(4).position(|w| w == b"hdrl")? + 4;
    let end = pos + u32le(d, pos - 8)? as usize;
    // (is video, is DivX 3, is audio) of the stream being read
    let mut cur: Option<(bool, bool, bool)> = None;
    while pos + 8 <= end.min(d.len()) {
        let (id, sz) = (&d[pos..pos + 4], u32le(d, pos + 4)? as usize);
        match id {
            b"LIST" => {
                if d.get(pos + 8..pos + 12)? == b"strl" {
                    if cur.is_some() {
                        index += 1;
                    }
                    cur = Some((false, false, false));
                }
                pos += 12;
                continue;
            }
            b"strh" => {
                let kind = d.get(pos + 8..pos + 12)?;
                let h = d.get(pos + 12..pos + 16)?.to_ascii_lowercase();
                let video = kind == b"vids";
                cur = Some((video, video && matches!(&h[..], b"div3" | b"mp43" | b"div4" | b"div5" | b"div6"), kind == b"auds"));
            }
            b"strf" => match cur {
                Some((true, true, _)) if out.video.is_none() => {
                    let w = u32le(d, pos + 12)? as i32;
                    let h = u32le(d, pos + 16)? as i32;
                    if w <= 0 || h == 0 || w > 4096 || h.abs() > 4096 {
                        return None;
                    }
                    out.video = Some((index, w as u16, h.unsigned_abs() as u16));
                }
                Some((_, _, true)) if out.audio.is_none() => {
                    let tag = u16::from_le_bytes(d.get(pos + 8..pos + 10)?.try_into().ok()?);
                    out.audio = Some((index, tag == 0x2000));
                }
                _ => {}
            },
            _ => {}
        }
        pos += 8 + sz + (sz & 1);
    }
    Some(out)
}

/// Whether `d` starts as an AVI file with a DivX 3 video stream (its header list only is looked at).
pub fn starts(d: &[u8]) -> bool {
    streams(&d[..d.len().min(1 << 16)]).is_some_and(|s| s.video.is_some())
}

/// The DivX 3 video of AVI file `d`, if it has one.
pub fn video(d: &[u8]) -> Option<Video> {
    let s = streams(d)?;
    let (vi, width, height) = s.video?;
    let tag = |i: usize, kind: &[u8; 2]| [b'0' + (i / 10) as u8, b'0' + (i % 10) as u8, kind[0], kind[1]];
    let vtag = tag(vi, b"dc");
    let atag = s.audio.map(|(i, _)| tag(i, b"wb"));
    let mut pos = d.windows(4).position(|w| w == b"movi")? + 4;
    let (mut frames, mut audio) = (Vec::new(), Vec::new());
    while pos + 8 <= d.len() {
        let (id, sz) = (&d[pos..pos + 4], u32le(d, pos + 4)? as usize);
        if id == b"LIST" {
            pos += 12;
            continue;
        }
        let known = id == b"JUNK" || (id[0].is_ascii_digit() && id[1].is_ascii_digit() && matches!(&id[2..], b"dc" | b"wb" | b"db"));
        if !known || pos + 8 + sz > d.len() {
            break;
        }
        if id == vtag {
            frames.push(pos + 8..pos + 8 + sz);
        } else if Some(id) == atag.as_ref().map(|t| &t[..]) {
            audio.push(pos + 8..pos + 8 + sz);
        }
        pos += 8 + sz + (sz & 1);
    }
    Some(Video { width, height, frames, audio, ac3: s.audio.is_some_and(|(_, a)| a) })
}

/// The formula of AVI file `input` with DivX 3 video, or None where it has none, or its frames do not come to less
/// than 97% by the model. The chunks of frames are coded on `threads` at once.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    encode_with(input, 0.97, threads)
}

/// [`encode`] taking the frames whatever they come to (for tests of the module).
pub fn encode_unchecked(input: &[u8], threads: usize) -> Option<Formula> {
    encode_with(input, f64::INFINITY, threads)
}

fn encode_with(input: &[u8], worst: f64, threads: usize) -> Option<Formula> {
    let v = video(input)?;
    if v.frames.len() < 4 || v.frames.iter().any(|f| f.len() > FRAME_MAX) {
        return None;
    }
    // the chunks in the order of the file: the video's frames (tag bit 0) and the audio's (tag bit 1)
    let mut items: Vec<(Range<usize>, u64)> =
        v.frames.iter().map(|r| (r.clone(), 0)).chain(v.audio.iter().map(|r| (r.clone(), 1))).collect();
    items.sort_by_key(|(r, _)| r.start);
    let (mut skeleton, mut layout, mut audio) = (Vec::with_capacity(input.len() / 8), Vec::new(), Vec::new());
    let (mut p, mut frames_len) = (0, 0usize);
    // the frames, cut in chunks of about CHUNK bytes: (frames' ranges)
    let mut chunks: Vec<Vec<Range<usize>>> = vec![Vec::new()];
    let mut chunk_bytes = 0usize;
    for (r, kind) in &items {
        varint::put(&mut layout, (r.start - p) as u64);
        varint::put(&mut layout, (r.len() as u64) << 1 | kind);
        skeleton.extend_from_slice(&input[p..r.start]);
        if *kind == 0 {
            if chunk_bytes >= CHUNK {
                chunks.push(Vec::new());
                chunk_bytes = 0;
            }
            chunks.last_mut().unwrap().push(r.clone());
            chunk_bytes += r.len();
            frames_len += r.len();
        } else {
            audio.extend_from_slice(&input[r.clone()]);
        }
        p = r.end;
    }
    skeleton.extend_from_slice(&input[p..]);
    let parts = aqr_common::pool::par_map(chunks.len(), threads, |i| {
        let mut codec = Codec::new(v.width as usize, v.height as usize);
        let mut enc = Encoder::new();
        for r in &chunks[i] {
            codec.encode(&mut enc, &input[r.clone()]);
        }
        enc.finish()
    });
    let mut coded = Vec::new();
    varint::put(&mut coded, chunks.len() as u64);
    for (c, part) in chunks.iter().zip(&parts) {
        varint::put(&mut coded, c.len() as u64);
        varint::put(&mut coded, part.len() as u64);
    }
    for part in &parts {
        coded.extend_from_slice(part);
    }
    if coded.len() as f64 > worst * frames_len as f64 {
        return None;
    }
    let mut f = Formula::default();
    let (c, s, l) = (f.push(CODED, coded), f.push(SKELETON, skeleton), f.push(LAYOUT, layout));
    // the audio: from the AC-3 module where the AC-3 stream comes to under 97% by it, else as it is
    let audio_node = match v.ac3.then(|| aqr_ac3::formula::encode_with(&audio, threads)).flatten() {
        Some(coded) => {
            let n = audio.len() as u64;
            let input = f.push(aqr_ac3::formula::CODED, coded);
            Node::apply(aqr_ac3::formula::MODULE, vec![], vec![input], n)
        }
        None => f.push(AUDIO, audio),
    };
    let mut params = Vec::new();
    params.extend_from_slice(&v.width.to_le_bytes());
    params.extend_from_slice(&v.height.to_le_bytes());
    f.root = Node::apply(MODULE, params, vec![c, s, l, audio_node], input.len() as u64);
    Some(f)
}

fn dims(params: &[u8]) -> Result<(usize, usize)> {
    let &[a, b, c, d] = params else { return Err(corrupt()) };
    Ok((u16::from_le_bytes([a, b]) as usize, u16::from_le_bytes([c, d]) as usize))
}

fn read_varint(data: &[u8], pos: &mut usize) -> Result<u64> {
    varint::get(data, pos).map_err(|_| corrupt())
}

/// The module `div3-recons` on whole inputs, natively: the chunks of frames decoded at once on `threads`, then the file
/// put together from the skeleton, the frames and the audio.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let &[coded, skeleton, layout, audio] = inputs else { return Err(corrupt()) };
    let (w, h) = dims(params)?;
    // the layout: (gap, length, is audio)
    let mut items = Vec::new();
    let mut pos = 0;
    while pos < layout.len() {
        let gap = read_varint(layout, &mut pos)?;
        let tag = read_varint(layout, &mut pos)?;
        items.push((gap as usize, (tag >> 1) as usize, tag & 1 == 1));
    }
    // the table of chunks
    let mut pos = 0;
    let n = read_varint(coded, &mut pos)?;
    if n > 1 << 24 {
        return Err(corrupt());
    }
    let mut table = Vec::new();
    for _ in 0..n {
        let frames = read_varint(coded, &mut pos)? as usize;
        let bytes = read_varint(coded, &mut pos)? as usize;
        table.push((frames, bytes));
    }
    // the frames' lengths of each chunk, and where its coded bytes are
    let mut lens = items.iter().filter(|i| !i.2).map(|i| i.1);
    let mut chunks = Vec::new();
    let mut at = pos;
    for &(frames, bytes) in &table {
        let l: Vec<usize> = lens.by_ref().take(frames).collect();
        if l.len() != frames || l.iter().any(|&n| n > FRAME_MAX) {
            return Err(corrupt());
        }
        let end = at.checked_add(bytes).filter(|&e| e <= coded.len()).ok_or_else(corrupt)?;
        chunks.push((at..end, l));
        at = end;
    }
    if lens.next().is_some() || at != coded.len() {
        return Err(corrupt());
    }
    let decoded = aqr_common::pool::par_map(chunks.len(), threads, |i| -> Result<Vec<u8>> {
        let (range, l) = &chunks[i];
        let mut codec = Codec::new(w, h);
        let mut rc = Decoder::new(SliceSource::new(&coded[range.clone()]));
        let mut out = Vec::with_capacity(l.iter().sum());
        let mut frame = Vec::with_capacity(FRAME_MAX + 8);
        for &n in l {
            frame = codec.decode(&mut rc, n, frame).map_err(|_| corrupt())?;
            out.extend_from_slice(&frame);
        }
        Ok(out)
    });
    let decoded = decoded.into_iter().collect::<Result<Vec<_>>>()?;
    let mut out = Vec::with_capacity(len.min(1 << 32) as usize);
    let (mut sk, mut au) = (0usize, 0usize);
    let (mut chunk, mut cpos) = (0usize, 0usize);
    for &(gap, n, is_audio) in &items {
        out.extend_from_slice(skeleton.get(sk..sk + gap).ok_or_else(corrupt)?);
        sk += gap;
        if is_audio {
            out.extend_from_slice(audio.get(au..au + n).ok_or_else(corrupt)?);
            au += n;
        } else {
            while decoded.get(chunk).is_some_and(|d| cpos == d.len()) {
                chunk += 1;
                cpos = 0;
            }
            out.extend_from_slice(decoded.get(chunk).and_then(|d| d.get(cpos..cpos + n)).ok_or_else(corrupt)?);
            cpos += n;
        }
    }
    out.extend_from_slice(&skeleton[sk..]);
    if out.len() as u64 != len || au != audio.len() {
        return Err(Error("output of another length"));
    }
    Ok(out)
}
