//! The blocks of an OBJ file as streams, and back (`FORMAT.md` specifies the streams).
//!
//! Blocks end at a line end where there is one; coded blocks carry state over (previous
//! values, templates), so they are decoded in order.
//!
//! With more than one thread, the line transform of blocks stays on the calling thread, in
//! order; verification (a decoder of its own) and the stream coding of each block run on
//! other threads, and decoding unpacks the streams of the blocks ahead on other threads. The
//! output is the same whatever the number of threads.

use crate::ans;
use crate::block::{self, NSTREAMS};
use crate::{Result, corrupt};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, mpsc};

/// Largest block the decoder accepts (bounds its memory).
pub(crate) const MAX_BLOCK: usize = 64 << 20;

/// How a stream is stored.
pub(crate) const M_STORED: u8 = 0;
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
        Options { block: 4 << 20, verify: true, force_whole: 0, threads: 1 }
    }
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

/// A block as stored: whole, or its streams (raw length, how, data).
pub(crate) enum Stored<'a> {
    /// The block itself.
    Whole(&'a [u8]),
    Coded(Box<[(usize, u8, &'a [u8]); NSTREAMS]>),
}

/// Unpacks the streams of a coded block into `streams`, or a whole block into `out`.
fn unpack(b: &Stored, streams: &mut Streams, out: &mut Vec<u8>) -> Result<()> {
    match b {
        Stored::Whole(d) => out.extend_from_slice(d),
        Stored::Coded(desc) => {
            for (s, &(raw, m, d)) in streams.iter_mut().zip(desc.iter()) {
                s.clear();
                if raw == 0 {
                    continue;
                }
                // (each stream as long as its longest, not twice that as it grows)
                s.reserve_exact(raw);
                if m == M_STORED {
                    s.extend_from_slice(d);
                } else if !ans::decode(d, raw, s) {
                    return Err(corrupt());
                }
            }
        }
    }
    Ok(())
}

/// Decodes blocks onto `w` until `len` bytes are written; `next(left)` gives the next block
/// (`left`: bytes still to come). On an error, what was written is not the original.
pub(crate) fn decode_blocks<'a>(
    next: &mut impl FnMut(usize) -> Result<(usize, Stored<'a>)>,
    len: usize,
    w: &mut Vec<u8>,
    threads: usize,
) -> Result<()> {
    // (the workers of the pool, if any is free)
    let workers = aqr_common::pool::reserve(threads.saturating_sub(1));
    if !workers.is_empty() {
        return decode_par(workers, next, len, w, threads);
    }
    // (each block decoded onto `w` itself)
    let mut done = 0;
    let mut dec = block::Decoder::default();
    // (a line takes 2 bytes at least)
    dec.reserve(len / 2);
    let mut streams = new_streams();
    let start = w.len();
    while done < len {
        let (n, b) = next(len - done)?;
        w.reserve(n);
        unpack(&b, &mut streams, w)?;
        if matches!(b, Stored::Coded(_)) && !dec.decode(&streams, w) {
            return Err(corrupt());
        }
        done += n;
        if w.len() - start != done {
            return Err(corrupt());
        }
    }
    Ok(())
}

/// A block whose streams (or whole contents) the other threads have decoded.
type Unpacked = Result<(Box<Streams>, Vec<u8>)>;

/// Work for the decompressing pool: a block to unpack, or a decoded block to print.
enum DJob<'a> {
    Unpack(usize, Stored<'a>),
    Print(usize, usize, block::Deferred),
}

enum DReport {
    Unpacked(usize, Unpacked),
    Printed(usize, Result<Vec<u8>>),
}

fn decode_par<'a>(
    workers: aqr_common::pool::Reservation,
    next: &mut impl FnMut(usize) -> Result<(usize, Stored<'a>)>,
    len: usize,
    w: &mut Vec<u8>,
    threads: usize,
) -> Result<()> {
    let limit = threads + 1;
    aqr_common::pool::scope_in(workers, |sc| {
        let (rtx, rrx) = mpsc::channel::<DReport>();
        let (jtx, jrx) = mpsc::channel::<DJob>();
        let jrx = Arc::new(Mutex::new(jrx));
        for _ in 0..sc.workers() {
            let (rtx, jrx) = (rtx.clone(), jrx.clone());
            sc.spawn(move || {
                loop {
                    let Ok(j) = jrx.lock().expect("job queue").recv() else { break };
                    let rep = match j {
                        DJob::Unpack(idx, b) => {
                            let mut streams = Box::new(new_streams());
                            let mut out = Vec::new();
                            DReport::Unpacked(idx, unpack(&b, &mut streams, &mut out).map(|_| (streams, out)))
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
        // Blocks sent to unpack (length, coded, result), from index `unpacked`; blocks
        // decoded, being printed (length, text), from index `written`.
        let mut unpacking: VecDeque<(usize, bool, Option<Unpacked>)> = VecDeque::new();
        let mut printing: VecDeque<(usize, Option<Result<Vec<u8>>>)> = VecDeque::new();
        let (mut read, mut done, mut unpacked, mut written) = (0usize, 0usize, 0usize, 0usize);
        while done < len {
            while unpacking.len() < limit && read < len {
                let (n, b) = next(len - read)?;
                unpacking.push_back((n, matches!(b, Stored::Coded(_)), None));
                jtx.send(DJob::Unpack(unpacked + unpacking.len() - 1, b)).expect("workers");
                read += n;
            }
            if let Some((n, text)) = printing.pop_front_if(|p| p.1.is_some()) {
                let text = text.expect("printed")?;
                w.extend_from_slice(&text);
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
        Ok(())
    })
}
