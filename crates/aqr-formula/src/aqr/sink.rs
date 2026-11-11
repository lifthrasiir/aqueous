//! The sink: streams of an archive written as they are made, each non-leaf block compressed as
//! soon as it is full and held so (of the store where one is set, [`crate::store`]) rather than
//! the stream's bytes (`docs/streaming-encoder-design.md`). A block is compressed as the encoder
//! compresses it at the end (`Encoder::write`): by its bytes and how its stream is compressed
//! alone, so that an archive of streams written to a sink is the same as of streams held whole.
//! The encoder writes the blocks as they are ([`Encoder::encode_sunk_to`]).
//!
//! A formula is sealed into a sink ([`Sink::seal`]): the bytes of its slices written to the
//! sink's stream of their class and kind (one of its own for a stream that [`Formula::merged`]
//! keeps apart), and the slices then of the sink's streams ([`Bytes::sunk`]). A slice of the same
//! bytes as one sealed before, or starting with such slices, is of those instead: the formulas
//! sealed share them, as [`Formula::share_slices`] has the slices of one formula share them.
//!
//! A sink made [`Sink::direct`] writes its blocks to the archive's file as they are filled, at
//! offsets from where it is told to start, in the order they were filled (whatever the threads),
//! rather than holding them: the archive's formula block then goes into the room left before
//! them (`docs/streaming-encoder-design.md`, "출력에 바로 쓰기"). Its streams' blocks are then
//! told of by their ordinals among those written ([`Sunk::ordinals`]).

use std::collections::VecDeque;
use std::sync::MutexGuard;

use super::*;
use crate::Bytes;

/// Where a direct sink writes its blocks: a file, at offsets.
pub trait Place: Send + Sync {
    /// Writes all of `d` at offset `at`.
    fn write_at(&self, at: u64, d: &[u8]) -> io::Result<()>;
}

/// Streams written block by block ([`Sink::append`]), their blocks compressed as they fill.
pub struct Sink {
    level: Level,
    threads: usize,
    /// Slices sealed of at least this many bytes are shared ([`Sink::seal`]).
    min: usize,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// Where the blocks are written as they are filled, if anywhere.
    direct: Option<Direct>,
    streams: Vec<SinkStream>,
    /// The streams of each class and kind that the formulas sealed share.
    by_class: HashMap<(Cow<'static, str>, Kind), usize>,
    /// The slices sealed of at least [`Sink::min`] bytes, by the hash of their first bytes.
    kept: HashMap<u64, Vec<Kept>>,
}

/// A slice sealed: where it is (a stream of the sink, its start and length), and the hash of
/// its bytes.
#[derive(Clone, Copy)]
struct Kept {
    stream: usize,
    at: u64,
    len: u64,
    hash: u128,
}

/// A slice: its stream, start and length.
type Piece = (usize, u64, u64);

/// The writing of a direct sink's blocks ([`Sink::direct`]).
struct Direct {
    place: Arc<dyn Place>,
    /// Where the next block goes.
    at: u64,
    /// The blocks filled but not written yet, in the order they were filled: their streams and
    /// indices in them.
    queue: VecDeque<(usize, usize)>,
    /// Blocks filled so far: the ordinal of the next one.
    filled: u64,
    /// Blocks queued at most while the first is being compressed.
    pending: usize,
    /// The first failure to write, after which nothing more is written.
    failed: Option<io::Error>,
}

/// A stream of a sink: its class and kind, bytes so far, the bytes of its block not full yet,
/// and its blocks encoded.
struct SinkStream {
    class: Cow<'static, str>,
    kind: Kind,
    how: How,
    len: u64,
    tail: Vec<u8>,
    blocks: Vec<Block>,
    /// Of a direct sink, the ordinal of each block filled.
    ordinals: Vec<u64>,
}

/// A block of a sink's stream: encoded, or being compressed on a worker of the pool (or, a
/// moment, on the thread that filled it), or written out (a direct sink's).
enum Block {
    Done(Bytes),
    Running(aqr_common::pool::Handle<'static, Result<Bytes>>),
    Here,
    Written,
}

/// The blocks of a formula's streams encoded before it is sealed ([`Sink::ready`]), by stream.
pub struct Ready(Vec<Option<Vec<Bytes>>>);

/// A stream of a sink as [`Sink::close`] gives it: its class, kind and length, and its non-leaf
/// blocks, encoded (each its block type, length and data); of a direct sink, none, but their
/// ordinals among the blocks it wrote.
pub struct Sunk {
    pub class: Cow<'static, str>,
    pub kind: Kind,
    pub len: u64,
    pub blocks: Vec<Bytes>,
    pub ordinals: Vec<u64>,
}

impl Sink {
    /// A sink of streams compressed as `enc` compresses them, sharing the slices sealed of at
    /// least `min` bytes.
    pub fn new(enc: &Encoder, min: usize) -> Sink {
        Sink { level: enc.level, threads: enc.threads, min, state: Mutex::default() }
    }

    /// [`Sink::new`] writing its blocks to `place` as they are filled, from offset `at` on, one
    /// after another in the order they were filled; up to `pending` waiting there for the first
    /// of them to be compressed.
    pub fn direct(enc: &Encoder, min: usize, place: Arc<dyn Place>, at: u64, pending: usize) -> Sink {
        let direct = Direct { place, at, queue: VecDeque::new(), filled: 0, pending: pending.max(1), failed: None };
        let state = State { direct: Some(direct), ..State::default() };
        Sink { level: enc.level, threads: enc.threads, min, state: Mutex::new(state) }
    }

    /// Whether the blocks are written as they are filled ([`Sink::direct`]).
    pub fn is_direct(&self) -> bool {
        self.state.lock().unwrap().direct.is_some()
    }

    /// A new stream of `class` and `kind`, empty, by its index.
    pub fn stream(&self, class: impl Into<Cow<'static, str>>, kind: Kind) -> usize {
        new_stream(&mut self.state.lock().unwrap(), self.level, class.into(), kind)
    }

    /// Appends `data` (in pieces) to stream `s`, of where it starts in it; each block filled is
    /// compressed then, on a worker of the pool where one is free, else on the calling thread.
    pub fn append(&self, s: usize, data: &[&[u8]]) -> Result<u64> {
        let mut st = self.state.lock().unwrap();
        let at = st.streams[s].len;
        for mut d in data.iter().copied() {
            while !d.is_empty() {
                let stream = &mut st.streams[s];
                let n = d.len().min(BLOCK - stream.tail.len());
                stream.tail.extend_from_slice(&d[..n]);
                stream.len += n as u64;
                d = &d[n..];
                if stream.tail.len() == BLOCK {
                    st = self.fill(st, s)?;
                }
            }
        }
        Ok(at)
    }

    /// The tail of stream `s` made a block of its own, compressed on a worker of the pool where
    /// one is free, else on the calling thread (the lock let go of meanwhile); of a direct sink,
    /// the blocks then written that can be.
    fn fill<'a>(&'a self, mut st: MutexGuard<'a, State>, s: usize) -> Result<MutexGuard<'a, State>> {
        let state = &mut *st;
        let stream = &mut state.streams[s];
        let (block, how) = (std::mem::take(&mut stream.tail), stream.how);
        let k = stream.blocks.len();
        if let Some(d) = &mut state.direct {
            stream.ordinals.push(d.filled);
            d.filled += 1;
            d.queue.push_back((s, k));
        }
        let (level, threads, spill) = (self.level, self.threads, state.direct.is_none());
        let mut w = aqr_common::pool::reserve(1);
        if !w.is_empty() {
            let h = w.spawn(move || encoded(level, threads, &block, how, spill));
            stream.blocks.push(Block::Running(h));
        } else {
            // (with the lock let go: the block's place kept)
            stream.blocks.push(Block::Here);
            drop(st);
            let b = encoded(level, threads, &block, how, spill)?;
            drop(block);
            st = self.state.lock().unwrap();
            st.streams[s].blocks[k] = Block::Done(b);
        }
        write_out(&mut st, false);
        Ok(st)
    }

    /// Seals formula `f`: the bytes of its slices (of streams not sunk) written to the sink,
    /// those of each class and kind to its stream (one of its own for a stream of
    /// [`Kind::Compressed`]), a slice of at least `Sink::min` bytes not written where it is
    /// the same as slices sealed before, one after another, or ends after them; the slices then
    /// of the sink's streams (of the formula's streams sunk, one of each), and the streams they
    /// were of dropped. Slices past the end of their stream, and streams kept apart, are left as
    /// they are. Formulas are to be sealed one at a time, in an order that does not depend on
    /// the threads (the archive's bytes do).
    pub fn seal(&self, f: &mut Formula) -> Result<()> {
        self.seal_with(f, None)
    }

    /// The blocks of formula `f`'s streams that [`Sink::seal`] would give streams of the sink of
    /// their own, encoded as it would (on the calling thread, which may be any): those that a
    /// single slice of `f` takes whole. Its sealing then takes them as they are
    /// ([`Sink::seal_ready`]), not looking for them among the slices sealed before.
    pub fn ready(&self, f: &Formula) -> Result<Ready> {
        let _s = aqr_common::trace::span("seal");
        let direct = self.is_direct();
        let mut slices: Vec<Vec<(u64, u64)>> = vec![Vec::new(); f.streams.len()];
        f.visit(&mut |n| {
            if let &Node::Slice { stream, start, len } = n
                && let Some(s) = slices.get_mut(stream)
            {
                s.push((start, len));
            }
        });
        let mut ready = Vec::with_capacity(f.streams.len());
        for (st, sl) in f.streams.iter().zip(&slices) {
            let len = st.data.len() as u64;
            let own = st.kind == Kind::Compressed || st.apart && direct;
            if !own || st.data.sunk_in().is_some() || st.data.is_lazy() || len == 0 || sl[..] != [(0, len)] {
                ready.push(None);
                continue;
            }
            let how = how_of(self.level, &st.class, st.kind);
            let blocks: Result<Vec<Bytes>> =
                block_ranges(len as usize).map(|r| encoded(self.level, 1, &st.data[r], how, !direct)).collect();
            ready.push(Some(blocks?));
        }
        Ok(Ready(ready))
    }

    /// [`Sink::seal`] of formula `f`, its streams of `ready` ([`Sink::ready`] of it) appended to
    /// streams of the sink of their own as they were encoded.
    pub fn seal_ready(&self, f: &mut Formula, ready: Ready) -> Result<()> {
        self.seal_with(f, Some(ready))
    }

    /// Appends the blocks of `len` bytes encoded already, `blocks` ([`Sink::ready`]), to stream
    /// `s`, which has no bytes not in a block; of where they start in it.
    fn append_encoded(&self, s: usize, len: u64, blocks: Vec<Bytes>) -> u64 {
        let mut st = self.state.lock().unwrap();
        let state = &mut *st;
        let stream = &mut state.streams[s];
        debug_assert!(stream.tail.is_empty() && stream.len.is_multiple_of(BLOCK as u64));
        let at = stream.len;
        stream.len += len;
        for b in blocks {
            if let Some(d) = &mut state.direct {
                stream.ordinals.push(d.filled);
                d.filled += 1;
                d.queue.push_back((s, stream.blocks.len()));
            }
            stream.blocks.push(Block::Done(b));
        }
        write_out(&mut st, false);
        at
    }

    fn seal_with(&self, f: &mut Formula, mut ready: Option<Ready>) -> Result<()> {
        let _s = aqr_common::trace::span("seal");
        // the slices in order (of the applications of several outputs first, then the root)
        let mut slices: Vec<(usize, u64, u64)> = Vec::new();
        f.visit(&mut |n| {
            if let &Node::Slice { stream, start, len } = n {
                slices.push((stream, start, len));
            }
        });
        let open = |f: &Formula, s: usize| f.streams.get(s).is_some_and(|s| s.data.sunk_in().is_none());
        let within = |f: &Formula, (s, a, l): (usize, u64, u64)| {
            a.checked_add(l).is_some_and(|e| e <= f.streams[s].data.len() as u64)
        };
        // streams of a slice past their end, and those kept apart ([`crate::Stream::apart`]), left
        // as they are
        // (but in a direct sink, whose streams kept apart have streams of their own: nothing is
        // to be written after it)
        let direct = self.is_direct();
        let mut past = vec![false; f.streams.len()];
        for &x in &slices {
            if open(f, x.0) && (!within(f, x) || f.streams[x.0].apart && !direct) {
                past[x.0] = true;
            }
        }
        // each slice as pieces of the sink's streams, by the slice (each once)
        let mut sealed: HashMap<Piece, Vec<Piece>> = HashMap::new();
        // the streams of the sink of a stream of its own, by the formula's stream
        let mut alone: HashMap<usize, usize> = HashMap::new();
        for x in slices {
            let (s, a, l) = x;
            if !open(f, s) || past[s] || l == 0 || sealed.contains_key(&x) {
                continue;
            }
            let st = &f.streams[s];
            if let Some(blocks) = ready.as_mut().and_then(|r| r.0.get_mut(s)?.take()) {
                let t = new_stream(&mut self.state.lock().unwrap(), self.level, st.class.clone(), st.kind);
                let at = self.append_encoded(t, l, blocks);
                sealed.insert(x, vec![(t, at, l)]);
                continue;
            }
            let bytes = &st.data[a as usize..][..l as usize];
            let mut st_lock = self.state.lock().unwrap();
            let state = &mut *st_lock;
            let (mut at, mut pieces) = (0usize, Vec::new());
            // (the slices sealed that the rest starts with, the longest each time)
            while bytes.len() - at >= self.min {
                let rest = &bytes[at..];
                let found = state.kept.get(&head(&rest[..self.min])).and_then(|ks| {
                    ks.iter()
                        .filter(|k| k.len as usize <= rest.len())
                        .filter(|k| xxhash_rust::xxh3::xxh3_128(&rest[..k.len as usize]) == k.hash)
                        .max_by_key(|k| k.len)
                        .copied()
                });
                let Some(k) = found else { break };
                pieces.push((k.stream, k.at, k.len));
                at += k.len as usize;
            }
            if at < bytes.len() {
                let rest = &bytes[at..];
                let target = match st.kind {
                    _ if st.apart => {
                        *alone.entry(s).or_insert_with(|| new_stream(state, self.level, st.class.clone(), st.kind))
                    }
                    Kind::Compressed => {
                        *alone.entry(s).or_insert_with(|| new_stream(state, self.level, st.class.clone(), st.kind))
                    }
                    _ => match state.by_class.get(&(st.class.clone(), st.kind)) {
                        Some(&t) => t,
                        None => {
                            let t = new_stream(state, self.level, st.class.clone(), st.kind);
                            state.by_class.insert((st.class.clone(), st.kind), t);
                            t
                        }
                    },
                };
                let start = state.streams[target].len;
                if rest.len() >= self.min {
                    let kept = Kept {
                        stream: target,
                        at: start,
                        len: rest.len() as u64,
                        hash: xxhash_rust::xxh3::xxh3_128(rest),
                    };
                    state.kept.entry(head(&rest[..self.min])).or_default().push(kept);
                }
                drop(st_lock);
                let start2 = self.append(target, &[rest])?;
                debug_assert_eq!(start, start2);
                pieces.push((target, start, rest.len() as u64));
            }
            sealed.insert(x, pieces);
        }
        // (the sink's streams of a stream of the formula's own done: their last blocks
        // compressed now, not held to the end, as nothing more goes to them)
        let mut alone: Vec<usize> = alone.into_values().collect();
        alone.sort_unstable();
        for t in alone {
            let st = self.state.lock().unwrap();
            if !st.streams[t].tail.is_empty() {
                drop(self.fill(st, t)?);
            }
        }
        if sealed.is_empty() {
            return Ok(());
        }
        // the slices of the sink's streams, by the formula's streams sunk (one of each, after its
        // own: of the stream's length, as far as its slices reach)
        let lens: Vec<u64> = {
            let st = self.state.lock().unwrap();
            st.streams.iter().map(|s| s.len).collect()
        };
        let mut of_sink: HashMap<usize, usize> = HashMap::new();
        for (i, s) in f.streams.iter().enumerate() {
            if let Some(k) = s.data.sunk_in() {
                of_sink.entry(k).or_insert(i);
            }
        }
        let classes: Vec<(Cow<'static, str>, Kind)> = {
            let st = self.state.lock().unwrap();
            st.streams.iter().map(|s| (s.class.clone(), s.kind)).collect()
        };
        let mut streams = std::mem::take(&mut f.streams);
        let mut cut = |n: &mut Node| {
            if let &mut Node::Slice { stream, start, len } = n
                && let Some(pieces) = sealed.get(&(stream, start, len))
            {
                let parts = pieces.iter().map(|&(k, a, l)| {
                    let i = *of_sink.entry(k).or_insert_with(|| {
                        streams.push(crate::Stream {
                            class: classes[k].0.clone(),
                            kind: classes[k].1,
                            content: false,
                            apart: false,
                            data: Bytes::sunk(k, 0),
                        });
                        streams.len() - 1
                    });
                    Node::slice(i, a, l)
                });
                *n = Node::concat(parts.collect::<Vec<_>>());
            }
        };
        f.shared.iter_mut().flat_map(|s| s.inputs_mut()).for_each(|n| n.visit_mut(&mut cut));
        f.root.visit_mut(&mut cut);
        // (each sunk stream as long as the sink's is now)
        for s in &mut streams {
            if let Some(k) = s.data.sunk_in() {
                s.data = Bytes::sunk(k, lens[k] as usize);
            }
        }
        f.streams = streams;
        // (empty slices of the streams sealed of nothing)
        let mut empty = |n: &mut Node| {
            if let &mut Node::Slice { len: 0, .. } = n {
                *n = Node::Concat(Vec::new());
            }
        };
        f.shared.iter_mut().flat_map(|s| s.inputs_mut()).for_each(|n| n.visit_mut(&mut empty));
        f.root.visit_mut(&mut empty);
        f.compact();
        Ok(())
    }

    /// The streams, their last blocks compressed (those not full).
    pub fn close(self) -> Result<Vec<Sunk>> {
        self.close_direct().map(|c| c.0)
    }

    /// [`Sink::close`], and of a direct sink, where the blocks it wrote end (else 0). Fails where
    /// a block was not written.
    pub fn close_direct(self) -> Result<(Vec<Sunk>, u64)> {
        let State { mut streams, mut direct, .. } = self.state.into_inner().unwrap();
        // (the last blocks of a direct sink's streams filled in the order of the streams)
        if let Some(d) = &mut direct {
            for (i, s) in streams.iter_mut().enumerate().filter(|(_, s)| !s.tail.is_empty()) {
                s.ordinals.push(d.filled);
                d.filled += 1;
                d.queue.push_back((i, s.blocks.len()));
            }
        }
        let tails: Vec<(usize, &[u8], How)> = streams
            .iter()
            .enumerate()
            .filter(|(_, s)| !s.tail.is_empty())
            .map(|(i, s)| (i, &s.tail[..], s.how))
            .collect();
        let (level, threads, spill) = (self.level, self.threads, direct.is_none());
        let encoded =
            aqr_common::pool::par_map(tails.len(), threads, |i| encoded(level, threads, tails[i].1, tails[i].2, spill));
        let mut last: Vec<Option<Bytes>> = (0..streams.len()).map(|_| None).collect();
        for ((i, ..), b) in tails.iter().zip(encoded) {
            last[*i] = Some(b?);
        }
        for (s, last) in streams.iter_mut().zip(last) {
            s.tail = Vec::new();
            s.blocks.extend(last.map(Block::Done));
        }
        let mut st = State { direct, streams, ..State::default() };
        write_out(&mut st, true);
        let end = match st.direct.as_mut() {
            Some(d) => {
                if let Some(e) = d.failed.take() {
                    return Err(Error::new(format!("writing the sink's blocks: {e}")));
                }
                debug_assert!(d.queue.is_empty());
                d.at
            }
            None => 0,
        };
        let mut out = Vec::with_capacity(st.streams.len());
        for s in st.streams {
            let mut blocks = Vec::with_capacity(s.blocks.len());
            for b in s.blocks {
                match b {
                    Block::Done(b) => blocks.push(b),
                    Block::Running(h) => blocks.push(h.join().unwrap_or_else(|p| std::panic::resume_unwind(p))?),
                    Block::Here => unreachable!("compressed before the sink is closed"),
                    Block::Written => {}
                }
            }
            out.push(Sunk { class: s.class, kind: s.kind, len: s.len, blocks, ordinals: s.ordinals });
        }
        Ok((out, end))
    }
}

/// Of a direct sink, writes out the blocks filled first that are compressed, in order; where more
/// than its `pending` are queued (or `all`), waits for the first (holding the lock: what it waits
/// for takes none). A block that failed to compress is written as nothing, the sink then failing.
fn write_out(st: &mut State, all: bool) {
    let State { direct, streams, .. } = st;
    let Some(d) = direct else { return };
    while let Some(&(s, k)) = d.queue.front() {
        let b = &mut streams[s].blocks[k];
        let done = match std::mem::replace(b, Block::Written) {
            Block::Done(x) => Ok(x),
            Block::Running(h) if all || d.queue.len() > d.pending => {
                h.join().unwrap_or_else(|p| std::panic::resume_unwind(p))
            }
            other => {
                *b = other;
                return;
            }
        };
        d.queue.pop_front();
        match done {
            Ok(x) if d.failed.is_none() => match d.place.write_at(d.at, &x) {
                Ok(()) => d.at += x.len() as u64,
                Err(e) => d.failed = Some(e),
            },
            Ok(_) => {}
            Err(e) => d.failed = d.failed.take().or_else(|| Some(io::Error::other(e.to_string()))),
        }
    }
}

fn new_stream(state: &mut State, level: Level, class: Cow<'static, str>, kind: Kind) -> usize {
    let how = how_of(level, &class, kind);
    let (tail, blocks, ordinals) = (Vec::new(), Vec::new(), Vec::new());
    state.streams.push(SinkStream { class, kind, how, len: 0, tail, blocks, ordinals });
    state.streams.len() - 1
}

/// The hash a slice sealed is found by: of its first [`Sink::min`] bytes.
fn head(d: &[u8]) -> u64 {
    xxhash_rust::xxh3::xxh3_64(d)
}

/// Non-leaf block of `d`, encoded as [`Encoder::write`] writes it (compressed as `how` tells, at
/// `level`, on up to `threads` threads), of the store where one is set if it is to `spill`.
fn encoded(level: Level, threads: usize, d: &[u8], how: How, spill: bool) -> Result<Bytes> {
    let c = THREAD_CCTX.with(|c| {
        let c = &mut c.borrow_mut();
        let plan = plan_by(level, how, &[d], &mut |sample, l| match sample {
            Some(sample) => compressed(c, &[sample], l, 1),
            None => compressed(c, &[d], l, 1),
        })?;
        Ok::<_, Error>(plan.carry_out(c, &[d], threads)?.0)
    })?;
    let mut out = Vec::new();
    content_block(&mut out, false, &[d], c.as_deref());
    let mut b = Bytes::from(out);
    if spill {
        crate::store::spill(&mut b);
    }
    Ok(b)
}
