//! 7z archives read by their header (an encoded header decoded first): the files' names, kinds
//! (directories, files, and symbolic links where the attributes carry a Unix mode), times
//! (modification, creation, access) and attributes (Windows', and the Unix mode where given);
//! the folders decoded on the pool's threads, a folder at a time, and cut into the files'
//! contents, their CRC-32s checked. Folders are chains of LZMA, LZMA2, DEFLATE, bzip2 or
//! stored coders and the filters over them (the branch converters, Delta, BCJ2), AES among
//! them decrypted by the passphrase given ([`Listing::unlock`]; an encrypted header needs it to
//! be read at all, [`Listing::read`]); a folder of another coder (PPMd), or encrypted without
//! the passphrase, is left out (its files told).

use std::borrow::Cow;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Condvar, Mutex};

use aqr_7z::header::{self, File, Folder, Streams, Top};
use aqr_bcj::bcj2;
use aqr_bcj::formula::Filter;
use aqr_common::{crc32, pool};
use aqr_formula::aqr::{EntryNode, Extra, Reached, Step, Tree, WIN_ATTRS_ALLOWED, name_from};

use crate::build::{self, Builder};
use crate::codec::Raw;
use crate::{Done, Select, Sink, codec};
use aqr_foreign::attrs::{DOS_DIR, S_IFDIR, S_IFLNK, S_IFMT, UNIX_EXTENSION};
use aqr_foreign::crypt::SEVENZ_AES_ID;

const BCJ2: &[u8] = &[3, 3, 1, 0x1b];
const DEFLATE: &[u8] = &[4, 1, 8];
const BZIP2: &[u8] = &[4, 2, 2];
const PPMD: &[u8] = &[3, 4, 1];

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
    notes: Vec<build::Note>,
    /// The passphrase of the encrypted folders, and whether the header was encrypted.
    pass: Option<Vec<u8>>,
    sealed: bool,
}

/// Bytes of the output of an encrypted folder decoded to check a passphrase by (a larger one
/// is not checked).
const CHECKED: u64 = 64 << 20;

/// Whether `folder` has an AES coder.
fn encrypted(folder: &Folder) -> bool {
    folder.coders.iter().any(|c| c.id == SEVENZ_AES_ID)
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
    pass: Option<&[u8]>,
) -> Result<Vec<u8>, Error> {
    let folder = &s.folders[f];
    let out = folder.output().ok_or("7z: a damaged folder")?;
    let data = output(d, folder, packs[f].start, spans, out, 0, threads, pass)?.into_owned();
    if folder.crc.is_some_and(|c| crc32::hash(&data) != c) {
        return Err("a folder's CRC mismatch".into());
    }
    Ok(data)
}

/// Output `o` of `folder` (whose first packed stream is `pack` among the archive's), AES
/// decrypted by `pass`.
#[allow(clippy::too_many_arguments)]
fn output<'d>(
    d: &'d [u8],
    folder: &Folder,
    pack: usize,
    spans: &[Range<u64>],
    o: u64,
    depth: usize,
    threads: usize,
    pass: Option<&[u8]>,
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
            Ok(o) => output(d, folder, pack, spans, o, depth + 1, threads, pass)?,
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
        SEVENZ_AES_ID => match pass {
            None => return Err(Error::Unknown("encrypted, and no passphrase given".into())),
            Some(p) => {
                let mut v = aqr_foreign::crypt::sevenz_aes_decrypt(p, &coder.props, one()?)?;
                if v.len() < size {
                    return Err("AES: cut short".into());
                }
                v.truncate(size);
                Cow::Owned(v)
            }
        },
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
    /// Archive `d` read, its header decrypted by `pass` where it is encrypted: `None` where it
    /// is and `pass` is not given or does not decrypt it.
    pub fn read(d: &'d [u8], pass: Option<&[u8]>) -> Result<Option<Listing<'d>>, String> {
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
            Top::Plain(h) => (h, false),
            Top::Encoded(s) => {
                let (packs, spans) = spans(&s, d.len())?;
                if s.folders.is_empty() {
                    return Err("7z: a damaged header".into());
                }
                let sealed = encrypted(&s.folders[0]);
                if sealed && pass.is_none() {
                    return Ok(None);
                }
                let hd = match folder_data(d, &s, &packs, &spans, 0, 1, pass) {
                    Ok(hd) => hd,
                    // (a wrong passphrase decrypts to what does not decode)
                    Err(_) if sealed => return Ok(None),
                    Err(Error::Unknown(e) | Error::Bad(e)) => return Err(format!("7z: the header: {e}")),
                };
                match header::parse(&hd) {
                    Some(Top::Plain(h)) => (h, sealed),
                    _ if sealed => return Ok(None),
                    _ => return Err("7z: a damaged header".into()),
                }
            }
        };
        let (h, sealed) = h;
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
            pass: pass.filter(|_| sealed).map(<[u8]>::to_vec),
            sealed,
        };
        l.files(&h.files);
        Ok(Some(l))
    }

    /// Whether its header or folders are encrypted.
    pub fn encrypted(&self) -> bool {
        self.sealed || self.main.folders.iter().any(encrypted)
    }

    /// Whether `pass` decrypts the encrypted folders, taken for them where it does: as the
    /// smallest of them decodes by it (where not too large to check). The passphrase of an
    /// encrypted header is taken already.
    pub fn unlock(&mut self, pass: &[u8]) -> bool {
        if self.pass.as_deref() == Some(pass) {
            return true;
        }
        let smallest = (0..self.main.folders.len())
            .filter(|&f| encrypted(&self.main.folders[f]))
            .min_by_key(|&f| self.main.folders[f].size().unwrap_or(u64::MAX));
        let ok = match smallest {
            None => false,
            Some(f) if self.main.folders[f].size().is_some_and(|n| n <= CHECKED) => {
                folder_data(self.d, &self.main, &self.packs, &self.spans, f, 1, Some(pass)).is_ok()
            }
            Some(_) => true,
        };
        if ok {
            self.pass = Some(pass.to_vec());
        }
        ok
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
            let lax = || build::lax(&file.name, true);
            if let Some((f, k)) = sub {
                self.units[f].files[k].1 = shown.clone();
            }
            if file.anti {
                continue;
            }
            let Some(names) = build::split(&file.name, true) else {
                self.notes.push((lax(), build::outside(&shown)));
                continue;
            };
            let attrib = file.attrib.unwrap_or(0);
            let mode = if attrib & UNIX_EXTENSION != 0 { attrib >> 16 } else { 0 };
            let mut x = Extra {
                mtime: file.mtime.and_then(aqr_foreign::time::from_filetime),
                crtime: file.ctime.and_then(aqr_foreign::time::from_filetime),
                atime: file.atime.and_then(aqr_foreign::time::from_filetime),
                ..Extra::default()
            };
            if mode != 0 {
                x.mode = Some(mode & 0o7777);
            }
            if attrib & WIN_ATTRS_ALLOWED != 0 {
                x.win_attrs = Some(attrib & WIN_ATTRS_ALLOWED);
            }
            let dir = !file.stream && !file.empty_file || attrib & DOS_DIR != 0 || mode & S_IFMT == S_IFDIR;
            let node = if dir {
                EntryNode::Dir
            } else if mode & S_IFMT == S_IFLNK {
                // (the target is its data: read before the tree is told)
                let Some(_) = sub else {
                    self.notes.push((lax(), build::dangling(&shown)));
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
                Err(e) => self.notes.push((lax(), e)),
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
        let data = folder_data(self.d, &self.main, &self.packs, &self.spans, f, threads, self.pass.as_deref())?;
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
                    let data =
                        folder_data(self.d, &self.main, &self.packs, &self.spans, f, threads, self.pass.as_deref())?;
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
    /// that `want` (by index) to `each` (with its index, on any thread) as soon as it is all
    /// decoded, and telling `tick` the bytes given as it goes. The parts of the folder decode on
    /// threads of their own, while what is decoded is given on the others. Only the parts
    /// holding the substreams from the first wanted to the last are decoded (LZMA2's parts are
    /// apart), and the last only as far as needed; the folder's CRC is checked where it is all.
    fn stream(
        &self,
        f: usize,
        threads: usize,
        want: &dyn Fn(usize) -> bool,
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
        // the substreams given, and the parts holding them (those before taken as done)
        let (Some(first), Some(last)) = ((0..ends.len()).find(|&k| want(k)), (0..ends.len()).rfind(|&k| want(k)))
        else {
            return Ok(());
        };
        let (lo, hi) = (if first == 0 { 0 } else { ends[first - 1] }, ends[last]);
        let p0 = (0..parts.len()).take_while(|&r| starts[r] + parts[r].1 <= lo && starts[r] < lo).count();
        let p1 = (0..parts.len()).find(|&r| r > p0 && starts[r] >= hi).unwrap_or(parts.len());
        let whole = (0..ends.len()).all(want) && p0 == 0 && p1 == parts.len();
        let mut buf = codec::room(size + codec::LZMA_ROOM)?;
        let room = buf.capacity();
        let raw = Raw(buf.as_mut_ptr());
        let done: Vec<usize> = (0..parts.len()).map(|r| if r < p0 { parts[r].1 } else { 0 }).collect();
        let progress = Mutex::new(Progress { done, failed: None, stop: false });
        let more = Condvar::new();
        let next = AtomicUsize::new(p0);
        // (the parts taken in order by as many threads as decode them)
        let work = || {
            loop {
                let r = next.fetch_add(1, Relaxed);
                let Some((p, n)) = parts[..p1].get(r) else { return };
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
            let mut k = first;
            while k <= last {
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
                let m = (k + ends[k..].partition_point(|&e| e <= front)).min(last + 1);
                let given: Vec<usize> = (k..m).filter(|&j| want(j)).collect();
                let made = pool::par_map(given.len(), threads, |i| {
                    let j = given[i];
                    let at = if j == 0 { 0 } else { ends[j - 1] };
                    // SAFETY: decoded (told under the lock), and written no more
                    each(j, unsafe { std::slice::from_raw_parts(raw.at(at), ends[j] - at) })
                });
                made.into_iter().collect::<Result<(), String>>()?;
                tick(given.iter().map(|&j| sizes[j]).sum());
                k = m;
            }
            // SAFETY: all decoded
            let all = unsafe { std::slice::from_raw_parts(raw.0, size) };
            if whole && self.main.folders[f].crc.is_some_and(|c| crc32::hash(all) != c) {
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
            // (what is past the last substream given not decoded)
            progress.lock().unwrap().stop = true;
            for h in handles {
                h.join().unwrap_or_else(|p| std::panic::resume_unwind(p));
            }
            r
        });
        drop(buf);
        r
    }

    pub fn tree(&self) -> &Tree {
        &self.b.tree
    }

    pub fn notes(&self) -> &[build::Note] {
        &self.notes
    }

    pub fn unpack(&self, threads: usize, sink: &dyn Sink, select: Option<&Select>) -> Result<Done, String> {
        let mut skipped = crate::tell(&self.notes, select, sink);
        let mut tree = self.b.tree.clone();
        let wanted = select.map(|s| s.contents(&tree, self.b.contents));
        let entries = select.map(|s| s.entries(&tree));
        let want = |c: Option<usize>| c.is_some_and(|c| self.b.live(c) && build::wanted(wanted.as_deref(), c));
        let links: Vec<_> = self.links.iter().filter(|l| entries.as_ref().is_none_or(|e| e[l.2])).collect();
        // the folders of links first, for their targets
        let mut early: Vec<usize> = links.iter().map(|l| l.0).collect();
        early.sort_unstable();
        early.dedup();
        let mut held: Vec<Option<Vec<Vec<u8>>>> = vec![None; self.units.len()];
        for (f, r) in early.iter().zip(pool::par_map(early.len(), threads, |k| self.decode(early[k], threads))) {
            held[*f] = Some(r.map_err(|e| match e {
                Error::Unknown(e) | Error::Bad(e) => format!("7z: {}: {e}", self.units[*f].name),
            })?);
        }
        for &&(f, k, i) in &links {
            let target = &held[f].as_ref().unwrap()[k];
            if target.is_empty() || target.contains(&0) {
                // (told as a special entry not created)
                sink.note(&build::dangling(&self.units[f].files[k].1));
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
        for &c in self.empty.iter().filter(|&&c| want(Some(c))) {
            sink.content(c, Vec::new())?;
        }
        // by folder and substream, whether it is decoded (all of a folder of a file, where
        // nothing is selected, as their CRC-32s are checked); by folder, whether it is, and the
        // bytes decoded of it
        let decodes = |f: usize, k: usize| match select {
            None => true,
            Some(_) => want(self.units[f].files[k].0),
        };
        let decoded: Vec<bool> = (0..self.units.len())
            .map(|f| match select {
                None => self.units[f].files.iter().any(|x| x.0.is_some()),
                Some(_) => (0..self.units[f].files.len()).any(|k| decodes(f, k)),
            })
            .collect();
        let given = |f: usize| -> u64 {
            (0..self.units[f].files.len()).filter(|&k| decodes(f, k)).map(|k| self.main.subs[f][k]).sum()
        };
        let packed: u64 = (0..self.units.len())
            .filter(|&f| decoded[f])
            .flat_map(|f| &self.spans[self.packs[f].clone()])
            .map(|r| r.end - r.start)
            .sum();
        let total: u64 = (0..self.units.len()).filter(|&f| decoded[f]).map(given).sum();
        let (done_packed, done) = (AtomicU64::new(0), AtomicU64::new(0));
        let failed = AtomicBool::new(false);
        let held: Vec<Mutex<Option<Vec<Vec<u8>>>>> = held.into_iter().map(Mutex::new).collect();
        let notes = Mutex::new(0usize);
        let made = pool::par_map(self.units.len(), threads, |f| -> Result<(), String> {
            let u = &self.units[f];
            if failed.load(Relaxed) || !decoded[f] {
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
                    let parts: Vec<_> = parts.into_iter().map(|p| Mutex::new(Some(p))).collect();
                    let r = pool::par_map(parts.len(), threads, |k| match u.files[k].0 {
                        Some(c) if want(Some(c)) => sink.content(c, parts[k].lock().unwrap().take().unwrap()),
                        _ => Ok(()),
                    });
                    tick(given(f));
                    r.into_iter().collect::<Result<(), String>>().map_err(Error::Bad)
                }
                None => {
                    let first: usize = self.main.subs[..f].iter().map(Vec::len).sum();
                    let each = |k: usize, d: &[u8]| {
                        if self.main.sub_crcs.get(first + k).copied().flatten().is_some_and(|c| crc32::hash(d) != c) {
                            return Err(format!("{}: CRC mismatch", u.files[k].1));
                        }
                        match u.files[k].0 {
                            Some(c) if want(Some(c)) => sink.content(c, d.to_vec()),
                            _ => Ok(()),
                        }
                    };
                    self.stream(f, threads, &|k| decodes(f, k), &each, &tick)
                }
            };
            let r = match r {
                Ok(()) => Ok(()),
                // (a folder of coders not known, or encrypted: its files left out; and one that
                // does not decode by the passphrase given, as it cannot be told from damage)
                Err(Error::Bad(_)) if encrypted(&self.main.folders[f]) => {
                    for (c, name) in &u.files {
                        if want(*c) {
                            sink.note(&format!("{name}: a wrong passphrase (or damaged), not extracted"));
                            *notes.lock().unwrap() += 1;
                        }
                    }
                    tick(given(f));
                    Ok(())
                }
                Err(Error::Unknown(e)) => {
                    for (c, name) in &u.files {
                        if want(*c) {
                            sink.note(&format!("{name}: {e}, not extracted"));
                            *notes.lock().unwrap() += 1;
                        }
                    }
                    // (nothing given of it: told as done all the same)
                    tick(given(f));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Kept, file_of};

    /// Of a solid folder of LZMA2 parts (aqr-7z's `blocks.7z`), a file selected decodes
    /// without the parts before it nor those after it: a damaged one of those fails the folder
    /// extracted whole, not the file.
    #[test]
    fn parts_apart() {
        let d =
            std::fs::read(format!("{}/../../codecs/aqr-7z/tests/data/blocks.7z", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let l = Listing::read(&d, None).unwrap().unwrap();
        let f = (0..l.units.len()).max_by_key(|&f| l.units[f].files.len()).unwrap();
        let Ok(parts) = l.parts(f) else { panic!("no parts") };
        assert!(parts.len() >= 3, "{} parts", parts.len());
        // where each part's packed bytes are, and where its output starts and ends
        let packed: Vec<std::ops::Range<usize>> = parts
            .iter()
            .map(|p| match p.0 {
                Part::Lzma2(s) => {
                    let at = s.as_ptr() as usize - d.as_ptr() as usize;
                    at..at + s.len()
                }
                _ => panic!("not of LZMA2 parts"),
            })
            .collect();
        let out: Vec<usize> = parts.iter().scan(0, |at, p| Some(std::mem::replace(at, *at + p.1))).collect();
        let first_end = parts[0].1;
        let last_start = *out.last().unwrap();
        // by substream: its start and end in the folder's output
        let sizes = &l.main.subs[f];
        let spans: Vec<(usize, usize)> = sizes
            .iter()
            .scan(0, |at, &n| {
                let s = *at;
                *at += n as usize;
                Some((s, *at))
            })
            .collect();
        let pick = |ok: &dyn Fn(usize, usize) -> bool| {
            (0..spans.len())
                .find(|&k| l.units[f].files[k].0.is_some() && spans[k].1 > spans[k].0 && ok(spans[k].0, spans[k].1))
        };
        let good = Kept::default();
        l.unpack(4, &good, None).unwrap();
        let good = good.0.into_inner().unwrap();
        for (k, damaged) in [
            (pick(&|s, _| s >= first_end).expect("a file past the first part"), 0),
            (pick(&|_, e| e <= last_start).expect("a file before the last part"), parts.len() - 1),
        ] {
            let mut bad = d.clone();
            let r = &packed[damaged];
            bad[r.start + r.len() / 2] ^= 0x55;
            let lb = Listing::read(&bad, None).unwrap().unwrap();
            assert!(lb.unpack(4, &Kept::default(), None).is_err(), "part {damaged} damaged");
            let c = l.units[f].files[k].0.unwrap();
            let kept = Kept::default();
            let done = lb.unpack(4, &kept, Some(&file_of(&l.b.tree, c))).unwrap();
            assert_eq!(done.skipped, 0);
            let kept = kept.0.into_inner().unwrap();
            assert_eq!(kept.keys().collect::<Vec<_>>(), [&c], "part {damaged} damaged");
            assert_eq!(kept[&c], good[&c], "part {damaged} damaged");
        }
    }
}
