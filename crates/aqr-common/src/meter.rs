//! The heap of a module's application measured natively, as the memory its instances would take
//! in WebAssembly: what the encoder declares as the application's memory limits (the instance's,
//! and a task's of each class) where it does not run the module as WebAssembly.
//!
//! A [`Meter`] counts the bytes of the blocks allocated under it and their peak. The calling
//! thread is under one while [`enter`]'s guard lives; the pool's workers are under the meter of
//! what they run for (`pool` passes it on, as WebAssembly runs that work in the instance), and
//! each item of `pool::par_map_apart` (a task in WebAssembly) is under a meter of its own
//! ([`task`]), which hands what it still holds at its end (its result) to the one above, as
//! `aqr_take` copies the result into the instance. Blocks of at least [`TRACKED`] bytes are
//! told back to the meter that allocated them as they are freed, on whatever thread; smaller
//! ones to the meter of the thread freeing them.
//!
//! None of it costs anything but a load of a flag while no meter is in use. The allocators of
//! this crate ([`crate::mem::Mapped`], [`crate::mem::Counting`]) tell it of their blocks.

use std::cell::Cell;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex};

use crate::blocks::{Blocks, clone_raw};

pub use crate::plain::Measured;

/// Blocks of at least this many bytes are told back to the meter that allocated them.
pub const TRACKED: usize = 4 << 10;

/// Bytes allocated under it, held and at most.
pub struct Meter {
    cur: AtomicI64,
    peak: AtomicI64,
    /// What it learned of its tasks.
    tasks: Mutex<Measured>,
    /// Where it ended (a task's): the meter its blocks freed after are told to.
    into: Mutex<Option<Arc<Meter>>>,
}

impl Meter {
    fn new() -> Arc<Meter> {
        Arc::new(Meter {
            cur: AtomicI64::new(0),
            peak: AtomicI64::new(0),
            tasks: Mutex::new(Measured::default()),
            into: Mutex::new(None),
        })
    }

    /// The meter that holds its bytes now: itself, or the one it ended into.
    fn live(self: &Arc<Meter>) -> Arc<Meter> {
        let mut m = self.clone();
        loop {
            let next = m.into.lock().unwrap().clone();
            match next {
                Some(n) => m = n,
                None => return m,
            }
        }
    }

    fn add(&self, n: i64) {
        let c = self.cur.fetch_add(n, Relaxed) + n;
        self.peak.fetch_max(c, Relaxed);
    }

    fn sub(&self, n: i64) {
        // (not below nothing: a block of another freed under it is not its own)
        let _ = self.cur.try_update(Relaxed, Relaxed, |c| Some((c - n).max(0)));
    }

    /// What it has seen so far.
    pub fn measured(&self) -> Measured {
        Measured { peak: self.peak.load(Relaxed).max(0) as u64, ..*self.tasks.lock().unwrap() }
    }
}

/// Meters in use: while none is, the allocators do nothing more.
static IN_USE: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// The meter of the calling thread (a leaked strong count while it is set: see `enter`).
    static CURRENT: Cell<*const Meter> = const { Cell::new(std::ptr::null()) };
}

/// The meter of the calling thread, if any.
pub fn current() -> Option<Arc<Meter>> {
    let p = CURRENT.try_with(Cell::get).ok()?;
    if p.is_null() {
        return None;
    }
    // SAFETY: a meter set by `enter`, whose guard holds a strong count
    Some(unsafe { clone_raw(p) })
}

/// The calling thread under `m` (nothing where `None`) until the guard is dropped.
#[must_use = "the meter is the thread's until the guard is dropped"]
pub struct Entered {
    m: Option<Arc<Meter>>,
    prev: *const Meter,
}

pub fn enter(m: Option<Arc<Meter>>) -> Entered {
    let Some(m) = m else { return Entered { m: None, prev: std::ptr::null() } };
    IN_USE.fetch_add(1, Relaxed);
    let prev = CURRENT.with(|c| c.replace(Arc::as_ptr(&m)));
    Entered { m: Some(m), prev }
}

impl Drop for Entered {
    fn drop(&mut self) {
        if self.m.take().is_some() {
            CURRENT.with(|c| c.set(self.prev));
            IN_USE.fetch_sub(1, Relaxed);
        }
    }
}

/// `f` under a new meter of its own (an application of a module), and what it saw.
pub fn measure<R>(f: impl FnOnce() -> R) -> (R, Measured) {
    let m = Meter::new();
    let r = {
        let _e = enter(Some(m.clone()));
        f()
    };
    let measured = m.measured();
    (r, measured)
}

/// `f` as a task of the calling thread's meter, if any: under a meter of its own, which hands
/// what it holds at the end to the one above. Without a meter, `f` alone.
pub fn task<R>(f: impl FnOnce() -> R) -> R {
    let Some(parent) = current() else { return f() };
    let start = parent.peak.load(Relaxed).max(0) as u64;
    let m = Meter::new();
    let r = {
        let _e = enter(Some(m.clone()));
        f()
    };
    let own = m.peak.load(Relaxed).max(0) as u64;
    // what it holds now goes to the parent, as its blocks freed from now on
    let held = m.cur.swap(0, Relaxed);
    *m.into.lock().unwrap() = Some(parent.clone());
    parent.add(held);
    let mut t = parent.tasks.lock().unwrap();
    // (and what its own tasks were)
    let inner = *m.tasks.lock().unwrap();
    t.task = t.task.max(own).max(inner.task);
    t.task_with_copy = t.task_with_copy.max(start + own).max(inner.task_with_copy);
    t.tasks += 1 + inner.tasks;
    r
}

// The blocks, as the allocators tell of them.

/// The tracked blocks by address: their meter (a strong count) and size.
static BLOCKS: Blocks<(usize, usize), 12> = Blocks::new();

thread_local! {
    /// Set while the meter's own bookkeeping allocates (the map), which is not counted.
    static BUSY: Cell<bool> = const { Cell::new(false) };
}

/// `f` with the bookkeeping's own allocations not counted; nothing where it is already within.
fn quiet(f: impl FnOnce()) {
    if BUSY.try_with(|b| b.replace(true)).unwrap_or(true) {
        return;
    }
    f();
    _ = BUSY.try_with(|b| b.set(false));
}

#[inline]
pub(crate) fn allocated(p: *mut u8, n: usize) {
    if IN_USE.load(Relaxed) == 0 || p.is_null() {
        return;
    }
    allocated_slow(p as usize, n);
}

#[cold]
fn allocated_slow(p: usize, n: usize) {
    let Some(m) = current() else { return };
    quiet(|| {
        m.add(n as i64);
        if n >= TRACKED {
            let m = Arc::into_raw(m) as usize;
            match BLOCKS.insert(p, (m, n)) {
                // SAFETY: a strong count the map held
                Some(old) => drop(unsafe { Arc::from_raw(old.0 as *const Meter) }),
                None => _ = BLOCKS_HELD.fetch_add(1, Relaxed),
            }
        }
    });
}

#[inline]
pub(crate) fn freed(p: *mut u8, n: usize) {
    // (a block tracked may be freed after its meter is no longer in use)
    if IN_USE.load(Relaxed) == 0 && (n < TRACKED || BLOCKS_HELD.load(Relaxed) == 0) {
        return;
    }
    freed_slow(p as usize, n);
}

/// Blocks tracked, not freed yet.
static BLOCKS_HELD: AtomicUsize = AtomicUsize::new(0);

#[cold]
fn freed_slow(p: usize, n: usize) {
    quiet(|| {
        if n >= TRACKED {
            let owner = BLOCKS.remove(p);
            if let Some((m, n)) = owner {
                BLOCKS_HELD.fetch_sub(1, Relaxed);
                // SAFETY: the strong count the map held
                let m = unsafe { Arc::from_raw(m as *const Meter) };
                m.live().sub(n as i64);
                return;
            }
        }
        if let Some(m) = current() {
            m.sub(n as i64);
        }
    });
}

#[inline]
pub(crate) fn reallocated(p: *mut u8, n: usize, q: *mut u8, m: usize) {
    // (both blocks held at the moment of the copy, as a growing vector's are in WebAssembly)
    crate::blocks::reallocated((p, n), (q, m), allocated, freed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tasks_hand_back() {
        // (the test binary's allocator is the system's: tell the meter by hand)
        let (_, m) = measure(|| {
            let me = current().unwrap();
            me.add(100);
            task(|| current().unwrap().add(1000));
            task(|| {
                let t = current().unwrap();
                t.add(50);
                t.sub(50);
            });
            me.sub(100);
        });
        assert_eq!(m.tasks, 2);
        assert_eq!(m.task, 1000);
        // (the second task copies the instance as it is then: 1100 at most, the first's handed back)
        assert_eq!(m.task_with_copy, 1150);
        // (the first task's 1000 handed back: 100 + 1000 at once)
        assert_eq!(m.peak, 1100);
    }
}
