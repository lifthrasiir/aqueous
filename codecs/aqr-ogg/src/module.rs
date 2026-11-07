//! The code of the module `ogg-recons` (FORMAT.md §3), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.
//!
//! The module takes the memory its plan ([`crate::plan`]) says, in this order: the CRC's tables,
//! the layout's lists, the streams' packets (all streams in one vector of each kind), the walk's
//! list, and the output; every vector reserved at its size.

use crate::layout::{self, Layout, PacketsRef, Source, Walk};
use crate::{Result, corrupt, crc, page};

/// What the packets input holds, in all streams: the bytes of the packets, their count, and the
/// lacing values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Totals {
    pub bytes: usize,
    pub packets: usize,
    pub lacing: usize,
}

/// Counts what the packets input `d` holds, cut by the streams' bytes in `layout`.
pub fn totals(layout: &Layout, d: &[u8]) -> Result<Totals> {
    let mut t = Totals { bytes: 0, packets: 0, lacing: 0 };
    let mut at = 0usize;
    for s in &layout.streams {
        let end = usize::try_from(s.bytes).ok().and_then(|n| at.checked_add(n)).ok_or_else(corrupt)?;
        let (bytes, packets) = layout::unframed(d.get(at..end).ok_or_else(corrupt)?)?;
        // (the room of the lacing values: see `PacketsRef::lacing_len`)
        t.lacing += bytes / 255 + packets - (s.open && packets > 0) as usize;
        (t.bytes, t.packets) = (t.bytes + bytes, t.packets + packets);
        at = end;
    }
    if at != d.len() { Err(corrupt()) } else { Ok(t) }
}

/// The streams' packets, lacing values and durations, of all streams one after another in a
/// vector of each.
pub struct Streams {
    /// The start of each stream in `bytes`, `ends` (and `durations`) and `lacing`, and the end of
    /// all.
    at: Vec<[usize; 3]>,
    durations: Vec<u64>,
    lacing: Vec<u8>,
    /// The packets' ends, in the bytes of their stream.
    ends: Vec<usize>,
    bytes: Vec<u8>,
}

impl Streams {
    /// The streams from the packets input `d` cut by the streams' bytes in `layout`.
    pub fn read(layout: &Layout, d: &[u8]) -> Result<Streams> {
        let t = totals(layout, d)?;
        let mut s = Streams {
            at: Vec::with_capacity(layout.streams.len() + 1),
            durations: vec![0; t.packets],
            lacing: Vec::with_capacity(t.lacing),
            ends: Vec::with_capacity(t.packets),
            bytes: Vec::with_capacity(t.bytes),
        };
        let mut from = 0usize;
        for st in &layout.streams {
            // (`totals` checked these)
            let to = from + st.bytes as usize;
            let start = [s.bytes.len(), s.ends.len(), s.lacing.len()];
            s.at.push(start);
            layout::unframe_into(&d[from..to], &mut s.bytes, &mut s.ends)?;
            let p = PacketsRef { bytes: &s.bytes[start[0]..], ends: &s.ends[start[1]..] };
            p.lacing_into(st.open, &mut s.lacing)?;
            st.predictor.durations_into(p, &mut s.durations[start[1]..p.ends.len() + start[1]])?;
            from = to;
        }
        s.at.push([s.bytes.len(), s.ends.len(), s.lacing.len()]);
        Ok(s)
    }

    pub fn len(&self) -> usize {
        self.at.len() - 1
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Stream `s`.
    pub fn source(&self, s: usize) -> Source<'_> {
        let (a, b) = (self.at[s], self.at[s + 1]);
        Source {
            bytes: &self.bytes[a[0]..b[0]],
            lacing: &self.lacing[a[2]..b[2]],
            durations: &self.durations[a[1]..b[1]],
        }
    }
}

/// The module: the Ogg file of `len` bytes from `inputs`, the layout record and the packets.
/// No params.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let &[layout, packets] = inputs else { return Err(corrupt()) };
    if !params.is_empty() {
        return Err(corrupt());
    }
    // (the output first: all else is freed above it, last made first)
    let len = usize::try_from(len).map_err(|_| corrupt())?;
    let mut out = Vec::with_capacity(len);
    // (the CRC's tables now, not at the first page: the plan has them here)
    crc::init();
    let l = Layout::read(layout, len as u64)?;
    let st = Streams::read(&l, packets)?;
    let mut w = Walk::new((0..st.len()).map(|s| st.source(s)), l.pages.iter().map(|p| p.stream));
    if !w.all_paged() {
        return Err(corrupt());
    }
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

/// Bytes of the structures the module holds in vectors, on WebAssembly (32-bit pointers;
/// checked there by assertions): what its memory plan ([`crate::plan`]) is made of.
pub mod wasm {
    /// A page of the layout, and a stream of the walk. (A stream of the layout holds no
    /// pointers and is as large everywhere.)
    pub const PAGE: u64 = 24;
    pub const WALKED: u64 = 64;
}

#[cfg(target_family = "wasm")]
const _: () = {
    assert!(size_of::<layout::Page>() == wasm::PAGE as usize);
    assert!(layout::WALKED_SIZE == wasm::WALKED as usize);
};
