//! Archives of several files ([`compress_tree`]; spec, "Segments and entries"): the files of a
//! [`Tree`] are recompressed as the entries of a ZIP archive are, each by its kind by its name
//! ([`kind_by_name`]) and up to [`Options::depth`] formats deep from the file, the PNG files
//! alike among them coded as sets; each file's formula is then a leaf of a segment of the
//! archive, closed after the batch that makes it [`Options::segment`] bytes long
//! ([`Writer::close`]). The files are made at once, on up to [`Options::threads`] threads (the formulas of
//! those of pure formats beforehand, as contents are; then each file in a context of its own).
//!
//! The files are taken in batches of about [`Options::batch`] bytes, in their order (by
//! extension, then by name; PE and ELF files by likeness, from where the first is, [`lay_alike`]):
//! each batch read ([`Source`]), made, laid out, checked and written in their order, a batch
//! made while the one before it still is ([`AHEAD`]), so that what is held at once is about
//! two batches', whatever the files' (but for a batch larger than that, made alone). Files of
//! the same content as one before are stored once. Files that may hold each other (a file and
//! a ZIP archive or gzip file of it, a file and the start of it) are put in one batch
//! ([`together`]), where what their streams share is stored once ([`Formula::share_slices`]):
//! the recompressors take what the batch holds as costing nothing ([`Held`]).
//!
//! A file is to be read without the others, so what files share is bounded: a set of PNG files
//! takes at most [`Options::set_bytes`] bytes of them (as it is decoded whole for any of them),
//! and the streams of a class and kind of the files of a batch are put one after another,
//! solid, in non-leaf blocks of at most [`Options::block`] bytes, but those that do not compress
//! ([`aqr::Encoder::compresses`]).
//!
//! With [`Options::verify`], each batch is decoded alone ([`aqr::SegmentWriter::standalone`],
//! the applications' outputs taken from those evaluated beforehand, as [`crate::compress`]
//! does) and its files compared with the input: those not given back (all of the batch's where
//! it does not decode) are made again kept raw.

use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::ops::Range;
use std::sync::atomic::AtomicBool;
use std::sync::{Condvar, Mutex};

use aqr_decompress::Native;
use aqr_formula::aqr::{self, Batch, ContentAt, Entry, EntryNode, Extra, Leaf, Level, Placed, SegmentWriter, Tree};
use aqr_formula::probe::{self, Held};
use aqr_formula::{Bytes, Formula, Kind, Module, Node};

use crate::progress::{Event, Files, Note, Notes, Tracker};
use crate::{Cx, Once, Options, detect, kind_by_name, nested, place_all, sets, verify};

/// Streams shorter than this are put together with those of their class and kind without
/// telling whether they compress.
const MERGE_PROBE: usize = 4 << 10;

/// A file's content to archive: its data, and a name telling its kind ([`kind_by_name`]).
#[derive(Debug, Clone, Copy)]
pub struct Input<'a> {
    pub name: &'a [u8],
    pub data: &'a [u8],
}

/// Where the contents of the files to archive come from, read as their batch is made.
pub trait Source: Sync {
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// A name telling the kind of content `c` ([`kind_by_name`]), and its order.
    fn name(&self, c: usize) -> &[u8];

    /// Bytes of content `c`, as it is to be read (for the batches: it may be read otherwise).
    fn size(&self, c: usize) -> u64;

    /// The bytes of content `c`, best shared rather than copied (a file mapped into memory,
    /// [`Bytes::shared`]): they are held as long as its batch is made.
    fn read(&self, c: usize) -> io::Result<Bytes>;

    /// Bytes `range` of content `c` (those within it), for a look at its start or its end.
    fn read_range(&self, c: usize, range: Range<u64>) -> io::Result<Vec<u8>> {
        let d = self.read(c)?;
        let end = (range.end as usize).min(d.len());
        Ok(d[(range.start as usize).min(end)..end].to_vec())
    }

    /// Fails where content `c` may not have been read as it is: where it changed while read
    /// (a file cut short while mapped, [`crate::mapped::Mapper::cut`]). Asked of the contents
    /// of each batch before it is written, those of the same content as others among them.
    fn intact(&self, c: usize) -> io::Result<()> {
        let _ = c;
        Ok(())
    }
}

impl Source for [Input<'_>] {
    fn len(&self) -> usize {
        <[Input]>::len(self)
    }

    fn name(&self, c: usize) -> &[u8] {
        self[c].name
    }

    fn size(&self, c: usize) -> u64 {
        self[c].data.len() as u64
    }

    fn read(&self, c: usize) -> io::Result<Bytes> {
        Ok(self[c].data.to_vec().into())
    }

    fn read_range(&self, c: usize, range: Range<u64>) -> io::Result<Vec<u8>> {
        let d = self[c].data;
        let end = (range.end as usize).min(d.len());
        Ok(d[(range.start as usize).min(end)..end].to_vec())
    }
}

/// Where an archive of several files is written ([`compress_tree_into`]).
pub trait Output: Write {
    /// Where the archive starts in the file written (its header magic): where its footers are
    /// placed by ([`aqr::crossing`]).
    fn base(&self) -> u64 {
        0
    }

    /// Whether what is written can be made durable ([`Output::sync`]) and written over
    /// ([`Output::patch`]): the archive is then committed as it is written, at each segment
    /// closed (spec, "Appending to an archive"), but with mirror blocks or a size hint block.
    fn commits(&self) -> bool {
        false
    }

    /// Whether the archive is committed once, as it ends, where it [commits](Output::commits):
    /// whole or not at all, the segments before closed without a footer (the archive appended to
    /// left as it was until then).
    fn commits_once(&self) -> bool {
        false
    }

    /// Makes what was written durable.
    fn sync(&mut self) -> io::Result<()> {
        self.flush()
    }

    /// Writes `bytes` over those written at `at` (from where the archive starts).
    fn patch(&mut self, at: u64, bytes: &[u8]) -> io::Result<()> {
        let _ = (at, bytes);
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    /// Ends what is written, once the archive is: what is held back written (the last chunk of
    /// an encryption).
    fn end(&mut self) -> io::Result<()> {
        self.flush()
    }
}

/// An output that only writes: an archive not committed as it is written.
pub struct Plain<'a>(pub &'a mut dyn Write);

impl Write for Plain<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl Output for Plain<'_> {}

/// An archive in memory, committed as it is written.
impl Output for Vec<u8> {
    fn commits(&self) -> bool {
        true
    }

    fn patch(&mut self, at: u64, bytes: &[u8]) -> io::Result<()> {
        let at = usize::try_from(at).ok().filter(|&a| a + bytes.len() <= self.len());
        let at = at.ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        self[at..at + bytes.len()].copy_from_slice(bytes);
        Ok(())
    }
}

/// An archive of several files, and what became of them.
pub struct Archived {
    pub archive: Vec<u8>,
    pub report: Report,
}

/// What became of the files of an archive.
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// By content.
    pub files: Vec<FileReport>,
    /// The non-leaf blocks that several files slice.
    pub solid: Vec<Solid>,
    /// The contents coded against each other, a set each.
    pub sets: Vec<Vec<usize>>,
    /// Bytes of the archive but for the files': its header and footer, the module, resource,
    /// dictionary, metadata and mirror blocks.
    pub overhead: usize,
    /// Bytes of the mirror blocks ([`Options::mirrors`]; and of the spacer blocks beside them,
    /// and of the footer as its offsets grew).
    pub mirrors: usize,
    /// The contents kept raw because the archive did not give them back.
    pub unverified: Vec<usize>,
    /// Bytes of the archive, and of the contents read.
    pub len: usize,
    pub read: u64,
    /// Where the archive was stopped before it was whole ([`Stop`]): what it left out.
    pub stopped: Option<Stopped>,
}

/// An archive to append to ([`compress_tree_into`]; spec, "Appending to an archive"), as written
/// to the output already: where its footer is and its last metadata block (from its header
/// magic: [`aqr::footer_of`]), and the least archive-wide link group it takes (negative; 0 where
/// none), the groups of the segments appended taken below it. Its footer is made a spacer block
/// by the first commit where `kill`; else the segments are written over it (from where it
/// starts: the output cut there already), as where it crosses a sector of the file
/// ([`aqr::crossing`]).
#[derive(Debug, Clone)]
pub struct Appending {
    pub footer: Range<usize>,
    pub metadata: usize,
    pub link_groups: i128,
    pub kill: bool,
}

/// What an archive stopped left out ([`Report::stopped`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stopped {
    /// Files (each hard link) and bytes of the contents not archived.
    pub files: u64,
    pub bytes: u64,
    /// Whether the archive holds anything: none where nothing was written before the stop.
    pub kept: bool,
}

/// A request to stop an archive being written ([`compress_tree_into`]), looked at as it is
/// written: the archive is then ended as it is, of the files written, and `ended` told of it
/// (spec, "`segment-status`": what it left out). `ended` may end the process rather than wait
/// for the work under way to be let go.
pub struct Stop<'a> {
    pub requested: &'a AtomicBool,
    pub ended: &'a (dyn Fn(&Report) + Sync),
}

impl Stop<'_> {
    fn is_requested(&self) -> bool {
        self.requested.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// The archive stopped and ended ([`Writer::stopped`]), as an error passed up to where it is
/// made the outcome.
#[derive(Debug)]
struct Ended(Report);

impl std::fmt::Display for Ended {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("stopped")
    }
}

impl std::error::Error for Ended {}

/// Whether `e` is the archive stopped and ended ([`Ended`]).
fn is_ended(e: &io::Error) -> bool {
    e.get_ref().is_some_and(|e| e.is::<Ended>())
}

#[derive(Debug, Clone, Default)]
pub struct FileReport {
    /// The format the data starts like ([`detect`]).
    pub format: Option<&'static str>,
    /// The modules its formula applies, in order (none: kept as it is).
    pub modules: Vec<Module>,
    /// Bytes of the archive that are the file's: its leaf block, and the part of the non-leaf
    /// blocks it slices by the bytes it slices of them (and of a set, by the sizes of its
    /// outputs). None for a file of the same content as another.
    pub archived: f64,
    /// The content it is the same as, stored once.
    pub same_as: Option<usize>,
    /// Bytes of the content.
    pub len: u64,
}

/// A non-leaf block that several files slice.
#[derive(Debug, Clone)]
pub struct Solid {
    pub class: String,
    pub kind: Kind,
    pub len: usize,
    pub archived: usize,
    /// The contents that slice it, in order.
    pub contents: Vec<usize>,
}

/// The archive of `tree`, whose files are of `contents` (by [`EntryNode::File`]), with what
/// became of them. Panics on a tree that is not one the spec allows.
///
/// [`EntryNode::File`]: aqr::EntryNode::File
pub fn compress_tree(tree: &Tree, contents: &[Input], opt: &Options) -> Archived {
    let mut archive = Vec::new();
    let report = compress_tree_to(tree, contents, opt, &mut archive, &|_| {}).expect("no failure but of the tree");
    Archived { archive, report }
}

/// The archive of `tree`, whose files are of the contents of `src`, written to `out` as it is
/// made, with what became of the files; telling `tell` how far it is ([`Event`]). Fails where a
/// content cannot be read or the archive written, and on a tree the spec does not allow.
pub fn compress_tree_to<S: Source + ?Sized>(
    tree: &Tree,
    src: &S,
    opt: &Options,
    out: &mut dyn Write,
    tell: &(dyn Fn(Event) + Sync),
) -> io::Result<Report> {
    compress_tree_into(tree, src, opt, &mut Plain(out), None, None, tell)
}

/// [`compress_tree_to`], written to `out`: committed as it is written where it
/// [commits](Output::commits), and ended ([`Output::end`]) once written; or, where `stop`
/// requests it, ended as it is ([`Stop`], [`Report::stopped`]). Where `from` is given, the
/// segments are appended to the archive it tells of, which `out` holds (and then has to
/// commit): `tree` is what they add to it and replace of it.
#[allow(clippy::too_many_arguments)]
pub fn compress_tree_into<S: Source + ?Sized>(
    tree: &Tree,
    src: &S,
    opt: &Options,
    out: &mut dyn Output,
    from: Option<&Appending>,
    stop: Option<&Stop>,
    tell: &(dyn Fn(Event) + Sync),
) -> io::Result<Report> {
    let (tracker, notes) = Tracker::new((0..src.len()).map(|c| src.size(c)).collect());
    crate::compressing();
    let _spill = crate::spilling(opt);
    std::thread::scope(|s| {
        s.spawn(|| tracker.run(tell));
        // (the notes gone with it, the tracker done)
        compress_tree_noting(tree, src, opt, out, from, stop, notes, tell)
    })
    .or_else(|e| match e.get_ref().and_then(|e| e.downcast_ref::<Ended>()) {
        Some(Ended(r)) => Ok(r.clone()),
        None => Err(e),
    })
}

#[allow(clippy::too_many_arguments)]
fn compress_tree_noting<S: Source + ?Sized>(
    tree: &Tree,
    src: &S,
    opt: &Options,
    out: &mut dyn Output,
    from: Option<&Appending>,
    stop: Option<&Stop>,
    notes: Notes,
    tell: &(dyn Fn(Event) + Sync),
) -> io::Result<Report> {
    // (commits only of an archive as it is written: mirror blocks make the footer theirs)
    let commit = (out.commits() && !opt.mirrors && !opt.size_hint).then(|| out.base());
    let once = out.commits_once();
    if from.is_some() && commit.is_none() {
        let e = "an archive appended to by commits alone: not with mirror blocks, a size hint block, nor to an output that does not commit";
        return Err(io::Error::new(io::ErrorKind::InvalidInput, e));
    }
    let mut out = match opt.mirrors {
        true => Out::Mirrored(aqr::Mirroring::new(out, opt.size_hint)),
        false => Out::Plain(out),
    };
    let n = src.len();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| sort_key(src.name(a)).cmp(&sort_key(src.name(b))).then(a.cmp(&b)));
    // the contents that may hold each other together, in the place of the first, a batch never
    // cutting them apart
    let (mut group, entries, exes) = together(src, opt, &notes)?;
    // (and the contents of an entry, its file's and its forks', for it to be of one segment)
    entry_units(tree, &mut group);
    lay_alike(&mut order, &exes, &group, src, opt, &notes)?;
    let mut members: HashMap<usize, Vec<usize>> = HashMap::new();
    order.iter().for_each(|&c| members.entry(group[c]).or_default().push(c));
    let mut batches: Vec<Vec<usize>> = vec![Vec::new()];
    let mut bytes = 0u64;
    for &c in &order {
        let Some(unit) = members.remove(&group[c]) else { continue };
        let size = unit.iter().fold(0u64, |n, &c| n.saturating_add(src.size(c)));
        if !batches.last().unwrap().is_empty() && bytes.saturating_add(size) > opt.batch as u64 {
            batches.push(Vec::new());
            bytes = 0;
        }
        batches.last_mut().unwrap().extend(unit);
        bytes = bytes.saturating_add(size);
    }

    // those made alone first: between others, one would wait for those before it to be
    // written, and those after it for it, the threads idle as the longest file of either is made
    let weight = |b: &Vec<usize>| b.iter().map(|&c| src.size(c)).sum::<u64>();
    batches.sort_by_key(|b| weight(b) <= (opt.batch as u64).saturating_mul(AHEAD));

    let w = match from {
        Some(a) if a.kill => SegmentWriter::appending(a.footer.clone(), a.metadata),
        Some(a) => SegmentWriter::replacing(a.footer.start, a.metadata),
        None => {
            let w = SegmentWriter::with_size_hint(opt.size_hint);
            out.write_all(w.head())?;
            w
        }
    };
    let w_at = w.len();
    let weights: Vec<u64> = batches.iter().map(weight).collect();
    let cx = Ctx { src, opt, notes: &notes, batches: &batches, weights: &weights, entries: &entries, tree, stop };
    let pipe = Pipe {
        reader: Mutex::new(Reader { first: HashMap::new() }),
        state: Mutex::new(State {
            claimed: 0,
            held: 0,
            made: (0..batches.len()).map(|_| None).collect(),
            failed: None,
            written: false,
        }),
        changed: Condvar::new(),
    };
    let mut wr = Writer {
        w,
        at: vec![None; n],
        r: Report { files: vec![FileReport::default(); n], ..Report::default() },
        next: 0,
        hold: opt.mirrors,
        held: std::collections::VecDeque::new(),
        held_len: 0,
        segment: Vec::new(),
        again: HashMap::new(),
        segment_at: w_at,
        segments: 0,
        metadata_end: None,
        inserted: 0,
        commit,
        once,
        files_of: files_of(tree, n),
        link_groups: from.map_or(0, |a| a.link_groups.min(0)),
    };
    std::thread::scope(|s| {
        // (a thread of no worker of the pool, which takes one only as one is free: as the
        // batches being made leave threads idle)
        s.spawn(|| pipe.help(&cx));
        // (the helper stopped however this ends, a panic too)
        struct Done<'a>(&'a Pipe);
        impl Drop for Done<'_> {
            fn drop(&mut self) {
                self.0.state.lock().unwrap_or_else(|e| e.into_inner()).written = true;
                self.0.changed.notify_all();
            }
        }
        let _done = Done(&pipe);
        wr.run(&pipe, &cx, tell, &mut out)
    })?;
    // the last segment's entries, or all of them where it is the only one
    let (last, contents) = match wr.segments {
        0 => {
            let at = |c: usize| wr.at[c].or_else(|| wr.at[wr.r.files[c].same_as?]);
            (None, (0..n).map(|c| at(c).expect("every content made")).collect())
        }
        _ => {
            let (t, contents) = wr.segment_tree(tree);
            (Some(t), contents)
        }
    };
    let tree = last.as_ref().unwrap_or(tree);
    if wr.commit.is_some() {
        let enc = aqr::Encoder::with_threads(opt.level, opt.threads).with_modules(aqr_decompress::wasm_of);
        let meta = wr.w.close_as(&enc, tree, &contents, aqr::Status::Complete).map_err(bad_tree)?;
        wr.commit(&mut out, &meta)?;
        let len = wr.w.len() - wr.w.pad().len();
        out.output().end()?;
        let mut r = wr.r;
        r.len = len;
        r.overhead = r.len - r.files.iter().map(|f| f.archived).sum::<f64>().round() as usize;
        r.unverified.sort_unstable();
        return Ok(r);
    }
    let Writer { w, mut r, held, held_len, inserted, .. } = wr;
    // (an encoder of the metadata block only)
    let enc = aqr::Encoder::with_threads(opt.level, opt.threads).with_modules(aqr_decompress::wasm_of);
    let bad = |e: aqr_formula::Error| io::Error::new(io::ErrorKind::InvalidInput, e.0);
    // with mirror blocks, the metadata block's before the batches held back, where it can be
    // far enough from it; else after it, as the others are ([`aqr::Mirroring`])
    let front = w.len() - held_len;
    let mirrored = match opt.mirrors {
        true => w.finish_mirrored(&enc, tree, &contents, front, opt.size_hint, aqr::Status::Complete).map_err(bad)?,
        false => None,
    };
    let (mirror, end) = match mirrored {
        Some(m) => m,
        None => (Vec::new(), w.finish(&enc, tree, &contents).map_err(bad)?.0),
    };
    out.write_all(&mirror)?;
    for b in &held {
        out.write_all(b)?;
    }
    out.write_all(&end)?;
    out.output().end()?;
    r.mirrors = mirror.len() + out.added() as usize;
    // (the mirror blocks of segments closed, written between batches, counted by the writer)
    r.len = w.len() + end.len() + r.mirrors - inserted;
    r.overhead = r.len - r.files.iter().map(|f| f.archived).sum::<f64>().round() as usize;
    r.unverified.sort_unstable();
    Ok(r)
}

/// Bytes of the batches let in and not yet written, as so many batches' ([`Options::batch`]): a
/// batch is read and analyzed while the one before it is still analyzed (as its longest files
/// leave threads idle), encoded or verified. One longer than these is made alone (and first);
/// one longer than a batch (contents that hold each other, kept together) beside others as
/// these allow. (10gb: of about 1,550 core-seconds of compression, 815 were idle with a batch at
/// a time.)
const AHEAD: u64 = 2;

/// What the batches being made share ([`compress_tree_noting`]).
struct Ctx<'a, S: ?Sized> {
    src: &'a S,
    opt: &'a Options,
    notes: &'a Notes,
    batches: &'a [Vec<usize>],
    /// The bytes of each batch, as it is let in ([`AHEAD`]).
    weights: &'a [u64],
    /// The contents found as entries or members of others ([`together`]).
    entries: &'a HashSet<usize>,
    /// The entries archived, those of each segment written as it is closed.
    tree: &'a Tree,
    stop: Option<&'a Stop<'a>>,
}

impl<S: ?Sized> Ctx<'_, S> {
    fn stopping(&self) -> bool {
        self.stop.is_some_and(Stop::is_requested)
    }
}

/// The batches being made: read in order (under [`Pipe::reader`]) as there is room for them,
/// each analyzed by the thread making the archive or, as a worker is free, by the helper
/// ([`Pipe::help`]), and written in order by the thread making the archive ([`Writer::run`]).
struct Pipe {
    reader: Mutex<Reader>,
    state: Mutex<State>,
    changed: Condvar,
}

/// The contents read: the first of each hash and length.
struct Reader {
    first: HashMap<(u128, usize), usize>,
}

/// Of each content of a batch read, its bytes and the content before it of the same content.
type Lens = Vec<(u64, Option<usize>)>;

struct State {
    /// The batches let in, and the weight of those not yet written.
    claimed: usize,
    held: u64,
    /// The batches analyzed, by index: their files and analysis, or none where nothing is to
    /// be made of the batch (its files all of contents before it).
    made: Vec<Option<(Lens, Option<Ready>)>>,
    failed: Option<io::Error>,
    /// Whether the archive is written (or failed), for the helper to stop.
    written: bool,
}

impl State {
    /// Whether the next batch to let in has room: those let in and not yet written are none,
    /// or with it of at most [`AHEAD`] batches' bytes, its bytes fitting in the budget of the
    /// process beside the heap in use ([`aqr_common::budget::global`]: what it holds, as its
    /// data is inflated, is about as many bytes again).
    fn room<S: ?Sized>(&self, cx: &Ctx<S>) -> bool {
        let w = cx.weights[self.claimed];
        self.held == 0
            || (self.held.saturating_add(w) <= (cx.opt.batch as u64).saturating_mul(AHEAD)
                && aqr_common::budget::global().room().fits(w))
    }
}

/// How often those waiting for room for a batch look again: the heap tells nobody as it
/// shrinks ([`State::room`]).
const LOOK: std::time::Duration = std::time::Duration::from_millis(10);

/// The files of a batch read and their data (none: all of contents before it).
type Taken = Option<(Vec<File>, Vec<Bytes>)>;

/// A batch let in: its index, its contents' [`Lens`], and what is [`Taken`] of it.
type Claimed = (usize, Lens, Taken);

/// A batch analyzed: its files and their analysis.
type Ready = (Vec<File>, Analyzed);

impl Pipe {
    /// Lets in and reads the next batch, where there is one and room for it.
    fn claim<S: Source + ?Sized>(&self, cx: &Ctx<S>) -> Option<Claimed> {
        // (read in order: the reader held from letting in to reading)
        let mut reader = self.reader.lock().unwrap();
        let i = {
            let mut st = self.state.lock().unwrap();
            let i = st.claimed;
            if st.failed.is_some() || i >= cx.batches.len() || cx.stopping() {
                return None;
            }
            if !st.room(cx) {
                return None;
            }
            (st.claimed, st.held) = (i + 1, st.held.saturating_add(cx.weights[i]));
            i
        };
        match reader.read(i, cx) {
            Ok((lens, taken)) => Some((i, lens, taken)),
            Err(e) => {
                self.state.lock().unwrap().failed.get_or_insert(e);
                self.changed.notify_all();
                None
            }
        }
    }

    /// Analyzes batch `i` of `taken`, for the writer.
    fn analyze<S: Source + ?Sized>(&self, (i, lens, taken): Claimed, cx: &Ctx<S>) {
        let made = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            taken.map(|(files, data)| {
                // (what is cut out of the files mapped shared, not copied: an image's files, say)
                let _known: Vec<_> = data.iter().map(aqr_formula::store::known).collect();
                let a = analyze(i, &files, data, &HashSet::new(), cx.opt, cx.notes);
                (files, a)
            })
        }));
        let mut st = self.state.lock().unwrap();
        match made {
            Ok(made) => st.made[i] = Some((lens, made)),
            // (the writer stopped rather than waiting for the batch)
            Err(p) => {
                st.failed.get_or_insert(io::Error::other(format!("batch {i} not analyzed: a panic")));
                drop(st);
                self.changed.notify_all();
                std::panic::resume_unwind(p);
            }
        }
        drop(st);
        self.changed.notify_all();
    }

    /// What the helper does until the archive is written: lets in and analyzes a batch on a
    /// worker of the pool as one is free, where there is room for one.
    fn help<S: Source + ?Sized>(&self, cx: &Ctx<S>) {
        // (none with a pool of one worker, which makes the archive: waiting for it to be free,
        // the helper would hold up the end of the archive, which waits for the helper)
        if aqr_common::pool::size() < 2 {
            return;
        }
        loop {
            {
                let mut st = self.state.lock().unwrap();
                loop {
                    if st.written || st.failed.is_some() || st.claimed >= cx.batches.len() || cx.stopping() {
                        return;
                    }
                    if st.room(cx) {
                        break;
                    }
                    st = self.changed.wait_timeout(st, LOOK).unwrap().0;
                }
            }
            // (the batch let in only once a worker is free, unless the writer has let it in
            // meanwhile)
            aqr_common::pool::run(|| {
                if let Some(c) = self.claim(cx) {
                    self.analyze(c, cx);
                }
            });
        }
    }
}

impl Reader {
    /// Batch `i` read, its files but those of contents before it.
    fn read<S: Source + ?Sized>(&mut self, i: usize, cx: &Ctx<S>) -> io::Result<(Lens, Taken)> {
        let _s = aqr_common::trace::span("read");
        let (src, batch) = (cx.src, &cx.batches[i]);
        let bytes = batch.iter().map(|&c| src.size(c)).sum();
        cx.notes.send(Note::Batch { batch: i, bytes, verify: cx.opt.verify, contents: batch.clone() });
        let read = aqr_common::pool::par_map(batch.len(), cx.opt.threads, |i| {
            let d = src.read(batch[i])?;
            let hash = xxhash_rust::xxh3::xxh3_128(&d);
            Ok::<_, io::Error>((d, hash))
        });
        let mut files: Vec<File> = Vec::with_capacity(batch.len());
        let mut data = Vec::with_capacity(batch.len());
        let mut lens = Vec::with_capacity(batch.len());
        for (&c, d) in batch.iter().zip(read) {
            let (d, hash) = d?;
            lens.push((d.len() as u64, None));
            match self.first.entry((hash, d.len())) {
                std::collections::hash_map::Entry::Occupied(e) => lens.last_mut().unwrap().1 = Some(*e.get()),
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(c);
                    files.push(File::of(c, &d, hash, kind_by_name(src.name(c)), cx.entries.contains(&c)));
                    data.push(d);
                }
            }
        }
        Ok((lens, (!files.is_empty()).then_some((files, data))))
    }
}

/// The archive written so far, a batch after another in their order, closing a segment after
/// the batch that makes it [`Options::segment`] bytes long.
struct Writer {
    w: SegmentWriter,
    at: Vec<Option<ContentAt>>,
    r: Report,
    /// The batch to write next.
    next: usize,
    /// Whether the last batches are held back, at least [`aqr::mirror::FAR`] bytes of them, for
    /// the metadata block's mirror block to be written before them ([`Options::mirrors`]).
    hold: bool,
    held: std::collections::VecDeque<Vec<u8>>,
    held_len: usize,
    /// The contents of the segment being written (those of the same content as one before it
    /// too), where it starts, and the segments closed.
    segment: Vec<usize>,
    segment_at: usize,
    segments: usize,
    /// Of the contents of segments closed that contents of this one are the same as: the one
    /// of them made again in this one.
    again: HashMap<usize, usize>,
    /// Where the last metadata block written ends: the blocks before it are mirrored before the
    /// next one ([`Options::mirrors`]).
    metadata_end: Option<u64>,
    /// Bytes of the mirror blocks written between batches.
    inserted: usize,
    /// Where the archive starts in the file, where it is committed as it is written, at each
    /// segment closed ([`Output::commits`]).
    commit: Option<u64>,
    /// Whether it is committed once, as it ends ([`Output::commits_once`]).
    once: bool,
    /// By content, the files of it (each hard link).
    files_of: Vec<u64>,
    /// The least archive-wide link group of the archive appended to ([`Appending`]): those of
    /// the segments written taken below it.
    link_groups: i128,
}

/// By content of `tree` (of `n`), the files of it (each hard link).
fn files_of(tree: &Tree, n: usize) -> Vec<u64> {
    let mut of = vec![0; n];
    for e in &tree.entries {
        if let EntryNode::File(c) = e.node {
            of[c] += 1;
        }
    }
    of
}

fn bad_tree(e: aqr_formula::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, e.0)
}

/// Where the archive is written: as it is, or with mirror blocks ([`Options::mirrors`]).
enum Out<'a> {
    Plain(&'a mut dyn Output),
    Mirrored(aqr::Mirroring<&'a mut dyn Output>),
}

impl Out<'_> {
    /// The output written to, past the mirror blocks.
    fn output(&mut self) -> &mut dyn Output {
        match self {
            Out::Plain(o) => &mut **o,
            Out::Mirrored(m) => &mut **m.get_mut(),
        }
    }

    /// Bytes of the mirror blocks written (and of the footer as it grew).
    fn added(&self) -> u64 {
        match self {
            Out::Plain(_) => 0,
            Out::Mirrored(m) => m.added(),
        }
    }
}

impl Write for Out<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Out::Plain(o) => o.write(buf),
            Out::Mirrored(m) => m.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Out::Plain(o) => o.flush(),
            Out::Mirrored(m) => m.flush(),
        }
    }
}

/// The contents of each entry of `tree` (its file's and its forks', and the root's forks) in one
/// group of `group` (each content's first of those that may hold each other, [`together`]),
/// which are in a batch together: an entry is of the segment of its contents.
fn entry_units(tree: &Tree, group: &mut [usize]) {
    let extras = std::iter::once(&tree.root).chain(tree.entries.iter().map(|e| &e.extra));
    let nodes = std::iter::once(None).chain(tree.entries.iter().map(|e| Some(&e.node)));
    let mut joined: Vec<Vec<usize>> = Vec::new();
    for (x, node) in extras.zip(nodes) {
        let file = match node {
            Some(&EntryNode::File(c)) => Some(c),
            _ => None,
        };
        let cs: Vec<usize> = file.into_iter().chain(x.forks.iter().map(|f| f.1)).collect();
        if cs.len() > 1 {
            joined.push(cs);
        }
    }
    if joined.is_empty() {
        return;
    }
    // (the groups joined, each the least of them)
    let mut parent: HashMap<usize, usize> = HashMap::new();
    fn root(p: &HashMap<usize, usize>, mut g: usize) -> usize {
        while let Some(&q) = p.get(&g).filter(|&&q| q != g) {
            g = q;
        }
        g
    }
    for cs in joined {
        let roots: Vec<usize> = cs.iter().map(|&c| root(&parent, group[c])).collect();
        let least = *roots.iter().min().expect("contents");
        roots.iter().for_each(|&r| _ = parent.insert(r, least));
    }
    for g in group.iter_mut() {
        *g = root(&parent, *g);
    }
}

impl Writer {
    /// Writes the batches in order: the next as it is analyzed, else analyzing the next to let
    /// in where there is room for it, else waiting.
    fn run<S: Source + ?Sized>(
        &mut self,
        pipe: &Pipe,
        cx: &Ctx<S>,
        tell: &(dyn Fn(Event) + Sync),
        out: &mut Out,
    ) -> io::Result<()> {
        while self.next < cx.batches.len() {
            if cx.stopping() {
                return Err(self.stopped(cx, out));
            }
            let made = {
                let mut st = pipe.state.lock().unwrap();
                if let Some(e) = st.failed.take() {
                    return Err(e);
                }
                st.made[self.next].take()
            };
            if let Some((lens, made)) = made {
                let r = self.write(self.next, &lens, made, cx, tell, out);
                let mut st = pipe.state.lock().unwrap();
                if let Err(e) = r {
                    // (ended as it is already: passed up, not ended again)
                    if is_ended(&e) {
                        return Err(e);
                    }
                    st.failed.get_or_insert(e);
                    continue;
                }
                st.held = st.held.saturating_sub(cx.weights[self.next]);
                self.next += 1;
                drop(st);
                pipe.changed.notify_all();
                let segment = cx.opt.segment;
                if segment > 0 && self.next < cx.batches.len() && self.w.segment_len() >= segment {
                    self.close(cx, out)?;
                }
                continue;
            }
            // (read and analyzed as a stop is looked for)
            if self.watched(cx, out, || pipe.claim(cx).map(|c| pipe.analyze(c, cx)).is_some())? {
                continue;
            }
            let st = pipe.state.lock().unwrap();
            if st.made[self.next].is_none() && st.failed.is_none() {
                let _w = aqr_common::trace::wait("batch");
                drop(pipe.changed.wait_timeout(st, LOOK).unwrap());
            }
        }
        Ok(())
    }

    /// Encodes, verifies and writes batch `i` of `made` (its files and analysis): those of its
    /// files not given back made again kept as they are.
    fn write<S: Source + ?Sized>(
        &mut self,
        i: usize,
        lens: &Lens,
        made: Option<Ready>,
        cx: &Ctx<S>,
        tell: &(dyn Fn(Event) + Sync),
        out: &mut Out,
    ) -> io::Result<()> {
        let (src, notes, batch) = (cx.src, cx.notes, &cx.batches[i]);
        for (&c, &(len, same_as)) in batch.iter().zip(lens) {
            self.r.read += len;
            (self.r.files[c].len, self.r.files[c].same_as) = (len, same_as);
        }
        self.segment.extend_from_slice(batch);
        if let Some((files, part)) = made {
            self.put(i, &files, part, cx, out)?;
        }
        // those of the same content as one of a segment closed made again, apart (by its first
        // of them, the others of the same content as it)
        let mut again: Vec<usize> = Vec::new();
        for &c in batch {
            let Some(d) = self.r.files[c].same_as else { continue };
            if self.at[d].is_some_and(|a| a.leaf < self.segment_at) {
                match self.again.entry(d) {
                    std::collections::hash_map::Entry::Occupied(e) => self.r.files[c].same_as = Some(*e.get()),
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert(c);
                        self.r.files[c].same_as = None;
                        again.push(c);
                    }
                }
            }
        }
        if !again.is_empty() {
            let data = aqr_common::pool::par_map(again.len(), cx.opt.threads, |k| src.read(again[k]));
            let data: Vec<Bytes> = data.into_iter().collect::<io::Result<_>>()?;
            let files: Vec<File> = (again.iter().zip(&data))
                .map(|(&c, d)| {
                    let hash = xxhash_rust::xxh3::xxh3_128(d);
                    File::of(c, d, hash, kind_by_name(src.name(c)), cx.entries.contains(&c))
                })
                .collect();
            let part = self.watched(cx, out, || analyze(i, &files, data, &HashSet::new(), cx.opt, notes))?;
            self.put(i, &files, part, cx, out)?;
        }
        batch.iter().try_for_each(|&c| src.intact(c))?;
        notes.send(Note::Written(i));
        tell(Event::Written { contents: batch, report: &self.r });
        Ok(())
    }

    /// Encodes, verifies and writes `files` (of batch `i`) of analysis `part`: those not given
    /// back made again kept as they are.
    fn put<S: Source + ?Sized>(
        &mut self,
        i: usize,
        files: &[File],
        mut part: Analyzed,
        cx: &Ctx<S>,
        out: &mut Out,
    ) -> io::Result<()> {
        let (src, opt, notes, batch) = (cx.src, cx.opt, cx.notes, &cx.batches[i]);
        let mut raw = HashSet::new();
        let mut made = loop {
            let w = &self.w;
            let made = self.watched(cx, out, move || finish(i, w, files, part, src, opt, notes))?;
            let bad: Vec<usize> = made.bad.iter().copied().filter(|k| !raw.contains(k)).collect();
            if bad.is_empty() {
                break made;
            }
            // (not given back as it was not read as it is: told so, not made again)
            files.iter().try_for_each(|f| src.intact(f.c))?;
            raw.extend(bad);
            let bytes = batch.iter().map(|&c| src.size(c)).sum();
            notes.send(Note::Batch { batch: i, bytes, verify: opt.verify, contents: batch.clone() });
            let data = reread(src, files, opt.threads)?;
            part = self.watched(cx, out, || analyze(i, files, data, &raw, opt, notes))?;
        };
        files.iter().try_for_each(|f| src.intact(f.c))?;
        self.w.commit(&made.batch);
        {
            let _s = aqr_common::trace::span("write");
            match self.hold {
                false => {
                    out.write_all(self.w.pad())?;
                    out.write_all(&made.batch.bytes)?
                }
                true => {
                    self.held_len += made.batch.bytes.len();
                    self.held.push_back(std::mem::take(&mut made.batch.bytes));
                    while let Some(b) = self.held.front()
                        && (self.held_len - b.len()) as u64 >= aqr::mirror::FAR
                    {
                        out.write_all(b)?;
                        self.held_len -= b.len();
                        self.held.pop_front();
                    }
                }
            }
        }
        let r = &mut self.r;
        for (k, file) in files.iter().enumerate() {
            let (l, o) = made.at[k];
            self.at[file.c] = Some(ContentAt { leaf: made.batch.leaf_at[l], output: o, size: file.len as u64 });
            let f = &made.files[k];
            let c = file.c;
            (r.files[c].format, r.files[c].modules, r.files[c].archived) = (f.format, f.modules.clone(), f.archived);
        }
        let global = |ks: &[usize]| ks.iter().map(|&k| files[k].c).collect::<Vec<_>>();
        for s in made.solid {
            r.solid.push(Solid { contents: global(&s.contents), ..s });
        }
        r.sets.extend(made.sets.iter().map(|s| global(s)));
        r.unverified.extend(raw.iter().map(|&k| files[k].c));
        Ok(())
    }

    /// Closes the segment: the batches held back written, then the mirror blocks of the
    /// segments closed before (as far from their blocks as this segment is long), then its
    /// metadata block, of its entries ([`Writer::segment_tree`]).
    fn close<S: Source + ?Sized>(&mut self, cx: &Ctx<S>, out: &mut Out) -> io::Result<()> {
        let _s = aqr_common::trace::span("write");
        for b in std::mem::take(&mut self.held) {
            out.write_all(&b)?;
        }
        self.held_len = 0;
        if let (Some(end), Out::Mirrored(m)) = (self.metadata_end, &mut *out) {
            let n = m.mirror_before(end)? as usize;
            self.w.skip(n);
            self.inserted += n;
        }
        let (t, contents) = self.segment_tree(cx.tree);
        let enc = aqr::Encoder::with_threads(cx.opt.level, cx.opt.threads).with_modules(aqr_decompress::wasm_of);
        let commits = self.commit.is_some() && !self.once;
        let status = if commits { self.left(cx) } else { aqr::Status::None };
        let meta = self.w.close_as(&enc, &t, &contents, status).map_err(bad_tree)?;
        if commits {
            self.commit(out, &meta)?;
        } else {
            out.write_all(self.w.pad())?;
            out.write_all(&meta)?;
        }
        self.metadata_end = Some(self.w.len() as u64);
        (self.segment_at, self.segments) = (self.w.len(), self.segments + 1);
        self.segment.clear();
        self.again.clear();
        Ok(())
    }

    /// What is left to write, as a commit tells it (`segment-status`): the files and bytes of the
    /// contents not written.
    fn left<S: Source + ?Sized>(&self, cx: &Ctx<S>) -> aqr::Status {
        let at = |c: usize| self.at[c].or_else(|| self.at[self.r.files[c].same_as?]);
        let (mut files, mut bytes) = (0, 0);
        for c in (0..self.at.len()).filter(|&c| at(c).is_none()) {
            files += self.files_of[c];
            bytes += cx.src.size(c);
        }
        aqr::Status::Incomplete { files: Some(files), bytes: Some(bytes) }
    }

    /// `work` done on a thread of its own while this one looks for a stop ([`Ctx::stop`]): where
    /// one is requested, the archive is ended as it is ([`Writer::stopped`]) without waiting for
    /// `work` (but where [`Stop::ended`] returns).
    fn watched<S: Source + ?Sized, T: Send>(
        &self,
        cx: &Ctx<S>,
        out: &mut Out,
        work: impl FnOnce() -> T + Send,
    ) -> io::Result<T> {
        if cx.stop.is_none() {
            return Ok(work());
        }
        std::thread::scope(|s| {
            // (this one woken as it is done, not left to look again)
            let me = std::thread::current();
            let h = s.spawn(move || {
                let t = work();
                me.unpark();
                t
            });
            while !h.is_finished() {
                if cx.stopping() {
                    return Err(self.stopped(cx, out));
                }
                std::thread::park_timeout(LOOK);
            }
            Ok(h.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
        })
    }

    /// Ends the archive as it is, of the batches written, as a stop requests ([`Stop`]), and
    /// tells [`Stop::ended`] of it: the error to pass up ([`Ended`]), or that of the ending.
    fn stopped<S: Source + ?Sized>(&self, cx: &Ctx<S>, out: &mut Out) -> io::Error {
        let _s = aqr_common::trace::span("write");
        match self.end_stopped(cx, out) {
            Ok(r) => {
                (cx.stop.expect("a stop").ended)(&r);
                io::Error::new(io::ErrorKind::Interrupted, Ended(r))
            }
            Err(e) => e,
        }
    }

    fn end_stopped<S: Source + ?Sized>(&self, cx: &Ctx<S>, out: &mut Out) -> io::Result<Report> {
        let (t, contents) = self.segment_tree(cx.tree);
        let status = self.left(cx);
        let aqr::Status::Incomplete { files: Some(files), bytes: Some(bytes) } = status else { unreachable!() };
        let mut r = Report { stopped: Some(Stopped { files, bytes, kept: false }), ..self.r.clone() };
        // (no file of the segment: the commit before stands, where there is one; where there is
        // none, nothing to keep, the directories alone not; nor where segments closed are not
        // committed, their footer made by Mirroring or a size hint block to be filled in)
        if contents.is_empty() {
            if self.commit.is_some() && self.segments > 0 {
                r.stopped.as_mut().unwrap().kept = true;
                r.len = self.w.len() - self.w.pad_due() as usize;
            }
            return Ok(r);
        }
        // (on a thread of no worker: the workers busy with the batch let go)
        let enc = aqr::Encoder::with_threads(cx.opt.level, 1).with_modules(aqr_decompress::wasm_of);
        if let Some(base) = self.commit {
            let (end, before) = self.w.end(&enc, &t, &contents, status, base).map_err(bad_tree)?;
            if self.w.pad_due() {
                out.write_all(&[0])?;
            }
            out.write_all(&end)?;
            let o = out.output();
            o.sync()?;
            if let Some(f) = before {
                let k = aqr::kill(f.len());
                o.patch(f.start as u64, &k)?;
                o.sync()?;
                o.patch((f.start + k.len()) as u64, &vec![0; f.len() - k.len()])?;
            }
            o.end()?;
            r.len = self.w.len() + end.len();
        } else {
            let front = self.w.len() - self.held_len;
            let mirrored = match cx.opt.mirrors {
                true => {
                    self.w.finish_mirrored(&enc, &t, &contents, front, cx.opt.size_hint, status).map_err(bad_tree)?
                }
                false => None,
            };
            let (mirror, end) = match mirrored {
                Some(m) => m,
                None => (Vec::new(), self.w.finish_as(&enc, &t, &contents, status).map_err(bad_tree)?.0),
            };
            out.write_all(&mirror)?;
            for b in &self.held {
                out.write_all(b)?;
            }
            out.write_all(&end)?;
            out.output().end()?;
            r.mirrors = mirror.len() + out.added() as usize;
            r.len = self.w.len() + end.len() + r.mirrors - self.inserted;
        }
        r.stopped.as_mut().unwrap().kept = true;
        r.overhead = r.len.saturating_sub(r.files.iter().map(|f| f.archived).sum::<f64>().round() as usize);
        r.unverified.sort_unstable();
        Ok(r)
    }

    /// Commits the segment closed with metadata block `meta` (spec, "Appending to an archive"):
    /// writes it and a footer after it, and once they are durable, makes the footer of the commit
    /// before a spacer block.
    fn commit(&mut self, out: &mut Out, meta: &[u8]) -> io::Result<()> {
        let base = self.commit.expect("an archive committed");
        out.write_all(self.w.pad())?;
        out.write_all(meta)?;
        let (footer, before) = self.w.footer(base);
        out.write_all(&footer)?;
        let o = out.output();
        o.sync()?;
        if let Some(f) = before {
            let k = aqr::kill(f.len());
            o.patch(f.start as u64, &k)?;
            o.sync()?;
            o.patch((f.start + k.len()) as u64, &vec![0; f.len() - k.len()])?;
        }
        Ok(())
    }

    /// The entries of the segment being written, of `tree`, and its contents (by
    /// [`EntryNode::File`]): those whose contents (file and forks) are of it, those of none in
    /// the first segment; and the directories they are in, again (their extra where they are of
    /// the segment, else none, which keeps theirs before). The root's extra likewise. Link
    /// groups are taken as archive-wide (negative), as the archive is of several segments.
    fn segment_tree(&self, tree: &Tree) -> (Tree, Vec<ContentAt>) {
        let at = |c: usize| self.at[c].or_else(|| self.at[self.r.files[c].same_as?]);
        // (those written: of a batch being made, none where the archive is stopped)
        let mine: HashSet<usize> = self.segment.iter().copied().filter(|&c| at(c).is_some()).collect();
        let first = self.segments == 0;
        // whether an entry (or the root, of an extra alone) is of the segment
        let of = |node: Option<&EntryNode>, x: &Extra| {
            let file = match node {
                Some(&EntryNode::File(c)) => Some(c),
                _ => None,
            };
            let mut cs = file.into_iter().chain(x.forks.iter().map(|f| f.1)).peekable();
            match cs.peek() {
                None => first,
                Some(_) => cs.all(|c| mine.contains(&c)),
            }
        };
        let n = tree.entries.len();
        let own: Vec<bool> = tree.entries.iter().map(|e| of(Some(&e.node), &e.extra)).collect();
        let mut taken = own.clone();
        for k in (0..n).rev() {
            if taken[k]
                && let Some(p) = tree.entries[k].parent
            {
                taken[p] = true;
            }
        }
        let mut contents: Vec<ContentAt> = Vec::new();
        let mut index: HashMap<usize, usize> = HashMap::new();
        let mut content = |c: usize| {
            *index.entry(c).or_insert_with(|| {
                contents.push(at(c).expect("a content of the segment made"));
                contents.len() - 1
            })
        };
        let below = self.link_groups;
        let extra = |x: &Extra, own: bool, content: &mut dyn FnMut(usize) -> usize| -> Extra {
            if !own {
                return Extra::default();
            }
            let mut x = x.clone();
            x.forks.iter_mut().for_each(|f| f.1 = content(f.1));
            x.link_group = x.link_group.map(|g| if g >= 0 { below - (g + 1) } else { g });
            x
        };
        let root = extra(&tree.root, of(None, &tree.root), &mut content);
        let mut new = vec![None; n];
        let mut entries = Vec::new();
        for (k, e) in tree.entries.iter().enumerate().filter(|&(k, _)| taken[k]) {
            let node = match e.node {
                EntryNode::File(c) => EntryNode::File(content(c)),
                ref node => node.clone(),
            };
            new[k] = Some(entries.len());
            let parent = e.parent.map(|p| new[p].expect("a directory taken"));
            entries.push(Entry { parent, name: e.name.clone(), node, extra: extra(&e.extra, own[k], &mut content) });
        }
        (Tree { root, entries }, contents)
    }
}

/// Contents of at least this many bytes are looked into for others they may hold
/// ([`together`]), as [`Held`] is.
const TOGETHER_MIN: u64 = Held::MIN as u64;

/// Samples of FSB5 banks of at least this many bytes tell banks that hold each other: a bank and
/// its patches share many samples of tens of KiB (of The Crew's, 44 of 46 MB of Vorbis samples
/// repeated, against 27 MB of samples of 256 KiB or more).
const FSB_SAMPLE_MIN: u64 = 16 << 10;

/// Members of ar, tar and cpio archives of at least this many bytes are looked for among the
/// contents ([`together`]): the files of a tar archive are mostly small.
const MEMBER_MIN: u64 = 4 << 10;

/// Slices shorter than this are not shared with others ([`Formula::share_slices`]): what they
/// share is found by compressing them solid, or not at all where they are apart (a tar
/// archive's members of a kind, and the same files of other kinds, apart from it).
pub(crate) const SLICE_MIN: usize = 1 << 10;

/// Pieces of the starts of more distinct contents than this tell nothing of which are alike
/// ([`together`]).
const CHUNK_COMMON: usize = 8;

/// Contents of at least this many bytes of the same bytes as others are put in one batch
/// ([`together`]): one smaller costs little made again in a segment of its own.
const SAME_MIN: u64 = 64 << 10;

/// The most bytes of contents put in one batch for holding each other ([`together`]).
const TOGETHER_MAX: u64 = 2 << 30;

/// For each content of `src`, the first of those that may hold each other (where the archive
/// stores what they share once, [`Formula::share_slices`], only within a batch): a ZIP archive,
/// 7z archive or gzip file and the contents of its entries (of the CRC-32 and size that its
/// central directory, header or trailer tells; and other archives with such entries), a git loose object
/// and the file of its data (inflated whole where its header tells a large one), an ar, tar or cpio
/// archive or ISO 9660 or UDF image and the contents of its members (read whole, one at a time,
/// for their CRC-32; those of at least [`MEMBER_MIN`] bytes), a git pack and its indexes (by name and the pack's
/// trailer, of any size), a `.lzma` or `.xz` file and those of what its streams hold
/// (decoded whole, [`stream_sums`]), an NSIS installer and those of the files its data stores
/// (read whole, the tarballs decoded, [`crate::nsis::sums`]), FSB5 banks and those of their samples (read whole, by the samples'
/// CRC-32; banks sharing samples so), and contents that start alike (their first 64 KiB, or
/// two or more of its pieces, [`probe::chunks`]; the same file, longer or shorter, or edited),
/// and contents of the same bytes (of at least [`SAME_MIN`] bytes, of a size others have, read
/// whole), for a segment not to make again one of another's.
/// Of the contents of at least [`TOGETHER_MIN`] bytes, which are read at their start (and the
/// archives at their end, the contents of the sizes of entries whole); together, no more than
/// [`TOGETHER_MAX`] bytes. Returns them, and the contents found
/// as entries or members of others, and the PE and ELF files.
fn together<S: Source + ?Sized>(
    src: &S,
    opt: &Options,
    notes: &Notes,
) -> io::Result<(Vec<usize>, HashSet<usize>, Vec<usize>)> {
    let _s = aqr_common::trace::span("probe");
    const HEAD: u64 = 64 << 10;
    // (the end of a ZIP archive holding its central directory, mostly)
    const TAIL: u64 = 1 << 20;
    let n = src.len();
    let large: Vec<usize> = (0..n).filter(|&c| src.size(c) >= TOGETHER_MIN).collect();
    notes.send(Note::More(large.len() as u64));
    // of each: the hash of its start and those of its start's pieces, the CRC-32 and size of
    // the entries it holds, and whether it only bundles files (whose members are known once it
    // is read whole) or is a PE file
    type Looked = (u64, Vec<u64>, Vec<(u32, u64)>, bool, bool);
    let looked = aqr_common::pool::par_map(large.len(), opt.threads, |i| -> io::Result<Looked> {
        let (c, size) = (large[i], src.size(large[i]));
        let head = src.read_range(c, 0..HEAD)?;
        // (what is not taken at this level holds nothing)
        let format = detect(&head).filter(|&f| crate::format_on(f, opt));
        let held = match format {
            Some(detect::Format::Zip) => aqr_zip::zip::entry_sums(&src.read_range(c, size.saturating_sub(TAIL)..size)?),
            // (its header and the packed stream of an encoded one: at its end, mostly)
            Some(detect::Format::SevenZip) => aqr_7z::formula::entry_sums(&mut |r| {
                src.read_range(c, r.clone()).ok().filter(|d| d.len() as u64 == r.end - r.start)
            }),
            Some(detect::Format::Gzip) => {
                let t = src.read_range(c, size - 8..size)?;
                let word = |p: usize| u32::from_le_bytes(t[p..p + 4].try_into().expect("4 bytes"));
                vec![(word(0), word(4) as u64)]
            }
            // a git loose object (inflated whole where its header tells a large object)
            Some(detect::Format::Zlib) if size <= TOGETHER_MAX => {
                crate::flate::git_loose_sum(&head, || src.read(c).ok(), TOGETHER_MIN).into_iter().collect()
            }
            _ => Vec::new(),
        };
        let mut held: Vec<(u32, u64)> = held.into_iter().filter(|e| e.1 >= TOGETHER_MIN).collect();
        if format == Some(detect::Format::Lzma) && size <= TOGETHER_MAX {
            held.extend(stream_sums(aqr_lzma::formula::data(&src.read(c)?)));
        }
        if format == Some(detect::Format::Xz) && size <= TOGETHER_MAX {
            held.extend(stream_sums(aqr_lzma::formula::data_xz(&src.read(c)?)));
        }
        // (an NSIS installer's stored files: tarballs of the files installed, mostly)
        if format == Some(detect::Format::Pe) && size <= TOGETHER_MAX {
            let at = crate::extent::pe_overlay(&head).filter(|&at| at < size);
            let first = at.map(|at| src.read_range(c, at..at + crate::nsis::FIRST_HEADER as u64)).transpose()?;
            if first.is_some_and(|h| crate::nsis::data_len(&h).is_some()) {
                let d = src.read(c)?;
                let blocks = crate::nsis::in_pe(&d).unwrap_or_default();
                held.extend(crate::nsis::sums(&d, &blocks, 1));
            }
        }
        // (banks sharing samples: a bank and its patches)
        if format == Some(detect::Format::Fsb5) && size <= TOGETHER_MAX {
            held.extend(aqr_fsb::fsb5::sample_sums(&src.read(c)?, FSB_SAMPLE_MIN));
        }
        let bundle = format.is_some_and(detect::Format::is_bundle);
        notes.send(Note::Looked);
        let (hash, chunks) = (xxhash_rust::xxh3::xxh3_64(&head), probe::chunks(&head));
        Ok((hash, chunks, held, bundle, matches!(format, Some(detect::Format::Pe | detect::Format::Elf))))
    });
    let mut parent: Vec<usize> = (0..n).collect();
    let mut bytes: Vec<u64> = (0..n).map(|c| src.size(c)).collect();
    fn root(p: &mut [usize], mut c: usize) -> usize {
        while p[c] != c {
            (p[c], c) = (p[p[c]], p[c]);
        }
        c
    }
    let mut join = |a: usize, b: usize| {
        let (a, b) = (root(&mut parent, a), root(&mut parent, b));
        if a != b && bytes[a].saturating_add(bytes[b]) <= TOGETHER_MAX {
            let (a, b) = (a.min(b), a.max(b));
            parent[b] = a;
            bytes[a] += bytes[b];
        }
    };
    // by what they start with (but for many alike, which are not the same file), and by the
    // entries of the archives (the contents of their sizes told by their CRC-32, of the sizes
    // of few)
    let mut heads: HashMap<u64, Vec<usize>> = HashMap::new();
    let mut by_chunk: HashMap<u64, Vec<usize>> = HashMap::new();
    // (what tells the same file: its size and start)
    let mut same: HashMap<usize, (u64, u64)> = HashMap::new();
    let mut sizes: HashMap<u64, Vec<usize>> = HashMap::new();
    let mut by_entry: HashMap<(u32, u64), Vec<usize>> = HashMap::new();
    let mut exes = Vec::new();
    for (&c, l) in large.iter().zip(looked) {
        let (head, chunks, mut entries, bundle, is_exe) = l?;
        if is_exe {
            exes.push(c);
        }
        // (one past what a batch may hold is not put with others)
        if bundle && src.size(c) <= TOGETHER_MAX {
            entries = member_sums(&src.read(c)?, opt.threads);
        }
        heads.entry(head).or_default().push(c);
        chunks.into_iter().for_each(|k| by_chunk.entry(k).or_default().push(c));
        same.insert(c, (src.size(c), head));
        entries.into_iter().for_each(|e| by_entry.entry(e).or_default().push(c));
    }
    // (of those that may be a member)
    for c in (0..n).filter(|&c| src.size(c) >= MEMBER_MIN) {
        sizes.entry(src.size(c)).or_default().push(c);
    }
    for cs in heads.values().filter(|cs| (2..=8).contains(&cs.len())) {
        cs.iter().for_each(|&c| join(cs[0], c));
    }
    // and by pieces of their starts, two or more in common ([`probe::chunks`]: the same start
    // but for some bytes, or shifted), but those that many files have (a common header, the
    // same stub), which tell little
    let mut common: HashMap<(usize, usize), u32> = HashMap::new();
    for cs in by_chunk.values().filter(|cs| (2..=CHUNK_COMMON * 8).contains(&cs.len())) {
        if cs.iter().map(|c| same[c]).collect::<HashSet<_>>().len() > CHUNK_COMMON {
            continue;
        }
        for (i, &a) in cs.iter().enumerate() {
            cs[i + 1..].iter().for_each(|&b| *common.entry((a.min(b), a.max(b))).or_default() += 1);
        }
    }
    // (in an order of their own, as joins stop at what a batch may hold)
    let mut alike: Vec<(usize, usize)> = common.into_iter().filter(|e| e.1 >= 2).map(|e| e.0).collect();
    alike.sort_unstable();
    alike.into_iter().for_each(|(a, b)| join(a, b));
    // a git pack and its index and reverse index, by the pack's trailer that theirs repeat
    // ([`crate::git::sets`])
    let git: Vec<(usize, u64)> = (0..n)
        .filter_map(|c| {
            let name = src.name(c);
            let back = if name.ends_with(b".pack") { 20 } else { 40 };
            let git = [&b".pack"[..], b".idx", b".rev"].iter().any(|e| name.ends_with(e));
            (git && src.size(c) >= back).then(|| (c, src.size(c) - back))
        })
        .collect();
    if git.len() > 1 {
        let trailers = aqr_common::pool::par_map(git.len(), opt.threads, |i| {
            let (c, at) = git[i];
            src.read_range(c, at..at + 20)
        });
        let mut by_trailer: HashMap<Vec<u8>, usize> = HashMap::new();
        for (&(c, _), t) in git.iter().zip(trailers) {
            let first = *by_trailer.entry(t?).or_insert(c);
            join(first, c);
        }
    }
    let wanted: Vec<usize> = {
        let mut w: Vec<usize> =
            by_entry.keys().filter_map(|e| sizes.get(&e.1).filter(|cs| cs.len() <= 8)).flatten().copied().collect();
        w.sort_unstable();
        w.dedup();
        w
    };
    notes.send(Note::More(wanted.len() as u64));
    let sums = aqr_common::pool::par_map(wanted.len(), opt.threads, |i| {
        let sum = src.read(wanted[i]).map(|d| aqr_common::crc32::hash(&d));
        notes.send(Note::Looked);
        sum
    });
    let mut by_sum: HashMap<(u32, u64), Vec<usize>> = HashMap::new();
    for (&c, sum) in wanted.iter().zip(sums) {
        by_sum.entry((sum?, src.size(c))).or_default().push(c);
    }
    let mut entries = HashSet::new();
    for (e, archives) in &by_entry {
        let alike = by_sum.get(e).map_or(&[][..], |cs| &cs[..]);
        archives.iter().chain(alike).for_each(|&c| join(archives[0], c));
        entries.extend(alike);
    }
    // contents of the same bytes, for those after the first to be stored as the same as it rather
    // than made again in a segment of their own ([`Writer::write`]): of a size (at least
    // [`SAME_MIN`] bytes) that others have, hashed whole; but those of a start that 2 to 8 have,
    // joined by it already
    let mut by_size: HashMap<u64, Vec<usize>> = HashMap::new();
    for c in (0..n).filter(|&c| src.size(c) >= SAME_MIN) {
        if same.get(&c).is_some_and(|s| (2..=8).contains(&heads[&s.1].len())) {
            continue;
        }
        by_size.entry(src.size(c)).or_default().push(c);
    }
    let mut wanted: Vec<usize> = by_size.into_values().filter(|cs| cs.len() > 1).flatten().collect();
    wanted.sort_unstable();
    notes.send(Note::More(wanted.len() as u64));
    let hashes = aqr_common::pool::par_map(wanted.len(), opt.threads, |i| {
        let hash = src.read(wanted[i]).map(|d| xxhash_rust::xxh3::xxh3_128(&d));
        notes.send(Note::Looked);
        hash
    });
    let mut by_hash: HashMap<(u128, u64), usize> = HashMap::new();
    for (&c, hash) in wanted.iter().zip(hashes) {
        let first = *by_hash.entry((hash?, src.size(c))).or_insert(c);
        join(first, c);
    }
    Ok(((0..n).map(|c| root(&mut parent, c)).collect(), entries, exes))
}

/// Contents `cs` alike laid next to each other in `order`, each chain of them
/// ([`probe::alike_chains`]) where its first is, the rest where they are; but those put
/// together with others (`group`, [`together`]): PE files, of which those of a program in
/// several versions, apart by their names, compress far better within a block of each other
/// (in 10gb, 436.7 → 395.6 MB in blocks of 16 MiB at zstd level 9, of 371.3 MB as one), and
/// ELF files alike. Of other files, those alike are mostly next to each other by their names
/// already.
fn lay_alike<S: Source + ?Sized>(
    order: &mut [usize],
    cs: &[usize],
    group: &[usize],
    src: &S,
    opt: &Options,
    notes: &Notes,
) -> io::Result<()> {
    let mut members = vec![0usize; group.len()];
    group.iter().for_each(|&g| members[g] += 1);
    let alone: HashSet<usize> = cs.iter().copied().filter(|&c| members[group[c]] == 1).collect();
    let these: Vec<usize> = order.iter().copied().filter(|c| alone.contains(c)).collect();
    if these.len() < 2 {
        return Ok(());
    }
    notes.send(Note::More(these.len() as u64));
    let likes = aqr_common::pool::par_map(these.len(), opt.threads, |i| {
        let like = src.read(these[i]).map(|d| probe::likeness(&d));
        notes.send(Note::Looked);
        like
    });
    let likes = likes.into_iter().collect::<io::Result<Vec<_>>>()?;
    // each chain where its first is, the rest where they are
    let mut key: HashMap<usize, (usize, usize)> = order.iter().enumerate().map(|(i, &c)| (c, (i, 0))).collect();
    for chain in probe::alike_chains(&likes) {
        let at = key[&these[chain[0]]].0;
        chain.iter().enumerate().skip(1).for_each(|(k, &i)| _ = key.insert(these[i], (at, k)));
    }
    order.sort_by_key(|c| key[c]);
    Ok(())
}

/// What the data of a `.lzma` or `.xz` file holds (None where it does not decode), as
/// [`together`] takes it: the CRC-32 and size of the members of an ar, tar or cpio archive
/// ([`member_sums`]), or else of all of it.
pub(crate) fn stream_sums(data: Option<Vec<u8>>) -> Vec<(u32, u64)> {
    match data {
        Some(data) if detect(&data).is_some_and(detect::Format::is_bundle) => member_sums(&data, 1),
        Some(data) if data.len() as u64 >= MEMBER_MIN => vec![(aqr_common::crc32::hash(&data), data.len() as u64)],
        _ => Vec::new(),
    }
}

/// The CRC-32 and size of the files' contents in ar, tar, cpio or asar archive or ISO 9660 or
/// UDF image `d`, those of at least [`MEMBER_MIN`] bytes.
fn member_sums(d: &[u8], threads: usize) -> Vec<(u32, u64)> {
    use aqr_bundle::{formula, layout::Role};
    let spans: Vec<std::ops::Range<usize>> = if aqr_iso::starts(d) {
        aqr_iso::pieces(d, &aqr_iso::files(d)).into_iter().filter(|p| p.file.is_some()).map(|p| p.span).collect()
    } else if aqr_asar::starts(d) {
        aqr_asar::formula::files(d).into_iter().map(|f| f.1).collect()
    } else {
        let recs = formula::detect(d).and_then(|f| formula::records(f, d)).unwrap_or_default();
        recs.iter().filter_map(|r| r.body()).filter(|b| b.role == Role::File).map(|b| b.span.clone()).collect()
    };
    let bodies: Vec<&[u8]> = spans.into_iter().filter(|s| s.len() as u64 >= MEMBER_MIN).map(|s| &d[s]).collect();
    aqr_common::pool::par_map(bodies.len(), threads, |i| (aqr_common::crc32::hash(bodies[i]), bodies[i].len() as u64))
}

/// What files are ordered by, to put those alike next to each other: the extension, then the
/// name, of the last component.
fn sort_key(name: &[u8]) -> (Vec<u8>, &[u8]) {
    let base = name.rsplit(|&b| b == b'/').next().unwrap_or(name);
    let ext = base.iter().rposition(|&b| b == b'.').filter(|&p| p > 0).map_or(&[][..], |p| &base[p + 1..]);
    (ext.to_ascii_lowercase(), base)
}

/// A file of a batch: its content, the kind its name tells, and what is known of its data,
/// which is held only by the formula of the batch ([`make`]).
struct File {
    c: usize,
    kind: Kind,
    /// Whether it is found as an entry or member of another ([`together`]).
    entry: bool,
    len: usize,
    /// Its XXH3-128 ([`check`]) and its checksum ([`aqr::checksum`]).
    hash: u128,
    sum: u64,
    /// The format it starts like ([`detect`]).
    format: Option<&'static str>,
}

impl File {
    fn of(c: usize, d: &[u8], hash: u128, kind: Kind, entry: bool) -> File {
        File { c, kind, entry, len: d.len(), hash, sum: aqr::checksum(d), format: detect(d).map(|f| f.name()) }
    }

    /// Whether `d` is the file's data.
    fn is(&self, d: &[u8]) -> bool {
        d.len() == self.len && xxhash_rust::xxh3::xxh3_128(d) == self.hash
    }
}

/// The data of `files` read again from `src`, failing where it is no longer what it was.
fn reread<S: Source + ?Sized>(src: &S, files: &[File], threads: usize) -> io::Result<Vec<Bytes>> {
    aqr_common::pool::par_map(files.len(), threads, |k| {
        let d = src.read(files[k].c)?;
        match files[k].is(&d) {
            true => Ok(d),
            false => Err(io::Error::other(format!("content {} changed while archived", files[k].c))),
        }
    })
    .into_iter()
    .collect()
}

/// A batch made: its blocks, each file's leaf and output (by its index in the batch), what
/// became of the files (by the same), and those not given back.
struct Made {
    batch: Batch,
    at: Vec<(usize, usize)>,
    files: Vec<FileReport>,
    solid: Vec<Solid>,
    sets: Vec<Vec<usize>>,
    bad: Vec<usize>,
}

/// Text of the files of a batch shorter than this in all is coded with dictionaries of each
/// text's own: one of them all would cost more than it saves.
const WORDS_MIN: usize = 256 << 10;

/// The dictionary of the text of a batch ([`Cx::words`]): of the files that are text (at least
/// [`aqr_wrt::formula::MIN_LEN`] bytes, but those kept raw and OBJ files, which aqr-obj takes),
/// where they are [`WORDS_MIN`] bytes in all and are recompressed at all ([`Options::level`]).
fn words(regions: &[&[u8]], files: &[File], raw: &HashSet<usize>, opt: &Options) -> Option<aqr_wrt::formula::Shared> {
    if !opt.recompresses() {
        return None;
    }
    let texts: Vec<&[u8]> = (0..files.len())
        .filter(|k| !raw.contains(k) && aqr_wrt::formula::starts(regions[*k]) && !aqr_obj::formula::starts(regions[*k]))
        .map(|k| regions[k])
        .collect();
    if texts.iter().map(|t| t.len()).sum::<usize>() < WORDS_MIN {
        return None;
    }
    aqr_wrt::formula::Shared::of(&texts, opt.threads)
}

/// A batch analyzed, to be encoded ([`finish`]): its formula, a leaf for each file or set of
/// them, each file's leaf and output, its encoder, and the outputs of applications evaluated
/// beforehand.
struct Analyzed {
    f: Formula,
    leaves: Vec<Leaf>,
    at: Vec<(usize, usize)>,
    enc: aqr::Encoder,
    memo: verify::Memo,
}

/// The formula of batch `i` of `files` (by index, the data of each in `data`), recompressed but
/// those (by index) in `raw`. The data is the formula's: where it is needed again, to verify
/// the batch, it is read from the source ([`finish`]).
fn analyze(i: usize, files: &[File], data: Vec<Bytes>, raw: &HashSet<usize>, opt: &Options, notes: &Notes) -> Analyzed {
    let _s = aqr_common::trace::span("analyze");
    // a formula of the files as the contents of its streams, each a file of its own
    let mut f = Formula::default();
    let mut slices = Vec::with_capacity(files.len());
    for (k, (file, d)) in files.iter().zip(data).enumerate() {
        slices.push(if raw.contains(&k) {
            f.push_as("raw", file.kind, d)
        } else {
            f.push_content("raw", file.kind, d)
        });
    }
    f.root = Node::Concat(slices);

    // nested a format deeper, so that the files are as deep as a file alone is
    let nopt = Options { depth: opt.depth.saturating_add(1), ..opt.clone() };
    let total = files.iter().map(|f| f.len).sum();
    let mut cx = Cx::of_len(total, &nopt);
    // (the archive has a module block of each module once)
    cx.enc.share_modules();
    cx.top_sets = Some(opt.set_bytes);
    // (the files known by where their copies are, and taken from the input; and what they
    // hold, a content told by where it is)
    if crate::inflated::on(opt) {
        f.streams.iter_mut().for_each(|s| s.data.share());
        let files: Vec<Bytes> = f.streams.iter().map(|s| s.data.clone()).collect();
        cx.bodies = Some(std::sync::Arc::new(crate::inflated::Bodies::of(&files, raw, opt.threads)));
    }
    let regions: Vec<&[u8]> = f.streams.iter().map(|s| &s.data[..]).collect();
    // (nothing to tell where nothing is recompressed)
    cx.held = opt.recompresses().then(|| std::sync::Arc::new(Held::of(&regions, opt.threads)));
    cx.share_words(words(&regions, files, raw, opt).map(std::sync::Arc::new));
    // (the text of a file found in another coded once, for both)
    cx.once = std::sync::Arc::new(Once::of(
        files.iter().enumerate().filter(|(k, f)| f.entry && !raw.contains(k)).map(|(_, f)| (f.len, f.hash)),
    ));
    cx.pre = opt.verify.then(|| std::sync::Arc::new(verify::Prevalidator::spawn(opt.threads, &regions)));
    let bases: Vec<usize> = f.streams.iter().map(|s| s.data.as_ptr() as usize).collect();
    let contents = files.iter().zip(&f.streams).map(|(file, s)| (file.c, &s.data[..]));
    cx.files = Some(std::sync::Arc::new(Files::of(notes, contents)));
    let mut taken = HashSet::new();
    if let Some((mut children, plan)) = nested(&mut f, &[], &nopt, 0, &cx) {
        let sets = sets::sets(&f, &mut children, &plan, &nopt, 0, &cx);
        for s in &sets {
            for &m in &s.members {
                if let &Node::Slice { stream, start, .. } = &children[m].0 {
                    taken.insert(bases[stream] + start as usize);
                }
            }
        }
        place_all(&mut f, children.into_iter().map(|c| c.1).collect(), sets, &nopt);
    }
    // (what the files hold of each other, by what they are taken for: a file and the same in
    // an archive, both kept as they are; files none of which is recompressed too)
    f.share_slices(SLICE_MIN);
    // (a text formula kept that none took)
    cx.once = Default::default();
    let deferred: Vec<_> = cx.deferred.borrow_mut().drain(..).collect();
    if let Some(pre) = &cx.pre {
        deferred.into_iter().filter(|(at, _)| !taken.contains(at)).for_each(|(_, j)| pre.send(j));
    }
    let memo = cx.pre.take().map(verify::Prevalidator::finish_shared).unwrap_or_default();
    cx.files = None;
    crate::memory::declare(&mut f, &Native { threads: opt.threads }, &memo.1);
    notes.send(Note::Analyzed(i));

    // a leaf for each file, or for each set of them
    let Node::Concat(nodes) = std::mem::replace(&mut f.root, Node::Concat(Vec::new())) else { unreachable!() };
    let mut leaves: Vec<Leaf> = Vec::new();
    let mut at = vec![(0, 0); files.len()];
    let mut set_leaf: HashMap<usize, usize> = HashMap::new();
    for (k, node) in nodes.into_iter().enumerate() {
        let sum = Some(files[k].sum);
        match node {
            Node::Output { shared, index, .. } => {
                let n = f.shared[shared].sizes.len();
                let l = *set_leaf.entry(shared).or_insert_with(|| {
                    leaves.push(Leaf { outputs: vec![Node::Concat(Vec::new()); n], sums: vec![None; n] });
                    leaves.len() - 1
                });
                (leaves[l].outputs[index], leaves[l].sums[index]) = (node, sum);
                at[k] = (l, index);
            }
            node => {
                leaves.push(Leaf { outputs: vec![node], sums: vec![sum] });
                at[k] = (leaves.len() - 1, 0);
            }
        }
    }

    // the streams of a class together, within a block each; but those that do not compress,
    // which would gain nothing and only make a file read alone read more
    for s in &mut f.streams {
        s.content = false;
        if s.kind != Kind::Compressed && s.data.len() >= MERGE_PROBE && !cx.enc.compresses(&s.data).expect("zstd") {
            s.kind = Kind::Compressed;
        }
    }
    let counts: Vec<usize> = leaves.iter().map(|l| l.outputs.len()).collect();
    f.root = Node::Concat(leaves.iter_mut().flat_map(|l| std::mem::take(&mut l.outputs)).collect());
    // (at level 0 each file a leaf block of its own, uncompressed: no formula block, which
    // would be Zstandard)
    if opt.level != Level::STORED {
        f.merge_classes_within(opt.block as u64);
    }
    let Node::Concat(nodes) = std::mem::replace(&mut f.root, Node::Concat(Vec::new())) else { unreachable!() };
    let mut nodes = nodes.into_iter();
    leaves.iter_mut().zip(counts).for_each(|(l, n)| l.outputs = nodes.by_ref().take(n).collect());

    let Cx { enc, .. } = cx;
    Analyzed { f, leaves, at, enc, memo }
}

/// Batch `i` of `files` of analysis `a`, to follow the blocks of `w`: encoded, and verified
/// (its files read again from `src`).
fn finish<S: Source + ?Sized>(
    i: usize,
    w: &SegmentWriter,
    files: &[File],
    a: Analyzed,
    src: &S,
    opt: &Options,
    notes: &Notes,
) -> Made {
    let Analyzed { f, leaves, at, mut enc, memo } = a;
    enc.on_precompress(Some(notes.encoding(i)));
    let encoding = aqr_common::trace::span("encode");
    let mut batch = w.batch(&enc, &f, &leaves).expect("slices within their streams");
    drop(encoding);
    drop(enc);
    let (reports, solid, sets) = report(&f, &leaves, &batch.layout, files, &at);
    drop(f);

    let bad = if opt.verify {
        let read =
            |k: usize, range: Range<usize>| src.read_range(files[k].c, range.start as u64..range.end as u64).ok();
        let mut m = verify::Cached::new(Native { threads: opt.threads }, memo, &read);
        for (k, f) in files.iter().enumerate() {
            m.whole.entry(f.len).or_default().push((k, f.hash));
        }
        let _s = aqr_common::trace::span("verify");
        check(i, w, &mut batch, files, &at, &m, opt.threads, notes)
    } else {
        Vec::new()
    };
    Made { batch, at, files: reports, solid, sets, bad }
}

/// The files of batch `b` (by index; each at its leaf and output in `at`) that its blocks, decoded
/// alone with the modules of `m`, do not give back: all of them where it does not decode.
#[allow(clippy::too_many_arguments)]
fn check(
    i: usize,
    w: &SegmentWriter,
    b: &mut Batch,
    files: &[File],
    at: &[(usize, usize)],
    m: &verify::Cached,
    threads: usize,
    notes: &Notes,
) -> Vec<usize> {
    // (the batch's bytes in the archive decoded, rather than a copy of them)
    let c = w.take_standalone(b);
    let bad = check_in(i, &c, b, files, at, m, threads, notes);
    w.give_back(b, c);
    bad
}

#[allow(clippy::too_many_arguments)]
fn check_in(
    batch: usize,
    c: &[u8],
    b: &Batch,
    files: &[File],
    at: &[(usize, usize)],
    m: &verify::Cached,
    threads: usize,
    notes: &Notes,
) -> Vec<usize> {
    let natives = aqr_decompress::natives(m);
    // the leaves in the order of their blocks, and of each the files of its outputs
    let mut by_block: Vec<usize> = (0..b.leaf_at.len()).collect();
    by_block.sort_by_key(|&l| b.leaf_at[l]);
    let mut of_leaf: Vec<Vec<(usize, usize)>> = vec![Vec::new(); b.leaf_at.len()];
    at.iter().enumerate().for_each(|(k, &(l, o))| of_leaf[l].push((o, k)));
    let bad = Mutex::new(Vec::new());
    let all = || (0..files.len()).collect();
    let Ok(mut a) = aqr::open(c, &natives) else { return all() };
    // within the process's budget, as `aqr` decodes an archive: a large leaf waits for others to
    // be done rather than run along with them (10gb: `enwik9` and `enwik9.zip`, 1.5 GB each), and
    // then takes the output of a module that is a whole file from the file where the leaf before
    // ran it ([`verify::Cached`])
    a.set_budget(aqr_common::budget::global());
    // (each leaf told of as of its first file)
    a.set_items(by_block.iter().map(|&l| of_leaf[l].first().map(|&(_, k)| files[k].c)).collect());
    let r = a.each_leaf(threads, |i, outputs| {
        let l = by_block[i];
        notes.send(Note::Verified(batch, of_leaf[l].iter().map(|&(_, k)| files[k].len as u64).sum()));
        let mut bad = bad.lock().unwrap();
        match outputs {
            Ok(outputs) => bad.extend(
                of_leaf[l].iter().filter(|&&(o, k)| !outputs.get(o).is_some_and(|d| files[k].is(d))).map(|&(_, k)| k),
            ),
            Err(_) => bad.extend(of_leaf[l].iter().map(|&(_, k)| k)),
        }
        Ok(())
    });
    if r.is_err() {
        return all();
    }
    let mut bad = bad.into_inner().unwrap();
    bad.sort_unstable();
    bad
}

/// What became of `files` (by index) in batch `layout` of formula `f` and its `leaves`, each
/// file at its leaf and output in `at`: its report, the solid blocks and the sets.
fn report(
    f: &Formula,
    leaves: &[Leaf],
    layout: &[(Placed, usize)],
    files: &[File],
    at: &[(usize, usize)],
) -> (Vec<FileReport>, Vec<Solid>, Vec<Vec<usize>>) {
    // by leaf, the bytes it slices of each stream, and the modules it applies
    let mut sliced: Vec<HashMap<usize, u64>> = vec![HashMap::new(); leaves.len()];
    let mut modules: Vec<Vec<Module>> = vec![Vec::new(); leaves.len()];
    fn visit(n: &Node, sliced: &mut HashMap<usize, u64>, modules: &mut Vec<Module>, shared: &mut [bool]) {
        n.visit(&mut |n| match n {
            &Node::Slice { stream, len: len @ 1.., .. } => *sliced.entry(stream).or_default() += len,
            Node::Apply(a) if !modules.contains(&a.module) => modules.push(a.module),
            &Node::Output { shared: s, .. } => shared[s] = true,
            _ => {}
        })
    }
    for (l, leaf) in leaves.iter().enumerate() {
        let mut shared = vec![false; f.shared.len()];
        leaf.outputs.iter().for_each(|n| visit(n, &mut sliced[l], &mut modules[l], &mut shared));
        for s in (0..f.shared.len()).rev() {
            if shared[s] {
                f.shared[s].apply.inputs.iter().for_each(|n| visit(n, &mut sliced[l], &mut modules[l], &mut shared));
                if !modules[l].contains(&f.shared[s].apply.module) {
                    modules[l].push(f.shared[s].apply.module);
                }
            }
        }
    }
    let mut by_stream: HashMap<usize, Vec<usize>> = HashMap::new();
    for (l, s) in sliced.iter().enumerate() {
        s.keys().for_each(|&st| by_stream.entry(st).or_default().push(l));
    }
    let mut bytes = vec![0f64; leaves.len()];
    let mut solid = Vec::new();
    // by leaf, its files
    let mut of_leaf: Vec<Vec<usize>> = vec![Vec::new(); leaves.len()];
    (0..files.len()).for_each(|k| of_leaf[at[k].0].push(k));
    for &(p, n) in layout {
        match p {
            Placed::Formula(l) | Placed::Data(l) => bytes[l] += n as f64,
            Placed::NonLeaf(st) => {
                let ls = &by_stream[&st];
                let total: u64 = ls.iter().map(|&l| sliced[l][&st]).sum();
                ls.iter().for_each(|&l| bytes[l] += n as f64 * sliced[l][&st] as f64 / total.max(1) as f64);
                let mut ks: Vec<usize> = ls.iter().flat_map(|&l| of_leaf[l].iter().copied()).collect();
                ks.sort_unstable();
                if ks.len() > 1 {
                    let s = &f.streams[st];
                    solid.push(Solid {
                        class: s.class.to_string(),
                        kind: s.kind,
                        len: s.data.len(),
                        archived: n,
                        contents: ks,
                    });
                }
            }
            Placed::Module | Placed::Resource | Placed::Dictionary | Placed::Metadata => {}
        }
    }
    let mut sets = Vec::new();
    for (l, ks) in of_leaf.iter().enumerate() {
        if leaves[l].outputs.len() > 1 {
            let mut set = ks.clone();
            set.sort_by_key(|&k| at[k].1);
            sets.push(set);
        }
    }
    let out_len = |k: usize| leaves[at[k].0].outputs[at[k].1].len() as f64;
    let leaf_len: Vec<f64> = leaves.iter().map(|l| l.outputs.iter().map(|o| o.len() as f64).sum()).collect();
    let reports = (0..files.len())
        .map(|k| {
            let l = at[k].0;
            FileReport {
                format: files[k].format,
                modules: modules[l].clone(),
                archived: match leaf_len[l] {
                    n if n > 0.0 => bytes[l] * out_len(k) / n,
                    _ => bytes[l] / leaves[l].outputs.len() as f64,
                },
                same_as: None,
                len: files[k].len as u64,
            }
        })
        .collect();
    (reports, solid, sets)
}
