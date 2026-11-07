//! The code of the modules (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds them into their Wasm modules. Each applies a filter's encoder to its
//! input, as the encoder of an archive did before compressing it.

use crate::bra::{self, Arch};
use crate::{Error, Result, bcj2, corrupt, delta};

/// The start address that a branch converter's parameters tell (none: 0; else le32), aligned as
/// its architecture wants.
fn start(arch: Arch, params: &[u8]) -> Result<u32> {
    let pc = match params {
        [] => 0,
        &[a, b, c, d] => u32::from_le_bytes([a, b, c, d]),
        _ => return Err(Error("bad parameters")),
    };
    if pc & arch.alignment() != 0 {
        return Err(Error("start not aligned"));
    }
    Ok(pc)
}

fn one<'a>(inputs: &[&'a [u8]], len: u64) -> Result<&'a [u8]> {
    let &[d] = inputs else { return Err(Error("one input wanted")) };
    if d.len() as u64 != len {
        return Err(Error("output of another length"));
    }
    Ok(d)
}

/// The module `bcj-{arch}`: the branch converter's encoding of the input.
pub fn bra(arch: Arch, params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let pc = start(arch, params)?;
    let mut d = one(inputs, len)?.to_vec();
    bra::convert(arch, &mut d, pc, true);
    Ok(d)
}

/// The module `bcj-delta`: the Delta filter's encoding of the input, the distance less one the
/// parameter byte.
pub fn delta(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let &[k] = params else { return Err(Error("bad parameters")) };
    let mut d = one(inputs, len)?.to_vec();
    delta::encode(&mut d, k as usize + 1);
    Ok(d)
}

/// The pages of the windows of `bcj-delta`'s input and output (as a part module,
/// [`delta_part`]).
pub const DELTA_WINDOW: u32 = 1;

/// The module `bcj-delta` as a part module (its entry point, of `aqr_api::entry!`): its input
/// encoded a piece at a time through windows of [`DELTA_WINDOW`] pages, the bytes `dist` before
/// each kept (those before the data zeros).
pub fn delta_part(params_len: u64, in0_len: u64, size: u64) {
    use aqr_api::{Input, Memory, Output, Params};
    if params_len != 1 || size != in0_len {
        aqr_api::finish(Err(Error("bad parameters, or output of another length")));
    }
    let mut k = [0];
    Params::read(0, &mut k);
    let dist = k[0] as usize + 1;
    let (w_in, w_out) = (Input::<0>::size(), Output::<0>::size());
    let (mut hist, mut h) = ([0u8; 256], 0);
    let mut buf = [0; 4096];
    let mut pos = 0;
    while pos < size {
        let n = (size - pos).min(buf.len() as u64).min(w_in - pos % w_in).min(w_out - pos % w_out);
        let piece = &mut buf[..n as usize];
        Input::<0>::request(pos + n);
        Input::<0>::read(pos % w_in, piece);
        Input::<0>::release(pos + n);
        for b in piece.iter_mut() {
            let x = *b;
            *b = x.wrapping_sub(hist[h]);
            hist[h] = x;
            h = if h + 1 == dist { 0 } else { h + 1 };
        }
        Output::<0>::write(pos % w_out, piece);
        Output::<0>::commit(pos + n);
        pos += n;
    }
}

/// The module `bcj2`: the main, call and jump streams of the data (input 0) with the bits that
/// the range coder's stream (input 1) codes, one after another.
pub fn bcj2(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let &[d, rc] = inputs else { return Err(Error("two inputs wanted")) };
    if !params.is_empty() {
        return Err(corrupt());
    }
    if d.len() as u64 != len {
        return Err(Error("output of another length"));
    }
    bcj2::encode_joined(d, rc)
}

/// The pages of the windows of a branch converter's input and output (as a part module,
/// [`bra_part`]).
pub const BRA_WINDOW: u32 = 1;

/// Bytes of the buffer of [`bra_part`]: the pieces it converts, with the bytes a step leaves
/// (at most 16) before them.
const BRA_BUF: usize = 4096 + 16;

/// The module `bcj-{arch}` as a part module (its entry point, of `aqr_api::entry!`): its input
/// converted a piece at a time through windows of [`BRA_WINDOW`] pages, whatever the length of
/// the data ([`bra::Converter`] carries what a piece needs of the ones before it): the same as
/// [`bra`], with nothing held but the buffer on the stack.
#[inline(always)]
pub fn bra_part(arch: Arch, params_len: u64, in0_len: u64, size: u64) {
    use aqr_api::{Input, Memory, Output, Params};
    if size != in0_len {
        return aqr_api::finish(Err(Error("output of another length")));
    }
    let mut p = [0; 4];
    if params_len != 0 && params_len != 4 {
        return aqr_api::finish(Err(Error("bad parameters")));
    }
    Params::read(0, &mut p[..params_len as usize]);
    let pc = match start(arch, &p[..params_len as usize]) {
        Ok(pc) => pc,
        Err(e) => return aqr_api::finish(Err(e)),
    };
    let mut conv = bra::Converter::new(arch, pc, true);
    let (w_in, w_out) = (Input::<0>::size(), Output::<0>::size());
    let mut buf = [0; BRA_BUF];
    // (the bytes the last step left at the front of the buffer; the offsets read and put out)
    let (mut have, mut read, mut put) = (0, 0u64, 0u64);
    let mut out = |mut d: &[u8]| {
        while !d.is_empty() {
            let n = (d.len() as u64).min(w_out - put % w_out) as usize;
            Output::<0>::write(put % w_out, &d[..n]);
            put += n as u64;
            Output::<0>::commit(put);
            d = &d[n..];
        }
    };
    while read < size {
        let n = (size - read).min((BRA_BUF - have) as u64).min(w_in - read % w_in);
        Input::<0>::request(read + n);
        Input::<0>::read(read % w_in, &mut buf[have..have + n as usize]);
        Input::<0>::release(read + n);
        read += n;
        have += n as usize;
        let done = conv.step(&mut buf[..have]);
        out(&buf[..done]);
        buf.copy_within(done..have, 0);
        have -= done;
    }
    out(&buf[..have]);
}

/// The pages of the windows of `bcj2`'s inputs and outputs (as a part module, [`bcj2_part`]).
pub const BCJ2_WINDOW: u32 = 1;

/// Bytes of the buffers of [`bcj2_part`]'s data, range coder's stream and each output.
const BCJ2_BUF: usize = 4096;

/// The `K`-th input of a part module taken in order through its window, in pieces.
struct Pull<const K: u32> {
    /// The offset of the next byte to read from the input, and its length.
    at: u64,
    len: u64,
    buf: [u8; BCJ2_BUF],
    /// The bytes of `buf` not taken yet are `lo..hi`.
    lo: usize,
    hi: usize,
}

impl<const K: u32> Pull<K>
where
    aqr_api::Input<K>: aqr_api::Memory,
{
    fn new(len: u64) -> Pull<K> {
        Pull { at: 0, len, buf: [0; BCJ2_BUF], lo: 0, hi: 0 }
    }

    /// Reads on until the buffer holds `want` bytes not taken (or the input has no more).
    fn fill(&mut self, want: usize) {
        use aqr_api::{Input, Memory};
        if self.hi - self.lo >= want || self.at == self.len {
            return;
        }
        self.buf.copy_within(self.lo..self.hi, 0);
        (self.hi, self.lo) = (self.hi - self.lo, 0);
        let w = Input::<K>::size();
        while self.hi < BCJ2_BUF && self.at < self.len {
            let n = (self.len - self.at).min((BCJ2_BUF - self.hi) as u64).min(w - self.at % w);
            Input::<K>::request(self.at + n);
            Input::<K>::read(self.at % w, &mut self.buf[self.hi..self.hi + n as usize]);
            Input::<K>::release(self.at + n);
            self.at += n;
            self.hi += n as usize;
        }
    }

    /// The offset of the next byte not taken.
    fn position(&self) -> u64 {
        self.at - (self.hi - self.lo) as u64
    }
}

impl<const K: u32> bcj2::RcBytes for Pull<K>
where
    aqr_api::Input<K>: aqr_api::Memory,
{
    fn next(&mut self) -> Option<u8> {
        self.fill(1);
        let b = *self.buf[..self.hi].get(self.lo)?;
        self.lo += 1;
        Some(b)
    }
}

/// The `K`-th output of a part module put in order through its window, of `size` bytes in all.
struct Push<const K: u32> {
    at: u64,
    size: u64,
    buf: [u8; BCJ2_BUF],
    n: usize,
}

impl<const K: u32> Push<K>
where
    aqr_api::Output<K>: aqr_api::Memory + aqr_api::Writable,
{
    fn new(size: u64) -> Push<K> {
        Push { at: 0, size, buf: [0; BCJ2_BUF], n: 0 }
    }

    #[inline]
    fn put(&mut self, d: &[u8]) -> Result<()> {
        if self.n + d.len() > BCJ2_BUF {
            self.flush()?;
        }
        self.buf[self.n..self.n + d.len()].copy_from_slice(d);
        self.n += d.len();
        Ok(())
    }

    /// Puts out the buffer; fails where that is past `size`.
    fn flush(&mut self) -> Result<()> {
        use aqr_api::{Memory, Output};
        if self.n as u64 > self.size - self.at {
            return Err(Error("output of another length"));
        }
        let mut d = &self.buf[..self.n];
        while !d.is_empty() {
            let w = Output::<K>::size();
            let k = (d.len() as u64).min(w - self.at % w) as usize;
            Output::<K>::write(self.at % w, &d[..k]);
            self.at += k as u64;
            Output::<K>::commit(self.at);
            d = &d[k..];
        }
        self.n = 0;
        Ok(())
    }

    /// Puts out what is left; fails where the output is not `size` bytes.
    fn finish(mut self) -> Result<()> {
        self.flush()?;
        match self.at == self.size {
            true => Ok(()),
            false => Err(Error("output of another length")),
        }
    }
}

/// The module `bcj2` as a part module (its entry point, of `aqr_api::entry!`): the main, call and
/// jump streams, its three outputs, put in order through windows of [`BCJ2_WINDOW`] pages from
/// the data and the range coder's stream taken in order, whatever the lengths: [`bcj2`]'s
/// output is theirs one after another. The sizes of the outputs are the streams' lengths.
pub fn bcj2_part(params_len: u64, in0_len: u64, in1_len: u64, size0: u64, size1: u64, size2: u64) {
    aqr_api::finish(bcj2_stream(params_len, in0_len, in1_len, [size0, size1, size2]));
}

fn bcj2_stream(params_len: u64, data_len: u64, rc_len: u64, sizes: [u64; 3]) -> Result<()> {
    use bcj2::{candidate, context};
    if params_len != 0 {
        return Err(corrupt());
    }
    // (each of the targets 4 bytes; the streams one after another make the data's length)
    let total = sizes[0].checked_add(sizes[1]).and_then(|n| n.checked_add(sizes[2]));
    if total != Some(data_len) {
        return Err(Error("output of another length"));
    }
    let mut rc = bcj2::Rc::new(Pull::<1>::new(rc_len)).ok_or(Error("bad range coder"))?;
    let mut probs = [1024u16; 258];
    let mut data = Pull::<0>::new(data_len);
    let (mut main, mut call, mut jump) = (Push::<0>::new(sizes[0]), Push::<1>::new(sizes[1]), Push::<2>::new(sizes[2]));
    let mut prev = 0u8;
    loop {
        // (a byte and the target after it)
        data.fill(5);
        if data.lo == data.hi {
            break;
        }
        let b = data.buf[data.lo];
        data.lo += 1;
        main.put(&[b])?;
        if !candidate(prev, b) || !rc.bit(&mut probs[context(prev, b)]).ok_or_else(corrupt)? {
            prev = b;
            continue;
        }
        let t: [u8; 4] = data.buf[data.lo..data.hi].get(..4).ok_or_else(corrupt)?.try_into().unwrap();
        let abs = u32::from_le_bytes(t).wrapping_add(data.position() as u32 + 4);
        match b == 0xe8 {
            true => call.put(&abs.to_be_bytes())?,
            false => jump.put(&abs.to_be_bytes())?,
        }
        prev = t[3];
        data.lo += 4;
    }
    main.finish()?;
    call.finish()?;
    jump.finish()
}
