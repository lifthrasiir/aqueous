//! The pieces of the pipelines used with more than one thread (see [`super::pcm`] and
//! aqr-flac's `codec`).
//!
//! Frames travel in batches (about [`BATCH_SAMPLES`] samples, so that a hand-over between
//! threads is rare next to the work on it) through stages on threads of their own, linked
//! by bounded channels, plus pools for the work that is independent per frame. A fixed
//! number of batches circulates: they are recycled to the first stage, which bounds the
//! memory and means that a send never blocks (every channel has room for all batches).
//!
//! Errors go down the pipeline as items, in frame order: a stage stops at the first one,
//! so the last stage meets the error the serial code would have returned. Panics on a
//! pool thread are forwarded to whoever reads its results; elsewhere a thread that dies
//! drops its channels, the threads next to it see that and stop, and the scope panics once
//! all have finished. The threads are workers of aqr-common's `pool`, reserved for a
//! pipeline at once: where there are not enough of them, the serial code runs instead.

use crate::audio::frame::{Frame, MAX_CHANNELS};
use crate::audio::model::Scale;
use crate::audio::residual::{Error, LmsDecoder, ResDecoder};
use aqr_common::pool::Scope;
use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Mutex};

/// Samples (all channels) per batch.
pub const BATCH_SAMPLES: usize = 1 << 16;
/// Items per batch at most.
pub const BATCH_ITEMS: usize = 64;

/// What a pool thread that panicked left.
pub type Panic = Box<dyn std::any::Any + Send>;

/// Batches in flight for `threads` threads: enough to keep the stages and the pool busy.
pub fn in_flight(threads: usize) -> usize {
    2 * threads + 4
}

/// Starts `n` threads that run `work` on the jobs of `jobs` and send the results (or the
/// panic) to `out`. A thread stops when `jobs` is closed, when `out` is, or after a panic.
pub fn pool<'scope, J, R, W>(
    sc: &'scope Scope<'scope, '_>,
    n: usize,
    jobs: Receiver<J>,
    out: SyncSender<Result<R, Panic>>,
    work: W,
) where
    J: Send + 'scope,
    R: Send + 'scope,
    W: FnMut(J) -> R + Clone + Send + 'scope,
{
    let jobs = Arc::new(Mutex::new(jobs));
    for _ in 0..n.max(1) {
        let (jobs, out, mut work) = (jobs.clone(), out.clone(), work.clone());
        sc.spawn(move || {
            loop {
                // The lock is held while waiting: the others wait for it instead.
                let Ok(j) = jobs.lock().map_err(drop).and_then(|r| r.recv().map_err(drop)) else { break };
                let r = catch_unwind(AssertUnwindSafe(|| work(j)));
                let failed = r.is_err();
                if out.send(r).is_err() || failed {
                    break;
                }
            }
        });
    }
}

/// The result of a pool job, or the pool's panic resumed here.
pub fn unwrap<R>(r: Result<R, Panic>) -> R {
    r.unwrap_or_else(|p| std::panic::resume_unwind(p))
}

/// Puts results that come in any order back in order (by index from 0).
pub struct Reorder<T> {
    next: usize,
    buf: VecDeque<Option<T>>,
}

impl<T> Default for Reorder<T> {
    fn default() -> Self {
        Reorder { next: 0, buf: VecDeque::new() }
    }
}

impl<T> Reorder<T> {
    pub fn put(&mut self, idx: usize, v: T) {
        let i = idx - self.next;
        if self.buf.len() <= i {
            self.buf.resize_with(i + 1, || None);
        }
        self.buf[i] = Some(v);
    }

    /// The next result in order, if it is in.
    pub fn pop(&mut self) -> Option<T> {
        let v = self.buf.front_mut()?.take()?;
        self.buf.pop_front();
        self.next += 1;
        Some(v)
    }
}

/// Frames to encode, in order (the first `n` of `frames`; the rest are kept for reuse).
#[derive(Default)]
pub struct EncBatch {
    pub idx: usize,
    pub frames: Vec<Frame>,
    pub n: usize,
}

impl EncBatch {
    /// Frame `n`, to fill (made if need be).
    pub fn next_frame(&mut self) -> &mut Frame {
        if self.frames.len() == self.n {
            self.frames.push(Frame::default());
        }
        &mut self.frames[self.n]
    }
}

/// An item of a stream being decoded.
pub enum Item<'a> {
    Frame(Frame),
    /// Bytes copied as they are (FLAC: junk between frames).
    Junk(&'a [u8]),
    /// An error in decoding a frame (FLAC: after the check of the output length).
    FrameErr(Error),
    /// Any other error.
    Err(Error),
}

/// Items being decoded, with what the stages add; `X`: the last stage's output.
pub struct DecBatch<'a, X> {
    pub idx: usize,
    pub items: Vec<Item<'a>>,
    /// Scale states from before each frame, from [`errors_stage`] for [`lms_stage`].
    pub sc0: Vec<[Scale; MAX_CHANNELS]>,
    /// The last batch (end of stream or an error).
    pub last: bool,
    pub x: X,
}

impl<X: Default> Default for DecBatch<'_, X> {
    fn default() -> Self {
        DecBatch { idx: 0, items: Vec::new(), sc0: Vec::new(), last: false, x: X::default() }
    }
}

/// How the first stage passes its batches on.
pub trait Pass<'a, X> {
    /// A batch to fill, `None` if the pipeline is closed.
    fn next(&mut self) -> Option<DecBatch<'a, X>>;
    /// Sends `b` down the pipeline; whether it is not closed.
    fn send(&mut self, b: DecBatch<'a, X>) -> bool;
}

/// Batches sent down a channel and coming back through `recycle`.
pub struct Chans<'r, 'a, X> {
    pub recycle: &'r Receiver<DecBatch<'a, X>>,
    pub tx: &'r SyncSender<DecBatch<'a, X>>,
}

impl<'a, X> Pass<'a, X> for Chans<'_, 'a, X> {
    fn next(&mut self) -> Option<DecBatch<'a, X>> {
        self.recycle.recv().ok()
    }

    fn send(&mut self, b: DecBatch<'a, X>) -> bool {
        self.tx.send(b).is_ok()
    }
}

/// Fills batches with items, in order, and sends them down the pipeline (the first stage
/// of decoding), through `P`.
pub struct Front<'a, P, X> {
    cur: DecBatch<'a, X>,
    free: Vec<Frame>,
    samples: usize,
    pass: P,
    reset: fn(&mut X),
}

/// Why the first stage stopped early: an error to pass on, or the pipeline closed.
pub enum Stop<'a> {
    Item(Item<'a>),
    Closed,
}

impl<'a> From<Error> for Stop<'a> {
    fn from(e: Error) -> Self {
        Stop::Item(Item::Err(e))
    }
}

impl<'a> From<aqr_common::Corrupt> for Stop<'a> {
    fn from(e: aqr_common::Corrupt) -> Self {
        Error::from(e).into()
    }
}

impl<'a, P: Pass<'a, X>, X: Default> Front<'a, P, X> {
    /// `None` if the pipeline is closed already.
    pub fn new(mut pass: P, reset: fn(&mut X)) -> Option<Self> {
        let mut f = Front { cur: pass.next()?, free: Vec::new(), samples: 0, pass, reset };
        f.take(0);
        Some(f)
    }

    fn take(&mut self, idx: usize) {
        for it in self.cur.items.drain(..) {
            if let Item::Frame(f) = it {
                self.free.push(f);
            }
        }
        self.cur.idx = idx;
        self.cur.sc0.clear();
        self.cur.last = false;
        (self.reset)(&mut self.cur.x);
        self.samples = 0;
    }

    /// Sends the last batch, with the error that stopped the stream if any.
    pub fn finish(mut self, end: Result<(), Stop<'a>>) {
        match end {
            Ok(()) => {}
            Err(Stop::Item(it)) => self.cur.items.push(it),
            Err(Stop::Closed) => return,
        }
        self.cur.last = true;
        let _ = self.pass.send(self.cur);
    }
}

/// Where the first stage of decoding puts its items, in order: batches of them sent down the
/// pipeline ([`Front`]), or each sent as it comes (in WebAssembly, `apart::Out`).
pub trait Sink<'a, X> {
    /// The batch's own data (for the frames being added).
    fn x(&mut self) -> &mut X;
    /// A frame to fill (a used one if there is one).
    fn frame(&mut self) -> Frame;
    /// Gives back a frame from [`Sink::frame`] that is not sent.
    fn unused(&mut self, f: Frame);
    /// Adds an item of `samples` samples.
    fn push(&mut self, it: Item<'a>, samples: usize) -> Result<(), Stop<'a>>;
}

impl<'a, P: Pass<'a, X>, X: Default> Sink<'a, X> for Front<'a, P, X> {
    fn x(&mut self) -> &mut X {
        &mut self.cur.x
    }

    fn frame(&mut self) -> Frame {
        self.free.pop().unwrap_or_default()
    }

    fn unused(&mut self, f: Frame) {
        self.free.push(f);
    }

    /// Sends the batch when it is full.
    fn push(&mut self, it: Item<'a>, samples: usize) -> Result<(), Stop<'a>> {
        self.cur.items.push(it);
        self.samples += samples;
        if self.samples >= BATCH_SAMPLES || self.cur.items.len() >= BATCH_ITEMS {
            let idx = self.cur.idx + 1;
            if !self.pass.send(std::mem::take(&mut self.cur)) {
                return Err(Stop::Closed);
            }
            self.cur = self.pass.next().ok_or(Stop::Closed)?;
            self.take(idx);
        }
        Ok(())
    }
}

/// [`ResDecoder::errors`] on each frame of `b`, and at the end of the stream the check of the
/// rANS stream ([`ResDecoder::finish`]); whether `b` is the last batch (an error stops the
/// stream, the items after it dropped).
pub fn errors_batch<X>(rd: &mut ResDecoder<'_>, b: &mut DecBatch<'_, X>) -> bool {
    b.sc0.clear();
    let mut failed = false;
    for i in 0..b.items.len() {
        match &mut b.items[i] {
            Item::Frame(f) => match rd.errors(f) {
                Ok(sc) => b.sc0.push(sc),
                Err(e) => {
                    b.items[i] = Item::FrameErr(e);
                    b.items.truncate(i + 1);
                    failed = true;
                    break;
                }
            },
            Item::Junk(_) => {}
            Item::FrameErr(_) | Item::Err(_) => {
                failed = true;
                break;
            }
        }
    }
    if b.last
        && !failed
        && let Err(e) = rd.finish()
    {
        b.items.push(Item::Err(e));
    }
    b.last |= failed;
    b.last
}

/// [`LmsDecoder::frame`] on each frame of `b`.
pub fn lms_batch<X>(ld: &mut LmsDecoder, b: &mut DecBatch<'_, X>) {
    let mut k = 0;
    for it in &mut b.items {
        if let Item::Frame(f) = it {
            ld.frame(f, &b.sc0[k]);
            k += 1;
        }
    }
}

/// The error decoding stage: [`errors_batch`] on each batch.
pub fn errors_stage<'a, X>(mut rd: ResDecoder<'a>, rx: Receiver<DecBatch<'a, X>>, tx: SyncSender<DecBatch<'a, X>>) {
    for mut b in rx {
        let last = errors_batch(&mut rd, &mut b);
        if tx.send(b).is_err() || last {
            break;
        }
    }
}

/// The LMS decoding stage: [`lms_batch`] on each batch.
pub fn lms_stage<'a, X>(rx: Receiver<DecBatch<'a, X>>, tx: SyncSender<DecBatch<'a, X>>) {
    let mut ld = LmsDecoder::default();
    for mut b in rx {
        lms_batch(&mut ld, &mut b);
        let last = b.last;
        if tx.send(b).is_err() || last {
            break;
        }
    }
}

/// Frames as the WebAssembly pipeline passes them ([`apart`]): in a buffer reserved for the
/// largest, read into a frame reserved for it ([`msg::frame_with`]), so that a stage takes the
/// same memory whatever it is given.
pub mod msg {
    use crate::audio::frame::{Frame, SUB_CONSTANT, SUB_LPC, Subframe};
    use crate::audio::model::Scale;

    /// Bytes of a FLAC frame's header (no CRC) at most.
    pub const HEADER: usize = 16;

    /// Bytes that [`put_frame`] writes at most of a frame of `channels` channels of `block`
    /// samples (with its residuals, and the scale states after it), and 2 before it: per
    /// channel its fields and lengths, 32 warm-up samples and coefficients, and 8 bytes a sample
    /// (verbatim; else partitions and residuals, of 5 at most).
    pub const fn item_room(channels: usize, block: usize) -> usize {
        2 + 1 + HEADER + 7 + 4 + 8 * 8 + channels * (8 + 8 * 32 + 4 * 32 + 8 + 8 * block)
    }

    /// Bytes of a [`Subframe`] on WebAssembly.
    pub const SUBFRAME: u64 = 408;

    /// A frame of room for `channels` channels of `block` samples and `parts` partitions a
    /// subframe: its header, its subframes (and their partitions), its residuals by channel, the
    /// verbatim samples; [`frame_plan`].
    pub fn frame_with(channels: usize, block: usize, parts: usize) -> Frame {
        let header = Vec::with_capacity(HEADER);
        let mut sub = Vec::with_capacity(channels);
        sub.extend((0..channels).map(|_| Subframe { params: Vec::with_capacity(parts), ..Default::default() }));
        let mut res = Vec::with_capacity(channels);
        res.extend((0..channels).map(|_| Vec::with_capacity(block)));
        let verbatim = Vec::with_capacity(channels * block);
        Frame { header, sub, res, verbatim, ..Default::default() }
    }

    /// Bytes of the blocks of [`frame_with`], as `block` rounds them.
    pub fn frame_plan(channels: usize, block: usize, parts: usize, bl: impl Fn(u64) -> u64) -> u64 {
        let (c, b) = (channels as u64, block as u64);
        bl(HEADER as u64) + bl(SUBFRAME * c) + c * bl(parts as u64) + bl(12 * c) + c * bl(4 * b) + bl(8 * c * b)
    }

    /// Appends frame `f` (its residuals where `res`, else only their lengths).
    pub fn put_frame(f: &Frame, res: bool, out: &mut Vec<u8>) {
        out.push(f.header.len() as u8);
        out.extend_from_slice(&f.header);
        out.extend_from_slice(&(f.block_size as u32).to_le_bytes());
        out.extend_from_slice(&[f.assign, f.channels as u8, f.bps as u8]);
        out.extend_from_slice(&(f.verbatim.len() as u32).to_le_bytes());
        f.verbatim.iter().for_each(|v| out.extend_from_slice(&v.to_le_bytes()));
        for (s, r) in f.sub[..f.channels].iter().zip(&f.res) {
            let order = s.order as usize;
            out.extend_from_slice(&[s.typ, s.wasted, s.bits, s.order, s.precision, s.shift as u8, s.method, s.porder]);
            let warm = if s.typ == SUB_CONSTANT { 1 } else { order };
            s.warm[..warm].iter().for_each(|v| out.extend_from_slice(&v.to_le_bytes()));
            if s.typ >= SUB_LPC {
                s.coefs[..order].iter().for_each(|v| out.extend_from_slice(&v.to_le_bytes()));
            }
            out.extend_from_slice(&(s.params.len() as u32).to_le_bytes());
            out.extend_from_slice(&s.params);
            out.extend_from_slice(&(r.len() as u32).to_le_bytes());
            if res {
                r.iter().for_each(|v| out.extend_from_slice(&v.to_le_bytes()));
            }
        }
    }

    fn take<'b>(b: &mut &'b [u8], n: usize) -> Option<&'b [u8]> {
        let (x, rest) = b.split_at_checked(n)?;
        *b = rest;
        Some(x)
    }

    fn u32_of(b: &mut &[u8]) -> Option<usize> {
        Some(u32::from_le_bytes(take(b, 4)?.try_into().unwrap()) as usize)
    }

    /// Reads into `f` (within its room, which it only grows) what [`put_frame`] wrote at the
    /// start of `b`, which moves past it; its residuals where `res`, else zeros.
    pub fn get_frame(b: &mut &[u8], f: &mut Frame, res: bool) -> Option<()> {
        let n = take(b, 1)?[0] as usize;
        f.header.clear();
        f.header.extend_from_slice(take(b, n)?);
        f.block_size = u32_of(b)?;
        let &[assign, channels, bps] = take(b, 3)? else { unreachable!() };
        (f.assign, f.channels, f.bps) = (assign, channels as usize, bps as u32);
        let n = u32_of(b)?;
        f.verbatim.clear();
        f.verbatim.extend(take(b, 8 * n)?.as_chunks::<8>().0.iter().map(|&v| i64::from_le_bytes(v)));
        if f.sub.len() < f.channels {
            f.sub.resize(f.channels, Subframe::default());
        }
        if f.res.len() < f.channels {
            f.res.resize(f.channels, Vec::new());
        }
        for (s, r) in f.sub[..f.channels].iter_mut().zip(&mut f.res) {
            let &[typ, wasted, bits, order, precision, shift, method, porder] = take(b, 8)? else { unreachable!() };
            (s.typ, s.wasted, s.bits, s.order, s.precision, s.shift) =
                (typ, wasted, bits, order, precision, shift as i8);
            (s.method, s.porder) = (method, porder);
            let warm = if typ == SUB_CONSTANT { 1 } else { order as usize };
            for (w, v) in s.warm.iter_mut().zip(take(b, 8 * warm)?.as_chunks::<8>().0) {
                *w = i64::from_le_bytes(*v);
            }
            if typ >= SUB_LPC {
                for (c, v) in s.coefs.iter_mut().zip(take(b, 4 * order as usize)?.as_chunks::<4>().0) {
                    *c = i32::from_le_bytes(*v);
                }
            }
            let n = u32_of(b)?;
            s.params.clear();
            s.params.extend_from_slice(take(b, n)?);
            let n = u32_of(b)?;
            r.clear();
            match res {
                true => r.extend(take(b, 4 * n)?.as_chunks::<4>().0.iter().map(|&v| i32::from_le_bytes(v))),
                false => r.resize(n, 0),
            }
        }
        Some(())
    }

    /// Appends the scale states `sc`.
    pub fn put_scales(sc: &[Scale; 8], out: &mut Vec<u8>) {
        use aqr_common::wire::Wire;
        sc.put(out);
    }

    /// The scale states that [`put_scales`] wrote at the start of `b`.
    pub fn get_scales(b: &mut &[u8]) -> Option<[Scale; 8]> {
        aqr_common::wire::Wire::get(b)
    }
}

/// The pipeline of a part module, of tasks and channels (`aqr_api::task`, `aqr_api::chan`): the
/// module's instance, which alone reaches the windows of its inputs and output, makes the first
/// stage's items and decodes their errors ([`ResStream`]), and sends each as a message to a task
/// running [`LmsDecoder::frame`], which deals them to `workers` tasks in turn (item `k` to worker
/// `k mod workers`); the instance takes what the workers make in the same turn, so in order, and
/// writes it. So that no channel is full where the instance waits for one, it keeps at most
/// [`in_flight`] items in the pipeline (each makes one message in a channel to a worker and one
/// from it, and each has room for [`CAP`]).
///
/// Every item is a message of its own: a byte telling whether it is the last (an error, or the
/// end), its kind and what it holds (a frame as [`msg::put_frame`] writes it; for junk, only its
/// length, which the instance copies from its stream where the item comes back). Each task takes
/// its messages into a buffer of room for the largest and reads each frame into a frame of its own
/// of room for the largest ([`Rooms`]), so that it takes the same memory whatever it is given:
/// what [`Plan`] counts.
///
/// The tasks read what comes until their input is closed, so that none waits for ever for one that
/// stopped, and close their outputs as they end; the instance, once it has the last item, closes
/// its output and reads what the workers still send.
pub mod apart {
    use super::*;
    use crate::audio::frame::Frame;
    use crate::audio::residual::ResStream;
    use aqr_api::chan::{self, Receiver, Sender};

    /// Kinds of items.
    pub const FRAME: u8 = 0;
    pub const JUNK: u8 = 1;
    pub const FRAME_ERR: u8 = 2;
    pub const ERR: u8 = 3;
    pub const END: u8 = 4;

    /// Messages a channel holds.
    const CAP: usize = 2;

    /// Items in the pipeline at most, with `workers` workers: a worker has at most [`CAP`] of
    /// them in its channels, and one more would block it, and so the stages before it.
    pub fn in_flight(workers: usize) -> usize {
        (workers * CAP).saturating_sub(1).max(1)
    }

    /// What the frames of a stream take at most: channels, samples a channel, partitions a
    /// subframe; and the bytes of a worker's message at most (the byte of the last, its kind's
    /// and what it says of an item).
    #[derive(Clone, Copy, Debug)]
    pub struct Rooms {
        pub channels: usize,
        pub block: usize,
        pub parts: usize,
        pub out: usize,
    }

    impl Rooms {
        /// Bytes of a message of the stages.
        pub fn item(&self) -> usize {
            super::msg::item_room(self.channels, self.block)
        }
    }

    /// The memory of the pipeline over what the module took before it (`top` bytes of the
    /// stack, the working memory up to the end of its page then), as [`run`] takes it, of
    /// `workers` workers: the most the instance takes, and the most the gang of its tasks (which
    /// are started together) take, with the messages its channels hold at most.
    ///
    /// - `work`: what each worker takes (its scratch) over its message buffers and frame (in that
    ///   order: the worker's scratch after its frame).
    /// - `start`: what the instance takes once the tasks are spawned: its decoders and streams,
    ///   and the pipe ([`Pipe::plan`]).
    pub struct Plan {
        pub top: u64,
        pub workers: usize,
        pub rooms: Rooms,
        pub work: u64,
        pub start: u64,
    }

    impl Plan {
        /// The limits of the instance and of the gang (its tasks, whose memories add up, and the
        /// messages its channels hold, which come with it), and what the messages may hold.
        pub fn bytes(&self) -> ((u64, u64), u64) {
            use aqr_api::stack::block;
            let pages = |n: u64| n.div_ceil(1 << 16) << 16;
            let w = self.workers.clamp(1, 64) as u64;
            let r = &self.rooms;
            let (item, out) = (block(r.item() as u64), block(r.out as u64));
            let frame = super::msg::frame_plan(r.channels, r.block, r.parts, block);
            // (before the spawn: the ends of the channels to the workers and from them)
            let top = self.top + 2 * block(8 * w);
            let task = |own: u64| pages(top).max(pages(top + own));
            let lms = block(8 * size_of::<crate::audio::model::Lms>() as u64);
            let queued = CAP as u64 * ((1 + w) * r.item() as u64 + w * r.out as u64);
            let gang = task(lms + 2 * item + frame) + w * task(item + frame + self.work + out) + queued;
            // (the instance: the tasks' handles, then what it starts)
            let instance = pages(top + block(1 + w) + self.start);
            ((instance, gang), queued)
        }
    }

    /// Why the first stage stopped early.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Fail {
        /// An error in decoding a frame (FLAC: after the check of the output length).
        Frame,
        /// Any other error.
        Err,
        /// The pipeline is stopped already (an error to be returned is kept).
        Halt,
    }

    impl From<Error> for Fail {
        fn from(_: Error) -> Self {
            Fail::Err
        }
    }

    impl From<aqr_common::Corrupt> for Fail {
        fn from(_: aqr_common::Corrupt) -> Self {
            Fail::Err
        }
    }

    /// The ends of the channels that the instance takes: to the LMS task, and from each worker.
    /// The channel to the LMS task is closed as the link is dropped, so that the tasks end where
    /// the instance leaves without a pipe (an error before it was made).
    pub struct Link<'a> {
        tx: Sender,
        rx: &'a [(Sender, Receiver)],
        rooms: Rooms,
        closed: bool,
    }

    impl Link<'_> {
        fn close(&mut self) {
            if !std::mem::replace(&mut self.closed, true) {
                self.tx.close();
            }
        }
    }

    impl Drop for Link<'_> {
        fn drop(&mut self) {
            self.close();
        }
    }

    /// Runs the pipeline: spawns the LMS task and `workers` workers, each made by `work` (which
    /// writes what it makes of each item after the byte of the last), then runs `body` on the
    /// instance with the ends of the channels, once the tasks are spawned: what it makes before
    /// the tasks do not copy. Gives what `body` gives, after the tasks have ended.
    pub fn run<W: FnMut(&Item<'_>, &mut Vec<u8>), R>(
        workers: usize,
        rooms: Rooms,
        work: impl Fn() -> W + Sync,
        body: impl FnOnce(Link<'_>) -> R,
    ) -> R {
        let workers = workers.clamp(1, 64);
        let (item, frame) = (rooms.item(), || msg::frame_with(rooms.channels, rooms.block, rooms.parts));
        let c2 = chan::channel(CAP);
        let c3: Vec<_> = (0..workers).map(|_| chan::channel(CAP)).collect();
        let c4: Vec<_> = (0..workers).map(|_| chan::channel(CAP)).collect();
        let task = |i: usize, _: &mut Vec<u8>| match i {
            0 => {
                let mut ld = LmsDecoder::default();
                let (mut rx, mut tx, mut f) = (Vec::with_capacity(item), Vec::with_capacity(item), frame());
                let (mut k, mut done) = (0, false);
                while c2.1.recv_into(&mut rx) {
                    if done {
                        continue;
                    }
                    done = rx[0] != 0;
                    let to = &c3[k % workers].0;
                    k += 1;
                    if rx[1] != FRAME {
                        to.send(&rx);
                        continue;
                    }
                    let mut b = &rx[2..];
                    msg::get_frame(&mut b, &mut f, true).expect("a message of the pipeline");
                    let sc = msg::get_scales(&mut b).expect("a message of the pipeline");
                    ld.frame(&mut f, &sc);
                    tx.clear();
                    tx.extend_from_slice(&[0, FRAME]);
                    msg::put_frame(&f, true, &mut tx);
                    to.send(&tx);
                }
                c3.iter().for_each(|c| c.0.close());
            }
            _ => {
                let j = i - 1;
                let (mut rx, mut f) = (Vec::with_capacity(item), frame());
                let mut w = work();
                let mut tx = Vec::with_capacity(rooms.out);
                while c3[j].1.recv_into(&mut rx) {
                    tx.clear();
                    tx.push(rx[0]);
                    match rx[1] {
                        FRAME => {
                            let mut b = &rx[2..];
                            msg::get_frame(&mut b, &mut f, true).expect("a message of the pipeline");
                            let it = Item::Frame(std::mem::take(&mut f));
                            w(&it, &mut tx);
                            let Item::Frame(g) = it else { unreachable!() };
                            f = g;
                        }
                        JUNK => {
                            tx.push(JUNK);
                            tx.extend_from_slice(&rx[2..6]);
                        }
                        FRAME_ERR => w(&Item::FrameErr(Error("(in a task)")), &mut tx),
                        ERR => w(&Item::Err(Error("(in a task)")), &mut tx),
                        _ => tx.push(END),
                    }
                    c4[j].0.send(&tx);
                }
                c4[j].0.close();
            }
        };
        // SAFETY: dropped below, before `task` goes
        let tasks = unsafe { aqr_api::task::spawn_gang(0, 1 + workers, &task) };
        let r = body(Link { tx: c2.0, rx: &c4, rooms, closed: false });
        drop(tasks);
        r
    }

    /// The instance's end of the pipeline: items made are sent down it (the frames' errors decoded
    /// first), and what the workers make of them is taken in order by `take`, which is given the
    /// message after its byte of the last (an `Err` ends the stream, as the last does).
    pub struct Pipe<'a, 't, E> {
        link: Link<'a>,
        errors: ResStream<'a>,
        /// The frame being made.
        f: Frame,
        msg: Vec<u8>,
        rx: Vec<u8>,
        take: &'t mut dyn FnMut(&[u8]) -> Result<(), E>,
        closed: Option<E>,
        sent: usize,
        got: usize,
        /// The workers whose channels are not closed.
        open: u64,
        /// How the stream ended, once it did.
        done: Option<Result<(), E>>,
    }

    impl<'a, 't, E> Pipe<'a, 't, E> {
        /// Bytes of the blocks that [`Pipe::new`] takes (as `block` rounds them).
        pub fn plan(rooms: &Rooms, block: impl Fn(u64) -> u64) -> u64 {
            let frame = super::msg::frame_plan(rooms.channels, rooms.block, rooms.parts, &block);
            frame + block(rooms.item() as u64) + block(rooms.out as u64)
        }

        /// The pipe on `link`, taking the messages that come back with `take`; `closed` is the
        /// error where a worker's channel is closed before its end.
        pub fn new(
            link: Link<'a>,
            errors: ResStream<'a>,
            take: &'t mut dyn FnMut(&[u8]) -> Result<(), E>,
            closed: E,
        ) -> Self {
            let r = link.rooms;
            let f = msg::frame_with(r.channels, r.block, r.parts);
            let (msg, rx) = (Vec::with_capacity(r.item()), Vec::with_capacity(r.out));
            let open = u64::MAX >> (64 - link.rx.len());
            Pipe { link, errors, f, msg, rx, take, closed: Some(closed), sent: 0, got: 0, open, done: None }
        }

        /// The frame to fill for [`Pipe::push_frame`].
        pub fn frame(&mut self) -> &mut Frame {
            &mut self.f
        }

        /// Takes the next message that comes back, for the item that is the oldest in the pipeline.
        fn recv_one(&mut self) {
            let j = self.got % self.link.rx.len();
            if !self.link.rx[j].1.recv_into(&mut self.rx) {
                self.open &= !(1 << j);
                self.done = Some(Err(self.closed.take().expect("a channel closed once")));
                return;
            }
            self.got += 1;
            if self.rx[1] == END {
                self.done = Some(Ok(()));
                return;
            }
            let r = (self.take)(&self.rx[1..]);
            if r.is_err() || self.rx[0] != 0 {
                self.done = Some(r);
            }
        }

        /// Sends `m` as the next item, once the pipeline has room for it.
        fn send(&mut self, m: &[u8]) -> Result<(), Fail> {
            let limit = in_flight(self.link.rx.len());
            while self.sent - self.got >= limit && self.done.is_none() {
                self.recv_one();
            }
            if self.done.is_some() {
                return Err(Fail::Halt);
            }
            self.link.tx.send(m);
            self.sent += 1;
            Ok(())
        }

        /// The error stage on the frame made ([`Pipe::frame`]), which goes down the pipeline.
        pub fn push_frame(&mut self) -> Result<(), Fail> {
            match self.errors.errors(&mut self.f) {
                Ok(sc) => {
                    let mut m = std::mem::take(&mut self.msg);
                    m.clear();
                    m.extend_from_slice(&[0, FRAME]);
                    msg::put_frame(&self.f, true, &mut m);
                    msg::put_scales(&sc, &mut m);
                    let r = self.send(&m);
                    self.msg = m;
                    r
                }
                Err(_) => {
                    self.send(&[1, FRAME_ERR])?;
                    Err(Fail::Halt)
                }
            }
        }

        /// Junk of `n` bytes, which the instance copies from its stream where it comes back.
        pub fn push_junk(&mut self, n: usize) -> Result<(), Fail> {
            let n = u32::try_from(n).map_err(|_| Fail::Err)?;
            let mut m = [0; 6];
            m[1] = JUNK;
            m[2..].copy_from_slice(&n.to_le_bytes());
            self.send(&m)
        }

        /// Ends the stream: the first stage ended with `end` (the stream of the rANS words is
        /// checked at its end), and the instance has taken everything back; `Ok` where the end of
        /// the stream came back, else the error that `take` gave or the channels' being closed.
        pub fn finish(mut self, end: Result<(), Fail>) -> Result<(), E> {
            match end {
                Ok(()) => {
                    let last = if self.errors.finish().is_err() { ERR } else { END };
                    let _ = self.send(&[1, last]);
                }
                Err(Fail::Frame) => drop(self.send(&[1, FRAME_ERR])),
                Err(Fail::Err) => drop(self.send(&[1, ERR])),
                Err(Fail::Halt) => {}
            }
            self.link.close();
            while self.done.is_none() {
                self.recv_one();
            }
            // (what is left read, in the same turn, for the stages to end)
            let w = self.link.rx.len();
            while self.open != 0 {
                let j = self.got % w;
                if self.open & 1 << j != 0 && !self.link.rx[j].1.recv_into(&mut self.rx) {
                    self.open &= !(1 << j);
                }
                self.got += 1;
            }
            self.done.take().expect("ended")
        }
    }
}
