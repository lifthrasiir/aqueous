//! The spill: files beside the output that large buffers of streams are regions of
//! ([`aqr_formula::store`]), mapped, so that what compressing a large file holds (the data of
//! a WIM file, the formulas of its contents) is written out and dropped by the system as
//! memory runs short rather than all held (`docs/streaming-encoder-design.md`). A file is a
//! segment of the regions, `PREFIX._$pill$PID.N` (the spill's prefix, the process's id, the
//! segment's number; aqr's prefix is `.` and the output's name, beside it), closed once they
//! are all dropped, and gone once closed: deleted on close (Windows, hidden and not indexed
//! until then), or unlinked as it is made (Unix). The bytes of a region dropped are taken
//! again for regions after it (the smallest free run that fits), so the spill takes about the
//! space of the regions held, not of all made, even where they are dropped out of turn. A
//! segment starts with [`MAGIC`], so that one left behind is not taken for a file of another
//! kind.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use aqr_formula::Bytes;
use memmap2::{MmapMut, MmapOptions};

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
    /// The segments regions are taken from, by number: and the end of the bytes ever taken of
    /// each (zeros after it, as the file was made).
    segments: HashMap<u64, (Weak<Segment>, usize)>,
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

    /// Bytes `at..at + len` of segment `id` free again, joined to the free runs beside them.
    fn release(&mut self, id: u64, mut at: usize, mut len: usize) {
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
    }

    /// `n` bytes of a segment taken from its free runs, the smallest that fits: the segment,
    /// where they start, and whether they were taken before (not zeros).
    fn take(&mut self, n: usize) -> Option<(Arc<Segment>, usize, bool)> {
        loop {
            let &(len, id, at) = self.by_len.range((n, 0, 0)..).next()?;
            self.remove(id, at);
            if len > n {
                self.insert(id, at + n, len - n);
            }
            // (one dropped is not taken from: it drops its runs as soon as it has the state)
            let Some((segment, top)) = self.segments.get_mut(&id) else { continue };
            let Some(segment) = segment.upgrade() else { continue };
            let used = at < *top;
            *top = (*top).max(at + n);
            return Some((segment, at, used));
        }
    }
}

struct Segment {
    /// The mapping, kept for its bytes (only through `base`), then its file (dropped after it).
    _map: MmapMut,
    _file: File,
    base: *mut u8,
    len: usize,
    /// Its number, and the state of the spill, whose free runs and regions it is of.
    id: u64,
    state: Weak<Mutex<State>>,
}

// (each region of a segment is written by its buffer alone, before it is read)
unsafe impl Send for Segment {}
unsafe impl Sync for Segment {}

impl Segment {
    fn at(&self, offset: usize) -> *mut u8 {
        debug_assert!(offset <= self.len);
        // SAFETY: within the mapping
        unsafe { self.base.add(offset) }
    }
}

impl Drop for Segment {
    fn drop(&mut self) {
        // (never dropped with the state held: see [`Spill::region`])
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
            state.release(self.segment.id, self.start, self.len.next_multiple_of(REGION_ALIGN));
        }
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
    /// (of the next number not taken).
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
        file.set_len(n as u64)?;
        // SAFETY: the file is this segment's alone
        let mut map = unsafe { MmapOptions::new().len(n).map_mut(&file)? };
        map[..MAGIC.len()].copy_from_slice(&MAGIC);
        let base = map.as_mut_ptr();
        let state_of = Arc::downgrade(&self.state);
        Ok(Arc::new(Segment { _map: map, _file: file, base, len: n, id, state: state_of }))
    }
}

impl aqr_formula::store::Store for Spill {
    fn region(&self, n: usize) -> Option<(*mut u8, Bytes)> {
        let need = n.next_multiple_of(REGION_ALIGN);
        let mut state = self.state.lock().unwrap();
        // (the segment made before, if it is let go, dropped once the state is)
        let mut before = None;
        let (segment, start, used) = if n > SEGMENT / 2 {
            (self.grow(&mut state, (HEADER + n).next_multiple_of(ALIGN)).ok()?, HEADER, false)
        } else {
            match state.take(need) {
                Some(taken) => taken,
                None => {
                    let s = self.grow(&mut state, SEGMENT).ok()?;
                    state.segments.insert(s.id, (Arc::downgrade(&s), HEADER + need));
                    state.insert(s.id, HEADER + need, SEGMENT - HEADER - need);
                    before = state.open.replace(s.clone());
                    (s, HEADER, false)
                }
            }
        };
        let at = segment.at(start);
        let view = Arc::new(View { segment, start, len: n, _counted: aqr_common::trace::outside(n) });
        state.regions.insert(at as usize, Arc::downgrade(&view));
        drop(state);
        drop(before);
        if used {
            // SAFETY: the region's bytes, no one else's
            unsafe { std::ptr::write_bytes(at, 0, n) };
        }
        Some((at, Bytes::shared(view)))
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

#[cfg(test)]
mod tests {
    use aqr_formula::store::Store;

    use super::*;

    /// The bytes of a region dropped are taken again, zeroed, by the regions after it (the
    /// smallest free run that fits), and runs side by side are joined: regions dropped out of
    /// turn take no more segments.
    #[test]
    fn taken_again() {
        let dir = std::env::temp_dir().join(format!("aqr-compress-taken-again-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let spill = Spill::create(&dir.join("again")).unwrap();
        let take = |n: usize| {
            let (at, bytes) = spill.region(n).unwrap();
            assert!(bytes.iter().all(|&b| b == 0));
            // SAFETY: the region's bytes, not read before
            unsafe { std::ptr::write_bytes(at, 0xff, n) };
            (at.addr(), bytes)
        };
        let (a, ra) = take(100 << 10);
        let (b, rb) = take(10 << 10);
        let (c, rc) = take(200 << 10);
        assert_eq!((b - a, c - b), (100 << 10, 10 << 10));
        // (the smallest run that fits: `b`'s, not the rest of the segment)
        drop(rb);
        let (e, re) = take(8 << 10);
        assert_eq!(e, b);
        // (the rest of `b`'s, `c`'s and the rest of the segment joined, `a`'s too small)
        drop((ra, rc));
        let (d, rd) = take(150 << 10);
        assert_eq!(d, b + (8 << 10));
        assert_eq!(take(100 << 10).0, a);
        // hundreds of regions taken and dropped in turn take one segment
        let mut held = vec![rd, re];
        for i in 0..400 {
            held.push(take((256 << 10) + i * 2048).1);
            if i % 3 != 0 {
                held.swap_remove(held.len() / 2);
            }
        }
        assert_eq!(spill.state.lock().unwrap().made, 1);
        drop((held, spill));
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
        let (at, bytes) = spill.region(100).unwrap();
        assert!(at.addr() % 4096 == 0);
        // SAFETY: the region's bytes, not read before (the magic kept, as no region overlaps it)
        unsafe { std::ptr::write_bytes(at, 0xff, 100) };
        assert!(bytes.iter().all(|&b| b == 0xff));
        let segment = spill.state.lock().unwrap().open.clone().unwrap();
        assert!(head(&segment) == magic);
        // (on Windows the file goes as the spill closes it)
        drop((segment, bytes, spill));
        std::fs::remove_dir(&dir).unwrap();
    }
}
