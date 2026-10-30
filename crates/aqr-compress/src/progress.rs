//! What [`compress_tree_to`] tells as it goes ([`Event`]): three phases, each its share of the
//! files done, and the files as they are recompressed and written.
//!
//! The threads at work send notes ([`Note`]) to a thread of their own ([`Tracker::run`]), which
//! adds them up and tells the events in order; only [`Event::Written`] is told by the thread
//! making the archive.
//!
//! [`compress_tree_to`]: crate::compress_tree_to

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use crate::archive::Report;

/// What [`compress_tree_to`](crate::compress_tree_to) tells as it goes, from any of two threads.
#[derive(Debug)]
#[non_exhaustive]
pub enum Event<'a> {
    /// The probe: files looked at (their starts, for those holding each other), of all to look
    /// at, which may grow as it finds more to look at.
    Probe {
        done: u64,
        total: u64,
    },
    /// The analysis: bytes of the files recompressed, of all the files' bytes. A batch made
    /// again (some of its files kept as they are) takes it back to where the batch began.
    Analysis {
        done: u64,
        total: u64,
    },
    /// The compression: bytes of the files compressed into blocks (and verified, where they
    /// are), of all the files' bytes; going back as [`Event::Analysis`] does.
    Compression {
        done: u64,
        total: u64,
    },
    /// Content `c` being recompressed, or done with (it may be taken up again).
    Started(usize),
    Ended(usize),
    /// Of content `c` being recompressed, the bytes of it done (as far as the recompressors
    /// tell: [`aqr_common::progress`]).
    Reached {
        content: usize,
        done: u64,
    },
    /// A batch written: its contents (those stored once with another's too), and what became of
    /// all written so far.
    Written {
        contents: &'a [usize],
        report: &'a Report,
    },
}

/// What the threads at work tell [`Tracker::run`].
pub(crate) enum Note {
    Looked,
    /// More files to look at.
    More(u64),
    /// Batch `batch` of so many bytes, of `contents` (by index), begins (again, where it is made
    /// again), its files verified or not. Batches are made at once, written in their order.
    Batch {
        batch: usize,
        bytes: u64,
        verify: bool,
        contents: Vec<usize>,
    },
    /// Of a batch: contents (by index) started and ended.
    Started(usize),
    Ended(usize),
    /// Of a batch: a part of a content done.
    Done(usize, std::ops::Range<u64>),
    /// Batch `n`'s analysis done with all of its contents.
    Analyzed(usize),
    /// Of batch `n`: the streams compressed (bytes done of all), and bytes of files verified.
    Encoded(usize, u64, u64),
    Verified(usize, u64),
    /// Batch `n` written.
    Written(usize),
}

/// Where the notes go, cheap to clone.
#[derive(Clone)]
pub(crate) struct Notes(Sender<Note>);

impl Notes {
    pub fn send(&self, n: Note) {
        // (nothing to do where the tracker is gone)
        let _ = self.0.send(n);
    }

    /// What the encoder tells of the streams of batch `batch` it compresses, as notes.
    pub fn encoding(&self, batch: usize) -> aqr_formula::aqr::Progress {
        let notes = self.0.clone();
        Arc::new(move |done, total| _ = notes.send(Note::Encoded(batch, done, total)))
    }
}

/// The contents of a batch by where their data is, for the analysis to tell them by.
pub(crate) struct Files {
    notes: Notes,
    by_at: HashMap<usize, usize>,
    /// Their data watched, for how far the work on each is.
    _watches: Vec<aqr_common::progress::Watch>,
}

impl Files {
    /// Of contents `(c, d)`, `c` by its data `d`.
    pub fn of<'a>(notes: &Notes, contents: impl IntoIterator<Item = (usize, &'a [u8])>) -> Files {
        let (mut by_at, mut watches) = (HashMap::new(), Vec::new());
        for (c, d) in contents {
            by_at.insert(d.as_ptr() as usize, c);
            let notes = notes.clone();
            watches.push(aqr_common::progress::watch(d, move |r| notes.send(Note::Done(c, r))));
        }
        Files { notes: notes.clone(), by_at, _watches: watches }
    }

    /// Content `c` at `at` started (`end` false) or ended; nothing for data that is no file's.
    pub fn tell(&self, at: *const u8, end: bool) {
        if let Some(&c) = self.by_at.get(&(at as usize)) {
            self.notes.send(if end { Note::Ended(c) } else { Note::Started(c) });
        }
    }
}

/// Adds up the notes into events.
pub(crate) struct Tracker {
    rx: Receiver<Note>,
    /// Bytes of each content.
    sizes: Vec<u64>,
}

impl Tracker {
    pub fn new(sizes: Vec<u64>) -> (Tracker, Notes) {
        let (tx, rx) = channel();
        (Tracker { rx, sizes }, Notes(tx))
    }

    /// Tells the events of the notes until every [`Notes`] is gone.
    pub fn run(self, tell: &(dyn Fn(Event) + Sync)) {
        let total: u64 = self.sizes.iter().sum();
        let (mut looked, mut to_look) = (0, 0);
        // the bytes of the batches written, the batches being made, and the batch of each of
        // their contents
        let mut base = 0u64;
        let mut runs: HashMap<usize, Run> = HashMap::new();
        let mut of: HashMap<usize, usize> = HashMap::new();
        let analysis = |base: u64, runs: &HashMap<usize, Run>| Event::Analysis {
            done: base + runs.values().map(Run::analysis).sum::<u64>(),
            total,
        };
        let compression = |base: u64, runs: &HashMap<usize, Run>| Event::Compression {
            done: base + runs.values().map(Run::compression).sum::<u64>(),
            total,
        };
        for n in self.rx {
            match n {
                Note::Looked => {
                    looked += 1;
                    tell(Event::Probe { done: looked, total: to_look.max(looked) });
                }
                Note::More(n) => {
                    to_look += n;
                    tell(Event::Probe { done: looked, total: to_look.max(looked) });
                }
                Note::Batch { batch, bytes, verify, contents } => {
                    contents.into_iter().for_each(|c| _ = of.insert(c, batch));
                    runs.insert(batch, Run { bytes, verify, ..Run::default() });
                    tell(analysis(base, &runs));
                    tell(compression(base, &runs));
                }
                Note::Started(c) => tell(Event::Started(c)),
                Note::Ended(c) => {
                    tell(Event::Ended(c));
                    let Some(run) = of.get(&c).and_then(|b| runs.get_mut(b)) else { continue };
                    if run.ended.insert(c) {
                        run.partly -= run.reached.remove(&c).map_or(0, |p| p.len);
                        run.analyzed = (run.analyzed + self.sizes[c]).min(run.bytes);
                        tell(analysis(base, &runs));
                    }
                }
                Note::Done(c, r) => {
                    let Some(run) = of.get(&c).and_then(|b| runs.get_mut(b)) else { continue };
                    if run.ended.contains(&c) {
                        continue;
                    }
                    let p = run.reached.entry(c).or_default();
                    let more = p.add(r.start..r.end.min(self.sizes[c]));
                    if more == 0 {
                        continue;
                    }
                    run.partly += more;
                    let done = p.len;
                    tell(Event::Reached { content: c, done });
                    tell(analysis(base, &runs));
                }
                Note::Analyzed(b) => {
                    let Some(run) = runs.get_mut(&b) else { continue };
                    (run.analyzed, run.partly) = (run.bytes, 0);
                    run.reached.clear();
                    tell(analysis(base, &runs));
                }
                Note::Encoded(b, done, all) => {
                    let Some(run) = runs.get_mut(&b) else { continue };
                    run.encoded = run.encoded.max(if all == 0 { 1.0 } else { done as f64 / all as f64 });
                    tell(compression(base, &runs));
                }
                Note::Verified(b, n) => {
                    let Some(run) = runs.get_mut(&b) else { continue };
                    run.verified += n;
                    tell(compression(base, &runs));
                }
                Note::Written(b) => {
                    base += runs.remove(&b).map_or(0, |r| r.bytes);
                    of.retain(|_, &mut batch| batch != b);
                    tell(analysis(base, &runs));
                    tell(compression(base, &runs));
                }
            }
        }
    }
}

/// How far a batch being made is ([`Tracker::run`]).
#[derive(Default)]
struct Run {
    bytes: u64,
    verify: bool,
    ended: HashSet<usize>,
    /// The parts done of the contents not ended yet, and the bytes of them all.
    reached: HashMap<usize, Parts>,
    partly: u64,
    analyzed: u64,
    encoded: f64,
    verified: u64,
}

impl Run {
    fn analysis(&self) -> u64 {
        (self.analyzed + self.partly).min(self.bytes)
    }

    fn compression(&self) -> u64 {
        // (encoding and verifying half each)
        let b = self.bytes as f64;
        let done = if self.verify {
            (self.encoded * b + self.verified.min(self.bytes) as f64) / 2.0
        } else {
            self.encoded * b
        };
        done as u64
    }
}

/// Parts of a content, as ranges apart.
#[derive(Default)]
struct Parts {
    /// By start, the end.
    ranges: BTreeMap<u64, u64>,
    /// Bytes of them all.
    len: u64,
}

impl Parts {
    /// Adds `r`, giving the bytes it adds.
    fn add(&mut self, r: Range<u64>) -> u64 {
        if r.is_empty() {
            return 0;
        }
        let (mut start, mut end) = (r.start, r.end);
        // (those it touches merged into it)
        if let Some((&s, &e)) = self.ranges.range(..=start).next_back()
            && e >= start
        {
            start = s;
            end = end.max(e);
        }
        let before = self.len;
        let touched: Vec<(u64, u64)> = self.ranges.range(start..=end).map(|(&s, &e)| (s, e)).collect();
        for (s, e) in touched {
            self.ranges.remove(&s);
            self.len -= e - s;
            end = end.max(e);
        }
        self.ranges.insert(start, end);
        self.len += end - start;
        self.len - before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts() {
        let mut p = Parts::default();
        assert_eq!(p.add(10..20), 10);
        assert_eq!(p.add(30..40), 10);
        assert_eq!(p.add(15..18), 0);
        assert_eq!(p.add(18..32), 10);
        assert_eq!(p.add(0..5), 5);
        assert_eq!(p.add(5..5), 0);
        assert_eq!(p.add(0..50), 15);
        assert_eq!((p.len, p.ranges.len()), (50, 1));
    }
}
