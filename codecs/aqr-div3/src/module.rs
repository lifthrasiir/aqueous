//! The code of the module `div3-recons` (see `formula`, and `FORMAT.md`), which needs no aqr-formula,
//! so that aqr-modules builds it into its Wasm module.
use alloc::vec::Vec;
use core::cell::RefCell;

use aqr_api::stream::{In, Pull, Put, Slice, Written};
use aqr_walk::chunks;
use aqr_walk::model::Model;
use aqr_walk::reader::Reader;

use crate::frame::MODEL_BITS;
use crate::{Codec, Decoder, Error, Result, corrupt};

/// Pages of the windows of `div3-recons` as a part module: of each input and of the output.
pub const WINDOW: u32 = 4;
/// The most bytes of a frame the module takes.
pub const FRAME_MAX: usize = 4 << 20;
/// Bytes of each buffer reading an input.
const BUF: usize = 16 << 10;

/// The memory plan of `div3-recons` as a part module ([`aqr_api::stack`]): what [`decode_part`] takes beyond
/// its initial memory (`initial` bytes) at most, whatever the inputs: the model and the frame (not where the frames are
/// kept: width and height 0), with the trees of its VLCs and what a frame's walk keeps of the picture of `width` by
/// `height`, and four buffers. None where the params are not two sizes.
pub fn plan(initial: u64, params: &[u8]) -> Option<u64> {
    let (w, h) = dims(params).ok()?;
    let mut m = aqr_api::stack::Sim::new(initial);
    if !kept(params) {
        m.take(Model::bytes(MODEL_BITS));
        crate::frame::codec_bytes(w, h).into_iter().for_each(|n| m.take(n));
        m.take(FRAME_MAX as u64 + 8);
    }
    m.take(BUF as u64);
    m.take(BUF as u64);
    m.take(BUF as u64);
    m.take(BUF as u64);
    Some(m.plan())
}

/// The file of `size` bytes from the coded frames (or those kept, where `width` and `height` are 0), the skeleton,
/// the layout and the audio (see `FORMAT.md`), put to `out`. The chunks are decoded one after the other here;
/// `formula::decode` does it at once natively.
#[allow(clippy::too_many_arguments)]
pub fn run(
    coded: &mut dyn Pull,
    coded_len: u64,
    skeleton: &mut dyn Pull,
    layout: &mut dyn Pull,
    audio: &mut dyn Pull,
    out: &mut dyn Put,
    width: usize,
    height: usize,
    size: u64,
) -> Result<()> {
    let kept = width == 0 && height == 0;
    // (in the order of the plan)
    let mut codec = (!kept).then(|| Codec::new(width, height));
    let mut frame: Vec<u8> = Vec::with_capacity(if kept { 0 } else { FRAME_MAX + 8 });
    let cod = RefCell::new(Reader::new(coded, BUF));
    let mut skel = Reader::new(skeleton, BUF);
    let mut lay = Reader::new(layout, BUF);
    let mut aud = Reader::new(audio, BUF);
    let chunks = match kept {
        true => Vec::new(),
        false => chunks::read(&mut cod.borrow_mut(), coded_len).map_err(|_| corrupt())?,
    };
    let mut chunks = chunks.into_iter();
    let mut dec: Option<Decoder<&RefCell<Reader>>> = None;
    let mut left = 0u64;
    let mut written = 0u64;
    while let Some(gap) = lay.varint().map_err(|_| corrupt())? {
        let tag = lay.varint().map_err(|_| corrupt())?.ok_or_else(corrupt)?;
        let n = tag >> 1;
        if written.checked_add(gap).and_then(|w| w.checked_add(n)).is_none_or(|w| w > size) {
            return Err(corrupt());
        }
        if !skel.copy(gap, out) {
            return Err(Error("input of another length"));
        }
        if tag & 1 == 0 {
            if n as usize > FRAME_MAX {
                return Err(corrupt());
            }
            let Some(codec) = codec.as_mut() else {
                if !cod.borrow_mut().copy(n, out) {
                    return Err(Error("input of another length"));
                }
                written += gap + n;
                continue;
            };
            if left == 0 {
                if dec.is_some() {
                    cod.borrow_mut().skip_rest();
                }
                let (frames, bytes) = chunks.next().ok_or_else(corrupt)?;
                // (the encoder makes no chunk of no frames or of no bytes, and each starts a model afresh)
                if frames == 0 || bytes == 0 {
                    return Err(corrupt());
                }
                cod.borrow_mut().limit(bytes);
                codec.reset();
                dec = Some(Decoder::new(&cod));
                left = frames;
            }
            left -= 1;
            let rc = dec.as_mut().ok_or_else(corrupt)?;
            frame = codec.decode(rc, n as usize, frame).map_err(|_| corrupt())?;
            // (a chunk read past its end decodes zeros: no frames out of a few bytes)
            if rc.past_end() {
                return Err(corrupt());
            }
            out.put(&frame).ok_or(Error("output of another length"))?;
        } else if !aud.copy(n, out) {
            return Err(Error("input of another length"));
        }
        written += gap + n;
    }
    if !skel.copy(size - written, out) {
        return Err(Error("input of another length"));
    }
    let coded_left = kept && cod.borrow_mut().next().is_some();
    if left != 0 || chunks.next().is_some() || coded_left || skel.next().is_some() || aud.next().is_some() {
        return Err(Error("input of another length"));
    }
    Ok(())
}

/// Whether `params` are of frames kept as they are (width and height 0).
pub fn kept(params: &[u8]) -> bool {
    params == [0; 4]
}

pub(crate) fn dims(params: &[u8]) -> Result<(usize, usize)> {
    let &[a, b, c, d] = params else { return Err(corrupt()) };
    Ok((u16::from_le_bytes([a, b]) as usize, u16::from_le_bytes([c, d]) as usize))
}

/// The module `div3-recons` on whole inputs, the chunks one after the other.
pub fn decode_serial(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let &[coded, skeleton, layout, audio] = inputs else { return Err(corrupt()) };
    let (w, h) = dims(params)?;
    let mut out = Vec::new();
    run(
        &mut Slice(coded),
        coded.len() as u64,
        &mut Slice(skeleton),
        &mut Slice(layout),
        &mut Slice(audio),
        &mut out,
        w,
        h,
        len,
    )?;
    if out.len() as u64 != len {
        return Err(Error("output of another length"));
    }
    Ok(out)
}

/// The module `div3-recons` as a part module (its entry point, of `aqr_api::entry!`): the inputs taken and the
/// output put in order through windows ([`WINDOW`]).
pub fn decode_part(params_len: u64, in0_len: u64, in1_len: u64, in2_len: u64, in3_len: u64, size: u64) {
    aqr_api::finish(part(params_len, in0_len, in1_len, in2_len, in3_len, size));
}

fn part(params_len: u64, in0_len: u64, in1_len: u64, in2_len: u64, in3_len: u64, size: u64) -> Result<()> {
    use aqr_api::Memory;
    let mut p = [0; 4];
    if params_len != 4 {
        return Err(corrupt());
    }
    aqr_api::Params::read(0, &mut p);
    let (w, h) = dims(&p)?;
    let mut out = Written::new(size);
    run(
        &mut In::<0>::new(in0_len),
        in0_len,
        &mut In::<1>::new(in1_len),
        &mut In::<2>::new(in2_len),
        &mut In::<3>::new(in3_len),
        &mut out,
        w,
        h,
        size,
    )?;
    match out.total() == size {
        true => Ok(()),
        false => Err(Error("output of another length")),
    }
}
