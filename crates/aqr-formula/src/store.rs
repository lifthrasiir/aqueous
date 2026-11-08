//! Where large buffers of streams are made: vectors of the heap by default; with a [`Store`]
//! set (aqr-compress's spill, files beside the output), regions of it, written as they are made
//! and read through a mapping read only, so that the system writes their pages out and drops
//! them as memory runs short rather than the process holding them all.

use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use crate::Bytes;

/// Bytes a buffer is of at least to be taken from the store; smaller ones stay on the heap.
pub const MIN: usize = 4 << 10;

/// Bytes a [`Writer`] of a length not told gathers on the heap before it takes a region of the
/// store: one finished within them is moved to the store as a region of its length, of the files
/// shared by all. (Most such streams are short: an archive's entries of a kind,
/// [`crate::ByKind`].)
pub const UNTOLD: usize = 8 << 20;

/// How much larger a region a [`Writer`] of a length not told takes when the one it has is full:
/// this many times the bytes it has, copied there, so that the bytes copied are a third of the
/// stream's at most and its region this many times its length at most. (A region of all a stream
/// might grow to made a file of 64 GiB each; on Windows a mapping takes commit by its length,
/// and some hundreds of them ran it out.)
pub const GROW: usize = 4;

/// Regions of bytes outside the heap.
pub trait Store: Send + Sync {
    /// A region to write of `len` bytes at most. None where there is no room.
    fn writer(&self, len: usize) -> Option<Box<dyn StoreWriter>>;

    /// `d` as bytes of the store, sharing them, where it is within a region of it still held
    /// (and the store would rather share the region than have it copied).
    fn find(&self, _d: &[u8]) -> Option<Bytes> {
        None
    }
}

/// A region of a store being written ([`Store::writer`]).
pub trait StoreWriter: Send {
    /// `d` written after what is: false where the region has no room for it (nothing written
    /// of it).
    fn write(&mut self, d: &[u8]) -> bool;

    /// The bytes written, as the store holds them.
    fn finish(self: Box<Self>) -> Bytes;
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

/// A buffer of a stream's bytes being made, written anywhere: of the heap, moved to the store
/// where it is large and one is set as it is made bytes. (Bytes made in order are better made by
/// a [`Writer`], which holds none of them.)
pub struct Buffer(Vec<u8>);

impl Buffer {
    /// A buffer of `n` bytes, zeroed.
    pub fn new(n: usize) -> Buffer {
        Buffer(vec![0; n])
    }

    /// Its first `n` bytes only.
    pub fn truncate(&mut self, n: usize) {
        self.0.truncate(n);
    }

    pub fn into_bytes(self) -> Bytes {
        let mut b: Bytes = self.0.into();
        spill(&mut b);
        b
    }
}

impl Deref for Buffer {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.0
    }
}

impl DerefMut for Buffer {
    fn deref_mut(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

/// Bytes of a stream made in order: written to the store as they come where one is set (and
/// they are not told to be few; of a length not told, past [`UNTOLD`] bytes, in regions growing
/// by [`GROW`]), else gathered on the heap. A stream of a formula is pushed of it as it is
/// (`Formula::push(class, writer)`), so that a recompressor making a long stream holds none of
/// it.
pub struct Writer {
    repr: WriterRepr,
    /// Of a length not told.
    untold: bool,
}

enum WriterRepr {
    Heap(Vec<u8>),
    /// A region of the store, and the bytes it takes.
    Stored(Box<dyn StoreWriter>, usize),
}

/// A region of the store of `GROW` times `need` bytes (and of [`UNTOLD`] at least), `d` written
/// to it first.
fn grown(d: &[u8], need: usize) -> Option<WriterRepr> {
    let cap = need.max(UNTOLD).checked_mul(GROW)?;
    let mut w = store()?.writer(cap)?;
    w.write(d).then_some(WriterRepr::Stored(w, cap))
}

impl Writer {
    /// A writer of `len` bytes at most where told.
    pub fn new(len: Option<usize>) -> Writer {
        let stored = match len {
            Some(n) if n >= MIN => store().and_then(|s| s.writer(n)).map(|w| WriterRepr::Stored(w, n)),
            _ => None,
        };
        let repr = stored.unwrap_or_else(|| WriterRepr::Heap(Vec::with_capacity(len.unwrap_or(0))));
        Writer { repr, untold: len.is_none() }
    }

    pub fn write(&mut self, d: &[u8]) {
        // (as a stream is made, a recompression over its heap gives up: `aqr_common::allowance`)
        aqr_common::allowance::check();
        match &mut self.repr {
            WriterRepr::Heap(v) => {
                // (of a length not told and long now: a region of the store, the bytes so far
                // first; else on the heap)
                let need = v.len() + d.len();
                if self.untold
                    && need > UNTOLD
                    && let Some(r) = grown(v, need)
                {
                    self.repr = r;
                    return self.write(d);
                }
                // (the store refusing it: not asked again)
                self.untold &= need <= UNTOLD;
                v.extend_from_slice(d)
            }
            WriterRepr::Stored(w, cap) => {
                if !w.write(d) {
                    let (cap, WriterRepr::Stored(w, _)) =
                        (*cap, std::mem::replace(&mut self.repr, WriterRepr::Heap(Vec::new())))
                    else {
                        unreachable!("stored")
                    };
                    let had = w.finish();
                    // (no room in its region: of a length not told, one larger, the bytes so far
                    // first; else the rest on the heap, after what it has)
                    if self.untold
                        && let Some(r) = grown(&had, cap.max(had.len() + d.len()))
                    {
                        self.repr = r;
                        drop(had);
                        return self.write(d);
                    }
                    self.untold = false;
                    let mut v = had.to_vec();
                    v.extend_from_slice(d);
                    self.repr = WriterRepr::Heap(v);
                }
            }
        }
    }

    pub fn into_bytes(self) -> Bytes {
        match self.repr {
            WriterRepr::Heap(v) if self.untold => {
                let mut b = v.into();
                spill(&mut b);
                b
            }
            WriterRepr::Heap(v) => v.into(),
            WriterRepr::Stored(w, _) => w.finish(),
        }
    }
}

impl From<Writer> for Bytes {
    fn from(w: Writer) -> Bytes {
        w.into_bytes()
    }
}

impl std::io::Write for Writer {
    fn write(&mut self, d: &[u8]) -> std::io::Result<usize> {
        Writer::write(self, d);
        Ok(d.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// `b` moved to the store where it is of the heap and large and a store is set (copied, its
/// heap let go), so that what a formula keeps is not held in memory.
pub fn spill(b: &mut Bytes) {
    if b.is_shared() || b.sunk_in().is_some() || b.is_lazy() || b.len() < MIN {
        return;
    }
    let Some(mut w) = store().and_then(|s| s.writer(b.len())) else { return };
    if w.write(b) {
        *b = w.finish();
    }
}

/// Bytes shared already (files mapped, say) that [`find`] shares rather than copies, while
/// their guards are kept, by the guards' ids: and whether they are of a file mapped
/// ([`known_mapped`]).
static KNOWN: RwLock<Vec<(u64, Bytes, bool)>> = RwLock::new(Vec::new());

/// Has [`find`] share bytes within `b` (where shared: its own are not) until the guard is
/// dropped: the input, so that what is cut out of it is not copied.
pub fn known(b: &Bytes) -> Known {
    register(b, false)
}

/// [`known`] of `b`, a file mapped read only, whose pages [`let_go`] drops out of the resident
/// set once they are read through.
pub fn known_mapped(b: &Bytes) -> Known {
    register(b, true)
}

fn register(b: &Bytes, mapped: bool) -> Known {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    if b.is_shared() && !b.is_empty() {
        KNOWN.write().unwrap().push((id, b.clone(), mapped));
    }
    Known(id)
}

/// Drops the pages of `d` out of the resident set where it is within a file mapped
/// ([`known_mapped`]; else nothing): read through and not needed soon, read again from the file
/// where they are ([`aqr_common::pages::let_go`]).
pub fn let_go(d: &[u8]) {
    let_go_at(d.as_ptr() as usize, d.len());
}

/// [`let_go`] of the `len` bytes at address `at` (which may be of bytes let go of already: then
/// not within a file mapped).
pub fn let_go_at(at: usize, len: usize) {
    let known = KNOWN.read().unwrap();
    let within = known.iter().any(|(_, b, mapped)| {
        *mapped && at.checked_sub(b.as_ptr() as usize).is_some_and(|start| start + len <= b.len())
    });
    drop(known);
    if within {
        // SAFETY: bytes of a file mapped read only, as `known_mapped` was told
        unsafe { aqr_common::pages::let_go(at as *const u8, len) };
    }
}

pub struct Known(u64);

impl Drop for Known {
    fn drop(&mut self) {
        KNOWN.write().unwrap().retain(|k| k.0 != self.0);
    }
}

/// `d` as bytes shared rather than a copy, where it is within bytes [`known`], or within a
/// region of the store still held (a stream spilled, say) and the store would rather share it.
pub fn find(d: &[u8]) -> Option<Bytes> {
    if d.len() < MIN {
        return None;
    }
    let at = d.as_ptr() as usize;
    let known = KNOWN.read().unwrap().iter().find_map(|(_, b, _)| {
        let start = at.checked_sub(b.as_ptr() as usize)?;
        (start + d.len() <= b.len()).then(|| b.slice(start..start + d.len()))
    });
    known.or_else(|| store()?.find(d))
}

/// [`spill`] of every stream of `f`.
pub fn spill_streams(f: &mut crate::Formula) {
    f.streams.iter_mut().for_each(|s| spill(&mut s.data));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store of regions on the heap, of `cap` bytes at most each.
    struct Small {
        cap: usize,
    }

    struct SmallWriter(Vec<u8>, usize);

    impl StoreWriter for SmallWriter {
        fn write(&mut self, d: &[u8]) -> bool {
            if self.0.len() + d.len() > self.1 {
                return false;
            }
            self.0.extend_from_slice(d);
            true
        }

        fn finish(self: Box<Self>) -> Bytes {
            Bytes::shared(Arc::new(self.0))
        }
    }

    impl Store for Small {
        fn writer(&self, len: usize) -> Option<Box<dyn StoreWriter>> {
            Some(Box::new(SmallWriter(Vec::new(), len.min(self.cap))))
        }
    }

    /// A writer writes to the store as it goes, and the rest to the heap where its region has no
    /// room; a formula's stream is pushed of it as it is.
    #[test]
    fn written_in_order() {
        let _g = set(Some(Arc::new(Small { cap: 10_000 })));
        let piece: Vec<u8> = (0..3000u32).map(|i| i as u8).collect();
        let mut w = Writer::new(None);
        for _ in 0..3 {
            w.write(&piece);
        }
        let b = w.into_bytes();
        assert!(b.is_shared() && b.len() == 9000);
        // (past the store's room: the rest on the heap, after what it has)
        let mut w = Writer::new(None);
        for _ in 0..5 {
            std::io::Write::write_all(&mut w, &piece).unwrap();
        }
        let mut f = crate::Formula::default();
        f.root = f.push("a", w);
        assert!(f.streams[0].data == piece.repeat(5) && !f.streams[0].data.is_shared());
        // (few bytes told: the heap's)
        let mut w = Writer::new(Some(100));
        w.write(&piece[..100]);
        assert!(!w.into_bytes().is_shared());
    }
}
