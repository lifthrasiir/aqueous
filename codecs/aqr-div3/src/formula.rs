//! aqr-div3 as a module of entry formulas (see aqr-formula): an AVI file whose video is DivX 3, rebuilt from its
//! frames coded by the model, the rest of its bytes (the skeleton) and the frames' places (the layout).
//! `FORMAT.md` specifies them.
use std::vec;
use std::vec::Vec;

use aqr_common::varint;
use aqr_formula::{Formula, Module, Node};

use crate::module::FRAME_MAX;
use crate::{Codec, Encoder};

pub use crate::module::decode;

/// The classes of the streams.
pub const CODED: &str = "div3/frames";
pub const SKELETON: &str = "div3/skeleton";
pub const LAYOUT: &str = "div3/layout";

/// The module `div3-recons`: params the width and height as two 16-bit numbers; inputs the coded frames, the skeleton
/// and the layout.
pub const MODULE: Module = Module::new("div3-recons", 1).with_plan(memory);

fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let &[_, _, _] = u.inputs else { return None };
    Some(crate::module::plan(u.initial()))
}

/// The video of an AVI file: its size, and where its frames are.
pub struct Video {
    pub width: u16,
    pub height: u16,
    pub frames: Vec<core::ops::Range<usize>>,
}

fn u32le(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

/// The DivX 3 video of AVI file `d`, if it has one.
pub fn video(d: &[u8]) -> Option<Video> {
    if d.len() < 12 || &d[..4] != b"RIFF" || &d[8..12] != b"AVI " {
        return None;
    }
    // the streams in the header list, in order: the first video stream of DivX 3 or its aliases
    let mut stream = None;
    let (mut index, mut width, mut height) = (0usize, 0u16, 0u16);
    let mut pos = d.windows(4).position(|w| w == b"hdrl")? + 4;
    let end = pos + u32le(d, pos - 8)? as usize;
    let mut cur: Option<(bool, bool)> = None; // (is video, is DivX 3)
    while pos + 8 <= end.min(d.len()) {
        let (id, sz) = (&d[pos..pos + 4], u32le(d, pos + 4)? as usize);
        match id {
            b"LIST" => {
                if d.get(pos + 8..pos + 12)? == b"strl" {
                    if cur.is_some() {
                        index += 1;
                    }
                    cur = Some((false, false));
                }
                pos += 12;
                continue;
            }
            b"strh" => {
                let video = d.get(pos + 8..pos + 12)? == b"vids";
                let h = d.get(pos + 12..pos + 16)?.to_ascii_lowercase();
                cur = Some((video, video && matches!(&h[..], b"div3" | b"mp43" | b"div4" | b"div5" | b"div6")));
            }
            b"strf" => {
                if cur == Some((true, true)) && stream.is_none() {
                    let w = u32le(d, pos + 12)? as i32;
                    let h = u32le(d, pos + 16)? as i32;
                    if w <= 0 || h == 0 || w > 4096 || h.abs() > 4096 {
                        return None;
                    }
                    (stream, width, height) = (Some(index), w as u16, h.unsigned_abs() as u16);
                }
            }
            _ => {}
        }
        pos += 8 + sz + (sz & 1);
    }
    let stream = stream?;
    let tag = [b'0' + (stream / 10) as u8, b'0' + (stream % 10) as u8, b'd', b'c'];
    let mut pos = d.windows(4).position(|w| w == b"movi")? + 4;
    let mut frames = Vec::new();
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
        if id == tag {
            frames.push(pos + 8..pos + 8 + sz);
        }
        pos += 8 + sz + (sz & 1);
    }
    Some(Video { width, height, frames })
}

/// The formula of AVI file `input` with DivX 3 video, or None where it has none, or its frames do not come to less
/// than 97% by the model. The same whatever `threads` is (none is used).
pub fn encode(input: &[u8], _threads: usize) -> Option<Formula> {
    let v = video(input)?;
    if v.frames.len() < 4 || v.frames.iter().any(|f| f.len() > FRAME_MAX) {
        return None;
    }
    let mut codec = Codec::new(v.width as usize, v.height as usize);
    let mut enc = Encoder::new();
    let (mut skeleton, mut layout) = (Vec::with_capacity(input.len() / 2), Vec::new());
    let (mut p, mut frames_len) = (0, 0usize);
    for f in &v.frames {
        varint::put(&mut layout, (f.start - p) as u64);
        varint::put(&mut layout, f.len() as u64);
        skeleton.extend_from_slice(&input[p..f.start]);
        codec.encode(&mut enc, &input[f.clone()]);
        frames_len += f.len();
        p = f.end;
    }
    skeleton.extend_from_slice(&input[p..]);
    let coded = enc.finish();
    if coded.len() as f64 > 0.97 * frames_len as f64 {
        return None;
    }
    let mut f = Formula::default();
    let inputs = vec![f.push(CODED, coded), f.push(SKELETON, skeleton), f.push(LAYOUT, layout)];
    let mut params = Vec::new();
    params.extend_from_slice(&v.width.to_le_bytes());
    params.extend_from_slice(&v.height.to_le_bytes());
    f.root = Node::apply(MODULE, params, inputs, input.len() as u64);
    Some(f)
}
