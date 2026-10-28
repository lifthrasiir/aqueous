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

    /// The batch's own data (for the frames being added).
    pub fn x(&mut self) -> &mut X {
        &mut self.cur.x
    }

    /// A frame to fill (a used one if there is one).
    pub fn frame(&mut self) -> Frame {
        self.free.pop().unwrap_or_default()
    }

    /// Gives back a frame from [`Front::frame`] that is not sent.
    pub fn unused(&mut self, f: Frame) {
        self.free.push(f);
    }

    /// Adds an item of `samples` samples; sends the batch when it is full.
    pub fn push(&mut self, it: Item<'a>, samples: usize) -> Result<(), Stop<'a>> {
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

/// The pipeline in WebAssembly, of tasks and channels (`aqr_api::task`, `aqr_api::chan`)
/// rather than threads: the first stage, [`errors_batch`] and [`lms_batch`] each a task, and
/// `workers` tasks working on the batches in turn (batch `k` on worker `k mod workers`), whose
/// results the module takes in the same turn, so in order. The batches pass as bytes ([`Wire`]),
/// and are not recycled.
///
/// A stage stops passing batches on after the last one (an error ends the stream), but reads
/// what comes until its input is closed, so that no stage waits for ever for one that stopped;
/// and every stage closes its outputs as it ends (a task closes only the channels it sent to).
#[cfg(target_family = "wasm")]
pub mod apart {
    use super::*;
    use aqr_api::chan::{self, Receiver, Sender};
    use aqr_common::wire::Wire;
    use std::sync::Mutex;

    impl Wire for Error {
        fn put(&self, _: &mut Vec<u8>) {}

        fn get(_: &mut &[u8]) -> Option<Self> {
            Some(Error("(in a task)"))
        }
    }

    impl Wire for crate::audio::frame::Subframe {
        fn put(&self, out: &mut Vec<u8>) {
            [self.typ, self.wasted, self.bits, self.order, self.precision, self.shift as u8, self.method, self.porder]
                .put(out);
            (self.warm, self.coefs).put(out);
            self.params.put(out);
        }

        fn get(b: &mut &[u8]) -> Option<Self> {
            let [typ, wasted, bits, order, precision, shift, method, porder] = <[u8; 8]>::get(b)?;
            let (warm, coefs) = Wire::get(b)?;
            let params = Wire::get(b)?;
            Some(Self { typ, wasted, bits, order, precision, shift: shift as i8, method, porder, warm, coefs, params })
        }
    }

    impl Wire for Frame {
        fn put(&self, out: &mut Vec<u8>) {
            self.header.put(out);
            (self.block_size, self.assign, self.channels, self.bps).put(out);
            self.sub.put(out);
            self.res.put(out);
            self.verbatim.put(out);
        }

        fn get(b: &mut &[u8]) -> Option<Self> {
            let header = Wire::get(b)?;
            let (block_size, assign, channels, bps) = Wire::get(b)?;
            Some(Frame {
                header,
                block_size,
                assign,
                channels,
                bps,
                sub: Wire::get(b)?,
                res: Wire::get(b)?,
                verbatim: Wire::get(b)?,
            })
        }
    }

    /// An item; the bytes of junk as where they are, in the data of the module, which every
    /// task has at the same place (made before the tasks are spawned, and not written since).
    impl Wire for Item<'_> {
        fn put(&self, out: &mut Vec<u8>) {
            match self {
                Item::Frame(f) => {
                    out.push(0);
                    f.put(out);
                }
                Item::Junk(j) => {
                    out.push(1);
                    (j.as_ptr() as usize, j.len()).put(out);
                }
                Item::FrameErr(_) => out.push(2),
                Item::Err(_) => out.push(3),
            }
        }

        fn get(b: &mut &[u8]) -> Option<Self> {
            Some(match u8::get(b)? {
                0 => Item::Frame(Frame::get(b)?),
                1 => {
                    let (p, n) = <(usize, usize)>::get(b)?;
                    // SAFETY: the junk of the data of the module, as `put` took it
                    Item::Junk(unsafe { std::slice::from_raw_parts(p as *const u8, n) })
                }
                2 => Item::FrameErr(Error("(in a task)")),
                3 => Item::Err(Error("(in a task)")),
                _ => return None,
            })
        }
    }

    impl Wire for DecBatch<'_, ()> {
        fn put(&self, out: &mut Vec<u8>) {
            (self.idx, self.last).put(out);
            self.items.put(out);
            self.sc0.put(out);
        }

        fn get(b: &mut &[u8]) -> Option<Self> {
            let (idx, last) = Wire::get(b)?;
            Some(DecBatch { idx, last, items: Wire::get(b)?, sc0: Wire::get(b)?, x: () })
        }
    }

    fn send<T: Wire>(tx: &Sender, v: &T) {
        let mut m = Vec::new();
        v.put(&mut m);
        tx.send(&m);
    }

    fn recv<T: Wire>(rx: &Receiver) -> Option<T> {
        let m = rx.recv()?;
        let mut s = &m[..];
        Some(T::get(&mut s).filter(|_| s.is_empty()).expect("a message of the pipeline"))
    }

    /// The first stage's batches, sent as they are filled.
    pub struct Out(Sender);

    impl<'a> Pass<'a, ()> for Out {
        fn next(&mut self) -> Option<DecBatch<'a, ()>> {
            Some(DecBatch::default())
        }

        fn send(&mut self, b: DecBatch<'a, ()>) -> bool {
            send(&self.0, &b);
            true
        }
    }

    /// Runs the pipeline: `front` fills the batches, then they go through the error and LMS
    /// stages (`rd`), then `work` makes of each a `W` on a worker, which `take` takes in order
    /// (with whether the batch was the last) until it returns `Err` or the last one comes.
    pub fn run<'a, 'r, W: Wire, E>(
        workers: usize,
        front: impl Fn(&mut Front<'a, Out, ()>) -> Result<(), Stop<'a>> + Sync,
        rd: ResDecoder<'r>,
        work: impl Fn(&DecBatch<'a, ()>) -> W + Sync,
        mut take: impl FnMut(W, bool) -> Result<(), E>,
        closed: E,
    ) -> Result<(), E>
    where
        ResDecoder<'r>: Send,
    {
        let workers = workers.max(1);
        let cap = 2;
        let (c1, c2) = (chan::channel(cap), chan::channel(cap));
        let c3: Vec<_> = (0..workers).map(|_| chan::channel(cap)).collect();
        let c4: Vec<_> = (0..workers).map(|_| chan::channel(cap)).collect();
        let rd = Mutex::new(Some(rd));
        let task = |i: usize, _: &mut Vec<u8>| match i {
            0 => {
                if let Some(mut e) = Front::new(Out(c1.0), |_| {}) {
                    let r = front(&mut e);
                    e.finish(r);
                }
                c1.0.close();
            }
            1 => {
                let mut rd = rd.lock().unwrap().take().expect("the residual decoder");
                let mut done = false;
                while let Some(mut b) = recv::<DecBatch<()>>(&c1.1) {
                    if !done {
                        done = errors_batch(&mut rd, &mut b);
                        send(&c2.0, &b);
                    }
                }
                c2.0.close();
            }
            2 => {
                let mut ld = LmsDecoder::default();
                let (mut k, mut done) = (0, false);
                while let Some(mut b) = recv::<DecBatch<()>>(&c2.1) {
                    if !done {
                        lms_batch(&mut ld, &mut b);
                        done = b.last;
                        send(&c3[k % workers].0, &b);
                        k += 1;
                    }
                }
                c3.iter().for_each(|c| c.0.close());
            }
            _ => {
                let j = i - 3;
                while let Some(b) = recv::<DecBatch<()>>(&c3[j].1) {
                    send(&c4[j].0, &(b.last, work(&b)));
                }
                c4[j].0.close();
            }
        };
        let tasks = aqr_api::task::spawn(3 + workers, &task);
        let (mut r, mut k) = (Ok(()), 0);
        let mut open = vec![true; workers];
        loop {
            let Some((last, w)) = recv::<(bool, W)>(&c4[k % workers].1) else {
                open[k % workers] = false;
                r = Err(closed);
                break;
            };
            k += 1;
            r = take(w, last);
            if r.is_err() || last {
                break;
            }
        }
        // (what is left read, in the same turn, for the stages to end)
        while open.contains(&true) {
            if open[k % workers] && c4[k % workers].1.recv().is_none() {
                open[k % workers] = false;
            }
            k += 1;
        }
        drop(tasks);
        r
    }
}
