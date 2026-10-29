//! aqr-jpeg as a module of entry formulas (see aqr-formula, FORMAT.md):
//! the streams of a coded JPEG as nine input slots, the frame type as the only parameter.
//!
//! Embedded JPEGs coded here are not decoded by the module: their decoded bytes are an input
//! slot, packed, which the formula fills with the children's formulas (this module again).

use crate::jpeg::Error;
use crate::{codec, fast, jpeg};
use aqr_common::varint;
use aqr_formula::{Formula, Kind, Module, Node};

pub use crate::module::decode;
use crate::module::in_pool;

pub const MODULE: Module = Module::new("jpeg-recons", 2).with_memory(memory);

/// Tasks of an instance that run at once (the module's slots).
const SLOTS: usize = 8;

/// Bytes of the decoding tables (`fast::Tables`, `N_SYM` = 32672 symbols: 4 bytes of the
/// decoder's `cum | freq << 16` and 4 of its lookup table each) with what `read_tables` and
/// `build_tables` hold while they are made (the frequencies and cumulative sums, 2 bytes a
/// symbol each), and slack.
const TABLES: u64 = 520 << 10;

/// Bytes of a block of coefficients and its mask.
const BLOCK: u64 = 136;

/// What the module holds at most besides its inputs and output.
///
/// Its instance holds the entries (pushed: three times), the blob, the parsed JPEG (the Huffman
/// tables of the scans), the decoding tables and the stripes' streams listed. The scan is coded
/// a stripe at a time (the sequential scan of all components) or whole:
/// - striped (`encode_striped`): each stripe a task (8 at once) with a copy of the above and its
///   own rows of blocks (136 bytes a block), the padded words and raw bytes it decodes from, and
///   the bits of its MCUs: unstuffed, in a buffer grown by doubling (twice the bytes, and the
///   old one held: three times) that is serialized for the result (as bytes pushed: three times)
///   while it is held; all stripes' bits are at most the bytes of the scans. Then the parts
///   (twice their bytes), the pieces made of them (the bytes, stuffed and with the markers)
///   with the parts and a part's bits again (three times), and the output made of the pieces.
/// - whole (`decode_whole`): the blocks of every component, then a scan at a time on this
///   thread, each coded in chunks (parts of twice the bytes, the shifted bits, pieces; the
///   progressive encoder's lists), the scans held until the output is made of them.
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let ([progressive @ (0 | 1)], 9) = (u.params, u.inputs.len()) else { return None };
    let c = |k| u.content(k);
    let (head, embedded, cut, info, scans, lens) = (c(0)?, c(1)?, c(2)?, c(3)?, c(4)?, c(6)?);
    let mut p = 0;
    let entries = codec::parse_entries(head, &mut p).ok()?;
    let items = aqr_common::packed::unpack(embedded)?;
    let mut emb: Vec<&[u8]> = Vec::with_capacity(entries.len());
    let mut it = items.iter();
    for &(_, back) in &entries {
        emb.push(match back {
            None => it.next()?,
            Some(k) => emb[k],
        });
    }
    let blob = codec::restore_blob(cut, &entries, &emb).ok()?;
    let j = jpeg::parse(&blob).ok()?;
    if j.progressive != (*progressive == 1) {
        return None;
    }
    // the scan info (see codec::decode_core): raw scans and truncated tails, whose lengths
    // come first in `lens`, then the stripes' (words, raw bytes)
    let get = |d: &[u8], p: &mut usize| fast::get_varint(d, p).ok();
    let mut p = 0;
    if get(info, &mut p)? != j.scans.len() as u64 {
        return None;
    }
    let (mut first_raw, mut first_cut, mut slices) = (false, None, 0u64);
    for k in 0..j.scans.len() {
        let f = *info.get(p)?;
        p += 1;
        if f & 2 != 0 {
            first_raw |= k == 0;
            slices += 1;
            continue;
        }
        get(info, &mut p)?;
        if f & 4 != 0 {
            let mcus = get(info, &mut p)?;
            get(info, &mut p)?;
            if k == 0 {
                first_cut = Some(mcus);
            }
            slices += 1;
        }
    }
    let ns = usize::try_from(get(info, &mut p)?).ok().filter(|&n| (1..=fast::MAX_STRIPES).contains(&n))?;
    let mut p = 0;
    for _ in 0..slices {
        get(lens, &mut p)?;
    }
    let mut stripes = Vec::with_capacity(ns);
    for _ in 0..ns {
        stripes.push((get(lens, &mut p)?, get(lens, &mut p)?));
    }

    let (nc, out) = (j.comps.len() as u64, u.output());
    let blob_len = blob.len() as u64;
    // the scans' bytes in the output (the raw ones included)
    let scan = out.checked_sub(blob_len)?;
    let mut parse = 16 << 10;
    for s in &j.scans {
        parse += 3 * (64 + 600 * s.comps.len() as u64);
    }
    // (entries are (usize, Option<usize>) of 12 bytes, pushed; the items and `emb` pointers)
    let m0 = u.base()
        + 3 * 12 * entries.len() as u64
        + 16 * entries.len() as u64
        + blob_len
        + parse
        + TABLES
        + (16 + 16) * ns as u64
        + scans.len() as u64
        + 2 * 1024 * nc
        + 4 * aqr_formula::PAGE;
    // the rows of block rows `rows` of each component (as fast::stripe_rows)
    let rows = if nc == 1 { j.comps[0].bh } else { j.comps[0].bh / j.comps[0].v };
    let held = |k: usize| -> u64 {
        j.comps
            .iter()
            .map(|c| {
                let f = if nc == 1 { 1 } else { c.v };
                (((k + 1) * rows / ns - k * rows / ns) * f * c.bw) as u64 * BLOCK + 36
            })
            .sum()
    };
    let striped = striped_scan(&j).filter(|_| !first_raw && first_cut.is_none_or(|m| m > 0));
    let Some(s) = striped else {
        // whole: every component's blocks, the words and raw bytes of a stripe, then the scans
        let blocks: u64 = j.comps.iter().map(|c| (c.bw * c.bh) as u64 * BLOCK + 36).sum();
        let pad = stripes.iter().map(|&(w, r)| w + r).max().unwrap_or(0) + 320 + 256;
        let decode = blocks + pad + (16 + 16 * nc) * ns as u64 + 24 * ns as u64;
        // a scan: chunks' parts (twice their bytes, three times as grown), pieces, shifted bits
        // and the scan; the progressive encoder's lists (32768 of 16 bytes) and its rows
        let work = 6 * scan + 3 * 32 + (512 << 10) + 3 * 1024 * nc;
        // the scans held, and the output
        let finish = scan + out;
        return Some(m0 + blocks + decode.max(work).max(finish) + 4 * 1024 * j.scans.len() as u64);
    };
    let nmcu = (s.mcux * s.mcuy) as u64;
    let emit = first_cut.map_or(nmcu, |m| m.min(nmcu));
    let ranges: Vec<_> = fast::stripe_mcus(&j, s.mcux, ns).into_iter().take_while(|r| (r.0 as u64) < emit).collect();
    let nr = ranges.len() as u64;
    let bpm: u64 = j.comps.iter().map(|c| if nc == 1 { 1 } else { (c.h * c.v) as u64 }).sum();
    let nseg = nmcu.div_ceil(if s.ri > 0 { s.ri as u64 } else { nmcu });
    // parts' bytes and the structures, then the pieces, a part's bits (three times) and the output
    let parts = 2 * (scan + 16 * nr) + 136 * bpm * nr + 12 * nseg + 64 * nr;
    let after = parts + scan + scan / 64 + 16 * nr + 3 * (scan + 32) + out;
    let mut peak = m0 + after;
    // a stripe's task: the rows, the padded words and raw bytes (while decoding), and its bits
    // (while coding, held while the result is made)
    let mut grown: Vec<u64> = (0..ranges.len())
        .map(|k| held(k) + stripes[k].0 + stripes[k].1 + 320 + 256 + 12 * nseg.div_ceil(nr.max(1)) + 136 * bpm)
        .collect();
    grown.sort_unstable_by(|a, b| b.cmp(a));
    let k = grown.len().min(SLOTS) as u64;
    if ranges.len() > 1 {
        peak = peak.max(m0 + k * m0 + grown.iter().take(SLOTS).sum::<u64>() + 8 * (scan + 16 * nr));
    } else {
        peak = peak.max(m0 + grown.first().copied().unwrap_or(0) + 3 * (scan + 32));
    }
    Some(peak)
}

/// The scan of a sequential file that codes every component in one scan, whose MCU grid is the
/// components' block grid (as `codec::striped_scan`).
fn striped_scan(j: &jpeg::Jpeg) -> Option<&jpeg::Scan> {
    let [s] = &j.scans[..] else { return None };
    if j.progressive || s.comps.len() != j.comps.len() || (s.ss, s.se, s.ah, s.al) != (0, 63, 0, 0) {
        return None;
    }
    let single = j.comps.len() == 1;
    j.comps
        .iter()
        .all(|c| {
            let (h, v) = if single { (1, 1) } else { (c.h, c.v) };
            c.bw == s.mcux * h && c.bh == s.mcuy * v
        })
        .then_some(s)
}

/// what a formula costs beyond its streams, for deciding whether coding an embedded JPEG pays off
const OVERHEAD: usize = 8;

/// The formula for `input`; None where it is not coded. `threads` bounds the parallelism: the
/// calling thread and workers of aqr-common's pool (0: rayon's pool as it is, the global one
/// outside any other).
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    in_pool(threads, || encode_at(input, 0))
}

fn encode_at(input: &[u8], depth: u32) -> Option<Formula> {
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| try_encode(input, depth)));
    r.ok()?.ok().filter(|f| size(f) < input.len())
}

/// Bytes of all streams of `f`.
fn size(f: &Formula) -> usize {
    f.streams.iter().map(|s| s.data.len()).sum()
}

fn try_encode(input: &[u8], depth: u32) -> Result<Formula, Error> {
    let core = codec::with_bufs(|coefs, clean| codec::encode_core(input, coefs, clean))?;
    // embedded JPEGs in the blob, coded recursively (only where it pays off)
    let entries = codec::embedded_entries(&core.blob, depth, |bytes| {
        let c = encode_at(bytes, depth + 1)?;
        (size(&c) + OVERHEAD < bytes.len()).then_some(c)
    });
    let mut head = Vec::with_capacity(4 + entries.len() * 8);
    codec::put_entries(&mut head, &entries);
    let cut = codec::cut_blob(&core.blob, &entries);
    let mut f = Formula::default();
    let head = f.push("jpeg/head", head);
    // a repeated entry has no bytes of its own, it is the earlier one
    let children: Vec<Node> = entries.into_iter().filter_map(|e| e.coded).map(|c| f.embed(c)).collect();
    let embedded = Node::pack(children);
    let cut = f.push("jpeg/cut", cut);
    let info = f.push("jpeg/info", core.info);
    // lengths of the scan streams, then of each stripe's words and raw bits
    let mut lens = Vec::new();
    let mut scans = Vec::new();
    for s in &core.scans {
        varint::put(&mut lens, s.len() as u64);
        scans.extend_from_slice(s);
    }
    let scans = f.push("jpeg/scans", scans);
    let model = core.model;
    let tables = f.push("jpeg/tables", model.tables);
    let (n, m) = model.stripes.iter().fold((0, 0), |(n, m), s| (n + s.rans.len() * 4, m + s.raw.len()));
    let (mut rans, mut bits) = (Vec::with_capacity(n), Vec::with_capacity(m));
    // each stripe is dropped once copied
    for s in model.stripes {
        let w = s.rans_bytes();
        varint::put(&mut lens, w.len() as u64);
        varint::put(&mut lens, s.raw.len() as u64);
        rans.extend_from_slice(&w);
        bits.extend_from_slice(&s.raw);
    }
    let lens = f.push("jpeg/lens", lens);
    let rans = f.push_as("jpeg/rans", Kind::Compressed, rans);
    let bits = f.push("jpeg/bits", bits);
    f.root = Node::apply(
        MODULE,
        vec![core.progressive as u8],
        vec![head, embedded, cut, info, scans, tables, lens, rans, bits],
        input.len() as u64,
    );
    Ok(f)
}
