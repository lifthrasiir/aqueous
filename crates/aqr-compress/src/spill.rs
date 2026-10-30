//! The spill: a file beside the output that large buffers of streams are regions of
//! ([`aqr_formula::store`]), mapped, so that what compressing a large file holds (the data of
//! a WIM file, the formulas of its contents) is written out and dropped by the system as
//! memory runs short rather than all held (`docs/streaming-encoder-design.md`). The file is
//! gone once closed: deleted on close (Windows), or unlinked as it is made (Unix).

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};

use aqr_formula::Bytes;
use memmap2::{MmapMut, MmapOptions};

/// Bytes the file grows by at a time, a mapping each; a region larger is a mapping of its own.
const SEGMENT: usize = 256 << 20;
/// Segments start at multiples of this (the mappings' granularity on Windows), the regions in
/// them at multiples of [`REGION_ALIGN`].
const ALIGN: usize = 64 << 10;
const REGION_ALIGN: usize = 16;

pub struct Spill {
    file: File,
    state: Mutex<State>,
}

struct State {
    len: u64,
    /// The segment regions are taken from, and its bytes taken.
    open: Option<(Arc<Segment>, usize)>,
}

struct Segment {
    /// The mapping, kept for its bytes (only through `base`).
    _map: MmapMut,
    base: *mut u8,
    len: usize,
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

/// A region of a segment, as bytes.
struct View {
    segment: Arc<Segment>,
    start: usize,
    len: usize,
}

impl AsRef<[u8]> for View {
    fn as_ref(&self) -> &[u8] {
        // SAFETY: a region of the mapping, written before it is read
        unsafe { std::slice::from_raw_parts(self.segment.at(self.start), self.len) }
    }
}

impl Spill {
    /// The spill at `path`, a file made there anew.
    pub fn create(path: &Path) -> io::Result<Spill> {
        let mut o = OpenOptions::new();
        o.read(true).write(true).create_new(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // FILE_FLAG_DELETE_ON_CLOSE; shared to read, write and delete
            o.custom_flags(0x0400_0000).share_mode(7);
        }
        let file = o.open(path)?;
        #[cfg(unix)]
        std::fs::remove_file(path)?;
        Ok(Spill { file, state: Mutex::new(State { len: 0, open: None }) })
    }

    /// A new segment of `n` bytes at the end of the file.
    fn grow(&self, state: &mut State, n: usize) -> io::Result<Arc<Segment>> {
        let at = state.len;
        self.file.set_len(at + n as u64)?;
        // SAFETY: the file is this spill's alone, and grows only
        let mut map = unsafe { MmapOptions::new().offset(at).len(n).map_mut(&self.file)? };
        state.len += n as u64;
        let base = map.as_mut_ptr();
        Ok(Arc::new(Segment { _map: map, base, len: n }))
    }
}

impl aqr_formula::store::Store for Spill {
    fn region(&self, n: usize) -> Option<(*mut u8, Bytes)> {
        let mut state = self.state.lock().unwrap();
        let (segment, start) = if n > SEGMENT / 2 {
            (self.grow(&mut state, n.next_multiple_of(ALIGN)).ok()?, 0)
        } else {
            match &mut state.open {
                Some((s, used)) if *used + n <= s.len => {
                    let start = *used;
                    *used = (*used + n).next_multiple_of(REGION_ALIGN);
                    (s.clone(), start)
                }
                _ => {
                    let s = self.grow(&mut state, SEGMENT).ok()?;
                    state.open = Some((s.clone(), n.next_multiple_of(REGION_ALIGN)));
                    (s, 0)
                }
            }
        };
        let at = segment.at(start);
        Some((at, Bytes::shared(Arc::new(View { segment, start, len: n }))))
    }
}
