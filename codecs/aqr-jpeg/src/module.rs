//! The code of the module (see `formula`, and FORMAT.md), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use aqr_common::packed;

use crate::codec::{self, CoreSource, Sizes};
use crate::jpeg::Error;
use crate::model::{get_bytes, get_varint};

/// The task slots of the module (aqr-modules' `jpeg-recons`).
pub const SLOTS: usize = 8;

/// The memory plan of the module ([`aqr_api::stack`]): the most its instance and tasks take at
/// once, its initial memory (`initial` bytes) with it, of params `params`, the nine `inputs`
/// and a JPEG of `len` bytes (`None` where they are not so): what `aqr_api::whole` copies, the
/// sizes read, the embedded entries and the blob restored, and what decoding takes
/// ([`codec::core_plan`]).
pub fn plan(params: &[u8], inputs: &[&[u8]], initial: u64, len: u64) -> Option<u64> {
    use aqr_api::stack::{block, whole};
    let (progressive, sizes) = self::params(params).ok()?;
    let &[head, embedded, cut, info, scans, tables, lens, rans, bits] = inputs else { return None };
    let lens64: Vec<u64> = inputs.iter().map(|i| i.len() as u64).collect();
    let n = |k: usize| k as u64;
    let mut top = initial + whole(n(params.len()), &[], &lens64);
    top += block(8 * n(sizes.stripes.len())) + block(4 * n(sizes.scans.len()));
    let mut p = 0;
    let entries = codec::parse_entries(head, &mut p).ok()?;
    let items = packed::unpack(embedded)?;
    let mut emb: Vec<&[u8]> = Vec::with_capacity(entries.len());
    let mut it = items.iter();
    for &(_, back) in &entries {
        emb.push(match back {
            None => it.next()?,
            Some(k) => emb[k],
        });
    }
    let blob = codec::restore_blob(cut, &entries, &emb).ok()?;
    top += block(12 * n(entries.len())) + block(8 * n(items.len())) + block(8 * n(emb.len())) + block(n(blob.len()));
    let mut slots = Slots { info, scans, tables, lens, rans, bits, pos: [0; 4] };
    let len = usize::try_from(len).ok()?;
    codec::core_plan(&blob, progressive, &mut slots, (&sizes, len), top, SLOTS)
}

/// The params of a main image of frame type `progressive` and sizes `z` ([`Sizes`]).
pub fn put_params(progressive: bool, z: &Sizes) -> Vec<u8> {
    let mut p = vec![progressive as u8];
    aqr_common::varint::put(&mut p, z.stripes.len() as u64);
    for &(a, b) in &z.stripes {
        aqr_common::varint::put(&mut p, a as u64);
        aqr_common::varint::put(&mut p, b as u64);
    }
    aqr_common::varint::put(&mut p, z.scans.len() as u64);
    z.scans.iter().for_each(|&n| aqr_common::varint::put(&mut p, n as u64));
    p
}

/// The frame type (true: progressive) and the sizes that params `p` give.
pub fn params(p: &[u8]) -> Result<(bool, Sizes), Error> {
    let bad = || Error("bad parameters".into());
    let (&f @ (0 | 1), _) = p.split_first().ok_or_else(bad)? else { return Err(bad()) };
    let mut at = 1;
    let mut get = || get_varint(p, &mut at).ok().and_then(|v| usize::try_from(v).ok()).ok_or_else(bad);
    let n = get()?;
    // (a varint is a byte at least)
    if n > p.len() {
        return Err(bad());
    }
    let mut stripes = Vec::with_capacity(n);
    for _ in 0..n {
        stripes.push((get()?, get()?));
    }
    let n = get()?;
    if n > p.len() {
        return Err(bad());
    }
    let mut scans = Vec::with_capacity(n);
    for _ in 0..n {
        scans.push(get()?);
    }
    drop(get);
    if at != p.len() {
        return Err(bad());
    }
    Ok((f == 1, Sizes { stripes, scans }))
}

/// The module: the JPEG of `len` bytes from `params` (the frame type, 1 byte: 0 sequential,
/// 1 progressive, then the sizes decoding takes, [`params`]) and the nine `inputs` (FORMAT.md),
/// on up to `threads` threads (0: rayon's pool as it is, the global one outside any other).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>, Error> {
    let (progressive, sizes) = self::params(params)?;
    let &[head, embedded, cut, info, scans, tables, lens, rans, bits] = inputs else {
        return Err(Error("bad parameters or inputs".into()));
    };
    let len = usize::try_from(len).map_err(|_| Error("bad length".into()))?;
    let r = in_pool(threads, || {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut p = 0;
            let entries = codec::parse_entries(head, &mut p)?;
            codec::fully_read(head, p, "head")?;
            // coded entries are the items of `embedded` in order, repeats are copies
            let mut items = packed::unpack(embedded).ok_or_else(|| Error("embedded not packed".into()))?.into_iter();
            let mut emb: Vec<&[u8]> = Vec::with_capacity(entries.len());
            for &(_, back) in &entries {
                let bytes = match back {
                    None => items.next().ok_or_else(|| Error("too few embedded items".into()))?,
                    Some(k) => emb[k],
                };
                emb.push(bytes);
            }
            if items.next().is_some() {
                return Err(Error("too many embedded items".into()));
            }
            let blob = codec::restore_blob(cut, &entries, &emb)?;
            let mut slots = Slots { info, scans, tables, lens, rans, bits, pos: [0; 4] };
            codec::with_bufs(|coefs, _| codec::decode_core(&blob, progressive, &mut slots, coefs, (&sizes, len)))
        }))
    });
    let out = r.map_err(|_| Error("internal panic".into()))??;
    if out.len() != len {
        return Err(Error("output length mismatch".into()));
    }
    Ok(out)
}

/// Runs `f` on a rayon pool of up to `threads` threads of aqr-common's pool, or as it is if 0
/// or within a rayon pool already (or in WebAssembly, where there is no rayon).
pub(crate) fn in_pool<R: Send>(threads: usize, f: impl FnOnce() -> R + Send) -> R {
    #[cfg(target_family = "wasm")]
    return {
        let _ = threads;
        f()
    };
    #[cfg(not(target_family = "wasm"))]
    match (threads > 0).then(|| aqr_common::pool::rayon_pool(threads)).flatten() {
        Some(pool) => pool.install(f),
        None => f(),
    }
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
