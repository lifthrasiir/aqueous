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
    /// What it gave last, of a capacity for the most it gives at once (reserved as it is made).
    buf: Vec<u8>,
}

impl<const K: u32> In<K>
where
    aqr_api::Input<K>: aqr_api::Memory,
{
    fn new(len: u64, most: u64) -> Result<In<K>> {
        Ok(In { at: 0, len, buf: Vec::with_capacity(usize::try_from(most).map_err(|_| corrupt())?) })
    }

    /// Its next `n` bytes, given to `f` a window's at most at a time (in a piece on top of the
    /// rest, made for them).
    fn each(&mut self, n: u64, f: &mut dyn FnMut(&[u8])) -> Result<()> {
        use aqr_api::{Input, Memory};
        let end = self.at.checked_add(n).filter(|&e| e <= self.len).ok_or_else(corrupt)?;
        let w = Input::<K>::size();
        let mut piece = Vec::with_capacity(n.min(w) as usize);
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

    /// Its next `n` bytes (no more than it was made for), held until the next call.
    fn take(&mut self, n: u64) -> Result<&[u8]> {
        let mut buf = std::mem::take(&mut self.buf);
        buf.clear();
        if n > buf.capacity() as u64 {
            return Err(corrupt());
        }
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

/// The output of [`decode_part`], written through its window as it is put; the bytes it is told
/// to hold kept, one after another in `held` (reserved for all of them), each run of them by
/// where it starts in the output and in `held` (in `runs`, reserved for one an entry).
struct Written {
    at: usize,
    held: Vec<u8>,
    runs: Vec<(usize, usize)>,
    holding: usize,
    /// Whether bytes not held were asked for, or more held than reserved (the archive is
    /// invalid).
    failed: bool,
}

impl Written {
    /// Where bytes `r` of the output are in `held`, if they are held.
    fn held(&self, r: &std::ops::Range<usize>) -> Option<std::ops::Range<usize>> {
        let i = self.runs.partition_point(|h| h.0 <= r.start).checked_sub(1)?;
        let (start, from) = self.runs[i];
        let to = self.runs.get(i + 1).map_or(self.held.len(), |h| h.1);
        let (a, b) = (from + (r.start - start), from + r.end.checked_sub(start)?);
        (b <= to).then_some(a..b)
    }

    /// Keeps of `n` bytes put now (`d`, else those of `held` from `within`) the first that it is
    /// to hold, as a new run unless they follow the last one.
    fn keep(&mut self, d: &[u8], within: Option<usize>, n: usize) {
        let k = self.holding.min(n);
        if k == 0 {
            return;
        }
        let follows = matches!(self.runs.last(), Some(&(s, f)) if s + (self.held.len() - f) == self.at);
        // (no more than reserved: past it, an invalid archive)
        if self.held.len() + k > self.held.capacity() || !follows && self.runs.len() == self.runs.capacity() {
            self.failed = true;
            return;
        }
        if !follows {
            self.runs.push((self.at, self.held.len()));
        }
        match within {
            Some(at) => self.held.extend_from_within(at..at + k),
            None => self.held.extend_from_slice(&d[..k]),
        }
        self.holding -= k;
    }

    /// Writes `d` to the output through its window.
    fn write(at: &mut usize, mut d: &[u8]) {
        use aqr_api::{Memory, Output};
        let w = Output::<0>::size() as usize;
        while !d.is_empty() {
            let k = d.len().min(w - *at % w);
            Output::<0>::write((*at % w) as u64, &d[..k]);
            *at += k;
            Output::<0>::commit(*at as u64);
            d = &d[k..];
        }
    }
}

impl aqr_common::out::Out for Written {
    fn len(&self) -> usize {
        self.at
    }

    fn put(&mut self, d: &[u8]) {
        // (within the output's size: told by `assemble_into`'s room)
        self.keep(d, None, d.len());
        Written::write(&mut self.at, d);
    }

    fn put_within(&mut self, r: std::ops::Range<usize>) {
        let Some(h) = self.held(&r) else {
            self.failed = true;
            return;
        };
        // (what it is to hold of them copied within `held` first, where it is: reserved)
        self.keep(&[], Some(h.start), h.len());
        Written::write(&mut self.at, &self.held[h]);
    }

    fn out(&self, r: std::ops::Range<usize>) -> &[u8] {
        // (where not held, nothing, and the archive found invalid at the end)
        self.held(&r).map_or(&[][..], |h| &self.held[h])
    }

    fn hold(&mut self, n: usize) {
        self.holding = n;
    }
}

/// Bytes of a run of the bytes held ([`Written::runs`]) on WebAssembly.
const RUN_BYTES: u64 = 8;
#[cfg(target_family = "wasm")]
const _: () = assert!(size_of::<(usize, usize)>() as u64 == RUN_BYTES);

/// The memory plan of the module as a part module ([`aqr_api::stack`]): what [`decode_part`]
/// takes beyond its initial memory at most, of a layout of `inputs[2]` bytes `layout` and inputs
/// of `inputs` bytes. In order: the layout (a window's piece on top as it is read), the marks of
/// bodies taken again (the lengths of the bodies on top while they are summed), the bytes held
/// (`layout::Prepared::held`) and their runs, the buffers of the inputs (the largest stored body and piece kept, and what
/// is derived of a deflated body), the local entries seen; and on top of them, of a record, the
/// extra field made apart (or a central header's name and extra field) and a window's piece of
/// an input.
pub fn plan(inputs: [u64; 4], layout: &[u8]) -> Option<u64> {
    use aqr_api::stack::block;
    let p = layout::Prepared::new(layout).ok()?;
    let c = p.counts;
    let w = WINDOW as u64 * 65536;
    let held = p.held();
    let piece = |n: u64| block(n.min(w));
    let base = block(inputs[2]);
    let reading = base + piece(inputs[2]);
    let again = base + block(c.entries);
    let summing = again + block(16 * c.entries);
    let held = again + block(held) + block(RUN_BYTES * c.entries);
    let bufs = held + block(c.stored) + block(c.kept) + block(layout::DERIVED as u64);
    let seen = bufs + block(layout::SEEN_BYTES * c.entries);
    let pieces = [c.stored, c.kept, layout::DERIVED as u64, inputs[1]].into_iter().map(piece).max().unwrap_or(0);
    let record = (block(c.extra) + pieces).max(block(c.name) + block(c.extra));
    Some(reading.max(summing).max(seen + record))
}

/// The module as a part module (its entry point, of `aqr_api::entry!`): what [`decode`] gives,
/// its inputs read and its output written through windows ([`WINDOW`]), the layout held, and of
/// the others an entry's at a time: the contents of a stored body (whose CRC-32 comes before
/// it), the bytes kept of a record; a deflated body goes through. The local headers' names and
/// extra fields are held for the central directory, and the bodies that later entries take
/// again. Its memory is laid out as [`plan`] tells.
pub fn decode_part(params_len: u64, data_len: u64, flate_len: u64, layout_len: u64, kept_len: u64, size: u64) {
    aqr_api::finish(zip_part(params_len, [data_len, flate_len, layout_len, kept_len], size));
}

fn zip_part(params_len: u64, inputs: [u64; 4], size: u64) -> Result<()> {
    if params_len != 0 {
        return Err(corrupt());
    }
    let size_of = |n: u64| usize::try_from(n).map_err(|_| corrupt());
    let mut l = In::<2>::new(inputs[2], inputs[2])?;
    l.take(inputs[2])?;
    let layout = std::mem::take(&mut l.buf);
    let prepared = layout::Prepared::new(&layout)?;
    let c = prepared.counts;
    let held = prepared.held();
    let mut out = Written {
        at: 0,
        held: Vec::with_capacity(size_of(held)?),
        runs: Vec::with_capacity(size_of(c.entries)?),
        holding: 0,
        failed: false,
    };
    let mut pulled = Pulled {
        data: In::new(inputs[0], c.stored)?,
        kept: In::new(inputs[3], c.kept)?,
        flate: In::new(inputs[1], layout::DERIVED as u64)?,
    };
    let len = size_of(size)?;
    layout::assemble_prepared(&layout, &prepared, &mut pulled, len, &mut out)?;
    if out.failed || out.at != len {
        return Err(corrupt());
    }
    Ok(())
}
