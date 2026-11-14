//! The code of the module `zstd-frames` (FORMAT.md, "zstd-frames"), which needs no aqr-formula:
//! a file of bytes kept and of Zstandard frames that libzstd wrote (at a level [`crate::compress`]
//! emulates), each frame made again of its content and put where its pieces are. The frames'
//! pieces may be anywhere in the file (a database's blobs, in pages of overflow chains), so a
//! frame is made where its first piece is, and held until its last one is put.
//!
//! It is a part module: the inputs (the layout, the bytes kept, the contents) taken in order in
//! pieces and the file put in order; what it holds is its compressor, a content and the frames
//! open, these in a pool of its own ([`Pool`]) whose size the params tell.

use alloc::vec::Vec;

use aqr_api::stream::{Pull, Put, Reader};

use crate::compress::{Compressor, frame_max};

/// How a step of the layout puts bytes (the low bits of its tag, the high its length): bytes
/// kept; bytes of a new frame, the next made (a varint of where in it they start); bytes of a
/// frame open (varints of which, 0 the newest, and of where in it they start). Where a piece
/// starts is told against where the frame's last one ended (0 for the first), zigzag.
pub const KEPT: u64 = 0;
pub const NEW: u64 = 1;
pub const PIECE: u64 = 2;
pub const SHIFT: u32 = 2;

/// Bytes that the pool's blocks are rounded up to.
pub const GRAIN: u64 = 64;

/// Pages of the windows of the module as a part module: of the layout, of the bytes kept and the
/// contents, and of the output.
pub const SMALL_WINDOW: u32 = 1;
pub const WINDOW: u32 = 16;

/// Bytes of the pieces in which the layout, and the bytes kept and the contents, are taken.
pub const SMALL_PIECE: usize = 1 << 12;
pub const BIG_PIECE: usize = 1 << 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error;

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("corrupt input")
    }
}

impl From<aqr_api::stream::Corrupt> for Error {
    fn from(_: aqr_api::stream::Corrupt) -> Self {
        Error
    }
}

pub type Result<T> = core::result::Result<T, Error>;

/// The params (FORMAT.md): the level, then varints of the most bytes of a content, the bytes of
/// the pool and the most frames open at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Params {
    pub level: u8,
    pub content: u64,
    pub pool: u64,
    pub open: u64,
}

impl Params {
    pub fn parse(p: &[u8]) -> Result<Params> {
        let (&level, mut rest) = p.split_first().ok_or(Error)?;
        let mut at = 0;
        let mut v = [0u64; 3];
        for x in &mut v {
            *x = aqr_common::varint::get(rest, &mut at).map_err(|_| Error)?;
        }
        rest = &rest[at..];
        let p = Params { level, content: v[0], pool: v[1], open: v[2] };
        let ok = rest.is_empty()
            && crate::compress::params(level as i32, 0).is_some()
            && p.content <= crate::compress::CONTENT_MAX as u64
            && p.pool <= 1 << 40
            && p.open <= 1 << 32;
        if ok { Ok(p) } else { Err(Error) }
    }

    pub fn bytes(&self) -> Vec<u8> {
        let mut v = alloc::vec![self.level];
        for x in [self.content, self.pool, self.open] {
            aqr_common::varint::put(&mut v, x);
        }
        v
    }
}

/// A pool of blocks of the frames open: the first free span that fits, of blocks rounded up to
/// [`GRAIN`] bytes; the encoder tells its size as the most of it ever taken (`top`), of the same
/// steps.
#[derive(Debug, Default)]
pub struct Pool {
    /// Free spans below `top`, in order.
    free: Vec<(u64, u64)>,
    /// The end of the spans taken.
    top: u64,
    /// The most `top` may be.
    limit: u64,
    /// The most `top` was.
    pub most: u64,
}

impl Pool {
    pub fn new(limit: u64) -> Pool {
        Pool { free: Vec::new(), top: 0, limit, most: 0 }
    }

    /// [`Pool::new`], room for `spans` free spans made at once.
    pub fn with_spans(limit: u64, spans: usize) -> Pool {
        let mut p = Pool::new(limit);
        p.free.reserve_exact(spans);
        p
    }

    /// A block of `n` bytes: its place, None where it would pass the limit.
    pub fn take(&mut self, n: u64) -> Option<u64> {
        let n = n.div_ceil(GRAIN) * GRAIN;
        if let Some(i) = self.free.iter().position(|&(_, l)| l >= n) {
            let (at, l) = self.free[i];
            if l == n {
                self.free.remove(i);
            } else {
                self.free[i] = (at + n, l - n);
            }
            return Some(at);
        }
        let at = self.top;
        if at + n > self.limit {
            return None;
        }
        self.top += n;
        self.most = self.most.max(self.top);
        Some(at)
    }

    /// The block of `n` bytes at `at` given back.
    pub fn give(&mut self, at: u64, n: u64) {
        let n = n.div_ceil(GRAIN) * GRAIN;
        let i = self.free.partition_point(|&(a, _)| a < at);
        self.free.insert(i, (at, n));
        // (joined with the spans around it, and with the top)
        if i + 1 < self.free.len() && self.free[i].0 + self.free[i].1 == self.free[i + 1].0 {
            self.free[i].1 += self.free[i + 1].1;
            self.free.remove(i + 1);
        }
        if i > 0 && self.free[i - 1].0 + self.free[i - 1].1 == self.free[i].0 {
            self.free[i - 1].1 += self.free[i].1;
            self.free.remove(i);
        }
        if let Some(&(a, l)) = self.free.last()
            && a + l == self.top
        {
            self.top = a;
            self.free.pop();
        }
    }

    /// Spans free at most: a frame given back joins two at most, so as many as frames open.
    fn spans(open: u64) -> u64 {
        open + 1
    }
}

/// A frame open: its block in the pool, its bytes, those put, and where its last piece ended.
#[derive(Debug, Clone, Copy)]
pub struct Open {
    pub at: u64,
    pub len: u64,
    pub put: u64,
    pub next: u64,
}

/// A zigzag varint's value as a signed one, and back.
pub fn unzigzag(v: u64) -> i64 {
    (v >> 1) as i64 ^ -((v & 1) as i64)
}

pub fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

/// The frames open, the newest last, as the layout tells of them: what both the encoder (of
/// the frames' lengths) and the module go by.
#[derive(Debug, Default)]
pub struct Frames {
    pub open: Vec<Open>,
    pub pool: Pool,
    /// The most open at once.
    pub most: usize,
}

impl Frames {
    pub fn new(p: &Params) -> Frames {
        let mut f = Frames { open: Vec::new(), pool: Pool::new(p.pool), most: 0 };
        f.open.reserve_exact(p.open as usize);
        f.pool.free.reserve_exact(Pool::spans(p.open) as usize);
        f
    }

    /// A new frame of `len` bytes: its block; None where the pool has no room for it.
    pub fn new_frame(&mut self, len: u64, limit: u64) -> Option<u64> {
        if self.open.len() as u64 >= limit {
            return None;
        }
        let at = self.pool.take(len)?;
        self.open.push(Open { at, len, put: 0, next: 0 });
        self.most = self.most.max(self.open.len());
        Some(at)
    }

    /// `n` bytes of the `j`-th newest frame open put, from `delta` bytes after where its last
    /// piece ended: where they are in the pool; the frame closed once as many bytes as it has
    /// are put. None where they are not in it.
    pub fn piece(&mut self, j: u64, delta: i64, n: u64) -> Option<u64> {
        let i = self.open.len().checked_sub(1)?.checked_sub(usize::try_from(j).ok()?)?;
        let o = &mut self.open[i];
        let start = o.next.checked_add_signed(delta)?;
        if n == 0 || n > o.len - o.put || start.checked_add(n)? > o.len {
            return None;
        }
        let at = o.at + start;
        o.put += n;
        o.next = start + n;
        if o.put == o.len {
            let o = self.open.remove(i);
            self.pool.give(o.at, o.len);
        }
        Some(at)
    }
}

/// The memory plan of the module as a part module ([`aqr_api::stack`]), beyond its initial
/// memory (`initial` bytes), of its params: the readers' pieces, the compressor, a content and
/// a frame made, the frames open and the pool.
pub fn plan(initial: u64, params: &[u8]) -> Option<u64> {
    use aqr_api::stack::{Sim, block};
    let p = Params::parse(params).ok()?;
    let mut m = Sim::new(initial);
    m.take(block(64));
    m.take(SMALL_PIECE as u64);
    m.take(BIG_PIECE as u64);
    m.take(SMALL_PIECE as u64);
    m.take(BIG_PIECE as u64);
    m.take(block(32 * p.open));
    m.take(block(16 * Pool::spans(p.open)));
    m.take(block(p.pool));
    m.take(block(p.content));
    m.take(block(frame_max(p.content as usize) as u64));
    m.take(Compressor::memory(p.content as usize, block));
    Some(m.plan())
}

/// Frames made at once, ahead of where they are put, on threads: natively; in WebAssembly one
/// at a time (as the plan has it).
pub const AHEAD: usize = 2048;

/// The frames made ahead of the layout, of the lengths and the contents, in order.
struct Ahead<'a, 'b, 'c> {
    p: &'a Params,
    lengths: &'a mut Reader<'b>,
    contents: &'a mut Reader<'c>,
    threads: usize,
    made: alloc::collections::VecDeque<Vec<u8>>,
    /// Of one thread: a compressor, a content and a frame.
    one: Option<(Compressor, Vec<u8>, Vec<u8>)>,
}

impl Ahead<'_, '_, '_> {
    /// The next frame, to `f`.
    fn next(&mut self, f: &mut dyn FnMut(&[u8]) -> Result<()>) -> Result<()> {
        let level = self.p.level as i32;
        if self.threads <= 1 {
            let (z, content, frame) = self.one.get_or_insert_with(|| {
                let n = self.p.content as usize;
                (Compressor::new(n), Vec::with_capacity(n), Vec::with_capacity(frame_max(n)))
            });
            let n = self.lengths.num()?;
            if n > self.p.content {
                return Err(Error);
            }
            content.clear();
            self.contents
                .each(n, |d| {
                    content.extend_from_slice(d);
                    Some(())
                })
                .ok_or(Error)?;
            frame.clear();
            if !z.compress(content, level, frame) {
                return Err(Error);
            }
            return f(frame);
        }
        if self.made.is_empty() {
            let mut batch = Vec::new();
            while batch.len() < AHEAD && !self.lengths.done() {
                let n = self.lengths.num()?;
                if n > self.p.content {
                    return Err(Error);
                }
                let mut c = Vec::with_capacity(n as usize);
                self.contents
                    .each(n, |d| {
                        c.extend_from_slice(d);
                        Some(())
                    })
                    .ok_or(Error)?;
                batch.push(c);
            }
            const GROUP: usize = 32;
            let content = self.p.content as usize;
            let made = aqr_common::pool::par_map(batch.len().div_ceil(GROUP), self.threads, |g| {
                let mut z = Compressor::new(content);
                let group = &batch[g * GROUP..(g * GROUP + GROUP).min(batch.len())];
                group
                    .iter()
                    .map(|c| {
                        let mut f = Vec::with_capacity(frame_max(c.len()));
                        z.compress(c, level, &mut f).then_some(f).ok_or(Error)
                    })
                    .collect::<Result<Vec<_>>>()
            });
            for g in made {
                self.made.extend(g?);
            }
        }
        f(&self.made.pop_front().ok_or(Error)?)
    }
}

/// The file of `size` bytes made of the layout, the bytes kept, the contents' lengths and the
/// contents, put to `out`: the frames made by libzstd's emulation on up to `threads` threads.
#[allow(clippy::too_many_arguments)]
pub fn build(
    p: &Params,
    layout: &mut Reader,
    kept: &mut Reader,
    lengths: &mut Reader,
    contents: &mut Reader,
    out: &mut dyn Put,
    size: u64,
    threads: usize,
) -> Result<()> {
    let mut frames = Frames::new(p);
    let mut pool = alloc::vec![0u8; p.pool as usize];
    let mut ahead = Ahead { p, lengths, contents, threads, made: Default::default(), one: None };
    let mut done = 0u64;
    while !layout.done() {
        let tag = layout.num()?;
        let n = tag >> SHIFT;
        match tag & ((1 << SHIFT) - 1) {
            KEPT => {
                kept.each(n, |d| out.put(d)).ok_or(Error)?;
                done += n;
            }
            NEW => {
                let delta = unzigzag(layout.num()?);
                ahead.next(&mut |frame| {
                    let at = frames.new_frame(frame.len() as u64, p.open).ok_or(Error)? as usize;
                    pool[at..at + frame.len()].copy_from_slice(frame);
                    Ok(())
                })?;
                let at = frames.piece(0, delta, n).ok_or(Error)? as usize;
                out.put(&pool[at..at + n as usize]).ok_or(Error)?;
                done += n;
            }
            PIECE => {
                let j = layout.num()?;
                let delta = unzigzag(layout.num()?);
                let at = frames.piece(j, delta, n).ok_or(Error)? as usize;
                out.put(&pool[at..at + n as usize]).ok_or(Error)?;
                done += n;
            }
            _ => return Err(Error),
        }
    }
    let all = frames.open.is_empty()
        && kept.done()
        && ahead.made.is_empty()
        && ahead.lengths.done()
        && ahead.contents.done()
        && done == size;
    if all { Ok(()) } else { Err(Error) }
}

/// The threads to make frames on: natively, as many as there are; in WebAssembly one.
fn threads() -> usize {
    #[cfg(target_arch = "wasm32")]
    return 1;
    #[cfg(not(target_arch = "wasm32"))]
    return std::thread::available_parallelism().map_or(1, |n| n.get());
}

/// The module natively, on up to `threads` threads: the file of `len` bytes made of `inputs`
/// (the layout, the bytes kept, the contents' lengths, the contents).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    use aqr_api::stream::Slice;
    let &[layout, kept, lengths, contents] = inputs else { return Err(Error) };
    let p = Params::parse(params)?;
    let (mut l, mut k, mut n, mut c) = (Slice(layout), Slice(kept), Slice(lengths), Slice(contents));
    let mut l = Reader::new(&mut l, SMALL_PIECE);
    let mut k = Reader::new(&mut k, BIG_PIECE);
    let mut n = Reader::new(&mut n, SMALL_PIECE);
    let mut c = Reader::new(&mut c, BIG_PIECE);
    let mut out = Vec::with_capacity(usize::try_from(len).map_err(|_| Error)?);
    build(&p, &mut l, &mut k, &mut n, &mut c, &mut out, len, threads)?;
    Ok(out)
}

/// The module as a part module (its entry point, of `aqr_api::entry!`).
pub fn decode_part(params_len: u64, layout_len: u64, kept_len: u64, lengths_len: u64, contents_len: u64, size: u64) {
    aqr_api::finish(part(params_len, [layout_len, kept_len, lengths_len, contents_len], size));
}

fn part(params_len: u64, lens: [u64; 4], size: u64) -> Result<()> {
    use aqr_api::Memory;
    use aqr_api::stream::{In, Written};
    if params_len > 64 {
        return Err(Error);
    }
    let mut params = alloc::vec![0; params_len as usize];
    aqr_api::Params::read(0, &mut params);
    let p = Params::parse(&params)?;
    let (mut l, mut k) = (In::<0>::new(lens[0]), In::<1>::new(lens[1]));
    let (mut n, mut c) = (In::<2>::new(lens[2]), In::<3>::new(lens[3]));
    let mut l = Reader::new(&mut l as &mut dyn Pull, SMALL_PIECE);
    let mut k = Reader::new(&mut k, BIG_PIECE);
    let mut n = Reader::new(&mut n, SMALL_PIECE);
    let mut c = Reader::new(&mut c, BIG_PIECE);
    let mut out = Written::new(size);
    build(&p, &mut l, &mut k, &mut n, &mut c, &mut out, size, threads())?;
    if out.total() == size { Ok(()) } else { Err(Error) }
}
