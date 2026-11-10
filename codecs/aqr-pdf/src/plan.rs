//! The memory plan of the module `pdf-recons` on WebAssembly ([`aqr_api::stack`]), as
//! [`crate::module::decode`] lays it out: the instance's memory taken block by block in the order
//! the module takes it ([`Sim`]). The module spawns no tasks. What it takes follows from the table
//! (its items, its token streams), the token coder's tables and the streams' layouts, which are
//! read as the decoder reads them.

use aqr_api::stack::{Sim, block, whole};

use crate::crypt::Crypt;
use crate::module::{iv_len, scan_table_with};
use crate::rans::BitR;
use crate::tok::{self, DIST_ALPHA, LIT_ALPHA, Pixels};

use crate::module::wasm;

/// The memory plan of `pdf-recons` ([`crate::module::decode`]): of its initial memory (`initial`
/// bytes), parameters `params`, inputs of `inputs` bytes, an output of `len`, and the inputs
/// themselves.
pub fn decode(initial: u64, params: &[u8], inputs: &[u64], len: u64, files: [&[u8]; 11]) -> Option<u64> {
    let crypt = Crypt::from_params(params).ok()?;
    let [table, _skel, _raw, deflate, png, jpeg, layout, tb, st, raw, headers] = files;
    let mut m = Sim::new(initial);
    m.take(whole(params.len() as u64, &[], inputs));
    // (`DecoderState::new`: the contexts, a table's room, the decoding tables, the frequencies and
    // where each table starts, which go once the decoding tables are built)
    if [layout, tb, st, raw, headers].iter().any(|p| !p.is_empty()) {
        m.take(tok::ctx_bytes());
        let (lit, dist) = tok::DecoderState::count_tables(tb).ok()?;
        m.peak(block(wasm::WORD * LIT_ALPHA as u64));
        let n = (lit + dist + 1) as u64;
        let alphabet = if lit > 0 {
            LIT_ALPHA
        } else if dist > 0 {
            DIST_ALPHA
        } else {
            1
        };
        let shift = alphabet.next_power_of_two().trailing_zeros();
        m.take(2 * (n << crate::rans::PROB_BITS));
        m.take(4 * (n << shift));
        m.peak(block(4 * (lit * LIT_ALPHA + dist * DIST_ALPHA + 1) as u64) + block(wasm::WORD * (n + 1)));
    }
    // (the packed slots unpacked, in order)
    for p in [deflate, png, jpeg] {
        let n = u64::from_le_bytes(*p.first_chunk()?);
        m.take(wasm::ITEM * n.min(p.len() as u64 / 8));
    }
    let size = usize::try_from(len).ok()?;
    let mut streams = vec![];
    let items = scan_table_with(table, size, iv_len(&crypt), |plain, px| streams.push((plain, px)))?;
    m.take(len);
    m.take(wasm::PLACED * items as u64);
    // (the streams one at a time: the layout's blocks and alts, the window, the writer and the
    // tokens of a block)
    let (mut l, bits) = (BitR::new(layout), layout.len() * 8);
    for (plain, px) in streams {
        let px = px.map(|(bpp, stride)| Pixels { bpp: bpp as usize, stride: stride as usize });
        let sl = tok::read_layout(&mut l, bits, plain).ok()?;
        let stored =
            |b: &(aqr_flate::deflate::Block, usize)| matches!(b.0.body, aqr_flate::deflate::Body::Stored { .. });
        let tokens = sl.blocks.iter().filter(|b| !stored(b)).map(|b| b.1).max().unwrap_or(0);
        let room = tok::writer_room(sl.blocks.iter().map(|b| (stored(b), b.1)));
        m.peak(
            block(wasm::BLOCK * sl.blocks.len() as u64)
                + block(wasm::WORD * sl.alts.len() as u64)
                + block(tok::window_room(px, plain) as u64)
                + block(room as u64)
                + block(wasm::TOKEN * tokens as u64),
        );
    }
    Some(m.plan())
}
