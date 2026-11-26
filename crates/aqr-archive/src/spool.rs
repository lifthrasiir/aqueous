//! Bytes queued in order: in memory up to a bound, the rest in a file of their own (in the
//! system's temporary directory, gone once closed) until they are taken, so that what a queue
//! holds takes no more memory than its bound however much is put in it ahead of being taken.

use std::collections::VecDeque;
use std::fs::File;
use std::io;
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};

/// Pieces of the memory held at most this long, the bytes put in joined into them, and read
/// of the file at most this long.
const PIECE: usize = 1 << 20;

pub(super) struct Spool {
    mem: VecDeque<Vec<u8>>,
    in_mem: usize,
    cap: usize,
    /// The file, and the bytes written to it and read of it: those between are queued after
    /// the memory's.
    file: Option<File>,
    written: u64,
    read: u64,
}

impl Spool {
    /// A spool of at most `cap` bytes in memory.
    pub(super) fn new(cap: usize) -> Spool {
        Spool { mem: VecDeque::new(), in_mem: 0, cap, file: None, written: 0, read: 0 }
    }

    /// The bytes queued.
    pub(super) fn len(&self) -> u64 {
        self.in_mem as u64 + (self.written - self.read)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// `d` queued after what is.
    pub(super) fn push(&mut self, d: &[u8]) -> io::Result<()> {
        // (after the file's bytes where it has any, for them to be taken in order)
        if self.written == self.read && self.in_mem + d.len() <= self.cap {
            self.in_mem += d.len();
            match self.mem.back_mut() {
                Some(last) if last.len() + d.len() <= PIECE => last.extend_from_slice(d),
                _ => self.mem.push_back(d.to_vec()),
            }
            return Ok(());
        }
        let f = match &mut self.file {
            Some(f) => f,
            None => self.file.insert(spool_file()?),
        };
        aqr_common::fsio::write_all_at(f, d, self.written)?;
        self.written += d.len() as u64;
        Ok(())
    }

    /// The first bytes queued, taken: none where there are none.
    pub(super) fn pop(&mut self) -> io::Result<Option<Vec<u8>>> {
        if let Some(p) = self.mem.pop_front() {
            self.in_mem -= p.len();
            return Ok(Some(p));
        }
        if self.read == self.written {
            return Ok(None);
        }
        let n = (self.written - self.read).min(PIECE as u64) as usize;
        let mut p = vec![0; n];
        aqr_common::fsio::read_exact_at(self.file.as_ref().expect("a file of bytes written"), &mut p, self.read)?;
        self.read += n as u64;
        if self.read == self.written {
            // (all of it taken: the file's space given back, and the memory taken again)
            (self.read, self.written) = (0, 0);
            self.file.as_ref().expect("a file").set_len(0)?;
        }
        Ok(Some(p))
    }
}

/// The files made of spools, for tests.
#[cfg(test)]
pub(super) static FILES: AtomicU64 = AtomicU64::new(0);

/// A file of its own in the system's temporary directory, gone once closed
/// ([`aqr_common::fsio::temp_file`]).
fn spool_file() -> io::Result<File> {
    #[cfg(test)]
    FILES.fetch_add(1, Ordering::Relaxed);
    aqr_common::fsio::temp_file("aqr-spool")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A spool's file is the owner's alone while it has a name (the system's directory is
    /// shared).
    #[cfg(unix)]
    #[test]
    fn file_of_the_owner() {
        use std::os::unix::fs::PermissionsExt;
        let f = spool_file().unwrap();
        assert_eq!(f.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    }

    /// What is put beyond the memory's bound goes to the file and comes back in order, pushes
    /// and pops interleaved; once all is taken, the memory takes it again.
    #[test]
    fn in_order() {
        let mut s = Spool::new(10_000);
        let (mut put, mut got) = (Vec::new(), Vec::new());
        let mut next = 0u32;
        let mut piece = |n: usize| {
            let d: Vec<u8> = (0..n)
                .map(|_| {
                    next += 1;
                    (next * 7) as u8
                })
                .collect();
            d
        };
        for round in 0..50 {
            for _ in 0..round % 7 + 1 {
                let d = piece(3000 + round * 100);
                s.push(&d).unwrap();
                put.extend_from_slice(&d);
            }
            for _ in 0..round % 5 {
                if let Some(p) = s.pop().unwrap() {
                    got.extend_from_slice(&p);
                }
            }
        }
        assert!(s.file.is_some());
        while let Some(p) = s.pop().unwrap() {
            got.extend_from_slice(&p);
        }
        assert!(got == put && s.is_empty());
        assert_eq!((s.read, s.written), (0, 0));
        s.push(b"again").unwrap();
        assert_eq!((s.in_mem, s.written), (5, 0));
        assert_eq!(s.pop().unwrap().as_deref(), Some(&b"again"[..]));
    }
}
