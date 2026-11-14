//! The module `div3-recons` natively on whole inputs: the chunks of frames decoded at once on
//! threads, then the file put together from the skeleton, the frames and the audio.

use alloc::vec::Vec;

use aqr_common::varint;
use aqr_walk::chunks;

use crate::module::{FRAME_MAX, dims};
use crate::{Codec, Decoder, Error, Result, SliceSource, corrupt};

/// The module `div3-recons` on whole inputs, natively: the chunks of frames decoded at once on `threads`, then the file
/// put together from the skeleton, the frames and the audio.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let &[coded, skeleton, layout, audio] = inputs else { return Err(corrupt()) };
    let (w, h) = dims(params)?;
    // the layout: (gap, length, is audio)
    let mut items = Vec::new();
    let mut pos = 0;
    while pos < layout.len() {
        let gap = varint::get(layout, &mut pos).map_err(|_| corrupt())?;
        let tag = varint::get(layout, &mut pos).map_err(|_| corrupt())?;
        items.push((gap as usize, (tag >> 1) as usize, tag & 1 == 1));
    }
    // the table of chunks
    let mut pos = 0;
    let table = chunks::read_at(coded, &mut pos).map_err(|_| corrupt())?;
    // the frames' lengths of each chunk, and where its coded bytes are
    let mut lens = items.iter().filter(|i| !i.2).map(|i| i.1);
    let mut chunks = Vec::new();
    let mut at = pos;
    for &(frames, bytes) in &table {
        let (frames, bytes) = (frames as usize, bytes as usize);
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
    let mut out = Vec::with_capacity(skeleton.len() + audio.len() + decoded.iter().map(Vec::len).sum::<usize>());
    let (mut sk, mut au) = (0usize, 0usize);
    let (mut chunk, mut cpos) = (0usize, 0usize);
    for &(gap, n, is_audio) in &items {
        out.extend_from_slice(skeleton.get(sk..sk.checked_add(gap).ok_or_else(corrupt)?).ok_or_else(corrupt)?);
        sk += gap;
        if is_audio {
            out.extend_from_slice(audio.get(au..au.checked_add(n).ok_or_else(corrupt)?).ok_or_else(corrupt)?);
            au += n;
        } else {
            while decoded.get(chunk).is_some_and(|d| cpos == d.len()) {
                chunk += 1;
                cpos = 0;
            }
            out.extend_from_slice(
                decoded.get(chunk).and_then(|d| d.get(cpos..cpos.checked_add(n)?)).ok_or_else(corrupt)?,
            );
            cpos += n;
        }
    }
    out.extend_from_slice(&skeleton[sk..]);
    if out.len() as u64 != len || au != audio.len() {
        return Err(Error("output of another length"));
    }
    Ok(out)
}
