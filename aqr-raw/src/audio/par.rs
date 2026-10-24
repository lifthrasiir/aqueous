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
//! drops its channels, the threads next to it see that and stop, and `thread::scope`
//! panics once all have finished.

use crate::audio::frame::{Frame, MAX_CHANNELS};
use crate::audio::model::Scale;
use crate::audio::residual::{Error, LmsDecoder, ResDecoder};
use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::Scope;

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

/// Fills batches with items, in order, and sends them down the pipeline (the first stage
/// of decoding). Batches come back through `recycle`.
pub struct Front<'a, 'r, X> {
    cur: DecBatch<'a, X>,
    free: Vec<Frame>,
    samples: usize,
    recycle: &'r Receiver<DecBatch<'a, X>>,
    tx: &'r SyncSender<DecBatch<'a, X>>,
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

impl<'a, 'r, X: Default> Front<'a, 'r, X> {
    /// `None` if the pipeline is closed already.
    pub fn new(
        recycle: &'r Receiver<DecBatch<'a, X>>,
        tx: &'r SyncSender<DecBatch<'a, X>>,
        reset: fn(&mut X),
    ) -> Option<Self> {
        let mut f = Front { cur: recycle.recv().ok()?, free: Vec::new(), samples: 0, recycle, tx, reset };
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
            self.tx.send(std::mem::take(&mut self.cur)).map_err(|_| Stop::Closed)?;
            self.cur = self.recycle.recv().map_err(|_| Stop::Closed)?;
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
        let _ = self.tx.send(self.cur);
    }
}

/// The error decoding stage: [`ResDecoder::errors`] on each frame. At the end of the
/// stream it checks the rANS stream ([`ResDecoder::finish`]).
pub fn errors_stage<'a, X>(mut rd: ResDecoder<'a>, rx: Receiver<DecBatch<'a, X>>, tx: SyncSender<DecBatch<'a, X>>) {
    for mut b in rx {
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
        let last = b.last || failed;
        b.last = last;
        if tx.send(b).is_err() || last {
            break;
        }
    }
}

/// The LMS decoding stage ([`LmsDecoder`]).
pub fn lms_stage<'a, X>(rx: Receiver<DecBatch<'a, X>>, tx: SyncSender<DecBatch<'a, X>>) {
    let mut ld = LmsDecoder::default();
    for mut b in rx {
        let mut k = 0;
        for it in &mut b.items {
            if let Item::Frame(f) = it {
                ld.frame(f, &b.sc0[k]);
                k += 1;
            }
        }
        let last = b.last;
        if tx.send(b).is_err() || last {
            break;
        }
    }
}
