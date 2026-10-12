//! The residual pipeline shared by the FLAC and PCM paths: per frame, the LMS stage and
//! the token / raw bit coding of [`model`], with their state across frames; the built-in
//! tables; the byte stream helpers and the serialisation of subframe parameters.

use crate::bits::{self, BitReader, FastWriter};
use crate::flac::{self, ESCAPE, Frame, SUB_CONSTANT, SUB_LPC, SUB_VERBATIM};
use crate::model::{self, Lms};
use crate::rans::{self, Tables, builtin};
use recons_common::varint;
use std::sync::OnceLock;

/// The built-in frequency tables (see `train`), or flat ones if none are built in.
static BUILTIN: &[u8] = include_bytes!("../model/builtin.bin");

#[derive(Debug)]
pub struct Error(pub &'static str);

/// Identifies the context layout the built-in tables were trained for.
pub fn layout_id() -> u32 {
    builtin::layout_id([model::N_CTX, model::N_TOK, model::TAPS, 1].map(|v| v as u32))
}

pub fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| Tables::new(&builtin_freqs().unwrap_or_else(default_freqs)))
}

/// Geometric-like tables for when nothing is built in.
fn default_freqs() -> Vec<Vec<u16>> {
    (0..model::N_CTX)
        .map(|_| {
            let counts: Vec<u64> = (0..model::N_TOK).map(|t| 1_000_000u64 >> (t / 3).min(40)).collect();
            let mut f = vec![0u16; model::N_TOK];
            rans::normalize(&counts, &mut f);
            f
        })
        .collect()
}

/// Serialises frequency tables (for the built-in model file).
pub fn freqs_to_bytes(f: &[Vec<u16>]) -> Vec<u8> {
    builtin::to_bytes(layout_id(), f)
}

fn builtin_freqs() -> Option<Vec<Vec<u16>>> {
    builtin::from_bytes(BUILTIN, layout_id(), std::iter::repeat_n(model::N_TOK, model::N_CTX))
}

/// The coded form of a file.
pub struct Coded {
    pub streams: Vec<Vec<u8>>,
    pub words: Vec<u32>,
    pub raw: [Vec<u8>; 2],
}

pub use varint::{put as put_varint, unzz, zz};

/// Byte stream reader.
pub struct Rd<'a> {
    pub d: &'a [u8],
    pub p: usize,
}

impl<'a> Rd<'a> {
    pub fn new(d: &'a [u8]) -> Self {
        Rd { d, p: 0 }
    }
    #[inline(always)]
    pub fn byte(&mut self) -> Result<u8, Error> {
        let b = *self.d.get(self.p).ok_or(Error("stream truncated"))?;
        self.p += 1;
        Ok(b)
    }
    #[inline(always)]
    pub fn varint(&mut self) -> Result<u64, Error> {
        varint::get(self.d, &mut self.p).map_err(|e| Error(e.0))
    }
    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let s = self.d.get(self.p..self.p.checked_add(n).ok_or(Error("size"))?).ok_or(Error("stream truncated"))?;
        self.p += n;
        Ok(s)
    }
    pub fn done(&self) -> bool {
        self.p == self.d.len()
    }
}

/// What is done per residual, by [`for_each_residual`] (a trait rather than a closure,
/// so that it is always inlined: the coder state then stays in registers).
trait Visit {
    /// Coder state per channel parity.
    type S: Copy;
    fn visit(&mut self, ch: usize, i: usize, sc: &mut model::Scale, st: &mut Self::S);
}

/// Residual coding order: time major, channels minor. Visits each residual with the
/// channel's scale state and the coder state of its parity (`st[ch & 1]`: two raw bit
/// streams, so that two channels decode independently).
#[inline(always)]
fn for_each_residual<V: Visit>(
    fr: &Frame,
    avg: &mut [model::Scale; flac::MAX_CHANNELS],
    st: &mut [V::S; 2],
    v: &mut V,
) {
    let n = fr.block_size;
    let mut orders = [usize::MAX; flac::MAX_CHANNELS];
    let mut chs = [0usize; flac::MAX_CHANNELS];
    let mut nch = 0;
    for ch in 0..fr.channels {
        if fr.sub[ch].has_residual() {
            orders[ch] = fr.sub[ch].order as usize;
            chs[nch] = ch;
            nch += 1;
        }
    }
    if nch == 0 {
        return;
    }
    let chs = &chs[..nch];
    let min_order = chs.iter().map(|&c| orders[c]).min().unwrap();
    let max_order = chs.iter().map(|&c| orders[c]).max().unwrap();
    for t in min_order..max_order {
        for &ch in chs {
            if t >= orders[ch] {
                v.visit(ch, t - orders[ch], &mut avg[ch], &mut st[ch & 1]);
            }
        }
    }
    // The common cases, with the state in locals.
    if nch == 2 && chs[0] & 1 == 0 && chs[1] & 1 == 1 {
        let (a, b) = (chs[0], chs[1]);
        let (oa, ob) = (orders[a], orders[b]);
        let (mut va, mut vb) = (avg[a], avg[b]);
        let (mut sa, mut sb) = (st[0], st[1]);
        for t in max_order..n {
            v.visit(a, t - oa, &mut va, &mut sa);
            v.visit(b, t - ob, &mut vb, &mut sb);
        }
        (avg[a], avg[b]) = (va, vb);
        *st = [sa, sb];
    } else if nch == 1 {
        let a = chs[0];
        let mut va = avg[a];
        let mut sa = st[a & 1];
        for t in max_order..n {
            v.visit(a, t - orders[a], &mut va, &mut sa);
        }
        avg[a] = va;
        st[a & 1] = sa;
    } else {
        for t in max_order..n {
            for &ch in chs {
                v.visit(ch, t - orders[ch], &mut avg[ch], &mut st[ch & 1]);
            }
        }
    }
}

/// Residual pointers by channel (null past the frame's channels).
fn res_ptrs(f: &mut Frame) -> [*mut i32; flac::MAX_CHANNELS] {
    std::array::from_fn(|ch| f.res.get_mut(ch).map_or(std::ptr::null_mut(), |r| r.as_mut_ptr()))
}

struct EncV {
    el: rans::EncLocal,
    res: [*mut i32; flac::MAX_CHANNELS],
}

impl Visit for EncV {
    type S = bits::FastBits;
    #[inline(always)]
    fn visit(&mut self, ch: usize, i: usize, sc: &mut model::Scale, st: &mut bits::FastBits) {
        // SAFETY: indices come from for_each_residual, within the channel's residuals.
        let e = unsafe { *self.res[ch].add(i) };
        model::encode_one(sc, (ch > 0) as usize, e, &mut self.el, st);
    }
}

/// Decoding; without `CHECKED`, the streams must have room for the whole frame (see
/// [`model::decode_one`]).
struct DecV<'a, const CHECKED: bool> {
    dl: rans::DecLocal,
    res: [*mut i32; flac::MAX_CHANNELS],
    _raw: std::marker::PhantomData<BitReader<'a>>,
}

impl<'a, const CHECKED: bool> Visit for DecV<'a, CHECKED> {
    type S = BitReader<'a>;
    #[inline(always)]
    fn visit(&mut self, ch: usize, i: usize, sc: &mut model::Scale, st: &mut BitReader<'a>) {
        let e = model::decode_one::<CHECKED>(sc, (ch > 0) as usize, &mut self.dl, st);
        // SAFETY: indices come from for_each_residual, within the channel's residuals.
        unsafe { *self.res[ch].add(i) = e };
    }
}

struct CountV<'a> {
    counts: &'a mut [Vec<u64>],
    res: [*mut i32; flac::MAX_CHANNELS],
}

impl Visit for CountV<'_> {
    type S = ();
    #[inline(always)]
    fn visit(&mut self, ch: usize, i: usize, sc: &mut model::Scale, _: &mut ()) {
        // SAFETY: indices come from for_each_residual, within the channel's residuals.
        let e = unsafe { *self.res[ch].add(i) };
        let (c, t) = model::count_one(sc, (ch > 0) as usize, e);
        self.counts[c][t as usize] += 1;
    }
}

/// Runs the LMS stage over the frame's residuals in place (channel pairs together).
///
/// `PIPE` only separates the instances of the pipeline stages ([`LmsEncoder`],
/// [`LmsDecoder`]) from the serial ones, down to the NEON loops: LLVM inlines the serial
/// chain whole when each function in it has a single caller; shared with the pipeline
/// (or forced with `#[inline(always)]`), it was compiled worse, about 2% of serial
/// decoding.
fn run_lms<const DEC: bool, const PIPE: bool>(f: &mut Frame, lms: &mut [Lms], sc: &[model::Scale; flac::MAX_CHANNELS]) {
    let mut chs = [0usize; flac::MAX_CHANNELS];
    let mut n = 0;
    for ch in 0..f.channels {
        if f.sub[ch].has_residual() {
            lms[ch].set_scale(&sc[ch]);
            chs[n] = ch;
            n += 1;
        }
    }
    let mut i = 0;
    while i + 1 < n {
        let (a, b) = (chs[i], chs[i + 1]);
        let (ra, rb) = f.res.split_at_mut(b);
        let (la, lb) = lms.split_at_mut(b);
        model::lms_run2_in::<DEC, PIPE>(&mut la[a], &mut ra[a], &mut lb[0], &mut rb[0]);
        i += 2;
    }
    if i < n {
        lms[chs[i]].run_in::<DEC, PIPE>(&mut f.res[chs[i]]);
    }
}

/// Encoder of the residuals of a stream of frames.
pub struct ResEncoder {
    lms: Vec<Lms>,
    avg: [model::Scale; flac::MAX_CHANNELS],
    enc: rans::Encoder,
    raw: [FastWriter; 2],
}

impl ResEncoder {
    /// `size`: about the input size, for preallocation.
    pub fn new(size: usize) -> Self {
        ResEncoder {
            lms: vec![Lms::default(); flac::MAX_CHANNELS],
            avg: [model::Scale::default(); flac::MAX_CHANNELS],
            enc: rans::Encoder::with_capacity(size),
            raw: [FastWriter::with_capacity(size / 4), FastWriter::with_capacity(size / 4)],
        }
    }

    /// Codes the frame's residuals (which the LMS stage overwrites).
    pub fn frame(&mut self, f: &mut Frame) {
        run_lms::<false, false>(f, &mut self.lms, &self.avg);
        self.code(f);
    }

    /// The coding part of [`ResEncoder::frame`], for errors from an [`LmsEncoder`] (this
    /// encoder's own LMS state is then unused).
    #[inline(always)]
    pub fn code(&mut self, f: &mut Frame) {
        let total: usize = f.res[..f.channels].iter().map(|r| r.len()).sum();
        self.enc.reserve(total);
        self.raw[0].reserve(total * 8 + 16);
        self.raw[1].reserve(total * 8 + 16);
        let mut rl = [self.raw[0].local(), self.raw[1].local()];
        let mut v = EncV { el: self.enc.local(tables()), res: res_ptrs(f) };
        for_each_residual(f, &mut self.avg, &mut rl, &mut v);
        self.enc.put_back(v.el);
        self.raw[0].put_back(rl[0]);
        self.raw[1].put_back(rl[1]);
    }

    /// The rANS words and the two raw bit streams.
    pub fn finish(self) -> (Vec<u32>, [Vec<u8>; 2]) {
        let [r0, r1] = self.raw;
        (self.enc.finish(tables()), [r0.finish(), r1.finish()])
    }
}

/// The LMS stage of [`ResEncoder`] on its own, for a pipeline (the coding on another
/// thread): it follows the scale state that the coder will have at each frame.
pub struct LmsEncoder {
    lms: Vec<Lms>,
    avg: [model::Scale; flac::MAX_CHANNELS],
}

impl Default for LmsEncoder {
    fn default() -> Self {
        LmsEncoder { lms: vec![Lms::default(); flac::MAX_CHANNELS], avg: [model::Scale::default(); flac::MAX_CHANNELS] }
    }
}

impl LmsEncoder {
    /// Residuals to errors, in place; [`ResEncoder::code`] then codes them.
    pub fn frame(&mut self, f: &mut Frame) {
        run_lms::<false, true>(f, &mut self.lms, &self.avg);
        let mut chs = (0..f.channels).filter(|&ch| f.sub[ch].has_residual());
        while let Some(a) = chs.next() {
            let b = chs.next();
            let (sa, sb) = match b {
                Some(b) => {
                    let (x, y) = self.avg.split_at_mut(b);
                    (&mut x[a], Some((&mut y[0], &f.res[b][..])))
                }
                None => (&mut self.avg[a], None),
            };
            model::advance(sa, &f.res[a], sb);
        }
    }
}

/// Decoder of the residuals of a stream of frames.
pub struct ResDecoder<'a> {
    lms: Vec<Lms>,
    avg: [model::Scale; flac::MAX_CHANNELS],
    dec: rans::Decoder<'static>,
    raw_len: [usize; 2],
    raw: [BitReader<'a>; 2],
}

impl<'a> ResDecoder<'a> {
    /// `raw` padded (see [`bits::padded`]).
    pub fn new(words: &[u32], raw: &'a [Vec<u8>; 2]) -> Result<Self, Error> {
        let t = tables();
        if t.flat_log() != Some(model::FLAT_LOG) {
            return Err(Error("tables"));
        }
        Ok(ResDecoder {
            lms: vec![Lms::default(); flac::MAX_CHANNELS],
            avg: [model::Scale::default(); flac::MAX_CHANNELS],
            dec: rans::Decoder::new(t, words),
            raw_len: [raw[0].len(), raw[1].len()],
            raw: [BitReader::new(&raw[0], 0), BitReader::new(&raw[1], 0)],
        })
    }

    /// Decodes the frame's residuals; the residual vectors must have their sizes.
    pub fn frame(&mut self, f: &mut Frame) -> Result<(), Error> {
        // The errors, then the LMS stage in place (with the scale state from before the
        // errors, as the encoder saw it).
        let sc0 = self.avg;
        self.decode_errors(f);
        run_lms::<true, false>(f, &mut self.lms, &sc0);
        self.check()
    }

    /// The first part of [`ResDecoder::frame`], for a pipeline: decodes the errors and
    /// returns the scale state from before them, for [`LmsDecoder::frame`] (this decoder's
    /// own LMS state is then unused).
    pub fn errors(&mut self, f: &mut Frame) -> Result<[model::Scale; flac::MAX_CHANNELS], Error> {
        let sc0 = self.avg;
        self.decode_errors(f);
        self.check()?;
        Ok(sc0)
    }

    #[inline(always)]
    fn decode_errors(&mut self, f: &mut Frame) {
        // Without bounds checks if the streams have room for the whole frame.
        let nsym: usize = f.res[..f.channels].iter().map(|r| r.len()).sum();
        let dl = self.dec.local();
        let fits = dl.room() > nsym && self.raw.iter().all(|r| r.room() > nsym * model::MAX_RAW_BITS);
        let res = res_ptrs(f);
        let dl = if fits {
            let mut v = DecV::<false> { dl, res, _raw: std::marker::PhantomData };
            for_each_residual(f, &mut self.avg, &mut self.raw, &mut v);
            v.dl
        } else {
            let mut v = DecV::<true> { dl, res, _raw: std::marker::PhantomData };
            for_each_residual(f, &mut self.avg, &mut self.raw, &mut v);
            v.dl
        };
        self.dec.put_back(dl);
    }

    #[inline(always)]
    fn check(&self) -> Result<(), Error> {
        if self.raw[0].pos > self.raw_len[0] * 8 || self.raw[1].pos > self.raw_len[1] * 8 {
            return Err(Error("raw bits exhausted"));
        }
        Ok(())
    }

    /// Checks that no stream was read past its end.
    pub fn finish(&self) -> Result<(), Error> {
        if !self.dec.in_bounds() {
            return Err(Error("rans stream exhausted"));
        }
        Ok(())
    }
}

/// The LMS stage of [`ResDecoder`] on its own, for a pipeline (see [`ResDecoder::errors`]).
pub struct LmsDecoder {
    lms: Vec<Lms>,
}

impl Default for LmsDecoder {
    fn default() -> Self {
        LmsDecoder { lms: vec![Lms::default(); flac::MAX_CHANNELS] }
    }
}

impl LmsDecoder {
    /// Errors to residuals, in place; `sc0` from [`ResDecoder::errors`].
    pub fn frame(&mut self, f: &mut Frame, sc0: &[model::Scale; flac::MAX_CHANNELS]) {
        run_lms::<true, true>(f, &mut self.lms, sc0);
    }
}

/// Counts (context, token) pairs of a stream of frames (for training).
pub struct ResCounter {
    lms: Vec<Lms>,
    avg: [model::Scale; flac::MAX_CHANNELS],
}

impl Default for ResCounter {
    fn default() -> Self {
        ResCounter { lms: vec![Lms::default(); flac::MAX_CHANNELS], avg: [model::Scale::default(); flac::MAX_CHANNELS] }
    }
}

impl ResCounter {
    pub fn frame(&mut self, f: &mut Frame, counts: &mut [Vec<u64>]) {
        run_lms::<false, false>(f, &mut self.lms, &self.avg);
        let mut v = CountV { counts, res: res_ptrs(f) };
        for_each_residual(f, &mut self.avg, &mut [(); 2], &mut v);
    }
}

/// Serialises the subframe parameters of `f` (type, wasted bits, predictor) to `sub`
/// and the samples stored in it (warm-up, constant, verbatim) to `warm`; with `rice`, also
/// the residual coding method and parameters.
pub fn put_subframes(f: &Frame, sub: &mut Vec<u8>, warm: &mut Vec<u8>, rice: bool) {
    let mut vi = 0;
    for sf in &f.sub[..f.channels] {
        sub.push(sf.typ << 1 | (sf.wasted > 0) as u8);
        if sf.wasted > 0 {
            sub.push(sf.wasted);
        }
        match sf.typ {
            SUB_CONSTANT => put_varint(warm, zz(sf.warm[0])),
            SUB_VERBATIM => {
                for &v in &f.verbatim[vi..vi + f.block_size] {
                    put_varint(warm, zz(v));
                }
                vi += f.block_size;
            }
            _ => {
                for &v in &sf.warm[..sf.order as usize] {
                    put_varint(warm, zz(v));
                }
                if sf.typ >= SUB_LPC {
                    sub.push(sf.precision);
                    sub.push(sf.shift as u8);
                    for &c in &sf.coefs[..sf.order as usize] {
                        put_varint(sub, zz(c as i64));
                    }
                }
                if rice {
                    sub.push(sf.method << 4 | sf.porder);
                    sub.extend_from_slice(&sf.params);
                }
            }
        }
    }
}

/// Reads what [`put_subframes`] wrote into `f`, whose block size, channel assignment,
/// channels and sample size must be set. Sizes the residual vectors.
pub fn get_subframes(f: &mut Frame, sub: &mut Rd, warm: &mut Rd, rice: bool) -> Result<(), Error> {
    let block = f.block_size;
    f.sub.resize(f.channels, Default::default());
    f.res.resize(f.channels, Vec::new());
    f.verbatim.clear();
    for ch in 0..f.channels {
        let side = matches!((f.assign, ch), (8, 1) | (9, 0) | (10, 1));
        let sf = &mut f.sub[ch];
        let x = sub.byte()?;
        sf.typ = x >> 1;
        sf.wasted = if x & 1 != 0 { sub.byte()? } else { 0 };
        let full = f.bps + side as u32;
        if sf.typ > 63 || sf.wasted as u32 >= full || (x & 1 != 0 && sf.wasted == 0) {
            return Err(Error("bad subframe"));
        }
        let bits = full - sf.wasted as u32;
        sf.bits = bits as u8;
        sf.order = 0;
        sf.params.clear();
        f.res[ch].clear();
        let rv = |r: &mut Rd| -> Result<i64, Error> {
            let v = unzz(r.varint()?);
            if (v >> (bits - 1)) != 0 && (v >> (bits - 1)) != -1 {
                return Err(Error("sample range"));
            }
            Ok(v)
        };
        match sf.typ {
            SUB_CONSTANT => sf.warm[0] = rv(warm)?,
            SUB_VERBATIM => {
                for _ in 0..block {
                    f.verbatim.push(rv(warm)?);
                }
            }
            typ if (flac::SUB_FIXED..=flac::SUB_FIXED + 4).contains(&typ) || typ >= SUB_LPC => {
                let order = if typ >= SUB_LPC { typ - SUB_LPC + 1 } else { typ - flac::SUB_FIXED } as usize;
                if order > block {
                    return Err(Error("order"));
                }
                sf.order = order as u8;
                for i in 0..order {
                    sf.warm[i] = rv(warm)?;
                }
                if typ >= SUB_LPC {
                    sf.precision = sub.byte()?;
                    sf.shift = sub.byte()? as i8;
                    if sf.precision == 0 || sf.precision > 15 || !(-16..16).contains(&sf.shift) {
                        return Err(Error("bad precision"));
                    }
                    for i in 0..order {
                        let c = unzz(sub.varint()?);
                        if (c >> (sf.precision - 1)) != 0 && (c >> (sf.precision - 1)) != -1 {
                            return Err(Error("coefficient range"));
                        }
                        sf.coefs[i] = c as i32;
                    }
                }
                if rice {
                    let b = sub.byte()?;
                    sf.method = b >> 4;
                    sf.porder = b & 15;
                    let np = 1usize << sf.porder;
                    if sf.method > 1 || !block.is_multiple_of(np) || block / np < order {
                        return Err(Error("bad partitions"));
                    }
                    let esc = (1u8 << (4 + sf.method)) - 1;
                    for _ in 0..np {
                        let k = sub.byte()?;
                        if (k & ESCAPE == 0 && k >= esc) || (k & ESCAPE != 0 && k & !(ESCAPE | 31) != 0) {
                            return Err(Error("bad parameter"));
                        }
                        sf.params.push(k);
                    }
                }
                f.res[ch].resize(block - order, 0);
            }
            _ => return Err(Error("bad subframe type")),
        }
    }
    Ok(())
}
