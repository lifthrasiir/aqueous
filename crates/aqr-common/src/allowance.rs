//! The heap an attempt may take ([`attempt`]: a recompression of some data, with what it runs on
//! the pool), and what makes it give up past it ([`check`]).
//!
//! An attempt's allocations are counted to its [`Allowance`]: those of the calling thread while
//! it runs, and of what it moves onto workers of the pool (`pool` passes the allowance on, as it
//! does [`crate::meter`]'s). Blocks of at least [`crate::mem::LARGE_MIN`] bytes are told back to
//! the allowance that allocated them as they are freed, on whatever thread and however late;
//! smaller ones are counted to the allowance of the thread allocating or freeing them, a
//! thread's own share gathered until it is [`FLUSH`] bytes either way (or the thread moves to
//! another allowance), so that they do not all take one counter.
//!
//! The allocator only counts: it must not unwind. An attempt over its allowance gives up at the
//! next [`check`] of a thread working for it, which unwinds (with [`Exceeded`], not a panic: no
//! hook runs) to the attempt: the pool checks as each item and job starts, and so do
//! `aqr_common::progress` as work is told done and `aqr_formula::store::Writer` as bytes are
//! written. What runs between checks may pass the allowance by what it allocates meanwhile.
//!
//! Attempts within an attempt (the contents of a container, recompressed by its recompressor)
//! share the outermost one's allowance: the innermost attempt running gives up first, and the
//! outer one goes on where that brought it back within its allowance. Attempts running at once
//! also give up where the large blocks of the process ([`crate::mem::large_in_use`]) pass the
//! allowance and they hold more than their share of it (the allowance over the outermost
//! attempts running), so that together they keep to about one allowance.
//!
//! Nothing of it costs anything but the load of a flag while no attempt runs.

use std::any::Any;
use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering::Relaxed};

use crate::blocks::{Blocks, clone_raw};
use crate::mem::LARGE_MIN;

pub use crate::plain::Exceeded;

/// Bytes of small blocks a thread gathers of its own before it counts them to its allowance.
const FLUSH: i64 = 256 << 10;

/// The heap held by an attempt and its limit.
struct Allowance {
    used: AtomicI64,
    limit: u64,
}

impl Allowance {
    fn new(limit: u64) -> Arc<Allowance> {
        Arc::new(Allowance { used: AtomicI64::new(0), limit })
    }

    fn add(&self, n: i64) {
        self.used.fetch_add(n, Relaxed);
    }

    /// The bytes it holds (as counted so far: the threads' small blocks not yet told aside).
    fn used(&self) -> u64 {
        self.used.load(Relaxed).max(0) as u64
    }

    /// Whether it is to give up: over its limit, or of more than its share of it while the
    /// large blocks of the process are.
    fn over(&self) -> bool {
        let used = self.used();
        if used > self.limit {
            return true;
        }
        let running = RUNNING.load(Relaxed).max(1) as u64;
        running > 1 && crate::mem::large_in_use() > self.limit && used > self.limit / running
    }
}

/// Allowances entered on some thread now: while none is, the allocators do nothing more.
static IN_USE: AtomicUsize = AtomicUsize::new(0);
/// Outermost attempts running.
static RUNNING: AtomicUsize = AtomicUsize::new(0);

/// What a thread counts to: its allowance (a strong count while it is set: see [`enter`]), the
/// bytes of small blocks it gathered for it, and whether a check unwinds for it (only within
/// [`attempt`], which catches it).
struct Local {
    cur: Cell<*const Allowance>,
    pending: Cell<i64>,
    armed: Cell<bool>,
}

thread_local! {
    static LOCAL: Local = const {
        Local { cur: Cell::new(std::ptr::null()), pending: Cell::new(0), armed: Cell::new(false) }
    };
}

impl Local {
    /// The bytes gathered told to the allowance.
    fn flush(&self) {
        let n = self.pending.replace(0);
        let p = self.cur.get();
        if n != 0 && !p.is_null() {
            // SAFETY: an allowance set by `enter`, whose guard holds a strong count
            unsafe { (*p).add(n) };
        }
    }
}

/// The allowance of the calling thread, and whether a check unwinds there: what work moved to
/// another thread runs under ([`enter`]).
#[derive(Clone, Default)]
pub struct Context {
    a: Option<Arc<Allowance>>,
    armed: bool,
}

impl Context {
    /// Of no allowance: what is counted to none and never gives up.
    pub const NONE: Context = Context { a: None, armed: false };
}

/// The [`Context`] of the calling thread (of none where it has none).
#[inline]
pub fn context() -> Context {
    if IN_USE.load(Relaxed) == 0 {
        return Context::NONE;
    }
    LOCAL
        .try_with(|l| {
            let p = l.cur.get();
            if p.is_null() {
                return Context::NONE;
            }
            // SAFETY: set by `enter`, whose guard holds a strong count
            let a = unsafe { clone_raw(p) };
            Context { a: Some(a), armed: l.armed.get() }
        })
        .unwrap_or(Context::NONE)
}

/// The calling thread under `cx` until the guard is dropped.
#[must_use = "the allowance is the thread's until the guard is dropped"]
pub struct Entered {
    a: Option<Arc<Allowance>>,
    prev: Option<(*const Allowance, bool)>,
}

/// The calling thread counting to `cx`'s allowance (none: to none) until the guard is dropped.
pub fn enter(cx: Context) -> Entered {
    let Context { a, armed } = cx;
    let none = a.is_none();
    // (nothing to do where the thread is of none and stays so)
    if none && IN_USE.load(Relaxed) == 0 {
        return Entered { a: None, prev: None };
    }
    let p = a.as_ref().map_or(std::ptr::null(), Arc::as_ptr);
    if !none {
        IN_USE.fetch_add(1, Relaxed);
    }
    let prev = LOCAL
        .try_with(|l| {
            l.flush();
            (l.cur.replace(p), l.armed.replace(armed && !none))
        })
        .ok();
    Entered { a, prev }
}

impl Drop for Entered {
    fn drop(&mut self) {
        if let Some((p, armed)) = self.prev {
            _ = LOCAL.try_with(|l| {
                l.flush();
                l.cur.set(p);
                l.armed.set(armed);
            });
        }
        if self.a.take().is_some() {
            IN_USE.fetch_sub(1, Relaxed);
        }
    }
}

/// Runs `f` as an attempt within `limit` bytes of heap: within the allowance of the attempt
/// that the calling thread works for, if any (whose limit holds), else within one of its own.
/// `Err` where it gave up ([`check`]); a panic of `f` is resumed.
pub fn attempt<R>(limit: u64, f: impl FnOnce() -> R) -> Result<R, Exceeded> {
    let cx = context();
    let outermost = cx.a.is_none();
    let a = cx.a.unwrap_or_else(|| Allowance::new(limit));
    if outermost {
        RUNNING.fetch_add(1, Relaxed);
    }
    let r = {
        let _e = enter(Context { a: Some(a), armed: true });
        catch_unwind(AssertUnwindSafe(f))
    };
    if outermost {
        RUNNING.fetch_sub(1, Relaxed);
    }
    match r {
        Ok(r) => Ok(r),
        Err(p) => match p.downcast::<Exceeded>() {
            Ok(e) => Err(*e),
            Err(p) => resume_unwind(p),
        },
    }
}

/// Gives up (unwinds with [`Exceeded`]) where the calling thread works for an attempt over its
/// allowance; nothing elsewhere. Code that may allocate much between the checks of the pool,
/// of progress and of a store's writers calls it as it goes.
#[inline]
pub fn check() {
    if IN_USE.load(Relaxed) != 0 && LOCAL.try_with(|l| l.armed.get()).unwrap_or(false) {
        check_slow();
    }
}

#[cold]
fn check_slow() {
    let Ok(a) = LOCAL.try_with(|l| {
        l.flush();
        let p = l.cur.get();
        // SAFETY: set by `enter`, whose guard holds a strong count (armed: it is set)
        (!p.is_null() && unsafe { (*p).over() }).then(|| unsafe { ((*p).used(), (*p).limit) })
    }) else {
        return;
    };
    if let Some((used, limit)) = a {
        resume_unwind(Box::new(Exceeded { used, limit }) as Box<dyn Any + Send>);
    }
}

// The blocks, as the allocators tell of them.

/// The large blocks counted by address: their allowance (a strong count) and size.
// (blocks of at least 64 KiB: the bits above their alignment)
static BLOCKS: Blocks<(usize, usize), 16> = Blocks::new();

/// Large blocks counted, not freed yet.
static BLOCKS_HELD: AtomicUsize = AtomicUsize::new(0);

/// `n` bytes more of small blocks of the calling thread (or fewer).
#[inline]
fn small(n: i64) {
    _ = LOCAL.try_with(|l| {
        if l.cur.get().is_null() {
            return;
        }
        let v = l.pending.get() + n;
        l.pending.set(v);
        if v.unsigned_abs() >= FLUSH as u64 {
            l.flush();
        }
    });
}

#[inline]
pub(crate) fn allocated(p: *mut u8, n: usize) {
    if IN_USE.load(Relaxed) == 0 || p.is_null() {
        return;
    }
    if n < LARGE_MIN {
        small(n as i64);
    } else {
        large_allocated(p as usize, n);
    }
}

#[cold]
fn large_allocated(p: usize, n: usize) {
    // (the map's own blocks are small: counted as the thread's, never taking a shard again)
    let Ok(Some(a)) = LOCAL.try_with(|l| {
        let p = l.cur.get();
        // SAFETY: set by `enter`, whose guard holds a strong count
        (!p.is_null()).then(|| unsafe { clone_raw(p) })
    }) else {
        return;
    };
    a.add(n as i64);
    let a = Arc::into_raw(a) as usize;
    match BLOCKS.insert(p, (a, n)) {
        // SAFETY: a strong count the map held
        Some(old) => drop(unsafe { Arc::from_raw(old.0 as *const Allowance) }),
        None => _ = BLOCKS_HELD.fetch_add(1, Relaxed),
    }
}

#[inline]
pub(crate) fn freed(p: *mut u8, n: usize) {
    if n >= LARGE_MIN {
        // (a block counted may be freed after its attempt)
        if BLOCKS_HELD.load(Relaxed) != 0 {
            large_freed(p as usize);
        }
    } else if IN_USE.load(Relaxed) != 0 {
        small(-(n as i64));
    }
}

#[cold]
fn large_freed(p: usize) {
    let owner = BLOCKS.remove(p);
    if let Some((a, n)) = owner {
        BLOCKS_HELD.fetch_sub(1, Relaxed);
        // SAFETY: the strong count the map held
        let a = unsafe { Arc::from_raw(a as *const Allowance) };
        a.add(-(n as i64));
    }
}

#[inline]
pub(crate) fn reallocated(p: *mut u8, n: usize, q: *mut u8, m: usize) {
    crate::blocks::reallocated((p, n), (q, m), allocated, freed);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::alloc::{GlobalAlloc, Layout};

    /// A block of `n` bytes of the binaries' allocator, which counts it (the test binary's own
    /// is the system's), and freeing it.
    fn alloc(n: usize) -> (*mut u8, Layout) {
        let l = Layout::from_size_align(n, 8).unwrap();
        // SAFETY: a block of this layout, freed by `free`
        (unsafe { crate::mem::Mapped.alloc(l) }, l)
    }

    /// What the calling thread's attempt holds, the small blocks told.
    fn held() -> u64 {
        let a = context().a.unwrap();
        _ = LOCAL.try_with(Local::flush);
        a.used()
    }

    fn free((p, l): (*mut u8, Layout)) {
        // SAFETY: allocated by `alloc` of `l`
        unsafe { crate::mem::Mapped.dealloc(p, l) }
    }

    /// An attempt over its allowance gives up at its next check, on a worker of the pool too;
    /// within it, it goes on.
    #[test]
    fn gives_up_at_a_check() {
        let r = attempt(4 << 20, || {
            let b = alloc(512 << 10);
            check();
            let used = held();
            assert!(used >= 512 << 10, "{used}");
            free(b);
            7
        });
        assert_eq!(r, Ok(7));
        let r = attempt(4 << 20, || {
            let b = alloc(8 << 20);
            // (the block freed by the unwinding, as a vector's would be)
            struct Free((*mut u8, Layout));
            impl Drop for Free {
                fn drop(&mut self) {
                    free(self.0);
                }
            }
            let _b = Free(b);
            crate::pool::par_map(4, 4, |_| check());
            unreachable!("given up");
        });
        assert!(matches!(r, Err(Exceeded { used, limit: 4194304 }) if used >= 8 << 20), "{r:?}");
        // (no check unwinds outside an attempt)
        let b = alloc(8 << 20);
        check();
        free(b);
    }

    /// A large block freed on another thread, or after its attempt, is told to the attempt's
    /// allowance; small ones count as their thread's.
    #[test]
    fn blocks_told_back() {
        let r = attempt(1 << 30, || {
            let b = alloc(1 << 20);
            let addr = b.0 as usize;
            std::thread::scope(|s| {
                s.spawn(move || free((addr as *mut u8, b.1)));
            });
            let smalls: Vec<_> = (0..64).map(|_| alloc(16 << 10)).collect();
            let used = held();
            assert!(used >= 1 << 20, "{used}");
            smalls.into_iter().for_each(free);
            let used = held();
            assert!(used < 64 << 10, "{used}");
            alloc(3 << 20)
        });
        let b = r.unwrap();
        free(b);
    }

    /// Attempts within an attempt share its allowance: the inner one gives up, the outer one
    /// goes on once back within it.
    #[test]
    fn nested() {
        let r = attempt(4 << 20, || {
            let inner = attempt(1 << 40, || {
                let b = alloc(8 << 20);
                let free_on_drop = scopeguard(b);
                check();
                drop(free_on_drop);
            });
            assert!(inner.is_err());
            check();
            1
        });
        assert_eq!(r, Ok(1));
    }

    fn scopeguard(b: (*mut u8, Layout)) -> impl Drop {
        struct G((*mut u8, Layout));
        impl Drop for G {
            fn drop(&mut self) {
                free(self.0);
            }
        }
        G(b)
    }

    /// A panic of the attempt is not taken for giving up.
    #[test]
    fn panics_resumed() {
        let r = catch_unwind(|| attempt(1, || panic!("a bug")));
        assert!(r.is_err());
    }
}
