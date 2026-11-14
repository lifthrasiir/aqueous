//! The map of counted blocks by address that the allocator observers keep ([`crate::allowance`],
//! [`crate::meter`], [`crate::trace`]), and the order they are told of a reallocation in.

use std::collections::BTreeMap;
use std::sync::Mutex;

const SHARDS: usize = 16;

/// Blocks by address in sixteen maps, so that threads allocating at once seldom share a lock.
/// Its nodes are allocations themselves: what takes it must not count them again (the observers'
/// own guards), and the lock survives a panic of another holder.
///
/// `LOW` is the bit the address is hashed from: the bits above the blocks' alignment.
pub(crate) struct Blocks<V, const LOW: u32> {
    shards: [Mutex<BTreeMap<usize, V>>; SHARDS],
}

impl<V, const LOW: u32> Blocks<V, LOW> {
    pub(crate) const fn new() -> Self {
        Blocks { shards: [const { Mutex::new(BTreeMap::new()) }; SHARDS] }
    }

    fn shard(&self, p: usize) -> &Mutex<BTreeMap<usize, V>> {
        &self.shards[((p >> LOW) ^ (p >> 20)) % SHARDS]
    }

    /// Notes block `p`; the value of one noted there before, if any.
    pub(crate) fn insert(&self, p: usize, v: V) -> Option<V> {
        self.shard(p).lock().unwrap_or_else(|e| e.into_inner()).insert(p, v)
    }

    /// Forgets block `p`, giving what was noted of it.
    pub(crate) fn remove(&self, p: usize) -> Option<V> {
        self.shard(p).lock().unwrap_or_else(|e| e.into_inner()).remove(&p)
    }
}

/// Block `p` of `n` bytes, now `q` of `m`, told as `freed` and `allocated` would be: in place,
/// the old one first; moved, both held at the moment of the copy.
#[inline(always)]
pub(crate) fn reallocated(
    (p, n): (*mut u8, usize),
    (q, m): (*mut u8, usize),
    allocated: impl FnOnce(*mut u8, usize),
    freed: impl FnOnce(*mut u8, usize),
) {
    if q.is_null() {
        return;
    }
    if p == q {
        freed(p, n);
        allocated(q, m);
    } else {
        allocated(q, m);
        freed(p, n);
    }
}

/// A new strong count of the `Arc` that `p` came from (`Arc::into_raw`, or `Arc::as_ptr` of one
/// kept alive meanwhile).
///
/// # Safety
/// `p` points into a live `Arc<T>` with a strong count that stays while this runs.
pub(crate) unsafe fn clone_raw<T>(p: *const T) -> std::sync::Arc<T> {
    // SAFETY: as the caller says
    unsafe {
        std::sync::Arc::increment_strong_count(p);
        std::sync::Arc::from_raw(p)
    }
}
