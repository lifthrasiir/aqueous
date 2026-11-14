//! The code of the module `mca-recons` (see `formula`, and FORMAT.md), which needs no
//! aqr-formula, so that aqr-modules builds it into its Wasm module. It is a part module: the
//! inputs taken in order in pieces and the file put in order, a chunk at a time; natively it is
//! applied whole, the chunks' zlib streams made on threads.

use aqr_api::stack::{Sim, block};
use aqr_api::stream::{In, Put, Reader, Slice, Written};
use aqr_flate::plan::wasm::{JOB, LANE, TOKEN, WINDOW_SLACK};
use aqr_flate::zlib::Params;

use crate::layout::{self, Chunks, PIECE, SMALL_PIECE, Streamed};
use crate::region::{self, CHUNKS, HEADER, SECTOR};
use crate::{Result, corrupt};

/// Pages of the windows of the module as a part module ([`decode_part`]): of the data, the
/// bytes kept and the output; and of the header and the layout.
pub const WINDOW: u32 = 16;
pub const SMALL_WINDOW: u32 = 1;

/// Bytes of a [`region::Chunk`] on WebAssembly.
const CHUNK: u64 = 8;

/// The memory plan of `mca-recons` as a part module ([`aqr_api::stack`]): what [`decode_part`]
/// takes beyond its initial memory (`initial` bytes), of the header `header`, the layout `layout`
/// and an output of `size` bytes: the header, the chunks, the pieces of the readers, then for the
/// largest chunk rebuilt its data, its sectors and zlib's emulator. Of the largest chunk, not of
/// the file.
pub fn plan(initial: u64, params: &[u8], header: &[u8], layout: &[u8], size: u64) -> Option<u64> {
    let p = layout::params(params).ok()?;
    let chunks = region::chunks(header.get(..HEADER)?, size / SECTOR as u64)?;
    let mut m = Sim::new(initial);
    m.take(HEADER as u64);
    m.take(CHUNK * CHUNKS as u64);
    m.take(SMALL_PIECE as u64);
    m.take(PIECE as u64);
    m.take(PIECE as u64);
    let mut most = 0;
    // each rebuilt chunk's: its sectors, its data and the emulator
    let mut at = 0;
    for c in &chunks {
        let tag = aqr_common::varint::get(layout, &mut at).ok()?;
        if tag & layout::HOW == layout::REBUILT {
            let (n, sectors) = (tag >> layout::SHIFT, c.count as u64 * SECTOR as u64);
            most = most.max(block(sectors + aqr_flate::write::TOKEN_SLACK as u64) + block(n) + emulator(p, n));
        }
    }
    m.peak(most);
    Some(m.plan())
}

/// The blocks that zlib's emulator takes over `n` bytes of data: its window, its heads and links,
/// a lane and a job, and the tokens of a block.
fn emulator(p: Params, n: u64) -> u64 {
    let w = 1u64 << p.wbits.clamp(9, 15);
    let bits = p.mem.clamp(1, 9) as u32 + 7;
    let tokens = block(TOKEN * (p.block_tokens() as u64).min(n + 1));
    block(2 * w + WINDOW_SLACK) + block(2 << bits) + block(2 * w) + block(LANE) + block(JOB) + tokens
}

/// The module: the region file of `len` bytes made of `inputs`: the header, the layout, the
/// chunks' data and the bytes kept; the chunks' zlib streams made on up to `threads` threads.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let &[header, layout, data, kept] = inputs else { return Err(corrupt()) };
    let p = layout::params(params)?;
    if header.len() != HEADER {
        return Err(corrupt());
    }
    // the chunks' data, and the streams of those rebuilt made at once
    let mut parts = Vec::new();
    let mut at = 0u64;
    for (n, rebuilt) in layout::made(header, layout, len)? {
        let end = at.checked_add(n).filter(|&e| e <= data.len() as u64).ok_or_else(corrupt)?;
        if rebuilt {
            parts.push(at as usize..end as usize);
        }
        at = end;
    }
    let made =
        aqr_common::pool::par_map(parts.len(), threads, |i| layout::zlib_onto(&data[parts[i].clone()], p, Vec::new()));
    let mut out = Vec::with_capacity(usize::try_from(len).map_err(|_| corrupt())?);
    let mut chunks = Made { data, at: 0, made: made.into_iter() };
    let (mut layout, mut kept) = (Slice(layout), Slice(kept));
    let (mut layout, mut kept) = (Reader::new(&mut layout, SMALL_PIECE), Reader::new(&mut kept, PIECE));
    layout::build_stream(header, &mut layout, &mut chunks, &mut kept, len, &mut out)?;
    Ok(out)
}

/// The chunks' data of a slice, the zlib streams made before.
struct Made<'a> {
    data: &'a [u8],
    at: usize,
    made: std::vec::IntoIter<Result<Vec<u8>>>,
}

impl Chunks for Made<'_> {
    fn rebuilt(&mut self, n: u64, mut out: Vec<u8>) -> Result<Vec<u8>> {
        self.at = usize::try_from(n).ok().and_then(|n| self.at.checked_add(n)).ok_or_else(corrupt)?;
        out.extend_from_slice(&self.made.next().ok_or_else(corrupt)??);
        Ok(out)
    }

    fn stored(&mut self, n: u64, out: &mut dyn Put) -> Result<()> {
        let end = usize::try_from(n).ok().and_then(|n| self.at.checked_add(n)).ok_or_else(corrupt)?;
        let d = self.data.get(self.at..end).ok_or_else(corrupt)?;
        self.at += d.len();
        out.put(d).ok_or_else(corrupt)
    }

    fn done(&mut self) -> bool {
        self.at == self.data.len() && self.made.len() == 0
    }
}

/// The module as a part module (its entry point, of `aqr_api::entry!`): what [`decode`] gives,
/// its inputs taken and its output put in order through windows ([`WINDOW`]), a chunk at a time.
pub fn decode_part(params_len: u64, header_len: u64, layout_len: u64, data_len: u64, kept_len: u64, size: u64) {
    aqr_api::finish(part(params_len, [header_len, layout_len, data_len, kept_len], size));
}

fn part(params_len: u64, lens: [u64; 4], size: u64) -> Result<()> {
    use aqr_api::Memory;
    let mut p = [0; 4];
    if params_len != 4 || lens[0] != HEADER as u64 {
        return Err(corrupt());
    }
    aqr_api::Params::read(0, &mut p);
    let params = layout::params(&p)?;
    let header = layout::read_header(&mut In::<0>::new(lens[0]))?;
    let (mut l, mut d, mut k) = (In::<1>::new(lens[1]), In::<2>::new(lens[2]), In::<3>::new(lens[3]));
    let mut layout = Reader::new(&mut l, SMALL_PIECE);
    let data = Reader::new(&mut d, PIECE);
    let mut kept = Reader::new(&mut k, PIECE);
    let mut out = Written::new(size);
    let mut chunks = Streamed { data, left: lens[2], params };
    layout::build_stream(&header, &mut layout, &mut chunks, &mut kept, size, &mut out)?;
    match out.total() == size {
        true => Ok(()),
        false => Err(corrupt()),
    }
}
