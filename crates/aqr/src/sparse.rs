//! Files written sparse: runs of zeros of whole units of [`UNIT`] bytes (at offsets of a multiple
//! of it), [`HOLE`] bytes at least, are left as holes rather than written; so is a run of zeros
//! to the end of the file of that length.
//!
//! What holes cost was measured on APFS, ext4 and NTFS: some 25–55 µs each to make (flushed),
//! against writing zeros at 1–3 GB/s, so that a hole pays for itself in time from 25–300 KB.
//! NTFS frees whole units of 64 KiB only, and a file marked sparse without a hole flushes
//! slower, so a file is marked sparse at its first hole. APFS fills holes skipped over (the
//! first 512 or so of a file), so they are punched once the file reaches past them. A file
//! system without holes (FAT, exFAT) fills them with zeros itself, as any file extended does.

use std::fs::File;
use std::io;
use std::ops::Range;

/// The bytes a hole is made of: NTFS's sparse unit, and a multiple of a page (16 KiB on Apple
/// silicon, under which holes slow reads).
pub const UNIT: usize = 64 << 10;

/// The least bytes of a hole: runs of zeros shorter are written.
pub const HOLE: u64 = 256 << 10;

/// The most bytes of data held before they are written.
const BUFFER: usize = 1 << 20;

/// A file written from its start, sparse ([`Sparse::finish`] ends it).
pub struct Sparse<'f> {
    f: &'f File,
    /// Where `out` goes in the file.
    at: u64,
    /// Data to write at `at`.
    out: Vec<u8>,
    /// Bytes of zeros after `out`, of whole units, neither written nor left as a hole yet.
    zeros: u64,
    /// The bytes of a unit begun after them.
    head: Vec<u8>,
    /// Holes left that the file does not reach past yet (to be punched where holes are punched).
    holes: Vec<Range<u64>>,
    /// Whether the file is marked sparse (where files are).
    marked: bool,
}

impl<'f> Sparse<'f> {
    /// Writing `f` (empty) sparse.
    pub fn new(f: &'f File) -> Self {
        Sparse { f, at: 0, out: Vec::new(), zeros: 0, head: Vec::new(), holes: Vec::new(), marked: false }
    }

    /// Writes `d` after what was written.
    pub fn write(&mut self, mut d: &[u8]) -> io::Result<()> {
        if !self.head.is_empty() {
            let n = (UNIT - self.head.len()).min(d.len());
            self.head.extend_from_slice(&d[..n]);
            d = &d[n..];
            if self.head.len() < UNIT {
                return Ok(());
            }
            let head = std::mem::take(&mut self.head);
            self.units(&head)?;
            self.head = head;
            self.head.clear();
        }
        let whole = d.len() / UNIT * UNIT;
        self.units(&d[..whole])?;
        self.head.extend_from_slice(&d[whole..]);
        Ok(())
    }

    /// Ends the file: what is held written, and its length set (where it ends in a hole).
    pub fn finish(mut self) -> io::Result<()> {
        let head = std::mem::take(&mut self.head);
        match zero(&head) && self.zeros + head.len() as u64 >= HOLE {
            // (zeros to the end: a hole, of a unit not whole at its end)
            true => self.zeros += head.len() as u64,
            false => self.data(&head)?,
        }
        self.settle()?;
        self.flush()?;
        if !self.holes.is_empty() {
            self.f.set_len(self.at)?;
            self.punch();
        }
        Ok(())
    }

    /// Takes `d`, of whole units: its runs of data written, its units of zeros counted.
    fn units(&mut self, d: &[u8]) -> io::Result<()> {
        let mut run = 0;
        for (k, u) in d.as_chunks::<UNIT>().0.iter().enumerate() {
            if zero(u) {
                if run < k * UNIT {
                    self.data(&d[run..k * UNIT])?;
                }
                run = (k + 1) * UNIT;
                self.zeros += UNIT as u64;
            }
        }
        if run < d.len() {
            self.data(&d[run..])?;
        }
        Ok(())
    }

    /// Writes data `d` after the zeros counted.
    fn data(&mut self, d: &[u8]) -> io::Result<()> {
        self.settle()?;
        if self.out.len() + d.len() > BUFFER {
            self.flush()?;
        }
        if d.len() < BUFFER {
            self.out.extend_from_slice(d);
            return Ok(());
        }
        write_at(self.f, self.at, d)?;
        self.at += d.len() as u64;
        self.punch();
        Ok(())
    }

    /// The zeros counted made a hole, or written where too few.
    fn settle(&mut self) -> io::Result<()> {
        let n = std::mem::take(&mut self.zeros);
        if n == 0 {
            return Ok(());
        }
        if n < HOLE {
            self.out.resize(self.out.len() + n as usize, 0);
            return Ok(());
        }
        self.flush()?;
        if !self.marked {
            self.marked = true;
            mark(self.f);
        }
        self.holes.push(self.at..self.at + n);
        self.at += n;
        Ok(())
    }

    /// Writes the data held.
    fn flush(&mut self) -> io::Result<()> {
        if self.out.is_empty() {
            return Ok(());
        }
        write_at(self.f, self.at, &self.out)?;
        self.at += self.out.len() as u64;
        self.out.clear();
        self.punch();
        Ok(())
    }

    /// Punches the holes left, the file reaching past them (where holes are punched).
    fn punch(&mut self) {
        for h in self.holes.drain(..) {
            punch(self.f, h);
        }
    }
}

/// Writes `d` to `f` (empty) sparse.
pub fn write_all(f: &File, d: &[u8]) -> io::Result<()> {
    let mut s = Sparse::new(f);
    s.write(d)?;
    s.finish()
}

/// Whether `d` is all zeros (an OR of bytes at a time, which vectorizes; told by spans, to stop
/// early at data).
fn zero(d: &[u8]) -> bool {
    d.chunks(512).all(|c| c.iter().fold(0, |s, &b| s | b) == 0)
}

fn write_at(f: &File, at: u64, d: &[u8]) -> io::Result<()> {
    aqr_common::fsio::write_all_at(f, d, at)
}

/// Marks `f` sparse, where files are (NTFS). Where it fails, the holes are written with zeros by
/// the file system.
fn mark(_f: &File) {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::IO::DeviceIoControl;
        use windows_sys::Win32::System::Ioctl::FSCTL_SET_SPARSE;
        let mut n = 0u32;
        // SAFETY: a handle of a file held; no buffers
        unsafe {
            DeviceIoControl(
                _f.as_raw_handle(),
                FSCTL_SET_SPARSE,
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
                0,
                &mut n,
                std::ptr::null_mut(),
            )
        };
    }
}

/// Punches hole `h` of `f` (APFS: holes skipped over are filled otherwise), from its start (of a
/// whole unit) to the end of its last whole block. Where it fails, the hole is as it is.
fn punch(_f: &File, _h: Range<u64>) {
    #[cfg(target_vendor = "apple")]
    {
        use std::os::fd::AsRawFd;
        let end = _h.end / 4096 * 4096;
        if end <= _h.start {
            return;
        }
        let p = libc::fpunchhole_t {
            fp_flags: 0,
            reserved: 0,
            fp_offset: _h.start as i64,
            fp_length: (end - _h.start) as i64,
        };
        // SAFETY: a descriptor of a file held, and the argument F_PUNCHHOLE takes
        unsafe { libc::fcntl(_f.as_raw_fd(), libc::F_PUNCHHOLE, &p) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Data, zeros and data of lengths `parts` (alternately, data first), given in pieces of
    /// `piece` bytes: the file the same bytes.
    fn round(parts: &[usize], piece: usize) {
        let mut d = Vec::new();
        for (k, &n) in parts.iter().enumerate() {
            match k % 2 {
                0 => d.extend((0..n).map(|i| (i % 251) as u8 | 1)),
                _ => d.resize(d.len() + n, 0),
            }
        }
        let p = std::env::temp_dir().join(format!("aqr-sparse-{}-{piece}-{}", std::process::id(), parts.len()));
        let f = File::create(&p).unwrap();
        let mut s = Sparse::new(&f);
        for c in d.chunks(piece) {
            s.write(c).unwrap();
        }
        s.finish().unwrap();
        drop(f);
        let back = std::fs::read(&p).unwrap();
        std::fs::remove_file(&p).unwrap();
        assert!(back == d, "{parts:?} in pieces of {piece}: other bytes");
    }

    #[test]
    fn same_bytes() {
        const U: usize = UNIT;
        let cases: &[&[usize]] = &[
            &[],
            &[0, 1],
            &[0, 5 * U],
            &[1, 5 * U],
            &[10, 4 * U + 7],
            &[U, 4 * U, U],
            &[100, 3 * U, 5, 8 * U, 3, 4 * U - 1, 2, 6 * U],
            &[3 * U + 1, 2 * U, 1, 10 * U, U / 2],
            &[0, 20 * U, 2 * BUFFER + 3, 20 * U, 1],
        ];
        for parts in cases {
            for piece in [1 << 30, 1000, U, U + 1, 3 * U / 2] {
                round(parts, piece);
            }
        }
    }
}
