//! The code of the module `ogg-recons` (FORMAT.md §3), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use crate::layout::{Layout, Packets, Walk};
use crate::{Result, corrupt, page};

/// Each stream's packets, lacing values and durations.
type Streams = (Vec<Packets>, Vec<Vec<u8>>, Vec<Vec<u64>>);

/// The streams' packets, lacing values and durations from the packets input `d` cut by the
/// streams' bytes in `layout`.
pub fn streams(layout: &Layout, d: &[u8]) -> Result<Streams> {
    let (mut packets, mut lacing, mut durations) = (Vec::new(), Vec::new(), Vec::new());
    let mut at = 0usize;
    for s in &layout.streams {
        let end = usize::try_from(s.bytes).ok().and_then(|n| at.checked_add(n)).ok_or_else(corrupt)?;
        let p = Packets::unframe(d.get(at..end).ok_or_else(corrupt)?)?;
        lacing.push(p.lacing(s.open)?);
        durations.push(s.predictor.durations(&p)?);
        packets.push(p);
        at = end;
    }
    if at != d.len() { Err(corrupt()) } else { Ok((packets, lacing, durations)) }
}

/// The module: the Ogg file of `len` bytes from `inputs`, the layout record and the packets.
/// No params.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let &[layout, packets] = inputs else { return Err(corrupt()) };
    if !params.is_empty() {
        return Err(corrupt());
    }
    let l = Layout::read(layout, len)?;
    let (packets, lacing, durations) = streams(&l, packets)?;
    let pages: Vec<usize> = l.pages.iter().map(|p| p.stream).collect();
    let mut seen = vec![false; l.streams.len()];
    pages.iter().for_each(|&s| seen[s] = true);
    if seen.contains(&false) {
        return Err(corrupt());
    }
    let mut w = Walk::new(&packets, &lacing, &durations, &pages);
    let len = usize::try_from(len).map_err(|_| corrupt())?;
    let mut out = Vec::with_capacity(len);
    for p in &l.pages {
        let step = w.page(p.stream, p.segments as usize)?;
        let (seq, granule) = (step.seq.wrapping_add(p.seq), step.granule.wrapping_add(p.granule));
        w.settle(p.stream, &step, seq, granule);
        let (values, body) = w.parts(p.stream, &step);
        if out.len() + page::HEADER + values.len() + body.len() > len {
            return Err(corrupt());
        }
        page::write(&mut out, step.kind ^ p.kind, granule, l.streams[p.stream].serial, seq, values, body);
    }
    if !w.done() || out.len() != len { Err(corrupt()) } else { Ok(out) }
}
