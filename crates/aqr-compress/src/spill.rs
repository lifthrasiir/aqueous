//! The spill: files beside the output that large buffers of streams are regions of
//! ([`aqr_formula::store`]), written as they are made (positioned writes) and read through
//! mappings read only, so that what compressing a large file holds (the data of a WIM file, the
//! formulas of its contents) is in the system's cache of the files, written out and dropped by
//! the system as memory runs short, rather than all held by the process
//! (`docs/streaming-encoder-design.md`).
//!
//! A file is a segment of the regions, `PREFIX._$pill$PID.N` (the spill's prefix, the process's
//! id, the segment's number; aqr's prefix is `.` and the output's name, beside it), sparse,
//! closed once they are all dropped, and gone once closed: deleted on close (Windows, hidden and
//! not indexed until then), or unlinked as it is made (Unix). A region of a known length is taken
//! of the free runs of the segments (the smallest that fits; the bytes of a region dropped are
//! taken again by those after it, so that the spill takes about the space of the regions held,
//! not of all made); one larger than half a segment has a segment of its own, the rest of it
//! free once it is written. (A stream of a length not known takes regions growing as it does:
//! `aqr_formula::store::Writer`.) A segment starts with `MAGIC`, so that
//! one left behind is not taken for a file of another kind. The bytes of a region dropped are
//! given back to the system ([`aqr_common::pages::discard`]: a hole punched in the file).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use aqr_formula::Bytes;
use aqr_formula::store::StoreWriter;
use memmap2::{Mmap, MmapOptions};

/// Bytes of a segment, a file and a mapping each; a region larger is a segment of its own.
const SEGMENT: usize = 256 << 20;
/// Segments are of multiples of this (the mappings' granularity on Windows), the regions in
/// them at multiples of [`REGION_ALIGN`].
const ALIGN: usize = 64 << 10;
const REGION_ALIGN: usize = 16;
/// The bytes a segment starts with (`aQr5pil/` in base64), then zeros to [`HEADER`] (a page),
/// its regions after them.
const MAGIC: [u8; 6] = [0x69, 0x0a, 0xf9, 0xa6, 0x29, 0x7f];
const HEADER: usize = 4 << 10;
/// Bytes of the segment's own copied at once through the heap ([`Segment::maps`]).
const BOUNCE: usize = 1 << 20;

pub struct Spill {
    /// The path the files' names extend.
    prefix: PathBuf,
    state: Arc<Mutex<State>>,
}

#[derive(Default)]
struct State {
    /// The segments made.
    made: u64,
    /// The segment made last for regions, held (not closed while it has none, so that one
    /// region taken and dropped after another does not make a file each time).
    open: Option<Arc<Segment>>,
    /// The segments regions are taken from, by number.
    segments: HashMap<u64, Weak<Segment>>,
    /// The free runs of bytes of those segments, by segment and start: their lengths.
    free: BTreeMap<(u64, usize), usize>,
    /// The same by length, then segment and start.
    by_len: BTreeSet<(usize, u64, usize)>,
    /// The regions held, by address, for [`Store::find`](aqr_formula::store::Store::find).
    regions: BTreeMap<usize, Weak<View>>,
}

impl State {
    fn insert(&mut self, id: u64, at: usize, len: usize) {
        self.free.insert((id, at), len);
        self.by_len.insert((len, id, at));
    }

    fn remove(&mut self, id: u64, at: usize) -> usize {
        let len = self.free.remove(&(id, at)).expect("a free run");
        self.by_len.remove(&(len, id, at));
        len
    }

    /// Bytes `at..at + len` of segment `id` free again, joined to the free runs beside them:
    /// the run they are of now.
    fn release(&mut self, id: u64, mut at: usize, mut len: usize) -> (usize, usize) {
        if let Some((&(i, before), &n)) = self.free.range(..(id, at)).next_back()
            && i == id
            && before + n == at
        {
            self.remove(id, before);
            (at, len) = (before, n + len);
        }
        if self.free.contains_key(&(id, at + len)) {
            len += self.remove(id, at + len);
        }
        self.insert(id, at, len);
        (at, len)
    }

    /// `n` bytes of a segment taken from its free runs, the smallest that fits: the segment,
    /// and where they start.
    fn take(&mut self, n: usize) -> Option<(Arc<Segment>, usize)> {
        loop {
            let &(len, id, at) = self.by_len.range((n, 0, 0)..).next()?;
            self.remove(id, at);
            if len > n {
                self.insert(id, at + n, len - n);
            }
            // (one dropped is not taken from: it drops its runs as soon as it has the state)
            let Some(segment) = self.segments.get(&id).and_then(Weak::upgrade) else { continue };
            return Some((segment, at));
        }
    }
}

struct Segment {
    /// The mapping, read only, kept for its bytes (only through `base`), and its file.
    _map: Mmap,
    file: File,
    base: *const u8,
    len: usize,
    /// Its number, and the state of the spill, whose free runs and regions it is of.
    id: u64,
    state: Weak<Mutex<State>>,
}

// (each region of a segment is written by its writer alone, before it is read)
unsafe impl Send for Segment {}
unsafe impl Sync for Segment {}

impl Segment {
    fn at(&self, offset: usize) -> *const u8 {
        debug_assert!(offset <= self.len);
        // SAFETY: within the mapping
        unsafe { self.base.add(offset) }
    }

    /// Whether `d` is of the segment's mapping. Its bytes are not written to its file from
    /// there: on macOS (APFS) a write whose pages are those of its source, mapped of the same
    /// file, waits for itself for good (uninterruptible, not even killed), the bytes apart or
    /// not, and regions side by side share pages. (Seen as a run of `aqr -c` at 0% CPU, in
    /// `pwrite`; reproduced in C with one thread, 64 KiB written after 64 KiB of the file
    /// mapped, their pages shared.)
    fn maps(&self, d: &[u8]) -> bool {
        let (base, at) = (self.base as usize, d.as_ptr() as usize);
        !d.is_empty() && at < base + self.len && base < at + d.len()
    }
}

impl Drop for Segment {
    fn drop(&mut self) {
        // (never dropped with the state held: see [`Spill::writer`])
        let Some(state) = self.state.upgrade() else { return };
        let mut state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.segments.remove(&self.id).is_some() {
            let runs: Vec<usize> = state.free.range((self.id, 0)..(self.id + 1, 0)).map(|(k, _)| k.1).collect();
            runs.into_iter().for_each(|at| _ = state.remove(self.id, at));
        }
    }
}

/// A region of a segment, as bytes.
struct View {
    segment: Arc<Segment>,
    start: usize,
    len: usize,
    /// Its bytes counted by what took it, for `--stats`.
    _counted: aqr_common::trace::Outside,
}

impl AsRef<[u8]> for View {
    fn as_ref(&self) -> &[u8] {
        // SAFETY: a region of the mapping, written before it is read
        unsafe { std::slice::from_raw_parts(self.segment.at(self.start), self.len) }
    }
}

impl Drop for View {
    fn drop(&mut self) {
        // (its segment dropped after the state is let go)
        let Some(state) = self.segment.state.upgrade() else { return };
        let mut state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.regions.remove(&(self.segment.at(self.start) as usize));
        if state.segments.contains_key(&self.segment.id) {
            let len = self.len.next_multiple_of(REGION_ALIGN);
            let (run, run_len) = state.release(self.segment.id, self.start, len);
            // the pages of the free run it is of now that it has bytes of given back (with the
            // state held, so that no region takes them meanwhile): else they stay in the file and
            // the system's cache until the segment goes. (Not all of the run: those of the
            // regions dropped before it were given back as they were, and giving back a run of
            // many small regions as each goes would take time as the square of their number.)
            let lo = (self.start / ALIGN * ALIGN).max(run);
            let hi = (self.start + len).next_multiple_of(ALIGN).min(run + run_len);
            let s = &self.segment;
            // SAFETY: within a free run of the segment's mapping, of all of its file: no
            // region's bytes, and those of one taken of it after are written before they are read
            unsafe { aqr_common::pages::discard(&s.file, s.base, s.at(lo), hi - lo) };
        }
    }
}

/// A region being written: of its segment, where it starts, the bytes it may take and those
/// written; and whether it is of a segment of its own.
struct Region {
    segment: Arc<Segment>,
    start: usize,
    cap: usize,
    len: usize,
    own: bool,
}

impl StoreWriter for Region {
    fn write(&mut self, d: &[u8]) -> bool {
        if self.len + d.len() > self.cap {
            return false;
        }
        let at = (self.start + self.len) as u64;
        let written = if self.segment.maps(d) {
            // (bytes of its own segment: through the heap, see [`Segment::maps`])
            let mut copy = Vec::with_capacity(d.len().min(BOUNCE));
            d.chunks(BOUNCE).enumerate().try_for_each(|(i, c)| {
                copy.clear();
                copy.extend_from_slice(c);
                aqr_common::fsio::write_all_at(&self.segment.file, &copy, at + (i * BOUNCE) as u64)
            })
        } else {
            aqr_common::fsio::write_all_at(&self.segment.file, d, at)
        };
        if written.is_err() {
            return false;
        }
        self.len += d.len();
        true
    }

    fn finish(self: Box<Self>) -> Bytes {
        let Region { segment, start, cap, len, own } = *self;
        let state = segment.state.upgrade().expect("the spill of a region being written");
        let mut state = state.lock().unwrap();
        // the rest of it free (of a segment of its own: of the segment, which regions are taken of
        // from now on)
        let used = len.next_multiple_of(REGION_ALIGN);
        if own {
            state.segments.insert(segment.id, Arc::downgrade(&segment));
        }
        if used < cap {
            state.release(segment.id, start + used, cap - used);
        }
        let at = segment.at(start) as usize;
        let view = Arc::new(View { segment, start, len, _counted: aqr_common::trace::outside(len) });
        state.regions.insert(at, Arc::downgrade(&view));
        drop(state);
        Bytes::shared(view)
    }
}

impl Spill {
    /// The spill of files made as `prefix._$pill$PID.N` (none until a region is taken), in the
    /// directory of `prefix`.
    pub fn create(prefix: &Path) -> io::Result<Spill> {
        let dir = prefix.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
        if prefix.file_name().is_none() || !dir.is_dir() {
            return Err(io::Error::new(io::ErrorKind::NotADirectory, "the spill's directory"));
        }
        Ok(Spill { prefix: prefix.to_owned(), state: Arc::default() })
    }

    /// A new segment of `n` bytes (the magic, and regions to fill the rest), a file of its own
    /// (of the next number not taken), sparse.
    fn grow(&self, state: &mut State, n: usize) -> io::Result<Arc<Segment>> {
        let mut o = OpenOptions::new();
        o.read(true).write(true).create_new(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // FILE_FLAG_DELETE_ON_CLOSE; shared to read, write and delete; hidden (from Explorer
            // while it is there) and not indexed (Windows Search would read the segments)
            o.custom_flags(0x0400_0000).share_mode(7).attributes(0x2 | 0x2000);
        }
        let (file, path, id) = loop {
            let mut path = OsString::from(&self.prefix);
            path.push(format!("._$pill${}.{}", std::process::id(), state.made));
            let path = PathBuf::from(path);
            state.made += 1;
            match o.open(&path) {
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                r => break (r?, path, state.made - 1),
            }
        };
        if cfg!(unix) {
            std::fs::remove_file(path)?;
        }
        sparse(&file)?;
        file.set_len(n as u64)?;
        aqr_common::fsio::write_all_at(&file, &MAGIC, 0)?;
        // SAFETY: the file is this segment's alone, written only by the positioned writes of its
        // regions' writers (each its own bytes), read by its regions after
        let map = unsafe { MmapOptions::new().len(n).map(&file)? };
        let base = map.as_ptr();
        let state_of = Arc::downgrade(&self.state);
        Ok(Arc::new(Segment { _map: map, file, base, len: n, id, state: state_of }))
    }
}

impl aqr_formula::store::Store for Spill {
    fn writer(&self, n: usize) -> Option<Box<dyn StoreWriter>> {
        let mut state = self.state.lock().unwrap();
        // (the segment made before, if it is let go, dropped once the state is)
        let mut before = None;
        let (segment, start, cap, own) = if n > SEGMENT / 2 {
            let size = HEADER.checked_add(n)?.next_multiple_of(ALIGN);
            (self.grow(&mut state, size).ok()?, HEADER, size - HEADER, true)
        } else {
            let need = n.max(1).next_multiple_of(REGION_ALIGN);
            match state.take(need) {
                Some((s, at)) => (s, at, need, false),
                None => {
                    let s = self.grow(&mut state, SEGMENT).ok()?;
                    state.segments.insert(s.id, Arc::downgrade(&s));
                    state.insert(s.id, HEADER + need, SEGMENT - HEADER - need);
                    before = state.open.replace(s.clone());
                    (s, HEADER, need, false)
                }
            }
        };
        drop(state);
        drop(before);
        Some(Box::new(Region { segment, start, cap, len: 0, own }))
    }

    fn find(&self, d: &[u8]) -> Option<Bytes> {
        let at = d.as_ptr() as usize;
        let view = self.state.lock().unwrap().regions.range(..=at).next_back()?.1.upgrade()?;
        let start = at - (*view).as_ref().as_ptr() as usize;
        // (sharing a region holds all of it: a copy where `d` is not most of it)
        (start + d.len() <= view.len && d.len() * 2 >= view.len)
            .then(|| Bytes::shared(view).slice(start..start + d.len()))
    }
}

/// `file` made sparse, so that its bytes not written take no room: as it is already (Unix's
/// file systems), or told so (Windows: `FSCTL_SET_SPARSE`).
#[cfg(unix)]
fn sparse(_: &File) -> io::Result<()> {
    Ok(())
}

#[cfg(windows)]
fn sparse(f: &File) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    unsafe extern "system" {
        fn DeviceIoControl(
            device: *mut core::ffi::c_void,
            code: u32,
            input: *const core::ffi::c_void,
            input_len: u32,
            output: *mut core::ffi::c_void,
            output_len: u32,
            returned: *mut u32,
            overlapped: *mut core::ffi::c_void,
        ) -> i32;
    }
    const FSCTL_SET_SPARSE: u32 = 0x0009_00c4;
    let mut returned = 0;
    // SAFETY: the file's own handle, no buffers, and a place for the bytes returned
    let ok = unsafe {
        DeviceIoControl(
            f.as_raw_handle().cast(),
            FSCTL_SET_SPARSE,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

#[cfg(test)]
mod tests {
    use aqr_formula::store::Store;

    use super::*;

    /// A region of `n` bytes of `fill`, of `spill`: where its bytes are, and its bytes.
    fn take(spill: &Spill, n: usize, fill: u8) -> (usize, Bytes) {
        let mut w = spill.writer(n).unwrap();
        assert!(w.write(&vec![fill; n]));
        let b = w.finish();
        assert!(b.len() == n && b.iter().all(|&x| x == fill));
        (b.as_ptr().addr(), b)
    }

    /// The bytes of a region dropped are taken again by the regions after it (the smallest free
    /// run that fits), and runs side by side are joined: regions dropped out of turn take no
    /// more segments. A region takes no more than what is written of it.
    #[test]
    fn taken_again() {
        let dir = std::env::temp_dir().join(format!("aqr-compress-taken-again-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let spill = Spill::create(&dir.join("again")).unwrap();
        let (a, ra) = take(&spill, 100 << 10, 1);
        let (b, rb) = take(&spill, 10 << 10, 2);
        let (c, rc) = take(&spill, 200 << 10, 3);
        assert_eq!((b - a, c - b), (100 << 10, 10 << 10));
        // (the smallest run that fits: `b`'s, not the rest of the segment)
        drop(rb);
        let (e, re) = take(&spill, 8 << 10, 4);
        assert_eq!(e, b);
        // (the rest of `b`'s, `c`'s and the rest of the segment joined, `a`'s too small)
        drop((ra, rc));
        let (d, rd) = take(&spill, 150 << 10, 5);
        assert_eq!(d, b + (8 << 10));
        assert_eq!(take(&spill, 100 << 10, 6).0, a);
        // (of 1000 bytes told, 10 written, more than told refused: the rest free for the next)
        let mut w = spill.writer(1000).unwrap();
        assert!(w.write(&[7; 10]) && !w.write(&[7; 999]));
        let (small, rs) = (w.finish(), take(&spill, 16, 8));
        assert_eq!((small.len(), rs.0 - small.as_ptr().addr()), (10, 16));
        // hundreds of regions taken and dropped in turn take one segment
        let mut held = vec![rd, re];
        for i in 0..400 {
            held.push(take(&spill, (256 << 10) + i * 2048, i as u8).1);
            if i % 3 != 0 {
                held.swap_remove(held.len() / 2);
            }
        }
        assert_eq!(spill.state.lock().unwrap().made, 1);
        drop((held, small, rs, spill));
        std::fs::remove_dir(&dir).unwrap();
    }

    /// A region larger than half a segment is of a segment of its own, the rest of which regions
    /// after it are taken of.
    #[test]
    fn large() {
        let dir = std::env::temp_dir().join(format!("aqr-compress-large-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let spill = Spill::create(&dir.join("large")).unwrap();
        let mut w = spill.writer(320 << 20).unwrap();
        let piece: Vec<u8> = (0..(3 << 20) + 7).map(|i| (i * 13) as u8).collect();
        for _ in 0..100 {
            assert!(w.write(&piece));
        }
        let b = w.finish();
        assert!(b.len() == 100 * piece.len() && b.chunks(piece.len()).all(|c| c == &piece[..]));
        let (at, small) = take(&spill, 4096, 9);
        assert_eq!(at, b.as_ptr().addr() + b.len().next_multiple_of(REGION_ALIGN));
        assert_eq!(spill.state.lock().unwrap().made, 1);
        drop((b, small, spill));
        std::fs::remove_dir(&dir).unwrap();
    }

    /// The pages of a region dropped are given back to the system, those of a region held kept
    /// (Linux: told by `mincore`, which tells of the file's pages in the system's cache).
    #[cfg(target_os = "linux")]
    #[test]
    fn dropped_given_back() {
        let dir = std::env::temp_dir().join(format!("aqr-compress-given-back-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let spill = Spill::create(&dir.join("back")).unwrap();
        let page = aqr_common::pages::size();
        // the pages of `n` bytes at `at` resident
        let resident = |at: usize, n: usize| {
            let mut v = vec![0u8; n.div_ceil(page)];
            // SAFETY: pages of a mapping of the spill, and a byte for each
            assert_eq!(unsafe { libc::mincore(at as *mut libc::c_void, n, v.as_mut_ptr()) }, 0);
            v.iter().filter(|&&b| b & 1 != 0).count()
        };
        let n = 64 * page;
        let (a, ra) = take(&spill, n, 0xff);
        let (b, rb) = take(&spill, n, 0xff);
        assert_eq!((resident(a, n), resident(b, n)), (64, 64));
        drop(ra);
        // (ext4 keeps the last page of a hole in its cache: of the file, not the process's)
        assert!(resident(a, n) <= 1 && resident(b, n) == 64);
        drop((rb, spill));
        std::fs::remove_dir(&dir).unwrap();
    }

    /// A region written with bytes of a region of the same segment, not read yet, sharing a
    /// page with it (regions side by side): copied, not waiting for good (macOS: see
    /// [`Segment::maps`]). Run on a thread of its own, so that a write that waits fails the
    /// test rather than holding it.
    #[test]
    fn of_its_own_segment() {
        let dir = std::env::temp_dir().join(format!("aqr-compress-own-segment-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let spill = Arc::new(Spill::create(&dir.join("own")).unwrap());
        let n = 40000;
        let data: Vec<u8> = (0..n).map(|i| (i * 7) as u8).collect();
        let mut w = spill.writer(n).unwrap();
        assert!(w.write(&data));
        let ra = w.finish();
        let (tx, rx) = std::sync::mpsc::channel();
        let s = spill.clone();
        std::thread::spawn(move || {
            let mut w = s.writer(n).unwrap();
            assert!(w.write(&ra));
            tx.send((ra, w.finish())).unwrap();
        });
        let (ra, b) = rx.recv_timeout(std::time::Duration::from_secs(20)).expect("the write waits for good");
        assert_eq!(b.as_ptr().addr(), ra.as_ptr().addr() + n.next_multiple_of(REGION_ALIGN));
        assert!(ra[..] == data[..] && b[..] == data[..]);
        drop((ra, b, spill));
        std::fs::remove_dir(&dir).unwrap();
    }

    /// Every segment starts with the magic (`aQr5pil/` in base64) and zeros to a page, its regions
    /// after them.
    #[test]
    fn magic() {
        let dir = std::env::temp_dir().join(format!("aqr-compress-magic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let spill = Spill::create(&dir.join("magic")).unwrap();
        // SAFETY: the start of the mapping, longer than its header
        let head = |s: &Segment| unsafe { std::slice::from_raw_parts(s.base, 4096) }.to_vec();
        let mut magic = vec![0; 4096];
        magic[..6].copy_from_slice(&[0x69, 0x0a, 0xf9, 0xa6, 0x29, 0x7f]);
        let segment = spill.grow(&mut spill.state.lock().unwrap(), SEGMENT).unwrap();
        assert!(head(&segment) == magic);
        drop(segment);
        let (at, bytes) = take(&spill, 100, 0xff);
        assert!(at % 4096 == 0);
        let segment = spill.state.lock().unwrap().open.clone().unwrap();
        assert!(head(&segment) == magic);
        // (on Windows the file goes as the spill closes it)
        drop((segment, bytes, spill));
        std::fs::remove_dir(&dir).unwrap();
    }
}
