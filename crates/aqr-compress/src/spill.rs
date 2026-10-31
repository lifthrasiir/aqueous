//! The spill: files beside the output that large buffers of streams are regions of
//! ([`aqr_formula::store`]), mapped, so that what compressing a large file holds (the data of
//! a WIM file, the formulas of its contents) is written out and dropped by the system as
//! memory runs short rather than all held (`docs/streaming-encoder-design.md`). A file is a
//! segment of the regions, `._$pill$PID.N` (the process's id, the segment's number), closed
//! once they are all dropped, and gone once closed: deleted on close (Windows), or unlinked as
//! it is made (Unix). So the spill takes the space of the regions held, not of all made. A
//! segment starts with [`MAGIC`], so that one left behind is not taken for a file of another
//! kind.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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
    dir: PathBuf,
    state: Mutex<State>,
}

struct State {
    /// The segments made.
    made: u64,
    /// The segment regions are taken from, and its bytes taken.
    open: Option<(Arc<Segment>, usize)>,
}

struct Segment {
    /// The mapping, kept for its bytes (only through `base`), then its file (dropped after it).
    _map: MmapMut,
    _file: File,
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
    /// The spill of files made in directory `dir` (none until a region is taken).
    pub fn create(dir: &Path) -> io::Result<Spill> {
        if !dir.is_dir() {
            return Err(io::Error::new(io::ErrorKind::NotADirectory, "the spill's directory"));
        }
        Ok(Spill { dir: dir.to_owned(), state: Mutex::new(State { made: 0, open: None }) })
    }

    /// A new segment of `n` bytes (the magic, and regions to fill the rest), a file of its own
    /// (of the next number not taken).
    fn grow(&self, state: &mut State, n: usize) -> io::Result<Arc<Segment>> {
        let mut o = OpenOptions::new();
        o.read(true).write(true).create_new(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // FILE_FLAG_DELETE_ON_CLOSE; shared to read, write and delete
            o.custom_flags(0x0400_0000).share_mode(7);
        }
        let (file, path) = loop {
            let path = self.dir.join(format!("._$pill${}.{}", std::process::id(), state.made));
            state.made += 1;
            match o.open(&path) {
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                r => break (r?, path),
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
        Ok(Arc::new(Segment { _map: map, _file: file, base, len: n }))
    }
}

impl aqr_formula::store::Store for Spill {
    fn region(&self, n: usize) -> Option<(*mut u8, Bytes)> {
        let mut state = self.state.lock().unwrap();
        let (segment, start) = if n > SEGMENT / 2 {
            (self.grow(&mut state, (HEADER + n).next_multiple_of(ALIGN)).ok()?, HEADER)
        } else {
            match &mut state.open {
                Some((s, used)) if *used + n <= s.len => {
                    let start = *used;
                    *used = (*used + n).next_multiple_of(REGION_ALIGN);
                    (s.clone(), start)
                }
                _ => {
                    let s = self.grow(&mut state, SEGMENT).ok()?;
                    state.open = Some((s.clone(), (HEADER + n).next_multiple_of(REGION_ALIGN)));
                    (s, HEADER)
                }
            }
        };
        let at = segment.at(start);
        Some((at, Bytes::shared(Arc::new(View { segment, start, len: n }))))
    }
}

#[cfg(test)]
mod tests {
    use aqr_formula::store::Store;

    use super::*;

    /// Every segment starts with the magic (`aQr5pil/` in base64) and zeros to a page, its regions
    /// after them.
    #[test]
    fn magic() {
        let dir = std::env::temp_dir().join(format!("aqr-compress-magic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let spill = Spill::create(&dir).unwrap();
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
        let segment = spill.state.lock().unwrap().open.as_ref().unwrap().0.clone();
        assert!(head(&segment) == magic);
        drop((segment, bytes));
        std::fs::remove_dir(&dir).unwrap();
    }
}
