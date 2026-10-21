//! The file format (`FORMAT.md` specifies it).
//!
//! ```text
//! magic 69 04 7f d1 b8 ed, u16-LE version (0), varint(length), u64-LE(xxh3 of the original)
//! blocks, until the length is reached:
//!   u8 mode, varint(raw length)
//!   mode 0 (whole): varint(coded length) zstd(block)
//!   mode 1 (coded): per stream (block::NSTREAMS):
//!                     varint(raw length) [u8(0 stored | 1 zstd | 2 rANS) varint(coded length) data]
//! ```
//!
//! Blocks end at a line end where there is one; coded blocks carry state over (previous
//! values, templates), so they are decoded in order.
//!
//! With more than one thread, the line transform of blocks stays on the calling thread, in
//! order; verification (a decoder of its own) and the stream coding of each block run on
//! other threads, and decompression decodes the streams of the blocks ahead on other
//! threads. The output is the same whatever the number of threads.

use crate::ans;
use crate::block::{self, NSTREAMS};
use crate::varint::{Rd, put_uv};
use crate::{Error, Result, corrupt};
use std::collections::VecDeque;
use std::io::Write;
use std::sync::{Arc, Mutex, mpsc};
use xxhash_rust::xxh3::{Xxh3, xxh3_64};

/// `aQR/0bjt` in base64.
const MAGIC: &[u8; 6] = &[0x69, 0x04, 0x7f, 0xd1, 0xb8, 0xed];
const VERSION: u16 = 0;
/// Magic and version.
const HEAD: usize = MAGIC.len() + 2;
/// Largest block the decoder accepts (bounds its memory).
pub(crate) const MAX_BLOCK: usize = 64 << 20;

/// How a stream is stored.
pub(crate) const M_STORED: u8 = 0;
const M_ZSTD: u8 = 1;
/// rANS coded (`ans`).
pub(crate) const M_ANS: u8 = 2;

/// Streams of LEB128 values, which may be rANS coded.
pub(crate) const AC_STREAMS: [bool; NSTREAMS] = {
    let mut t = [false; NSTREAMS];
    let mut i = 0;
    while i < NSTREAMS {
        t[i] = i == block::S_SCALE
            || (i >= block::S_FESC && i < block::S_FESC + 3)
            || (i >= block::S_DUP && i < block::S_DUP + 3)
            || i >= block::S_VAL;
        i += 1;
    }
    t
};

pub(crate) type Streams = [Vec<u8>; NSTREAMS];

pub(crate) fn new_streams() -> Streams {
    std::array::from_fn(|_| Vec::new())
}

#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub level: i32,
    /// Input bytes per block.
    pub block: usize,
    /// Decode every block and compare (stored whole on a mismatch).
    pub verify: bool,
    /// For tests: blocks (index mod 64) stored whole as if verification failed.
    pub force_whole: u64,
    /// Threads (1: all on the calling thread).
    pub threads: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options { level: 3, block: 4 << 20, verify: true, force_whole: 0, threads: 1 }
    }
}

/// Raw and coded sizes per stream, for analysis.
#[derive(Clone, Debug)]
pub struct Stats {
    pub raw: [u64; NSTREAMS],
    pub coded: [u64; NSTREAMS],
    /// Blocks stored whole, and their coded size.
    pub whole: u64,
    /// Blocks rANS coded, per stream.
    pub rc: [u64; NSTREAMS],
    pub whole_coded: u64,
    /// Seconds per phase (summed over threads).
    pub transform_s: f64,
    pub zstd_s: f64,
    pub verify_s: f64,
}

impl Default for Stats {
    fn default() -> Self {
        Stats {
            raw: [0; NSTREAMS],
            coded: [0; NSTREAMS],
            whole: 0,
            rc: [0; NSTREAMS],
            whole_coded: 0,
            transform_s: 0.0,
            zstd_s: 0.0,
            verify_s: 0.0,
        }
    }
}

impl Stats {
    fn add(&mut self, o: &Stats) {
        for i in 0..NSTREAMS {
            self.raw[i] += o.raw[i];
            self.coded[i] += o.coded[i];
            self.rc[i] += o.rc[i];
        }
        self.whole += o.whole;
        self.whole_coded += o.whole_coded;
        self.transform_s += o.transform_s;
        self.zstd_s += o.zstd_s;
        self.verify_s += o.verify_s;
    }
}

pub fn compress(src: &[u8]) -> Vec<u8> {
    compress_with(src, &Options::default(), &mut Stats::default())
}

pub fn compress_with(src: &[u8], opt: &Options, st: &mut Stats) -> Vec<u8> {
    let mut out = Vec::new();
    compress_to(src, opt, st, &mut out).expect("writing to a Vec");
    out
}

/// The end of the block starting at `p`: at most `bsize` bytes, at a line end if there is
/// one.
pub(crate) fn block_end(src: &[u8], p: usize, bsize: usize) -> usize {
    let e = (p + bsize).min(src.len());
    if e < src.len()
        && let Some(i) = memchr::memrchr(b'\n', &src[p..e])
    {
        return p + i + 1;
    }
    e
}

/// Scratch space and the zstd context for coding blocks.
struct Coder {
    cz: zstd::bulk::Compressor<'static>,
    z: Vec<u8>,
    ac: Vec<u8>,
}

impl Coder {
    fn new(level: i32) -> Self {
        Coder { cz: zstd::bulk::Compressor::new(level).expect("zstd"), z: Vec::new(), ac: Vec::new() }
    }

    /// Appends block `b` stored whole.
    fn whole(&mut self, b: &[u8], out: &mut Vec<u8>, st: &mut Stats) {
        out.push(0);
        put_uv(out, b.len() as u64);
        self.z.clear();
        self.z.reserve(zstd::zstd_safe::compress_bound(b.len()));
        self.cz.compress_to_buffer(b, &mut self.z).expect("zstd");
        put_uv(out, self.z.len() as u64);
        out.extend_from_slice(&self.z);
        st.whole += b.len() as u64;
        st.whole_coded += self.z.len() as u64;
    }

    /// Appends a coded block of `n` bytes from its streams.
    fn coded(&mut self, n: usize, streams: &Streams, out: &mut Vec<u8>, st: &mut Stats) {
        out.push(1);
        put_uv(out, n as u64);
        let (z, ac) = (&mut self.z, &mut self.ac);
        for (i, s) in streams.iter().enumerate() {
            put_uv(out, s.len() as u64);
            st.raw[i] += s.len() as u64;
            if s.is_empty() {
                continue;
            }
            // Both zstd and rANS for streams that may take either.
            let try_ans = AC_STREAMS[i] && s.len() > 64;
            z.clear();
            if s.len() > 64 {
                z.reserve(zstd::zstd_safe::compress_bound(s.len()));
                self.cz.compress_to_buffer(s, &mut *z).expect("zstd");
            }
            let (mut m, mut d): (u8, &[u8]) =
                if !z.is_empty() && z.len() < s.len() { (M_ZSTD, &z[..]) } else { (M_STORED, &s[..]) };
            if try_ans {
                // rANS coded, if smaller. (Verification covers the transform; rANS, like
                // zstd, is trusted: tests and fuzzing cover it.)
                ac.clear();
                if ans::encode(s, ac).is_some() && ac.len() < d.len() {
                    (m, d) = (M_ANS, &ac[..]);
                    st.rc[i] += 1;
                }
            }
            out.push(m);
            put_uv(out, d.len() as u64);
            out.extend_from_slice(d);
            st.coded[i] += d.len() as u64;
        }
    }
}

fn header(src: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    put_uv(&mut out, src.len() as u64);
    out.extend_from_slice(&xxh3_64(src).to_le_bytes());
    out
}

/// Compresses `src` into `w`, a block at a time: memory besides the input stays bounded
/// by the block size (and the threads).
pub fn compress_to<W: Write>(src: &[u8], opt: &Options, st: &mut Stats, w: &mut W) -> std::io::Result<()> {
    if opt.threads > 1 {
        return compress_par(src, opt, st, w);
    }
    let mut out = header(src);
    let bsize = opt.block.clamp(1 << 10, MAX_BLOCK);
    let mut enc = block::Encoder::default();
    let mut dec = block::Decoder::default();
    let mut streams = new_streams();
    let mut check = Vec::new();
    let mut coder = Coder::new(opt.level);
    let mut p = 0;
    let mut nblock = 0;
    while p < src.len() {
        w.write_all(&out)?;
        out.clear();
        let e = block_end(src, p, bsize);
        let b = &src[p..e];
        p = e;
        let t = std::time::Instant::now();
        let (em, dm) = (enc.mark(), dec.mark());
        enc.encode(b, &mut streams);
        st.transform_s += t.elapsed().as_secs_f64();
        let mut ok = opt.force_whole >> (nblock % 64) & 1 == 0;
        nblock += 1;
        if ok && opt.verify {
            let t = std::time::Instant::now();
            check.clear();
            ok = dec.decode(&streams, &mut check) && check == b;
            st.verify_s += t.elapsed().as_secs_f64();
        }
        let t = std::time::Instant::now();
        if !ok {
            enc.reset(&em);
            dec.reset(&dm);
            coder.whole(b, &mut out, st);
        } else {
            coder.coded(b.len(), &streams, &mut out, st);
        }
        st.zstd_s += t.elapsed().as_secs_f64();
    }
    w.write_all(&out)
}

/// What the other threads report about a block of generation `generation` (blocks after
/// one stored whole are coded again, in a new generation).
enum Report {
    Parsed { generation: u64, idx: usize, block: Box<block::ParsedBlock>, secs: f64 },
    Verified { generation: u64, idx: usize, ok: bool, secs: f64 },
    Coded { generation: u64, idx: usize, out: Vec<u8>, st: Box<Stats> },
}

/// Work for the pool: parsing a block's lines, or coding its streams.
enum Job {
    Parse { generation: u64, idx: usize, range: std::ops::Range<usize>, styles: block::Styles },
    Code { generation: u64, idx: usize, n: usize, streams: Arc<Streams> },
}

struct VerifyJob {
    generation: u64,
    idx: usize,
    range: std::ops::Range<usize>,
    streams: Arc<Streams>,
    /// Stored whole whatever (`Options::force_whole`).
    forced: bool,
}

/// A block sent for parsing.
struct Parsing {
    idx: usize,
    range: std::ops::Range<usize>,
    styles: block::Styles,
    parsed: Option<Box<block::ParsedBlock>>,
}

/// A block coded, in flight.
struct Pending {
    idx: usize,
    range: std::ops::Range<usize>,
    mark: block::Mark,
    verified: Option<bool>,
    coded: Option<(Vec<u8>, Box<Stats>)>,
}

impl Pending {
    fn done(&self) -> bool {
        matches!(self, Pending { verified: Some(false), .. } | Pending { verified: Some(true), coded: Some(_), .. })
    }
}

fn compress_par<W: Write>(src: &[u8], opt: &Options, st: &mut Stats, w: &mut W) -> std::io::Result<()> {
    w.write_all(&header(src))?;
    let bsize = opt.block.clamp(1 << 10, MAX_BLOCK);
    // Blocks in flight at each stage: enough to keep the threads busy, few enough to bound
    // memory.
    let limit = opt.threads + 1;
    std::thread::scope(|sc| {
        let (rtx, rrx) = mpsc::channel::<Report>();
        let (vtx, vrx) = mpsc::channel::<VerifyJob>();
        let (jtx, jrx) = mpsc::channel::<Job>();
        if opt.verify {
            let rtx = rtx.clone();
            sc.spawn(move || {
                // The decoder the file will have: a block that fails stays undone, and the
                // rest of its generation is skipped (the encoder codes it again).
                let mut dec = block::Decoder::default();
                let mut check = Vec::new();
                let mut dead = None;
                for j in vrx {
                    if dead == Some(j.generation) {
                        continue;
                    }
                    let t = std::time::Instant::now();
                    let ok = !j.forced && {
                        let dm = dec.mark();
                        check.clear();
                        let ok = dec.decode(&j.streams, &mut check) && check == src[j.range];
                        if !ok {
                            dec.reset(&dm);
                        }
                        ok
                    };
                    if !ok {
                        dead = Some(j.generation);
                    }
                    let secs = t.elapsed().as_secs_f64();
                    if rtx.send(Report::Verified { generation: j.generation, idx: j.idx, ok, secs }).is_err() {
                        break;
                    }
                }
            });
        }
        let jrx = Arc::new(Mutex::new(jrx));
        for _ in 0..opt.threads.saturating_sub(2).max(1) {
            let (rtx, jrx) = (rtx.clone(), jrx.clone());
            sc.spawn(move || {
                let mut coder = Coder::new(opt.level);
                loop {
                    let Ok(j) = jrx.lock().expect("job queue").recv() else { break };
                    let t = std::time::Instant::now();
                    let r = match j {
                        Job::Parse { generation, idx, range, styles } => {
                            let block = Box::new(block::parse_block(&styles, &src[range]));
                            Report::Parsed { generation, idx, block, secs: t.elapsed().as_secs_f64() }
                        }
                        Job::Code { generation, idx, n, streams } => {
                            let mut out = Vec::new();
                            let mut st = Stats::default();
                            coder.coded(n, &streams, &mut out, &mut st);
                            st.zstd_s = t.elapsed().as_secs_f64();
                            Report::Coded { generation, idx, out, st: Box::new(st) }
                        }
                    };
                    if rtx.send(r).is_err() {
                        break;
                    }
                }
            });
        }
        drop(rtx);
        let mut enc = block::Encoder::default();
        let mut coder = Coder::new(opt.level);
        let mut streams = new_streams();
        let mut parsing: VecDeque<Parsing> = VecDeque::new();
        let mut pending: VecDeque<Pending> = VecDeque::new();
        let mut generation = 0u64;
        // Next block to send for parsing, its index, and the styles before it.
        let (mut dp, mut didx, mut chain) = (0usize, 0usize, enc.styles());
        let mut out = Vec::new();
        loop {
            while parsing.len() < limit && dp < src.len() {
                let e = block_end(src, dp, bsize);
                chain = block::next_styles(&chain, &src[dp..e]);
                jtx.send(Job::Parse { generation, idx: didx, range: dp..e, styles: chain }).expect("pool");
                parsing.push_back(Parsing { idx: didx, range: dp..e, styles: chain, parsed: None });
                (dp, didx) = (e, didx + 1);
            }
            if pending.front().is_some_and(Pending::done) {
                let f = pending.pop_front().expect("front");
                if let Some((o, s)) = f.coded.filter(|_| f.verified == Some(true)) {
                    st.add(&s);
                    w.write_all(&o)?;
                    continue;
                }
                // Undo it and the blocks after it, store it whole, code the rest again.
                enc.reset(&f.mark);
                let t = std::time::Instant::now();
                out.clear();
                coder.whole(&src[f.range.clone()], &mut out, st);
                st.zstd_s += t.elapsed().as_secs_f64();
                w.write_all(&out)?;
                (generation, dp, didx, chain) = (generation + 1, f.range.end, f.idx + 1, enc.styles());
                pending.clear();
                parsing.clear();
                continue;
            }
            if pending.len() < limit && parsing.front().is_some_and(|p| p.parsed.is_some()) {
                let p = parsing.pop_front().expect("front");
                let (idx, range) = (p.idx, p.range);
                let t = std::time::Instant::now();
                let mark = enc.mark();
                enc.encode_parsed(&p.styles, &src[range.clone()], p.parsed.as_deref().expect("parsed"), &mut streams);
                let streams = Arc::new(std::mem::replace(&mut streams, new_streams()));
                st.transform_s += t.elapsed().as_secs_f64();
                let forced = opt.force_whole >> (idx % 64) & 1 == 1;
                if opt.verify {
                    let j = VerifyJob { generation, idx, range: range.clone(), streams: streams.clone(), forced };
                    vtx.send(j).expect("verifier");
                }
                if !forced {
                    jtx.send(Job::Code { generation, idx, n: range.len(), streams }).expect("pool");
                }
                let verified = if opt.verify { None } else { Some(!forced) };
                pending.push_back(Pending { idx, range, mark, verified, coded: None });
                continue;
            }
            if pending.is_empty() && parsing.is_empty() {
                break;
            }
            match rrx.recv().expect("worker threads") {
                Report::Parsed { generation: g, idx: i, block, secs } if g == generation => {
                    let base = parsing.front().expect("parsing").idx;
                    parsing[i - base].parsed = Some(block);
                    st.transform_s += secs;
                }
                Report::Verified { generation: g, idx: i, ok, secs } if g == generation => {
                    let base = pending.front().expect("pending").idx;
                    pending[i - base].verified = Some(ok);
                    st.verify_s += secs;
                }
                Report::Coded { generation: g, idx: i, out, st: s } if g == generation => {
                    let base = pending.front().expect("pending").idx;
                    pending[i - base].coded = Some((out, s));
                }
                _ => {}
            }
        }
        Ok(())
    })
}

pub fn decompress(src: &[u8]) -> Result<Vec<u8>> {
    decompress_threads(src, 1)
}

/// `decompress` with `threads` threads.
pub fn decompress_threads(src: &[u8], threads: usize) -> Result<Vec<u8>> {
    let mut r = Rd::new(src);
    let len = if r.bytes(HEAD).is_some() { r.uv() as usize } else { 0 };
    // The output is the caller's; reserve it only up to what the input can plausibly hold.
    let mut out = Vec::with_capacity(len.min(src.len().saturating_mul(64)));
    decompress_to_threads(src, &mut out, threads)?;
    Ok(out)
}

/// A block as stored: whole, or its streams (raw length, how, data).
pub(crate) enum Stored<'a> {
    /// Zstd data of the block.
    Whole(&'a [u8]),
    /// The block itself (formulas).
    Plain(&'a [u8]),
    Coded(Box<[(usize, u8, &'a [u8]); NSTREAMS]>),
}

/// Reads the next block's description (`left`: bytes of the file still to come).
fn read_block<'a>(r: &mut Rd<'a>, left: usize) -> Result<(usize, Stored<'a>)> {
    let mode = r.u8();
    let n = r.uv() as usize;
    if r.overrun() || n == 0 || n > MAX_BLOCK || n > left {
        return Err(corrupt());
    }
    let b = match mode {
        0 => {
            let c = r.uv() as usize;
            Stored::Whole(r.bytes(c).ok_or_else(corrupt)?)
        }
        1 => {
            let mut s = Box::new([(0, 0, &[][..]); NSTREAMS]);
            for (i, e) in s.iter_mut().enumerate() {
                let raw = r.uv() as usize;
                if raw == 0 {
                    continue;
                }
                let m = r.u8();
                let c = r.uv() as usize;
                let ok = match m {
                    M_STORED => c == raw,
                    M_ZSTD => true,
                    M_ANS => AC_STREAMS[i],
                    _ => false,
                };
                if raw > 8 * n + 64 || !ok {
                    return Err(corrupt());
                }
                *e = (raw, m, r.bytes(c).ok_or_else(corrupt)?);
            }
            Stored::Coded(s)
        }
        _ => return Err(corrupt()),
    };
    Ok((n, b))
}

/// Decodes the streams of a coded block into `streams`, or a whole block (of `n` bytes)
/// into `out`.
fn unpack(
    b: &Stored,
    n: usize,
    dz: &mut zstd::bulk::Decompressor,
    streams: &mut Streams,
    out: &mut Vec<u8>,
) -> Result<()> {
    match b {
        Stored::Whole(d) => {
            let s = out.len();
            out.reserve(n);
            dz.decompress_to_buffer(d, out).map_err(|_| corrupt())?;
            if out.len() - s != n {
                return Err(corrupt());
            }
        }
        Stored::Plain(d) => out.extend_from_slice(d),
        Stored::Coded(desc) => {
            for (s, &(raw, m, d)) in streams.iter_mut().zip(desc.iter()) {
                s.clear();
                if raw == 0 {
                    continue;
                }
                match m {
                    M_STORED => s.extend_from_slice(d),
                    M_ANS => {
                        if !ans::decode(d, raw, s) {
                            return Err(corrupt());
                        }
                    }
                    _ => {
                        s.reserve(raw);
                        dz.decompress_to_buffer(d, s).map_err(|_| corrupt())?;
                        if s.len() != raw {
                            return Err(corrupt());
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Reads the file header: the length and the hash.
fn read_header(r: &mut Rd) -> Result<(usize, u64)> {
    if r.bytes(MAGIC.len()) != Some(MAGIC) {
        return Err(Error("not an objr file"));
    }
    if r.bytes(2) != Some(&VERSION.to_le_bytes()[..]) {
        return Err(Error("unsupported objr format version"));
    }
    let len = r.uv() as usize;
    let hash = u64::from_le_bytes(r.bytes(8).ok_or_else(corrupt)?.try_into().unwrap());
    if r.overrun() {
        return Err(corrupt());
    }
    Ok((len, hash))
}

/// Decompresses `src` into `w`, a block at a time; returns the length. On an error, what
/// was written is not the original.
pub fn decompress_to<W: Write>(src: &[u8], w: &mut W) -> Result<usize> {
    decompress_to_threads(src, w, 1)
}

/// `decompress_to` with `threads` threads.
pub fn decompress_to_threads<W: Write>(src: &[u8], w: &mut W, threads: usize) -> Result<usize> {
    let mut r = Rd::new(src);
    let (len, hash) = read_header(&mut r)?;
    let digest = decode_blocks(&mut |left| read_block(&mut r, left), len, w, threads, true)?;
    if !r.done() || digest != hash {
        return Err(Error("checksum mismatch"));
    }
    Ok(len)
}

/// Decodes blocks into `w` until `len` bytes are written; `next(left)` gives the next block
/// (`left`: bytes still to come). Returns the XXH3-64 of the output if `hashed`, else 0.
pub(crate) fn decode_blocks<'a, W: Write>(
    next: &mut impl FnMut(usize) -> Result<(usize, Stored<'a>)>,
    len: usize,
    w: &mut W,
    threads: usize,
    hashed: bool,
) -> Result<u64> {
    if threads > 1 {
        return decompress_par(next, len, w, threads, hashed);
    }
    let mut out = Vec::new();
    let mut done = 0;
    let mut h = hashed.then(Xxh3::new);
    let mut dec = block::Decoder::default();
    let mut dz = zstd::bulk::Decompressor::new().map_err(|_| corrupt())?;
    let mut streams = new_streams();
    while done < len {
        out.clear();
        let (n, b) = next(len - done)?;
        unpack(&b, n, &mut dz, &mut streams, &mut out)?;
        if matches!(b, Stored::Coded(_)) && (!dec.decode(&streams, &mut out) || out.len() != n) {
            return Err(corrupt());
        }
        if let Some(h) = &mut h {
            h.update(&out);
        }
        w.write_all(&out).map_err(|_| Error("write error"))?;
        done += n;
    }
    Ok(h.map_or(0, |h| h.digest()))
}

/// A block whose streams (or whole contents) the other threads have decoded.
type Unpacked = Result<(Box<Streams>, Vec<u8>)>;

/// Work for the decompressing pool: a block to unpack, or a decoded block to print.
enum DJob<'a> {
    Unpack(usize, usize, Stored<'a>),
    Print(usize, usize, block::Deferred),
}

enum DReport {
    Unpacked(usize, Unpacked),
    Printed(usize, Result<Vec<u8>>),
}

fn decompress_par<'a, W: Write>(
    next: &mut impl FnMut(usize) -> Result<(usize, Stored<'a>)>,
    len: usize,
    w: &mut W,
    threads: usize,
    hashed: bool,
) -> Result<u64> {
    let limit = threads + 1;
    std::thread::scope(|sc| {
        let (rtx, rrx) = mpsc::channel::<DReport>();
        let (jtx, jrx) = mpsc::channel::<DJob>();
        let jrx = Arc::new(Mutex::new(jrx));
        for _ in 0..threads.saturating_sub(1).max(1) {
            let (rtx, jrx) = (rtx.clone(), jrx.clone());
            sc.spawn(move || {
                let Ok(mut dz) = zstd::bulk::Decompressor::new() else { return };
                loop {
                    let Ok(j) = jrx.lock().expect("job queue").recv() else { break };
                    let rep = match j {
                        DJob::Unpack(idx, n, b) => {
                            let mut streams = Box::new(new_streams());
                            let mut out = Vec::new();
                            DReport::Unpacked(
                                idx,
                                unpack(&b, n, &mut dz, &mut streams, &mut out).map(|_| (streams, out)),
                            )
                        }
                        DJob::Print(idx, n, d) => {
                            let mut out = Vec::with_capacity(n);
                            d.print(&mut out);
                            DReport::Printed(idx, if out.len() == n { Ok(out) } else { Err(corrupt()) })
                        }
                    };
                    if rtx.send(rep).is_err() {
                        break;
                    }
                }
            });
        }
        drop(rtx);
        let mut dec = block::Decoder::default();
        let mut h = hashed.then(Xxh3::new);
        // Blocks sent to unpack (length, coded, result), from index `unpacked`; blocks
        // decoded, being printed (length, text), from index `written`.
        let mut unpacking: VecDeque<(usize, bool, Option<Unpacked>)> = VecDeque::new();
        let mut printing: VecDeque<(usize, Option<Result<Vec<u8>>>)> = VecDeque::new();
        let (mut read, mut done, mut unpacked, mut written) = (0usize, 0usize, 0usize, 0usize);
        while done < len {
            while unpacking.len() < limit && read < len {
                let (n, b) = next(len - read)?;
                unpacking.push_back((n, matches!(b, Stored::Coded(_)), None));
                jtx.send(DJob::Unpack(unpacked + unpacking.len() - 1, n, b)).expect("workers");
                read += n;
            }
            if let Some((n, text)) = printing.pop_front_if(|p| p.1.is_some()) {
                let text = text.expect("printed")?;
                if let Some(h) = &mut h {
                    h.update(&text);
                }
                w.write_all(&text).map_err(|_| Error("write error"))?;
                (done, written) = (done + n, written + 1);
                continue;
            }
            if printing.len() < limit && unpacking.front().is_some_and(|u| u.2.is_some()) {
                let (n, coded, res) = unpacking.pop_front().expect("front");
                let (streams, whole) = res.expect("unpacked")?;
                let idx = unpacked;
                unpacked += 1;
                if coded {
                    // Values here, in order; the text on the pool.
                    let mut d = block::Deferred::default();
                    if !dec.decode(&streams, &mut d) {
                        return Err(corrupt());
                    }
                    jtx.send(DJob::Print(idx, n, d)).expect("workers");
                    printing.push_back((n, None));
                } else {
                    printing.push_back((n, Some(Ok(whole))));
                }
                continue;
            }
            match rrx.recv().expect("workers") {
                DReport::Unpacked(i, res) => unpacking[i - unpacked].2 = Some(res),
                DReport::Printed(i, res) => printing[i - written].1 = Some(res),
            }
        }
        drop(jtx);
        Ok(h.map_or(0, |h| h.digest()))
    })
}
