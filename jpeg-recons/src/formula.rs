//! jpeg-recons as a module of entry formulas (see recons-formula, FORMAT.md "Formula module"):
//! the container's streams as nine input slots, the frame type as the only parameter.
//!
//! Embedded JPEGs coded here are not decoded by the module: their decoded bytes are an input
//! slot, which the formula fills with the children's formulas (this module again).

use crate::container::{self, CoreSource, Timing};
use crate::fast::{get_bytes, get_varint};
use crate::jpeg::Error;
use recons_common::varint;
use recons_formula::{Formula, Module, Node};

pub const MODULE: Module = Module { name: "jpeg-recons", version: 1 };

/// what a formula costs beyond its streams, for deciding whether coding an embedded JPEG pays off
const OVERHEAD: usize = 8;

/// The formula for `input`; None where the container would store it. `threads` (0: the global
/// rayon pool) bounds the parallelism.
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
    let core =
        container::with_bufs(|coefs, clean| container::compress_core(input, &mut Timing::default(), coefs, clean))?;
    // embedded JPEGs in the blob, coded recursively (only where it pays off)
    let entries = container::embedded_entries(&core.blob, depth, |bytes| {
        let c = encode_at(bytes, depth + 1)?;
        (size(&c) + OVERHEAD < bytes.len()).then_some(c)
    });
    let mut head = Vec::with_capacity(4 + entries.len() * 8);
    container::put_entries(&mut head, &entries);
    let cut = container::cut_blob(&core.blob, &entries);
    let mut f = Formula::default();
    let head = f.push("jpeg/head", head);
    // a repeated entry has no bytes of its own, it is the earlier one
    let children: Vec<Node> = entries.into_iter().filter_map(|e| e.coded).map(|c| f.embed(c)).collect();
    let embedded = Node::concat(children);
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
    let tables = f.push("jpeg/tables", core.model.tables.clone());
    let (mut rans, mut bits) = (Vec::new(), Vec::new());
    for s in &core.model.stripes {
        let w = s.rans_bytes();
        varint::put(&mut lens, w.len() as u64);
        varint::put(&mut lens, s.raw.len() as u64);
        rans.extend_from_slice(&w);
        bits.extend_from_slice(&s.raw);
    }
    let lens = f.push("jpeg/lens", lens);
    let rans = f.push("jpeg/rans", rans);
    let bits = f.push("jpeg/bits", bits);
    f.root = Node::apply(
        MODULE,
        vec![core.progressive as u8],
        vec![head, embedded, cut, info, scans, tables, lens, rans, bits],
        input.len() as u64,
    );
    Ok(f)
}

/// The scan, table and stripe streams of the main image from their slots.
struct Slots<'a> {
    info: &'a [u8],
    scans: &'a [u8],
    tables: &'a [u8],
    lens: &'a [u8],
    rans: &'a [u8],
    bits: &'a [u8],
    /// read positions in `scans`, `lens`, `rans` and `bits`
    pos: [usize; 4],
}

impl<'a> CoreSource<'a> for Slots<'a> {
    fn info(&mut self) -> Result<&'a [u8], Error> {
        Ok(self.info)
    }
    fn scan(&mut self) -> Result<&'a [u8], Error> {
        let n = get_varint(self.lens, &mut self.pos[1])?;
        get_bytes(self.scans, &mut self.pos[0], n)
    }
    fn tables(&mut self) -> Result<&'a [u8], Error> {
        Ok(self.tables)
    }
    fn stripe(&mut self) -> Result<(&'a [u8], &'a [u8]), Error> {
        let (w, b) = (get_varint(self.lens, &mut self.pos[1])?, get_varint(self.lens, &mut self.pos[1])?);
        Ok((get_bytes(self.rans, &mut self.pos[2], w)?, get_bytes(self.bits, &mut self.pos[3], b)?))
    }
    fn finish(&mut self) -> Result<(), Error> {
        let ends = [self.scans.len(), self.lens.len(), self.rans.len(), self.bits.len()];
        if self.pos == ends { Ok(()) } else { Err(Error("trailing data".into())) }
    }
}

/// The module: the JPEG of `len` bytes from `params` (the frame type, 1 byte: 0 sequential,
/// 1 progressive) and the nine `inputs` (FORMAT.md). `threads` as in [`encode`].
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>, Error> {
    let (&[progressive @ (0 | 1)], &[head, embedded, cut, info, scans, tables, lens, rans, bits]) = (params, inputs)
    else {
        return Err(Error("bad parameters or inputs".into()));
    };
    let r = in_pool(threads, || {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut p = 0;
            let entries = container::parse_entries(head, &mut p, true)?;
            container::fully_read(head, p, "head")?;
            // coded entries take their bytes from `embedded` in order, repeats are copies
            let (mut q, mut emb): (usize, Vec<&[u8]>) = (0, Vec::with_capacity(entries.len()));
            for &(_, n, back) in &entries {
                let bytes = match back {
                    None => get_bytes(embedded, &mut q, n as u64)?,
                    Some(k) => emb[k],
                };
                if bytes.len() != n {
                    return Err(Error("embedded length mismatch".into()));
                }
                emb.push(bytes);
            }
            container::fully_read(embedded, q, "embedded")?;
            let blob = container::restore_blob(cut, &entries, &emb)?;
            let mut slots = Slots { info, scans, tables, lens, rans, bits, pos: [0; 4] };
            container::with_bufs(|coefs, _| {
                container::decompress_core(&blob, progressive == 1, &mut slots, &mut Timing::default(), coefs)
            })
        }))
    });
    let out = r.map_err(|_| Error("internal panic".into()))??;
    if out.len() as u64 != len {
        return Err(Error("output length mismatch".into()));
    }
    Ok(out)
}

/// Runs `f` on a pool of `threads` threads, or as it is if 0.
fn in_pool<R: Send>(threads: usize, f: impl FnOnce() -> R + Send) -> R {
    match threads {
        0 => f(),
        n => match rayon::ThreadPoolBuilder::new().num_threads(n).build() {
            Ok(pool) => pool.install(f),
            Err(_) => f(),
        },
    }
}
