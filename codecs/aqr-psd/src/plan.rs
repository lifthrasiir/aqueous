//! The memory plan of the module `psd-recons` on WebAssembly ([`aqr_api::stack`]), as
//! [`crate::codec::decode_formula`] lays it out: the instance's memory taken block by block in
//! the order the module takes it ([`Sim`]). What the module takes follows from its head (read here
//! as the module reads it, [`get_head`]), the lengths of its inputs and output, and for a group
//! coded with a hint the structure of the file before its composite (parsed and rendered as the
//! module does it, from the skeleton alone: the pixels do not change it).

use aqr_api::stack::{Sim, block};

use crate::codec::{Coding, GroupDef, Head, Span, get_head, group_split};
use crate::composite::{self, Report, RowRenderer};
use crate::module::wasm;
use crate::rans::Tables;

/// The room of a vector grown by pushes to hold `n` items (of no more than a kilobyte): none for
/// none, else four at least and a power of two.
fn grown(n: usize) -> u64 {
    if n == 0 { 0 } else { n.next_power_of_two().max(4) as u64 }
}

/// The memory plan of `psd-recons` ([`crate::module::decode`]): of its initial memory (`initial`
/// bytes), parameters `params`, inputs of `inputs` bytes, an output of `len`, and the head,
/// skeleton and model streams.
pub fn decode(
    initial: u64,
    params: &[u8],
    inputs: &[u64],
    len: u64,
    (head, skeleton, model): (&[u8], &[u8], &[u8]),
) -> Option<u64> {
    if !params.is_empty() || inputs.len() != 4 {
        return None;
    }
    let total = usize::try_from(len).ok().filter(|&t| t as u64 <= 1 << 42)?;
    let Head { spans, groups, .. } = &get_head(head, total).ok()?;
    let mut m = Sim::new(initial);
    m.take(aqr_api::stack::whole(0, &[], inputs));
    // the file, then what `get_head` makes
    m.take(len);
    m.take(wasm::SPAN * spans.len() as u64);
    for s in spans.iter() {
        if let Coding::Rle { plane_lens, exceptions, .. } = &s.coding {
            m.take(4 * plane_lens.len() as u64);
            m.take(wasm::EXCEPTION * exceptions.len() as u64);
        }
    }
    m.take(8 * spans.len() as u64);
    m.take(wasm::GROUP * groups.len() as u64);
    for g in groups.iter() {
        m.take(wasm::PLANE_REF * g.planes.len() as u64);
        m.take(wasm::OPT_INDEX * g.same_as.len() as u64);
    }
    // `decode_pixels`: what is done, the planes left, and the model's player
    m.take(spans.len() as u64);
    m.take(4 * spans.len() as u64);
    let room = crate::model::player_tables(model.len());
    let [lut, fc] = Tables::bytes(room, 256);
    m.take(2 * crate::model::TABLES as u64);
    m.take(4 * 256 * (room as u64 - 1));
    m.take(lut);
    m.take(fc);
    for g in groups.iter() {
        let top = m.top;
        group(&mut m, g, spans, total, skeleton)?;
        m.top = top;
    }
    Some(m.plan())
}

/// `n` rows of `w` bytes, as `model::zero_rows` makes them.
fn rows(m: &mut Sim, n: usize, w: usize) {
    m.take(wasm::VEC * n as u64);
    (0..n).for_each(|_| m.take(w as u64));
}

/// What `decode_group` makes of its blocks for group `g`, in its order.
fn group(m: &mut Sim, g: &GroupDef, spans: &[Span], total: usize, skeleton: &[u8]) -> Option<()> {
    let n = g.planes.len();
    let w = spans[g.planes[0].0].w;
    m.take(n as u64);
    let rle = g.planes.iter().filter(|&&(s, _)| matches!(spans[s].coding, Coding::Rle { .. })).count();
    m.take((rle * w) as u64);
    m.take(wasm::PLANE_ROWS * n as u64);
    m.take(wasm::ROW_SINK * n as u64);
    if g.hinted {
        hint(m, spans, group_split(g, spans, total), skeleton, w)?;
    }
    m.take(wasm::OPT_INDEX * n as u64);
    for _ in 0..4 {
        rows(m, n, w);
    }
    rows(m, if g.hinted { n } else { 0 }, w);
    rows(m, n, w);
    m.take(2 * w as u64);
    if g.hinted {
        // (the references to the hint's rows, made for each row)
        m.peak(block(8 * n as u64));
    }
    Some(())
}

/// What `hint_psd` and the renderer make, for the file before the composite (up to `split`).
fn hint(m: &mut Sim, spans: &[Span], split: usize, skeleton: &[u8], w: usize) -> Option<()> {
    // (the file up to there: the skeleton in the gaps, the spans zero)
    let mut prefix = vec![0u8; split];
    let (mut prev, mut at) = (0, 0);
    for s in spans.iter().take_while(|s| s.start <= split) {
        let gap = prefix.get_mut(prev..s.start)?;
        gap.copy_from_slice(skeleton.get(at..at + gap.len())?);
        at += gap.len();
        prev = s.end;
        if s.start == split {
            break;
        }
    }
    let psd = crate::psd::parse(&prefix).ok()?;
    if psd.width as usize != w || psd.depth != 8 || psd.mode != 3 {
        return None;
    }
    // `psd::parse`: the resources, the layers and their vectors, the global tags (made before
    // the layers where the layers are in a global tag)
    m.take(grown(psd.resources.len()) * wasm::RESOURCE);
    let layers = &psd.layer_info.layers;
    let span = &psd.layer_info.span;
    let in_tag = !layers.is_empty()
        && psd.global_tags.iter().any(|t| {
            matches!(&t.key, b"Layr" | b"Lr16" | b"Lr32") && t.data.start <= span.start && span.end <= t.data.end
        });
    let globals = grown(psd.global_tags.len()) * wasm::TAG;
    if in_tag {
        m.take(globals);
    }
    m.take(wasm::LAYER * layers.len() as u64);
    for l in layers.iter() {
        m.take(wasm::CHANNEL * l.channels.len() as u64);
        m.take(l.name.len() as u64);
        m.take(grown(l.tags.len()) * wasm::TAG);
    }
    if !in_tag {
        m.take(globals);
    }
    // `RowRenderer::new`
    let r = RowRenderer::new(&psd, &prefix, &mut Report::quiet()).ok()?;
    let shape = r.shape();
    m.take(wasm::OP * shape.ops as u64);
    m.take(wasm::LAYER_ROWS * shape.layers as u64);
    // (the temporary vectors: where the layers go, the open dividers, the folders skipped)
    let dividers = layers.iter().filter(|l| composite::section(l, &prefix) == 3).count() as u64;
    let n = layers.len() as u64;
    m.peak(block(4 * n) + block(4 * dividers) + block(n));
    m.take(wasm::ROW * shape.levels as u64);
    (0..shape.levels).for_each(|_| m.take(4 * w as u64));
    (0..5).for_each(|_| m.take(shape.widest as u64));
    m.take(w as u64);
    Some(())
}
