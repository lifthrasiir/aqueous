//! Memories as values, so that the code of a module takes them as arguments rather than naming
//! them: [`Src`] and [`Dst`]. Natively they are byte slices ([`Bytes`], [`BytesMut`]), and the
//! code runs as a library on the bytes it is given; within the module they are [`Mem`] and [`Out`], the memories of the
//! application, which only the entry point names.
//!
//! Both check their offsets against their lengths, not the sizes of the memories (which are
//! padded to pages), so that a module fails wherever its code would fail natively.

use core::fmt::{self, Write};
use core::ops::Range;

use crate::{CopyTo, Memory, Output, Writable};

/// Bytes to read: the parameters, a resource or an input.
pub trait Src {
    /// The number of bytes.
    fn len(&self) -> u64;

    /// Copies `dst.len()` bytes from offset `at` into `dst`, trapping (panicking natively) past
    /// [`Src::len`].
    fn read(&self, at: u64, dst: &mut [u8]);

    /// All the bytes, where they are in the working memory already (natively), so that they
    /// need not be copied.
    fn as_slice(&self) -> Option<&[u8]> {
        None
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Bytes to write: an output, of a length fixed beforehand, read back as it is written and
/// copied into from an `S` directly.
pub trait Dst<S> {
    /// The number of bytes.
    fn len(&self) -> u64;

    /// Copies `src` to offset `at`, trapping past [`Dst::len`].
    fn write(&mut self, at: u64, src: &[u8]);

    /// Copies `dst.len()` bytes from offset `at` into `dst`, trapping past [`Dst::len`].
    fn read(&self, at: u64, dst: &mut [u8]);

    /// Copies `len` bytes from offset `from` of `src` to offset `at`, without the working
    /// memory; traps past the length of either.
    fn copy_from(&mut self, at: u64, src: &S, from: u64, len: u64);

    /// All the bytes, where they are in the working memory (natively), to be written in place.
    fn as_mut_slice(&mut self) -> Option<&mut [u8]> {
        None
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A byte slice as a [`Src`], natively.
#[derive(Clone, Copy, Debug)]
pub struct Bytes<'a>(pub &'a [u8]);

/// A byte slice as a [`Dst`], natively, copied into from [`Bytes`].
#[derive(Debug)]
pub struct BytesMut<'a>(pub &'a mut [u8]);

impl Src for Bytes<'_> {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }

    fn read(&self, at: u64, dst: &mut [u8]) {
        dst.copy_from_slice(&self.0[range(self.len(), at, dst.len() as u64)]);
    }

    fn as_slice(&self) -> Option<&[u8]> {
        Some(self.0)
    }
}

impl<'a> Dst<Bytes<'a>> for BytesMut<'_> {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }

    fn write(&mut self, at: u64, src: &[u8]) {
        let r = range(self.len(), at, src.len() as u64);
        self.0[r].copy_from_slice(src);
    }

    fn read(&self, at: u64, dst: &mut [u8]) {
        dst.copy_from_slice(&self.0[range(self.len(), at, dst.len() as u64)]);
    }

    fn copy_from(&mut self, at: u64, src: &Bytes<'a>, from: u64, len: u64) {
        let r = range(self.len(), at, len);
        self.0[r].copy_from_slice(&src.0[range(src.len(), from, len)]);
    }

    fn as_mut_slice(&mut self) -> Option<&mut [u8]> {
        Some(self.0)
    }
}

/// `at..at + n` within `len` bytes, else a trap.
fn range(len: u64, at: u64, n: u64) -> Range<usize> {
    match at.checked_add(n) {
        Some(end) if end <= len => at as usize..end as usize,
        _ => out_of_bounds(),
    }
}

#[cold]
fn out_of_bounds() -> ! {
    crate::log(b"aqr_api: out of bounds");
    crate::trap()
}

/// A memory of the application other than the outputs, of the length the entry point gives,
/// copied into [`Out`].
#[derive(Clone, Copy)]
pub struct Mem {
    len: u64,
    read: fn(u64, &mut [u8]),
    copy: fn(u64, u64, u64),
}

impl Mem {
    /// The first `len` bytes of `M`, such as `Mem::new::<Input<0>>(in0_len)`. Only the memories
    /// named so are imported.
    pub fn new<M: CopyTo<0>>(len: u64) -> Mem {
        Mem { len, read: M::read, copy: M::copy_to }
    }
}

impl Src for Mem {
    fn len(&self) -> u64 {
        self.len
    }

    fn read(&self, at: u64, dst: &mut [u8]) {
        range(self.len, at, dst.len() as u64);
        (self.read)(at, dst)
    }
}

/// The output of the application ([`Output`]`<0>`), of the size the entry point declares.
pub struct Out {
    len: u64,
}

impl Out {
    pub fn new(len: u64) -> Out {
        Out { len }
    }
}

impl Dst<Mem> for Out {
    fn len(&self) -> u64 {
        self.len
    }

    fn write(&mut self, at: u64, src: &[u8]) {
        range(self.len, at, src.len() as u64);
        <Output<0> as Writable>::write(at, src)
    }

    fn read(&self, at: u64, dst: &mut [u8]) {
        range(self.len, at, dst.len() as u64);
        Output::<0>::read(at, dst)
    }

    fn copy_from(&mut self, at: u64, src: &Mem, from: u64, len: u64) {
        range(self.len, at, len);
        range(src.len, from, len);
        (src.copy)(at, from, len)
    }
}

/// All the bytes of `s` in the working memory: borrowed natively, else read.
#[cfg(feature = "alloc")]
pub fn load<S: Src>(s: &S) -> alloc::borrow::Cow<'_, [u8]> {
    match s.as_slice() {
        Some(b) => alloc::borrow::Cow::Borrowed(b),
        None => {
            let mut b = alloc::vec![0; usize::try_from(s.len()).unwrap_or_else(|_| out_of_bounds())];
            s.read(0, &mut b);
            alloc::borrow::Cow::Owned(b)
        }
    }
}

/// Runs `f`, code taking byte slices, on all the bytes of the parameters, the resources and the
/// inputs ([`load`]) and the length of `out`, and writes its output to `out`, which it must
/// fill exactly (else a trap). For code not written against [`Src`] and [`Dst`] (yet), at the
/// cost of copying everything within a module.
#[cfg(feature = "alloc")]
pub fn whole<S: Src, D: Dst<S>, E>(
    params: &S,
    resources: &[S],
    inputs: &[S],
    out: &mut D,
    f: impl FnOnce(&[u8], &[&[u8]], &[&[u8]], u64) -> Result<alloc::vec::Vec<u8>, E>,
) -> Result<(), E> {
    use alloc::vec::Vec;
    let params = load(params);
    let resources: Vec<_> = resources.iter().map(load).collect();
    let inputs: Vec<_> = inputs.iter().map(load).collect();
    let resources: Vec<&[u8]> = resources.iter().map(|r| &r[..]).collect();
    let inputs: Vec<&[u8]> = inputs.iter().map(|i| &i[..]).collect();
    let o = f(&params, &resources, &inputs, out.len())?;
    if o.len() as u64 != out.len() {
        crate::log(b"aqr_api: output of another length");
        crate::trap()
    }
    out.write(0, &o);
    Ok(())
}

/// Returns if `r` is `Ok`; else logs the error and traps, as a module fails.
pub fn finish<E: fmt::Debug>(r: Result<(), E>) {
    if let Err(e) = r {
        let mut msg = Msg { buf: [0; 512], len: 0 };
        let _ = write!(msg, "{e:?}");
        crate::log(&msg.buf[..msg.len]);
        crate::trap()
    }
}

/// A message, cut where it does not fit.
struct Msg {
    buf: [u8; 512],
    len: usize,
}

impl Write for Msg {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let n = s.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}
