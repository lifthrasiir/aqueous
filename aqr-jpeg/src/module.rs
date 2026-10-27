//! The code of the module (see `formula`, and FORMAT.md), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use aqr_common::packed;

use crate::codec::{self, CoreSource};
use crate::fast::{get_bytes, get_varint};
use crate::jpeg::Error;

/// The module: the JPEG of `len` bytes from `params` (the frame type, 1 byte: 0 sequential,
/// 1 progressive) and the nine `inputs` (FORMAT.md), on up to `threads` threads (0: rayon's
/// pool as it is, the global one outside any other).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>, Error> {
    let (&[progressive @ (0 | 1)], &[head, embedded, cut, info, scans, tables, lens, rans, bits]) = (params, inputs)
    else {
        return Err(Error("bad parameters or inputs".into()));
    };
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
            codec::with_bufs(|coefs, _| codec::decode_core(&blob, progressive == 1, &mut slots, coefs))
        }))
    });
    let out = r.map_err(|_| Error("internal panic".into()))??;
    if out.len() as u64 != len {
        return Err(Error("output length mismatch".into()));
    }
    Ok(out)
}

/// Runs `f` on a rayon pool of up to `threads` threads of aqr-common's pool, or as it is if 0
/// or within a rayon pool already.
pub(crate) fn in_pool<R: Send>(threads: usize, f: impl FnOnce() -> R + Send) -> R {
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
