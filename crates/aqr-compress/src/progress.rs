//! What [`compress_tree_to`] tells as it goes ([`Event`]): three phases, each its share of the
//! files done, and the files as they are recompressed and written.
//!
//! The threads at work send notes ([`Note`]) to a thread of their own ([`Tracker::run`]), which
//! adds them up and tells the events in order; only [`Event::Written`] is told by the thread
//! making the archive.
//!
//! [`compress_tree_to`]: crate::compress_tree_to

use std::collections::{HashMap, HashSet};
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
    /// A batch of so many bytes begins (again, where it is made again), its files verified or
    /// not.
    Batch {
        bytes: u64,
        verify: bool,
    },
    /// Of the batch: contents (by index) started and ended, and analysis done with all.
    Started(usize),
    Ended(usize),
    Analyzed,
    /// Of the batch: the streams compressed (bytes done of all), and bytes of files verified.
    Encoded(u64, u64),
    Verified(u64),
    /// The batch written.
    Written,
}

/// Where the notes go, cheap to clone.
#[derive(Clone)]
pub(crate) struct Notes(Sender<Note>);

impl Notes {
    pub fn send(&self, n: Note) {
        // (nothing to do where the tracker is gone)
        let _ = self.0.send(n);
    }

    /// What the encoder tells of the streams it compresses, as notes.
    pub fn encoding(&self) -> aqr_formula::aqr::Progress {
        let notes = self.0.clone();
        Arc::new(move |done, total| _ = notes.send(Note::Encoded(done, total)))
    }
}

/// The contents of a batch by where their data is, for the analysis to tell them by.
pub(crate) struct Files {
    pub notes: Notes,
    pub by_at: HashMap<usize, usize>,
}

impl Files {
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
        // where the batch began, its bytes and whether it is verified, and within it
        let (mut base, mut bytes, mut verify) = (0u64, 0u64, false);
        let mut ended: HashSet<usize> = HashSet::new();
        let (mut analyzed, mut encoded, mut verified) = (0u64, 0.0f64, 0u64);
        let compression = |encoded: f64, verified: u64, bytes: u64, verify: bool| -> u64 {
            // (encoding and verifying half each)
            let b = bytes as f64;
            (if verify { (encoded * b + verified.min(bytes) as f64) / 2.0 } else { encoded * b }) as u64
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
                Note::Batch { bytes: b, verify: v } => {
                    (bytes, verify) = (b, v);
                    ended.clear();
                    (analyzed, encoded, verified) = (0, 0.0, 0);
                    tell(Event::Analysis { done: base, total });
                    tell(Event::Compression { done: base, total });
                }
                Note::Started(c) => tell(Event::Started(c)),
                Note::Ended(c) => {
                    tell(Event::Ended(c));
                    if ended.insert(c) {
                        analyzed = (analyzed + self.sizes[c]).min(bytes);
                        tell(Event::Analysis { done: base + analyzed, total });
                    }
                }
                Note::Analyzed => {
                    analyzed = bytes;
                    tell(Event::Analysis { done: base + analyzed, total });
                }
                Note::Encoded(done, all) => {
                    encoded = encoded.max(if all == 0 { 1.0 } else { done as f64 / all as f64 });
                    tell(Event::Compression { done: base + compression(encoded, verified, bytes, verify), total });
                }
                Note::Verified(b) => {
                    verified += b;
                    tell(Event::Compression { done: base + compression(encoded, verified, bytes, verify), total });
                }
                Note::Written => {
                    base += bytes;
                    bytes = 0;
                    tell(Event::Analysis { done: base, total });
                    tell(Event::Compression { done: base, total });
                }
            }
        }
    }
}
