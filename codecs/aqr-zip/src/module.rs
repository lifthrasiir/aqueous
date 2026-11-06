//! The code of the module (see `formula`, and FORMAT.md), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module; and the encoder's helpers to run jobs in
//! parallel.

use crate::layout;
use crate::{Result, corrupt};

/// Bytes from which a DEFLATE stream gets threads of its own: two of aqr-flate's segments.
#[cfg(feature = "encode")]
const LARGE: usize = 2 << 20;

/// `f(item, threads)` on every element of `items`, by `threads` threads, the largest by `size`
/// first: those of at least [`LARGE`] with all of them (aqr-flate codes segments in parallel,
/// but parses a stream on one), the others with one each, all side by side, so that the small
/// fill the threads a large one leaves idle.
#[cfg(feature = "encode")]
pub(crate) fn par_jobs<T: Sync, R: Send>(
    items: &[T],
    size: impl Fn(&T) -> usize + Sync,
    threads: usize,
    f: impl Fn(&T, usize) -> R + Sync,
) -> Vec<R> {
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(size(&items[i])));
    let done = par_map(&order, threads, |&i| f(&items[i], if size(&items[i]) >= LARGE { threads } else { 1 }));
    let mut out: Vec<Option<R>> = (0..items.len()).map(|_| None).collect();
    for (i, r) in order.into_iter().zip(done) {
        out[i] = Some(r);
    }
    out.into_iter().map(Option::unwrap).collect()
}

/// `f` on every element of `items`, on up to `threads` threads.
#[cfg(feature = "encode")]
pub(crate) fn par_map<T: Sync, R: Send>(items: &[T], threads: usize, f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    aqr_common::pool::par_map(items.len(), threads, |i| f(&items[i]))
}

/// The module `MODULE`: the archive of `len` bytes rebuilt from `inputs`: the contents of
/// the stored bodies, the deflated bodies with what aqr-flate's module derives of them
/// (`layout::deflated`), the layout and the bytes kept.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, _threads: usize) -> Result<Vec<u8>> {
    let (&[], &[data, flate, layout, kept]) = (params, inputs) else { return Err(corrupt()) };
    layout::build(layout, data, flate, kept, len)
}

/// Pages of the windows of the module as a part module ([`decode_part`]): of each input and of
/// the output.
pub const WINDOW: u32 = 16;

/// The `K`-th input of a part module read in order, through its window.
struct In<const K: u32> {
    at: u64,
    len: u64,
    /// What it gave last.
    buf: Vec<u8>,
}

impl<const K: u32> In<K>
where
    aqr_api::Input<K>: aqr_api::Memory,
{
    fn new(len: u64) -> In<K> {
        In { at: 0, len, buf: Vec::new() }
    }

    /// Its next `n` bytes, given to `f` a window's at most at a time.
    fn each(&mut self, n: u64, f: &mut dyn FnMut(&[u8])) -> Result<()> {
        use aqr_api::{Input, Memory};
        let end = self.at.checked_add(n).filter(|&e| e <= self.len).ok_or_else(corrupt)?;
        let w = Input::<K>::size();
        let mut piece = Vec::new();
        while self.at < end {
            let k = (end - self.at).min(w - self.at % w);
            piece.resize(k as usize, 0);
            Input::<K>::request(self.at + k);
            Input::<K>::read(self.at % w, &mut piece);
            Input::<K>::release(self.at + k);
            self.at += k;
            f(&piece);
        }
        Ok(())
    }

    /// Its next `n` bytes, held until the next call.
    fn take(&mut self, n: u64) -> Result<&[u8]> {
        let mut buf = std::mem::take(&mut self.buf);
        buf.clear();
        buf.reserve_exact(usize::try_from(n).map_err(|_| corrupt())?);
        self.each(n, &mut |d| buf.extend_from_slice(d))?;
        self.buf = buf;
        Ok(&self.buf)
    }
}

/// The inputs of [`decode_part`]: the contents of the stored bodies, the deflated bodies each
/// after what flate-set derives of it, and the bytes kept.
struct Pulled {
    data: In<0>,
    flate: In<1>,
    kept: In<3>,
}

impl layout::Inputs for Pulled {
    fn data(&mut self, n: u64) -> Result<&[u8]> {
        self.data.take(n)
    }

    fn kept(&mut self, n: u64) -> Result<&[u8]> {
        self.kept.take(n)
    }

    fn entry(&mut self) -> Result<layout::Entry> {
        let d = self.flate.take(layout::DERIVED as u64)?;
        layout::entry_of(d.try_into().map_err(|_| corrupt())?)
    }

    fn body(&mut self, n: u64, out: &mut dyn aqr_common::out::Out) -> Result<()> {
        self.flate.each(n, &mut |d| out.put(d))
    }

    fn done(&self) -> bool {
        self.data.at == self.data.len && self.flate.at == self.flate.len && self.kept.at == self.kept.len
    }
}

/// The output of [`decode_part`], written through its window as it is put, the bytes it is told
/// to hold kept (each run by where it starts).
struct Written {
    at: usize,
    held: Vec<(usize, Vec<u8>)>,
    holding: usize,
    /// Whether bytes not held were asked for (the archive is invalid).
    failed: bool,
}

impl Written {
    fn held(&self, r: &std::ops::Range<usize>) -> Option<&[u8]> {
        let i = self.held.partition_point(|h| h.0 <= r.start).checked_sub(1)?;
        let (start, d) = &self.held[i];
        d.get(r.start - start..r.end.checked_sub(*start)?)
    }
}

impl aqr_common::out::Out for Written {
    fn len(&self) -> usize {
        self.at
    }

    fn put(&mut self, mut d: &[u8]) {
        use aqr_api::{Memory, Output};
        let k = self.holding.min(d.len());
        if k > 0 {
            match self.held.last_mut() {
                Some((start, h)) if *start + h.len() == self.at => h.extend_from_slice(&d[..k]),
                _ => self.held.push((self.at, d[..k].to_vec())),
            }
            self.holding -= k;
        }
        // (within the output's size: told by `assemble_into`'s room)
        let w = Output::<0>::size() as usize;
        while !d.is_empty() {
            let k = d.len().min(w - self.at % w);
            Output::<0>::write((self.at % w) as u64, &d[..k]);
            self.at += k;
            Output::<0>::commit(self.at as u64);
            d = &d[k..];
        }
    }

    fn put_within(&mut self, r: std::ops::Range<usize>) {
        match self.held(&r).map(<[u8]>::to_vec) {
            Some(d) => self.put(&d),
            None => self.failed = true,
        }
    }

    fn out(&self, r: std::ops::Range<usize>) -> &[u8] {
        // (where not held, nothing, and the archive found invalid at the end)
        self.held(&r).unwrap_or_default()
    }

    fn hold(&mut self, n: usize) {
        self.holding = n;
    }
}

/// The module as a part module (its entry point, of `aqr_api::entry!`): what [`decode`] gives,
/// its inputs read and its output written through windows ([`WINDOW`]), the layout held, and of
/// the others an entry's at a time: the contents of a stored body (whose CRC-32 comes before
/// it), the bytes kept of a record; a deflated body goes through. The local headers' names and
/// extra fields are held for the central directory, and the bodies that later entries take
/// again.
pub fn decode_part(params_len: u64, data_len: u64, flate_len: u64, layout_len: u64, kept_len: u64, size: u64) {
    aqr_api::finish(zip_part(params_len, [data_len, flate_len, layout_len, kept_len], size));
}

fn zip_part(params_len: u64, inputs: [u64; 4], size: u64) -> Result<()> {
    if params_len != 0 {
        return Err(corrupt());
    }
    let layout = In::<2>::new(inputs[2]).take(inputs[2])?.to_vec();
    let mut pulled = Pulled { data: In::new(inputs[0]), flate: In::new(inputs[1]), kept: In::new(inputs[3]) };
    let mut out = Written { at: 0, held: Vec::new(), holding: 0, failed: false };
    let len = usize::try_from(size).map_err(|_| corrupt())?;
    layout::assemble_into(&layout, &mut pulled, len, &mut out)?;
    if out.failed || out.at != len {
        return Err(corrupt());
    }
    Ok(())
}
