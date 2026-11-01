//! 7z archives read by their header (an encoded header decoded first): the files' names, kinds
//! (directories, files, and symbolic links where the attributes carry a Unix mode), times
//! (modification, creation, access) and attributes (Windows', and the Unix mode where given);
//! the folders decoded on the pool's threads, a folder at a time, and cut into the files'
//! contents, their CRC-32s checked. Folders are chains of LZMA, LZMA2, DEFLATE, bzip2 or
//! stored coders and the filters over them (the branch converters, Delta, BCJ2); a folder of
//! another coder (PPMd, AES) is left out (its files told).

use std::borrow::Cow;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Condvar, Mutex};

use aqr_7z::header::{self, File, Folder, Streams, Top};
use aqr_bcj::bcj2;
use aqr_bcj::formula::Filter;
use aqr_common::{crc32, pool};
use aqr_formula::aqr::{EntryNode, Extra, Reached, Step, WIN_ATTRS_ALLOWED, name_from};

use crate::build::{self, Builder};
use crate::codec::Raw;
use crate::{Done, Sink, codec, sys};

const S_IFMT: u32 = 0o170000;
const S_IFDIR: u32 = 0o040000;
const S_IFLNK: u32 = 0o120000;
/// The bit of 7-Zip's attributes telling a Unix mode in their high 16 bits.
const UNIX_EXTENSION: u32 = 0x8000;
const DIRECTORY: u32 = 0x10;

const BCJ2: &[u8] = &[3, 3, 1, 0x1b];
const DEFLATE: &[u8] = &[4, 1, 8];
const BZIP2: &[u8] = &[4, 2, 2];
const PPMD: &[u8] = &[3, 4, 1];
const AES: &[u8] = &[6, 0xf1, 7, 1];

/// Why a folder is not decoded: coders not known here or encryption (its files left out, told),
/// or damage.
enum Error {
    Unknown(String),
    Bad(String),
}

impl From<String> for Error {
    fn from(e: String) -> Error {
        Error::Bad(e)
    }
}

impl From<&str> for Error {
    fn from(e: &str) -> Error {
        Error::Bad(e.into())
    }
}

/// A folder to decode: the files of its substreams in order (their contents, `None` where left
/// out, and names), and what it is shown as.
struct Unit {
    files: Vec<(Option<usize>, String)>,
    name: String,
    /// Whether it holds a link (its target read before the tree is told).
    links: bool,
}

pub struct Listing<'d> {
    d: &'d [u8],
    main: Streams,
    packs: Vec<Range<usize>>,
    spans: Vec<Range<u64>>,
    b: Builder,
    units: Vec<Unit>,
    /// The contents of no data (empty files), and the links' entries by folder and substream.
    empty: Vec<usize>,
    links: Vec<(usize, usize, usize)>,
    notes: Vec<String>,
}

/// A part of a folder's data, decoded apart from the others: a segment of an LZMA2 stream, an
/// LZMA stream (as it goes, both), or the whole data of other coders (filters too) at once.
enum Part<'d> {
    Lzma2(&'d [u8]),
    Lzma(&'d [u8], u8),
    Whole,
}

/// What is given the data of each substream of a folder, as [`Listing::stream`] decodes it.
type Each<'a> = dyn Fn(usize, &[u8]) -> Result<(), String> + Sync + 'a;

/// The state of a folder decoded as it is written: the bytes of each part decoded so far, and
/// why it stopped.
struct Progress {
    done: Vec<usize>,
    failed: Option<Error>,
    stop: bool,
}

/// The output of folder `f` of `s` (of archive `d`): its coders run from their packed streams.
fn folder_data(
    d: &[u8],
    s: &Streams,
    packs: &[Range<usize>],
    spans: &[Range<u64>],
    f: usize,
    threads: usize,
) -> Result<Vec<u8>, Error> {
    let folder = &s.folders[f];
    let out = folder.output().ok_or("7z: a damaged folder")?;
    let data = output(d, folder, packs[f].start, spans, out, 0, threads)?.into_owned();
    if folder.crc.is_some_and(|c| crc32::hash(&data) != c) {
        return Err("a folder's CRC mismatch".into());
    }
    Ok(data)
}

/// Output `o` of `folder` (whose first packed stream is `pack` among the archive's).
fn output<'d>(
    d: &'d [u8],
    folder: &Folder,
    pack: usize,
    spans: &[Range<u64>],
    o: u64,
    depth: usize,
    threads: usize,
) -> Result<Cow<'d, [u8]>, Error> {
    let bad = || Error::Bad("a damaged folder".into());
    if depth > folder.coders.len() {
        return Err(bad());
    }
    let c = folder.coder_of_out(o).ok_or_else(bad)?;
    let coder = &folder.coders[c];
    let size = usize::try_from(*folder.sizes.get(o as usize).ok_or_else(bad)?).map_err(|_| bad())?;
    let (base, _) = folder.bases(c);
    let mut inputs = Vec::new();
    for i in base..base + coder.ins {
        inputs.push(match folder.source(i).ok_or_else(bad)? {
            Ok(o) => output(d, folder, pack, spans, o, depth + 1, threads)?,
            Err(k) => {
                let r = spans.get(pack + k).ok_or_else(bad)?;
                Cow::Borrowed(d.get(r.start as usize..r.end as usize).ok_or("cut short")?)
            }
        });
    }
    let one = || inputs.first().map(|i| &i[..]).ok_or_else(bad);
    let data: Cow<[u8]> = match &coder.id[..] {
        header::COPY => Cow::Owned(one()?.to_vec()),
        header::LZMA => {
            let p = coder.props.first().ok_or_else(bad)?;
            Cow::Owned(codec::lzma(one()?, *p, Some(size as u64), size)?.0)
        }
        header::LZMA2 => Cow::Owned(codec::lzma2_par(one()?, size, threads)?.0),
        DEFLATE => Cow::Owned(codec::inflate(one()?, size, size)?.0),
        BZIP2 => Cow::Owned(codec::bzip2(one()?, size)?),
        BCJ2 if inputs.len() == 4 => {
            let s = bcj2::Streams { main: inputs[0].to_vec(), call: inputs[1].to_vec(), jump: inputs[2].to_vec() };
            Cow::Owned(bcj2::decode(&s, &inputs[3], size).map_err(|e| format!("BCJ2: {e:?}"))?)
        }
        AES => return Err(Error::Unknown("encrypted".into())),
        PPMD => return Err(Error::Unknown("of PPMd, not known here".into())),
        id => match Filter::from_7z(id, &coder.props) {
            Some(f) if coder.ins == 1 => {
                let mut v = inputs.pop().ok_or_else(bad)?.into_owned();
                f.decode(&mut v);
                Cow::Owned(v)
            }
            _ => return Err(Error::Unknown(format!("of coder {id:02x?}, not known here"))),
        },
    };
    if data.len() != size {
        return Err("a coder's output not of its size".into());
    }
    Ok(data)
}

/// The packed streams of each folder (by index), and the span of each in the archive.
type Spans = (Vec<Range<usize>>, Vec<Range<u64>>);

/// The packed streams of each folder of `s`, and the span of each in the archive.
fn spans(s: &Streams, len: usize) -> Result<Spans, String> {
    let packs = s.folder_packs().ok_or("7z: a damaged header")?;
    let spans = s.pack_spans().ok_or("7z: a damaged header")?;
    if spans.iter().any(|r| r.end > len as u64) {
        return Err("7z: cut short".into());
    }
    Ok((packs, spans))
}

impl<'d> Listing<'d> {
    pub fn read(d: &'d [u8]) -> Result<Listing<'d>, String> {
        let (span, crc) = header::signature(d).ok_or("7z: not a 7z archive")?;
        let h = usize::try_from(span.start)
            .ok()
            .zip(usize::try_from(span.end).ok())
            .and_then(|(s, e)| d.get(s..e))
            .ok_or("7z: cut short")?;
        if crc32::hash(h) != crc {
            return Err("7z: a damaged header (its CRC)".into());
        }
        let h = match header::parse(h).ok_or("7z: a damaged header")? {
            Top::Plain(h) => h,
            Top::Encoded(s) => {
                let (packs, spans) = spans(&s, d.len())?;
                if s.folders.is_empty() {
                    return Err("7z: a damaged header".into());
                }
                let hd = folder_data(d, &s, &packs, &spans, 0, 1).map_err(|e| match e {
                    Error::Unknown(e) | Error::Bad(e) => format!("7z: the header: {e}"),
                })?;
                match header::parse(&hd) {
                    Some(Top::Plain(h)) => h,
                    _ => return Err("7z: a damaged header".into()),
                }
            }
        };
        let (packs, spans) = spans(&h.main, d.len())?;
        let mut l = Listing {
            d,
            main: h.main,
            packs,
            spans,
            b: Builder::default(),
            units: Vec::new(),
            empty: Vec::new(),
            links: Vec::new(),
            notes: Vec::new(),
        };
        l.files(&h.files);
        Ok(l)
    }

    fn files(&mut self, files: &[File]) {
        // the substreams, by folder, taken by the files with data in order
        let mut subs = self.main.subs.iter().enumerate().flat_map(|(f, s)| (0..s.len()).map(move |k| (f, k)));
        self.units =
            self.main.folders.iter().map(|_| Unit { files: Vec::new(), name: String::new(), links: false }).collect();
        for (i, s) in self.main.subs.iter().enumerate() {
            self.units[i].files = vec![(None, String::new()); s.len()];
        }
        for file in files {
            let sub = if file.stream { subs.next() } else { None };
            let shown = String::from_utf8_lossy(&file.name).into_owned();
            if let Some((f, k)) = sub {
                self.units[f].files[k].1 = shown.clone();
            }
            if file.anti {
                continue;
            }
            let Some(names) = build::split(&file.name, true) else {
                self.notes.push(format!("{shown}: a path reaching out of the directory, not extracted"));
                continue;
            };
            let attrib = file.attrib.unwrap_or(0);
            let mode = if attrib & UNIX_EXTENSION != 0 { attrib >> 16 } else { 0 };
            let mut x = Extra {
                mtime: file.mtime.and_then(sys::filetime),
                crtime: file.ctime.and_then(sys::filetime),
                atime: file.atime.and_then(sys::filetime),
                ..Extra::default()
            };
            if mode != 0 {
                x.mode = Some(mode & 0o7777);
            }
            if attrib & WIN_ATTRS_ALLOWED != 0 {
                x.win_attrs = Some(attrib & WIN_ATTRS_ALLOWED);
            }
            let dir = !file.stream && !file.empty_file || attrib & DIRECTORY != 0 || mode & S_IFMT == S_IFDIR;
            let node = if dir {
                EntryNode::Dir
            } else if mode & S_IFMT == S_IFLNK {
                // (the target is its data: read before the tree is told)
                let Some(_) = sub else {
                    self.notes.push(format!("{shown}: a link to nothing, not extracted"));
                    continue;
                };
                EntryNode::Link(Vec::new())
            } else {
                let c = self.b.content();
                match sub {
                    Some((f, k)) => self.units[f].files[k].0 = Some(c),
                    None => self.empty.push(c),
                }
                EntryNode::File(c)
            };
            let link = matches!(node, EntryNode::Link(_));
            let r = match (dir, names.is_empty()) {
                (true, true) => self.b.add(&[], None, x),
                _ => self.b.add(&names, Some(node), x),
            };
            match r {
                Ok(Some(i)) if link => {
                    let (f, k) = sub.unwrap();
                    self.units[f].links = true;
                    self.links.push((f, k, i));
                }
                Ok(_) => {}
                Err(e) => self.notes.push(e),
            }
        }
        for u in &mut self.units {
            let mut named = u.files.iter().filter(|f| !f.1.is_empty());
            u.name = match (named.next(), u.files.len()) {
                (Some(f), 1) => f.1.clone(),
                (Some(f), n) => format!("{} and {} more files", f.1, n - 1),
                _ => String::new(),
            };
        }
    }

    pub fn units(&self) -> Vec<String> {
        self.units.iter().map(|u| u.name.clone()).collect()
    }

    /// The data of folder `f` cut into its substreams, their CRC-32s checked.
    fn decode(&self, f: usize, threads: usize) -> Result<Vec<Vec<u8>>, Error> {
        let data = folder_data(self.d, &self.main, &self.packs, &self.spans, f, threads)?;
        let sizes = &self.main.subs[f];
        if sizes.iter().sum::<u64>() != data.len() as u64 {
            return Err("a folder not of its files' sizes".into());
        }
        let first: usize = self.main.subs[..f].iter().map(Vec::len).sum();
        let mut at = 0;
        let mut out = Vec::with_capacity(sizes.len());
        for (k, &n) in sizes.iter().enumerate() {
            let part = &data[at..at + n as usize];
            at += n as usize;
            if self.main.sub_crcs.get(first + k).copied().flatten().is_some_and(|c| crc32::hash(part) != c) {
                return Err(format!("{}: CRC mismatch", self.units[f].files[k].1).into());
            }
            out.push(part.to_vec());
        }
        Ok(out)
    }

    /// The parts folder `f` decodes by, and the bytes of each.
    fn parts(&self, f: usize) -> Result<Vec<(Part<'d>, usize)>, Error> {
        let folder = &self.main.folders[f];
        let size = folder.size().and_then(|n| usize::try_from(n).ok()).ok_or("a damaged folder")?;
        if let Some(c) = folder.single() {
            let r = &self.spans[self.packs[f].start];
            let input = self.d.get(r.start as usize..r.end as usize).ok_or("cut short")?;
            match &c.id[..] {
                header::LZMA2 => {
                    let (segs, _) = codec::lzma2_segments(input)?;
                    return Ok(segs.into_iter().map(|(span, n)| (Part::Lzma2(&input[span]), n)).collect());
                }
                header::LZMA => {
                    return Ok(vec![(Part::Lzma(input, *c.props.first().ok_or("a damaged folder")?), size)]);
                }
                _ => {}
            }
        }
        Ok(vec![(Part::Whole, size)])
    }

    /// Decodes part `p` of folder `f`, `n` bytes, into `buf` (of room for `room`), telling `done`
    /// the bytes written so far as it goes (false: stop, returning false).
    ///
    /// # Safety
    /// `buf[..room]` is allocated and nobody else's while this runs, `room >= n`, and more by
    /// [`codec::LZMA_ROOM`] for an LZMA stream.
    #[allow(clippy::too_many_arguments)]
    unsafe fn decode_part(
        &self,
        f: usize,
        p: &Part,
        buf: *mut u8,
        n: usize,
        room: usize,
        threads: usize,
        done: &mut dyn FnMut(usize) -> bool,
    ) -> Result<bool, Error> {
        // SAFETY: as the caller's
        unsafe {
            match *p {
                Part::Lzma2(input) => Ok(codec::lzma2_segment(input, buf, n, done)?),
                Part::Lzma(input, props) => Ok(codec::lzma_part(input, props, buf, n, room, done)?),
                Part::Whole => {
                    let data = folder_data(self.d, &self.main, &self.packs, &self.spans, f, threads)?;
                    if data.len() != n {
                        return Err("a coder's output not of its size".into());
                    }
                    std::ptr::copy_nonoverlapping(data.as_ptr(), buf, n);
                    Ok(done(n))
                }
            }
        }
    }

    /// Decodes folder `f` on up to `threads` threads, giving the data of each of its substreams
    /// to `each` (with its index, on any thread) as soon as it is all decoded, and telling
    /// `tick` the bytes given as it goes. The parts of the folder decode on threads of their
    /// own, while what is decoded is given on the others.
    fn stream(
        &self,
        f: usize,
        threads: usize,
        each: &Each,
        tick: &dyn Fn(u64),
    ) -> Result<(), Error> {
        let parts = self.parts(f)?;
        let size: usize = parts.iter().map(|p| p.1).sum();
        if Some(size as u64) != self.main.folders[f].size() {
            return Err("a coder's output not of its size".into());
        }
        let sizes = &self.main.subs[f];
        if sizes.iter().sum::<u64>() != size as u64 {
            return Err("a folder not of its files' sizes".into());
        }
        let ends: Vec<usize> = sizes
            .iter()
            .scan(0, |at, &n| {
                *at += n as usize;
                Some(*at)
            })
            .collect();
        let starts: Vec<usize> = parts.iter().scan(0, |at, p| Some(std::mem::replace(at, *at + p.1))).collect();
        let mut buf = codec::room(size + codec::LZMA_ROOM)?;
        let room = buf.capacity();
        let raw = Raw(buf.as_mut_ptr());
        let progress = Mutex::new(Progress { done: vec![0; parts.len()], failed: None, stop: false });
        let more = Condvar::new();
        let next = AtomicUsize::new(0);
        // (the parts taken in order by as many threads as decode them)
        let work = || {
            loop {
                let r = next.fetch_add(1, Relaxed);
                let Some((p, n)) = parts.get(r) else { return };
                let mut done = |k| {
                    let mut g = progress.lock().unwrap();
                    g.done[r] = k;
                    more.notify_all();
                    !g.stop
                };
                // (the last part has the room past the data)
                let space = if r + 1 == parts.len() { room - starts[r] } else { *n };
                // SAFETY: the parts' places in `buf` are apart, within its room
                match unsafe { self.decode_part(f, p, raw.at(starts[r]), *n, space, threads, &mut done) } {
                    Ok(true) => {}
                    Ok(false) => return,
                    Err(e) => {
                        let mut g = progress.lock().unwrap();
                        g.failed.get_or_insert(e);
                        g.stop = true;
                        more.notify_all();
                        return;
                    }
                }
            }
        };
        // the bytes decoded from the start on
        let reached = |done: &[usize]| {
            let whole = done.iter().zip(&parts).take_while(|(d, p)| **d == p.1).count();
            starts.get(whole).map_or(size, |s| s + done[whole])
        };
        let write = || -> Result<(), Error> {
            let mut k = 0;
            while k < ends.len() {
                let front = {
                    let mut g = progress.lock().unwrap();
                    loop {
                        if let Some(e) = g.failed.take() {
                            return Err(e);
                        }
                        let front = reached(&g.done);
                        if front >= ends[k] {
                            break front;
                        }
                        g = more.wait(g).unwrap();
                    }
                };
                let m = k + ends[k..].partition_point(|&e| e <= front);
                let made = pool::par_map(m - k, threads, |i| {
                    let j = k + i;
                    let at = if j == 0 { 0 } else { ends[j - 1] };
                    // SAFETY: decoded (told under the lock), and written no more
                    each(j, unsafe { std::slice::from_raw_parts(raw.at(at), ends[j] - at) })
                });
                made.into_iter().collect::<Result<(), String>>()?;
                tick((ends[m - 1] - if k == 0 { 0 } else { ends[k - 1] }) as u64);
                k = m;
            }
            // SAFETY: all decoded
            let all = unsafe { std::slice::from_raw_parts(raw.0, size) };
            if self.main.folders[f].crc.is_some_and(|c| crc32::hash(all) != c) {
                return Err("a folder's CRC mismatch".into());
            }
            Ok(())
        };
        let r = pool::scope(parts.len().min(threads.saturating_sub(1)), |sc| {
            let w = sc.workers().min(parts.len());
            if w == 0 {
                work();
                return write();
            }
            let handles: Vec<_> = (0..w).map(|_| sc.spawn(work)).collect();
            let r = write();
            if r.is_err() {
                progress.lock().unwrap().stop = true;
            }
            for h in handles {
                h.join().unwrap_or_else(|p| std::panic::resume_unwind(p));
            }
            r
        });
        drop(buf);
        r
    }

    pub fn unpack(&self, threads: usize, sink: &dyn Sink) -> Result<Done, String> {
        self.notes.iter().for_each(|n| sink.note(n));
        let mut skipped = self.notes.len();
        let mut tree = self.b.tree.clone();
        // the folders of links first, for their targets
        let early: Vec<usize> = (0..self.units.len()).filter(|&f| self.units[f].links).collect();
        let mut held: Vec<Option<Vec<Vec<u8>>>> = vec![None; self.units.len()];
        for (f, r) in early.iter().zip(pool::par_map(early.len(), threads, |k| self.decode(early[k], threads))) {
            held[*f] = Some(r.map_err(|e| match e {
                Error::Unknown(e) | Error::Bad(e) => format!("7z: {}: {e}", self.units[*f].name),
            })?);
        }
        for &(f, k, i) in &self.links {
            let target = &held[f].as_ref().unwrap()[k];
            if target.is_empty() || target.contains(&0) {
                // (told as a special entry not created)
                sink.note(&format!("{}: a link to nothing, not extracted", self.units[f].files[k].1));
                skipped += 1;
                tree.entries[i].node = EntryNode::Special(0);
            } else {
                tree.entries[i].node = EntryNode::Link(name_from(target));
            }
        }
        let all: Vec<usize> = (0..tree.entries.len()).collect();
        if !all.is_empty() {
            sink.entries(&tree, &all)?;
        }
        for &c in self.empty.iter().filter(|&&c| self.b.live(c)) {
            sink.content(c, Vec::new())?;
        }
        let packed: u64 = self.spans.iter().map(|r| r.end - r.start).sum();
        let total: u64 = self.main.folders.iter().filter_map(Folder::size).sum();
        let (done_packed, done) = (AtomicU64::new(0), AtomicU64::new(0));
        let failed = AtomicBool::new(false);
        let held: Vec<std::sync::Mutex<Option<Vec<Vec<u8>>>>> = held.into_iter().map(std::sync::Mutex::new).collect();
        let notes = std::sync::Mutex::new(0usize);
        let made = pool::par_map(self.units.len(), threads, |f| -> Result<(), String> {
            let u = &self.units[f];
            if failed.load(Relaxed) || u.files.iter().all(|c| c.0.is_none()) {
                return Ok(());
            }
            sink.step(Step::Started(f));
            let tick = |k: u64| {
                let k = done.fetch_add(k, Relaxed) + k;
                let p = done_packed.load(Relaxed);
                sink.step(Step::Reached(Reached { decompressed: (p, packed), evaluated: (k, total) }));
            };
            let r = match held[f].lock().unwrap().take() {
                // (the files of a folder written on as many threads as it takes)
                Some(parts) => {
                    let parts: Vec<_> = parts.into_iter().map(|p| std::sync::Mutex::new(Some(p))).collect();
                    let r = pool::par_map(parts.len(), threads, |k| match u.files[k].0 {
                        Some(c) if self.b.live(c) => sink.content(c, parts[k].lock().unwrap().take().unwrap()),
                        _ => Ok(()),
                    });
                    tick(self.main.folders[f].size().unwrap_or(0));
                    r.into_iter().collect::<Result<(), String>>().map_err(Error::Bad)
                }
                None => {
                    let first: usize = self.main.subs[..f].iter().map(Vec::len).sum();
                    let each = |k: usize, d: &[u8]| {
                        if self.main.sub_crcs.get(first + k).copied().flatten().is_some_and(|c| crc32::hash(d) != c) {
                            return Err(format!("{}: CRC mismatch", u.files[k].1));
                        }
                        match u.files[k].0 {
                            Some(c) if self.b.live(c) => sink.content(c, d.to_vec()),
                            _ => Ok(()),
                        }
                    };
                    self.stream(f, threads, &each, &tick)
                }
            };
            let r = match r {
                Ok(()) => Ok(()),
                // (a folder of coders not known, or encrypted: its files left out)
                Err(Error::Unknown(e)) => {
                    for (c, name) in &u.files {
                        if c.is_some() {
                            sink.note(&format!("{name}: {e}, not extracted"));
                            *notes.lock().unwrap() += 1;
                        }
                    }
                    // (nothing given of it: told as done all the same)
                    tick(self.main.folders[f].size().unwrap_or(0));
                    Ok(())
                }
                Err(Error::Bad(e)) => Err(format!("7z: {}: {e}", u.name)),
            };
            sink.step(Step::Ended(f));
            if r.is_err() {
                failed.store(true, Relaxed);
            }
            let range = &self.packs[f];
            let p: u64 = self.spans[range.clone()].iter().map(|r| r.end - r.start).sum();
            done_packed.fetch_add(p, Relaxed);
            tick(0);
            r
        });
        made.into_iter().collect::<Result<(), String>>()?;
        Ok(Done { tree, skipped: skipped + notes.into_inner().unwrap() })
    }
}
