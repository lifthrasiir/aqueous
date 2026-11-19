//! aqr-div3 as a module of entry formulas (see aqr-formula): an AVI file rebuilt from its video frames (coded by the
//! model where they are DivX 3, else kept as they are), the rest of its bytes (the skeleton), the audio's bytes (as
//! they are, or from aqr-ac3's module for AC-3, aqr-mp3's for MP3) and the places of the frames and audio chunks (the
//! layout). `FORMAT.md` specifies them.
use std::ops::Range;
use std::vec;
use std::vec::Vec;

use aqr_common::varint;
use aqr_formula::{Formula, Kind, Module, Node};

use crate::module::FRAME_MAX;
pub use crate::native::decode;
use crate::{Codec, Encoder};

aqr_common::zopt! {
    /// The bytes of frames a chunk of the coded frames holds, about: each chunk has a model of its own, so that they are
    /// coded and decoded at once.
    pub static CHUNK: Opt<usize> = Opt::size("div3:chunk", 16 << 20, "frame bytes of a chunk, about");
}

/// The classes of the streams.
pub const CODED: &str = "div3/frames";
/// The frames as they are, where the video is not coded (not DivX 3, or not coming to less).
pub const KEPT: &str = "div3/kept";
pub const SKELETON: &str = "div3/skeleton";
pub const LAYOUT: &str = "div3/layout";
pub const AUDIO: &str = "div3/audio";

/// The module `div3-recons`: params the width and height as two 16-bit numbers (both 0: the frames kept); inputs the
/// coded (or kept) frames, the skeleton, the layout and the audio.
pub const MODULE: Module = Module::new("div3-recons", 3).with_plan(memory);

fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let &[_, _, _, _] = u.inputs else { return None };
    crate::module::plan(u.initial(), u.params)
}

/// The video of an AVI file: whether it is DivX 3, its size, where its frames are, and where its audio chunks are.
pub struct Video {
    /// Whether the video stream is DivX 3 (by its handler), its frames to be coded by the model.
    pub div3: bool,
    /// The width and height of DivX 3 video; 0 for other video.
    pub width: u16,
    pub height: u16,
    pub frames: Vec<Range<usize>>,
    pub audio: Vec<Range<usize>>,
    /// Whether the audio stream is AC-3 (format tag 0x2000).
    pub ac3: bool,
    /// Whether the audio stream is MPEG audio of layer 3 (format tag 0x55).
    pub mp3: bool,
}

fn u32le(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

/// The streams of an AVI file's header list: the index of the first video stream with whether it is DivX 3 and its
/// width and height (0 where it is not DivX 3), and the index of the first audio stream with its format tag.
struct Streams {
    video: Option<(usize, bool, u16, u16)>,
    audio: Option<(usize, u16)>,
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
                cur = Some((
                    video,
                    video && matches!(&h[..], b"div3" | b"mp43" | b"div4" | b"div5" | b"div6"),
                    kind == b"auds",
                ));
            }
            b"strf" => match cur {
                Some((true, true, _)) if out.video.is_none() => {
                    let w = u32le(d, pos + 12)? as i32;
                    let h = u32le(d, pos + 16)? as i32;
                    if w <= 0 || h == 0 || w > 4096 || h.abs() > 4096 {
                        return None;
                    }
                    out.video = Some((index, true, w as u16, h.unsigned_abs() as u16));
                }
                Some((true, false, _)) if out.video.is_none() => out.video = Some((index, false, 0, 0)),
                Some((_, _, true)) if out.audio.is_none() => {
                    let tag = u16::from_le_bytes(d.get(pos + 8..pos + 10)?.try_into().ok()?);
                    out.audio = Some((index, tag));
                }
                _ => {}
            },
            _ => {}
        }
        pos += 8 + sz + (sz & 1);
    }
    Some(out)
}

/// The format tags of the audio taken: AC-3 and MP3.
const AC3_TAG: u16 = 0x2000;
const MP3_TAG: u16 = 0x55;

/// Whether `d` starts as an AVI file with a DivX 3 video stream, or with a video stream and AC-3 or MP3 audio (its
/// header list only is looked at).
pub fn starts(d: &[u8]) -> bool {
    streams(&d[..d.len().min(1 << 16)]).is_some_and(|s| {
        s.video.is_some_and(|v| v.1) || (s.video.is_some() && s.audio.is_some_and(|a| matches!(a.1, AC3_TAG | MP3_TAG)))
    })
}

/// The video of AVI file `d`, if it has a video stream.
pub fn video(d: &[u8]) -> Option<Video> {
    let s = streams(d)?;
    let (vi, div3, width, height) = s.video?;
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
        let known = id == b"JUNK"
            || (id[0].is_ascii_digit() && id[1].is_ascii_digit() && matches!(&id[2..], b"dc" | b"wb" | b"db"));
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
    let tag = s.audio.map(|a| a.1);
    Some(Video { div3, width, height, frames, audio, ac3: tag == Some(AC3_TAG), mp3: tag == Some(MP3_TAG) })
}

/// The formula of AVI file `input`, or None where neither its frames (DivX 3) come to at most 97% by the model nor its
/// audio (AC-3, MP3) to less by its module. The chunks of frames are coded on `threads` at once.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    encode_with(input, 0.97, threads)
}

/// [`encode`] taking the frames and the audio whatever they come to (for tests of the module).
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
    // the frames, cut in chunks of about `CHUNK` bytes: (frames' ranges)
    let mut chunks: Vec<Vec<Range<usize>>> = vec![Vec::new()];
    let mut chunk_bytes = 0usize;
    let chunk = CHUNK.get();
    for (r, kind) in &items {
        varint::put(&mut layout, (r.start - p) as u64);
        varint::put(&mut layout, (r.len() as u64) << 1 | kind);
        skeleton.extend_from_slice(&input[p..r.start]);
        if *kind == 0 {
            if chunk_bytes >= chunk {
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
    // the frames coded where they are DivX 3 and come to at most `worst` of themselves, else kept as they are
    let coded =
        (v.div3.then(|| code(input, &v, &chunks, threads))).filter(|c| c.len() as f64 <= worst * frames_len as f64);
    let mut f = Formula::default();
    // the audio: from the AC-3 module where the AC-3 stream comes to under 97% by it, from the MP3 module where the MP3
    // stream comes to less, else as it is
    let recoded = if v.ac3 {
        aqr_ac3::formula::encode_with(&audio, threads).map(|coded| {
            let input = f.push(aqr_ac3::formula::CODED, coded);
            Node::apply(aqr_ac3::formula::MODULE, vec![], vec![input], audio.len() as u64)
        })
    } else if v.mp3 {
        aqr_mp3::formula::encode(&audio, threads).map(|g| f.embed(g))
    } else {
        None
    };
    if coded.is_none() && recoded.is_none() && worst.is_finite() {
        return None;
    }
    let (frames, (width, height)) = match coded {
        Some(c) => (f.push(CODED, c), (v.width, v.height)),
        None => {
            let kept: Vec<u8> = v.frames.iter().flat_map(|r| input[r.clone()].iter().copied()).collect();
            (f.push_as(KEPT, Kind::Compressed, kept), (0, 0))
        }
    };
    let (s, l) = (f.push(SKELETON, skeleton), f.push(LAYOUT, layout));
    let audio_node = recoded.unwrap_or_else(|| f.push(AUDIO, audio));
    let mut params = Vec::new();
    params.extend_from_slice(&width.to_le_bytes());
    params.extend_from_slice(&height.to_le_bytes());
    f.root = Node::apply(MODULE, params, vec![frames, s, l, audio_node], input.len() as u64);
    Some(f)
}

/// The frames of `chunks` coded by the model, the chunks on `threads` at once, after the table of chunks.
fn code(input: &[u8], v: &Video, chunks: &[Vec<Range<usize>>], threads: usize) -> Vec<u8> {
    let parts = aqr_common::pool::par_map(chunks.len(), threads, |i| {
        let mut codec = Codec::new(v.width as usize, v.height as usize);
        let mut enc = Encoder::new();
        for r in &chunks[i] {
            codec.encode(&mut enc, &input[r.clone()]);
        }
        enc.finish()
    });
    let mut coded = Vec::new();
    aqr_walk::chunks::put(&mut coded, chunks.iter().zip(&parts).map(|(c, part)| (c.len() as u64, part.len() as u64)));
    for part in &parts {
        coded.extend_from_slice(part);
    }
    coded
}
