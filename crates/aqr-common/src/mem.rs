//! The global allocator of the binaries ([`Mapped`]); heap accounting for memory profiling
//! (bench -m, probes and memory tests), phase marks (feature `memprof`), and the process's
//! peak resident set.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

/// Counts heap bytes in use and their peak. A growing realloc counts the old and the new
/// block together, as it may need both at once. Registered by the binaries (a library
/// does not pick the global allocator).
pub struct Counting;

pub static CUR: AtomicUsize = AtomicUsize::new(0);
pub static PEAK: AtomicUsize = AtomicUsize::new(0);

/// Blocks of at least this many bytes are counted always, by [`Mapped`] and [`Counting`]
/// ([`large_in_use`]): those smaller are not, so that their allocations, the most of them,
/// do not all take one counter of the process (a size compared is all they cost).
pub const LARGE_MIN: usize = 64 << 10;

static LARGE: AtomicUsize = AtomicUsize::new(0);

/// The bytes of the blocks of at least [`LARGE_MIN`] in use, by the allocators of this module
/// (none by another): about all of the heap where it is large, which the budget of the process
/// looks at ([`crate::budget`]).
pub fn large_in_use() -> u64 {
    LARGE.load(Relaxed) as u64
}

#[inline]
fn large_added(n: usize) {
    if n >= LARGE_MIN {
        LARGE.fetch_add(n, Relaxed);
    }
}

#[inline]
fn large_removed(n: usize) {
    if n >= LARGE_MIN {
        LARGE.fetch_sub(n, Relaxed);
    }
}

/// Allocations of at least this many bytes are logged ([`log_large`]); 0: none.
static LOG_MIN: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// Set while an allocation is being logged, whose own allocations are not.
    static LOGGING: Cell<bool> = const { Cell::new(false) };
}

#[inline]
fn add(n: usize) {
    large_added(n);
    let c = CUR.fetch_add(n, Relaxed) + n;
    PEAK.fetch_max(c, Relaxed);
    let min = LOG_MIN.load(Relaxed);
    if min != 0 && n >= min {
        log(n, c);
    }
}

#[cold]
fn log(n: usize, cur: usize) {
    if LOGGING.with(|l| l.replace(true)) {
        return;
    }
    // The two innermost frames of the aqr crates, past this module's.
    let bt = std::backtrace::Backtrace::force_capture().to_string();
    let frames: Vec<&str> = bt
        .lines()
        .map(str::trim)
        .filter(|l| l.contains("aqr_") && !l.contains("aqr_common::mem::") && !l.starts_with("at "))
        .take(2)
        .collect();
    eprintln!("  +{:7.1} MB  in use {:7.1} MB  {}", n as f64 / 1e6, cur as f64 / 1e6, frames.join(" <- "));
    LOGGING.with(|l| l.set(false));
}

/// Logs every allocation of at least `min` bytes to stderr, with the heap in use and the
/// two innermost frames of the aqr crates (`None`: stops). Slow: a backtrace each.
pub fn log_large(min: Option<usize>) {
    LOG_MIN.store(min.unwrap_or(0), Relaxed);
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            add(l.size());
        }
        crate::meter::allocated(p, l.size());
        crate::allowance::allocated(p, l.size());
        p
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(l) };
        if !p.is_null() {
            add(l.size());
        }
        crate::meter::allocated(p, l.size());
        crate::allowance::allocated(p, l.size());
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        crate::meter::freed(p, l.size());
        crate::allowance::freed(p, l.size());
        unsafe { System.dealloc(p, l) };
        large_removed(l.size());
        CUR.fetch_sub(l.size(), Relaxed);
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, l, n) };
        crate::meter::reallocated(p, l.size(), q, n);
        crate::allowance::reallocated(p, l.size(), q, n);
        if !q.is_null() {
            large_removed(l.size());
            large_added(n);
            if n >= l.size() {
                // Both blocks at the moment of the copy.
                PEAK.fetch_max(CUR.load(Relaxed) + n, Relaxed);
                CUR.fetch_add(n - l.size(), Relaxed);
                let min = LOG_MIN.load(Relaxed);
                if min != 0 && n - l.size() >= min {
                    log(n - l.size(), CUR.load(Relaxed));
                }
            } else {
                CUR.fetch_sub(l.size() - n, Relaxed);
            }
        }
        q
    }
}

/// Starts a peak measurement; returns the bytes in use now.
pub fn peak_reset() -> usize {
    let c = CUR.load(Relaxed);
    PEAK.store(c, Relaxed);
    c
}

/// The result of `f` and the peak heap bytes above those in use when it started.
pub fn peak<R>(f: impl FnOnce() -> R) -> (R, usize) {
    let base = peak_reset();
    let r = f();
    (r, PEAK.load(Relaxed).saturating_sub(base))
}

/// The peak resident set of the process so far, in bytes (0 where unknown).
pub fn max_rss() -> u64 {
    #[cfg(unix)]
    {
        // SAFETY: an all-zero `rusage` is valid, and RUSAGE_SELF fills it.
        let mut u: libc::rusage = unsafe { std::mem::zeroed() };
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) } != 0 {
            return 0;
        }
        // bytes on macOS, KiB elsewhere
        let unit = if cfg!(target_vendor = "apple") { 1 } else { 1024 };
        u.ru_maxrss as u64 * unit
    }
    #[cfg(not(unix))]
    0
}

/// Prints the heap in use and the peak since the last mark (with feature `memprof`).
#[inline]
pub fn mark(_label: &str) {
    #[cfg(feature = "memprof")]
    {
        eprintln!(
            "mem {:>24}: cur {:>8.1} MB, peak {:>8.1} MB",
            _label,
            CUR.load(Relaxed) as f64 / 1e6,
            PEAK.load(Relaxed) as f64 / 1e6
        );
        PEAK.store(CUR.load(Relaxed), Relaxed);
    }
}

/// Blocks of at least this many bytes are mapped by [`Mapped`] (as of 10gb: 1 MiB below 256 KiB
/// and 4 MiB keeps decompression the smallest, of the same speed).
pub const MAPPED_MIN: usize = 1 << 20;

/// The system's allocator, but for blocks of at least [`MAPPED_MIN`] bytes, which are mapped
/// and unmapped at once (on Apple's systems): macOS's gives back the pages of large blocks
/// freed only some seconds later, as large blocks come and go faster than that (10gb
/// decompression of 4.4 GB in use took 7.5 GB). A growing block is extended in place where the
/// pages after it are free, as the system's realloc does. Registered by the binaries.
///
/// On Windows, the system's allocator, but for blocks of [`cache::MIN`] to [`cache::MAX`]
/// bytes, a few of which each thread keeps as they are freed, for the next it allocates of
/// their size ([`cache`]).
///
/// Elsewhere the system's allocator alone: glibc's reuses the large blocks it keeps, where
/// mapping each anew faulted in every page of it (4 KiB on x86-64) and unmapping took the
/// process's lock of mappings and flushed the other threads' TLBs. On 10gb `aqr -K` (then `-t`; 4 threads
/// of a cloud VM) that was 17% of the CPU time in the kernel, and more with more threads.
pub struct Mapped;

#[cfg(target_vendor = "apple")]
mod mapped {
    pub fn round(n: usize) -> usize {
        n.next_multiple_of(crate::pages::size())
    }

    /// `n` bytes mapped (zeroed), at `at` if given and free there; null where they could not be.
    pub fn map(at: *mut u8, n: usize) -> *mut u8 {
        let (prot, flags) = (libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANON);
        // SAFETY: a new mapping, without MAP_FIXED: nothing mapped already is touched
        let p = unsafe { libc::mmap(at.cast(), n, prot, flags, -1, 0) };
        if p == libc::MAP_FAILED { std::ptr::null_mut() } else { p.cast() }
    }

    /// # Safety
    /// `p..p + n` are pages mapped by [`map`] and nothing uses them any more.
    pub unsafe fn unmap(p: *mut u8, n: usize) {
        // SAFETY: as the caller guarantees
        unsafe { libc::munmap(p.cast(), n) };
    }
}

/// The blocks a thread keeps as they are freed, of sizes rounded up to a quarter of a power of
/// two, at most [`cache::PER_CLASS`] of each size and [`cache::HELD_MAX`] bytes in all, until
/// it allocates another of the size or ends. Windows' heap gives the pages of such blocks back
/// as they are freed, so that a block allocated anew faults its pages in again, and threads
/// doing so wait for each other: aqr-minecraft's encoding (zlib's emulator made anew for each
/// chunk's stream) went from 49.8 MB/s on one thread to 10.0 MB/s a thread on 16 (a Ryzen 7
/// 9700X). The bytes kept are not counted as the heap's.
#[cfg(windows)]
pub mod cache {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::UnsafeCell;

    /// The sizes of the blocks kept: from `MIN` bytes, below `MAX`.
    pub const MIN: usize = 32 << 10;
    pub const MAX: usize = 4 << 20;
    const LOG_MIN: u32 = MIN.trailing_zeros();
    const CLASSES: usize = 4 * (MAX.trailing_zeros() - LOG_MIN) as usize + 1;
    pub const PER_CLASS: usize = 4;
    pub const HELD_MAX: usize = 4 << 20;
    /// The alignment of the blocks (the system's own: those of more are not kept).
    const ALIGN: usize = 16;

    /// Whether blocks of layout `l` are kept.
    #[inline]
    pub fn of(l: Layout) -> bool {
        (MIN..MAX).contains(&l.size()) && l.align() <= ALIGN
    }

    /// The class of a block of `n` bytes ([`of`]), and the bytes of the blocks of it.
    #[inline]
    pub fn class(n: usize) -> (usize, usize) {
        let o = usize::BITS - 1 - (n - 1).leading_zeros();
        let base = 1usize << o;
        let q = ((n - base) * 4).div_ceil(base);
        ((o + 1 - LOG_MIN) as usize * 4 + q - 4, base + q * (base / 4))
    }

    fn layout(size: usize) -> Layout {
        // SAFETY: a size above 0, a power of two
        unsafe { Layout::from_size_align_unchecked(size, ALIGN) }
    }

    struct Cache {
        held: usize,
        counts: [u8; CLASSES],
        blocks: [[*mut u8; PER_CLASS]; CLASSES],
    }

    impl Drop for Cache {
        fn drop(&mut self) {
            for c in 0..CLASSES {
                let size = (MIN << (c / 4)) / 4 * (4 + c % 4);
                for &p in &self.blocks[c][..self.counts[c] as usize] {
                    // SAFETY: allocated by the system as of this layout, and kept here alone
                    unsafe { System.dealloc(p, layout(size)) };
                }
            }
        }
    }

    thread_local! {
        static CACHE: UnsafeCell<Cache> = const {
            UnsafeCell::new(Cache { held: 0, counts: [0; CLASSES], blocks: [[std::ptr::null_mut(); PER_CLASS]; CLASSES] })
        };
    }

    /// A block of layout `l` ([`of`]), zeroed where `zeroed`.
    ///
    /// # Safety
    /// As [`GlobalAlloc::alloc`].
    pub unsafe fn alloc(l: Layout, zeroed: bool) -> *mut u8 {
        let (c, size) = class(l.size());
        // SAFETY: nothing allocates within, so that the cache is not taken twice
        let kept = CACHE.try_with(|k| unsafe {
            let k = &mut *k.get();
            let n = k.counts[c] as usize;
            (n > 0).then(|| {
                k.counts[c] -= 1;
                k.held -= size;
                k.blocks[c][n - 1]
            })
        });
        match kept {
            Ok(Some(p)) => {
                if zeroed {
                    // SAFETY: a block of `size` bytes, at least `l.size()`
                    unsafe { std::ptr::write_bytes(p, 0, l.size()) };
                }
                p
            }
            // SAFETY: a layout of a size above 0
            _ if zeroed => unsafe { System.alloc_zeroed(layout(size)) },
            _ => unsafe { System.alloc(layout(size)) },
        }
    }

    /// Block `p` of layout `l` ([`of`]) freed: kept, or given back.
    ///
    /// # Safety
    /// As [`GlobalAlloc::dealloc`].
    pub unsafe fn dealloc(p: *mut u8, l: Layout) {
        let (c, size) = class(l.size());
        // SAFETY: as in `alloc`
        let kept = CACHE.try_with(|k| unsafe {
            let k = &mut *k.get();
            let n = k.counts[c] as usize;
            let keep = n < PER_CLASS && k.held + size <= HELD_MAX;
            if keep {
                k.blocks[c][n] = p;
                k.counts[c] += 1;
                k.held += size;
            }
            keep
        });
        if kept != Ok(true) {
            // SAFETY: allocated by `alloc` (or `realloc`) as of this layout
            unsafe { System.dealloc(p, layout(size)) }
        }
    }

    /// Block `p` of layout `l` made `n` bytes, one of the two of a size kept ([`of`]).
    ///
    /// # Safety
    /// As [`GlobalAlloc::realloc`].
    pub unsafe fn realloc(p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        // SAFETY: of the block's alignment, as the caller guarantees
        let nl = unsafe { Layout::from_size_align_unchecked(n, l.align()) };
        let have = if of(l) { layout(class(l.size()).1) } else { l };
        let want = if of(nl) { class(n).1 } else { n };
        if want == have.size() {
            return p;
        }
        // SAFETY: the block's layout as it was allocated, of the same alignment
        unsafe { System.realloc(p, have, want) }
    }
}

impl Mapped {
    #[inline]
    fn mapped(l: Layout) -> bool {
        cfg!(target_vendor = "apple") && l.size() >= MAPPED_MIN && l.align() <= 4096
    }
}

impl Mapped {
    unsafe fn raw_alloc(&self, l: Layout) -> *mut u8 {
        #[cfg(target_vendor = "apple")]
        if Self::mapped(l) {
            return mapped::map(std::ptr::null_mut(), mapped::round(l.size()));
        }
        #[cfg(windows)]
        if cache::of(l) {
            return unsafe { cache::alloc(l, false) };
        }
        unsafe { System.alloc(l) }
    }
    unsafe fn raw_alloc_zeroed(&self, l: Layout) -> *mut u8 {
        #[cfg(target_vendor = "apple")]
        if Self::mapped(l) {
            return mapped::map(std::ptr::null_mut(), mapped::round(l.size()));
        }
        #[cfg(windows)]
        if cache::of(l) {
            return unsafe { cache::alloc(l, true) };
        }
        unsafe { System.alloc_zeroed(l) }
    }
    unsafe fn raw_dealloc(&self, p: *mut u8, l: Layout) {
        #[cfg(target_vendor = "apple")]
        if Self::mapped(l) {
            // SAFETY: mapped by `alloc` as of this size, and the caller's no more
            return unsafe { mapped::unmap(p, mapped::round(l.size())) };
        }
        #[cfg(windows)]
        if cache::of(l) {
            return unsafe { cache::dealloc(p, l) };
        }
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn raw_realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        // SAFETY (all): `l` is the block's layout, `n` its new size, of the same alignment
        let nl = unsafe { Layout::from_size_align_unchecked(n, l.align()) };
        #[cfg(windows)]
        if cache::of(l) || cache::of(nl) {
            return unsafe { cache::realloc(p, l, n) };
        }
        let (old, new) = (Self::mapped(l), Self::mapped(nl));
        if !old && !new {
            return unsafe { System.realloc(p, l, n) };
        }
        #[cfg(target_vendor = "apple")]
        if old && new {
            let (have, want) = (mapped::round(l.size()), mapped::round(n));
            if want <= have {
                if want < have {
                    unsafe { mapped::unmap(p.add(want), have - want) };
                }
                return p;
            }
            // the pages right after it, where free
            let end = unsafe { p.add(have) };
            let more = mapped::map(end, want - have);
            if more == end {
                return p;
            }
            if !more.is_null() {
                unsafe { mapped::unmap(more, want - have) };
            }
        }
        let q = unsafe { self.raw_alloc(nl) };
        if !q.is_null() {
            unsafe { std::ptr::copy_nonoverlapping(p, q, l.size().min(n)) };
            unsafe { self.raw_dealloc(p, l) };
        }
        q
    }
}

/// (The heap counted for [`crate::trace`], once it is on; its large blocks always,
/// [`large_in_use`].)
unsafe impl GlobalAlloc for Mapped {
    #[inline]
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { self.raw_alloc(l) };
        if !p.is_null() {
            large_added(l.size());
        }
        crate::trace::allocated(p, l.size());
        crate::meter::allocated(p, l.size());
        crate::allowance::allocated(p, l.size());
        p
    }
    #[inline]
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let p = unsafe { self.raw_alloc_zeroed(l) };
        if !p.is_null() {
            large_added(l.size());
        }
        crate::trace::allocated(p, l.size());
        crate::meter::allocated(p, l.size());
        crate::allowance::allocated(p, l.size());
        p
    }
    #[inline]
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        large_removed(l.size());
        crate::trace::freed(p, l.size());
        crate::meter::freed(p, l.size());
        crate::allowance::freed(p, l.size());
        unsafe { self.raw_dealloc(p, l) }
    }
    #[inline]
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        let q = unsafe { self.raw_realloc(p, l, n) };
        if !q.is_null() {
            large_removed(l.size());
            large_added(n);
        }
        crate::trace::reallocated(p, l.size(), q, n);
        crate::meter::reallocated(p, l.size(), q, n);
        crate::allowance::reallocated(p, l.size(), q, n);
        q
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapped() {
        let a = Mapped;
        for (from, to) in
            [(100, 5 << 20), (3 << 20, 9 << 20), (9 << 20, 2 << 20), (2 << 20, 1000), (MAPPED_MIN, MAPPED_MIN + 1)]
        {
            let l = Layout::from_size_align(from, 8).unwrap();
            // SAFETY: blocks of these layouts, written and read within them
            unsafe {
                let p = a.alloc_zeroed(l);
                assert!(!p.is_null());
                assert!(std::slice::from_raw_parts(p, from).iter().all(|&b| b == 0));
                (0..from).for_each(|i| *p.add(i) = i as u8);
                let q = a.realloc(p, l, to);
                assert!(!q.is_null());
                assert!((0..from.min(to)).all(|i| *q.add(i) == i as u8), "{from} -> {to}");
                (0..to).for_each(|i| *q.add(i) = !(i as u8));
                a.dealloc(q, Layout::from_size_align(to, 8).unwrap());
            }
        }
    }

    /// The classes of the blocks kept; blocks of their sizes grown and shrunk across classes and
    /// bounds, freed and taken again (zeroed where asked), on threads at once.
    #[cfg(windows)]
    #[test]
    fn cached() {
        assert_eq!(cache::class(cache::MIN), (0, cache::MIN));
        assert_eq!(cache::class(cache::MIN + 1), (1, cache::MIN / 4 * 5));
        assert_eq!(cache::class(2 * cache::MIN), (4, 2 * cache::MIN));
        assert_eq!(cache::class(cache::MAX - 1).1, cache::MAX);
        for n in (cache::MIN..cache::MAX).step_by(997) {
            let (c, size) = cache::class(n);
            assert!(size >= n && size - n <= size / 5 && cache::class(size) == (c, size), "{n}");
            assert_eq!(size, (cache::MIN << (c / 4)) / 4 * (4 + c % 4), "{n}");
        }
        let a = &Mapped;
        std::thread::scope(|s| {
            for t in 0..4usize {
                s.spawn(move || {
                    for round in 0..300usize {
                        let sizes = [1000, 40_000, 70_000 + round, 1 << 20, 3 << 20, 5 << 20, cache::MIN];
                        let (from, to) = (sizes[(round + t) % 7], sizes[(round * 3 + t / 2) % 7]);
                        let l = Layout::from_size_align(from, 8).unwrap();
                        // SAFETY: blocks of these layouts, written and read within them
                        unsafe {
                            let p = a.alloc_zeroed(l);
                            assert!(std::slice::from_raw_parts(p, from).iter().all(|&b| b == 0), "{from}");
                            (0..from).step_by(97).for_each(|i| *p.add(i) = (i + t) as u8);
                            let q = a.realloc(p, l, to);
                            assert!((0..from.min(to)).step_by(97).all(|i| *q.add(i) == (i + t) as u8));
                            std::ptr::write_bytes(q, 0xff, to);
                            a.dealloc(q, Layout::from_size_align(to, 8).unwrap());
                        }
                    }
                });
            }
        });
    }
}
