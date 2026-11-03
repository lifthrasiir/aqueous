//! The sink: streams of an archive written as they are made, each non-leaf block compressed as
//! soon as it is full and held so (of the store where one is set, [`crate::store`]) rather than
//! the stream's bytes (`docs/streaming-encoder-design.md`). A block is compressed as the encoder
//! compresses it at the end ([`Encoder::write`]): by its bytes and how its stream is compressed
//! alone, so that an archive of streams written to a sink is the same as of streams held whole.
//! The encoder writes the blocks as they are ([`Encoder::encode_sunk_to`]).
//!
//! A formula is sealed into a sink ([`Sink::seal`]): the bytes of its slices written to the
//! sink's stream of their class and kind (one of its own for a stream that [`Formula::merged`]
//! keeps apart), and the slices then of the sink's streams ([`Bytes::sunk`]). A slice of the same
//! bytes as one sealed before, or starting with such slices, is of those instead: the formulas
//! sealed share them, as [`Formula::share_slices`] has the slices of one formula share them.

use super::*;
use crate::Bytes;

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

/// A stream of a sink: its class and kind, bytes so far, the bytes of its block not full yet,
/// and its blocks encoded.
struct SinkStream {
    class: Cow<'static, str>,
    kind: Kind,
    how: How,
    len: u64,
    tail: Vec<u8>,
    blocks: Vec<Block>,
}

/// A block of a sink's stream: encoded, or being compressed on a worker of the pool (or, a
/// moment, on the thread that filled it).
enum Block {
    Done(Bytes),
    Running(aqr_common::pool::Handle<'static, Result<Bytes>>),
    Here,
}

/// A stream of a sink as [`Sink::close`] gives it: its class, kind and length, and its non-leaf
/// blocks, encoded (each its block type, length and data).
pub struct Sunk {
    pub class: Cow<'static, str>,
    pub kind: Kind,
    pub len: u64,
    pub blocks: Vec<Bytes>,
}

impl Sink {
    /// A sink of streams compressed as `enc` compresses them, sharing the slices sealed of at
    /// least `min` bytes.
    pub fn new(enc: &Encoder, min: usize) -> Sink {
        Sink { level: enc.level, threads: enc.threads, min, state: Mutex::default() }
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
                    let (block, how) = (std::mem::take(&mut stream.tail), stream.how);
                    let (level, threads) = (self.level, self.threads);
                    let mut w = aqr_common::pool::reserve(1);
                    if !w.is_empty() {
                        let h = w.spawn(move || encoded(level, threads, &block, how));
                        stream.blocks.push(Block::Running(h));
                        continue;
                    }
                    // (with the lock let go: the block's place kept)
                    let k = stream.blocks.len();
                    stream.blocks.push(Block::Here);
                    drop(st);
                    let b = encoded(level, threads, &block, how)?;
                    drop(block);
                    st = self.state.lock().unwrap();
                    st.streams[s].blocks[k] = Block::Done(b);
                }
            }
        }
        Ok(at)
    }

    /// Seals formula `f`: the bytes of its slices (of streams not sunk) written to the sink,
    /// those of each class and kind to its stream (one of its own for a stream of
    /// [`Kind::Compressed`]), a slice of at least [`Sink::min`] bytes not written where it is
    /// the same as slices sealed before, one after another, or ends after them; the slices then
    /// of the sink's streams (of the formula's streams sunk, one of each), and the streams they
    /// were of dropped. Slices past the end of their stream, and streams kept apart, are left as
    /// they are. Formulas are to be sealed one at a time, in an order that does not depend on
    /// the threads (the archive's bytes do).
    pub fn seal(&self, f: &mut Formula) -> Result<()> {
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
        let mut past = vec![false; f.streams.len()];
        for &x in &slices {
            if open(f, x.0) && (!within(f, x) || f.streams[x.0].apart) {
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
        f.shared.iter_mut().flat_map(|s| &mut s.apply.inputs).for_each(|n| n.visit_mut(&mut cut));
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
        f.shared.iter_mut().flat_map(|s| &mut s.apply.inputs).for_each(|n| n.visit_mut(&mut empty));
        f.root.visit_mut(&mut empty);
        f.compact();
        Ok(())
    }

    /// The streams, their last blocks compressed (those not full).
    pub fn close(self) -> Result<Vec<Sunk>> {
        let streams = self.state.into_inner().unwrap().streams;
        let tails: Vec<(usize, &[u8], How)> = streams
            .iter()
            .enumerate()
            .filter(|(_, s)| !s.tail.is_empty())
            .map(|(i, s)| (i, &s.tail[..], s.how))
            .collect();
        let (level, threads) = (self.level, self.threads);
        let encoded =
            aqr_common::pool::par_map(tails.len(), threads, |i| encoded(level, threads, tails[i].1, tails[i].2));
        let mut last: Vec<Option<Bytes>> = (0..streams.len()).map(|_| None).collect();
        for ((i, ..), b) in tails.iter().zip(encoded) {
            last[*i] = Some(b?);
        }
        let mut out = Vec::with_capacity(streams.len());
        for (s, last) in streams.into_iter().zip(last) {
            let mut blocks = Vec::with_capacity(s.blocks.len() + 1);
            for b in s.blocks {
                blocks.push(match b {
                    Block::Done(b) => b,
                    Block::Running(h) => h.join().unwrap_or_else(|p| std::panic::resume_unwind(p))?,
                    Block::Here => unreachable!("compressed before the sink is closed"),
                });
            }
            blocks.extend(last);
            out.push(Sunk { class: s.class, kind: s.kind, len: s.len, blocks });
        }
        Ok(out)
    }
}

fn new_stream(state: &mut State, level: Level, class: Cow<'static, str>, kind: Kind) -> usize {
    let how = how_of(level, &class, kind);
    state.streams.push(SinkStream { class, kind, how, len: 0, tail: Vec::new(), blocks: Vec::new() });
    state.streams.len() - 1
}

/// The hash a slice sealed is found by: of its first [`Sink::min`] bytes.
fn head(d: &[u8]) -> u64 {
    xxhash_rust::xxh3::xxh3_64(d)
}

/// Non-leaf block of `d`, encoded as [`Encoder::write`] writes it (compressed as `how` tells, at
/// `level`, on up to `threads` threads), of the store where one is set.
fn encoded(level: Level, threads: usize, d: &[u8], how: How) -> Result<Bytes> {
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
    crate::store::spill(&mut b);
    Ok(b)
}
