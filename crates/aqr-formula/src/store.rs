//! Where large buffers of streams are made: vectors of the heap by default; with a [`Store`]
//! set (aqr-compress's spill, a file beside the output), regions of it, mapped, whose pages
//! the system writes out and drops as memory runs short, rather than holding them all.

use std::ops::{Deref, DerefMut};
use std::sync::{Arc, RwLock};

use crate::Bytes;

/// Bytes a buffer is of at least to be taken from the store; smaller ones stay on the heap.
pub const MIN: usize = 4 << 10;

/// Regions of bytes outside the heap.
pub trait Store: Send + Sync {
    /// A region of `n` bytes, zeroed: where to write it, and its bytes as the store holds them
    /// (not to be read until written). None where there is no room.
    fn region(&self, n: usize) -> Option<(*mut u8, Bytes)>;
}

static STORE: RwLock<Option<Arc<dyn Store>>> = RwLock::new(None);

/// Sets `store` as the store of the process until the guard is dropped (the one before back).
pub fn set(store: Option<Arc<dyn Store>>) -> Guard {
    Guard(std::mem::replace(&mut *STORE.write().unwrap(), store))
}

pub struct Guard(Option<Arc<dyn Store>>);

impl Drop for Guard {
    fn drop(&mut self) {
        *STORE.write().unwrap() = self.0.take();
    }
}

fn store() -> Option<Arc<dyn Store>> {
    STORE.read().unwrap().clone()
}

/// A buffer of a stream's bytes being made: of the store where it is large and one is set,
/// else of the heap.
pub struct Buffer(Repr);

enum Repr {
    Heap(Vec<u8>),
    Stored { at: *mut u8, len: usize, bytes: Bytes },
}

// (the region is the buffer's alone until it is made bytes)
unsafe impl Send for Buffer {}
unsafe impl Sync for Buffer {}

impl Buffer {
    /// A buffer of `n` bytes, zeroed.
    pub fn new(n: usize) -> Buffer {
        if n >= MIN
            && let Some((at, bytes)) = store().and_then(|s| s.region(n))
        {
            return Buffer(Repr::Stored { at, len: n, bytes });
        }
        Buffer(Repr::Heap(vec![0; n]))
    }

    /// Its first `n` bytes only.
    pub fn truncate(&mut self, n: usize) {
        match &mut self.0 {
            Repr::Heap(v) => v.truncate(n),
            Repr::Stored { len, .. } => *len = (*len).min(n),
        }
    }

    pub fn into_bytes(self) -> Bytes {
        match self.0 {
            Repr::Heap(v) => v.into(),
            Repr::Stored { len, bytes, .. } => bytes.prefix(len),
        }
    }
}

impl Deref for Buffer {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match &self.0 {
            Repr::Heap(v) => v,
            // SAFETY: the region of the store, of at least `len` bytes, is this buffer's
            &Repr::Stored { at, len, .. } => unsafe { std::slice::from_raw_parts(at, len) },
        }
    }
}

impl DerefMut for Buffer {
    fn deref_mut(&mut self) -> &mut [u8] {
        match &mut self.0 {
            Repr::Heap(v) => v,
            // SAFETY: as above, and no one else writes it or reads it until it is made bytes
            &mut Repr::Stored { at, len, .. } => unsafe { std::slice::from_raw_parts_mut(at, len) },
        }
    }
}

/// `b` moved to the store where it is of the heap and large and a store is set (copied, its
/// heap let go), so that what a formula keeps is not held in memory.
pub fn spill(b: &mut Bytes) {
    if b.is_shared() || b.len() < MIN {
        return;
    }
    let Some((at, bytes)) = store().and_then(|s| s.region(b.len())) else { return };
    // SAFETY: the region is of `b.len()` bytes and no one else's
    unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), at, b.len()) };
    *b = bytes;
}

/// [`spill`] of every stream of `f`.
pub fn spill_streams(f: &mut crate::Formula) {
    f.streams.iter_mut().for_each(|s| spill(&mut s.data));
}
