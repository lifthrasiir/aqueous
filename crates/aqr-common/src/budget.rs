//! The heap the work may take at once ([`Budget`]: what `aqr -M` sets), and the system's
//! memory it is made of ([`total`], [`available`]).
//!
//! A budget counts the heap only: memory that the system can only swap out. The pages of files
//! mapped (the inputs, the spill of `aqr -c`) are not counted, as the system writes them to
//! their own files, or drops them, as memory runs short.

use std::sync::{Condvar, Mutex};
use std::time::Duration;

/// The budget by default: about what the work took before there was one (10gb at 10 threads:
/// decoding 2.56 GB of peak footprint in 19.5 s, against 4.1–4.6 GB in 16.6–20.1 s with no
/// budget, as `enwik9` decodes alone first and `enwik9.zip` and the leaves after it wait for it).
pub const DEFAULT: u64 = 2_000_000_000;

/// Bytes of heap held by the work of the process at once, within a limit. Each consumer weighs
/// its units (the leaves of an archive decoded, say) and waits for room for the next
/// ([`Budget::wait`]): the bytes the units in have taken (their weights, which they may not
/// have allocated yet), or the heap in use where more (of [`Budget::measuring`]: what is
/// allocated by work that takes nothing too), and the next unit's, within the limit. A consumer
/// holding nothing takes one in whatever is in use, so that it makes progress (one of them
/// waiting for another inside it, too); a unit larger than the limit is so taken alone, the
/// limit passed by it.
pub struct Budget {
    state: Mutex<State>,
    changed: Condvar,
    heap: Option<fn() -> u64>,
}

/// The limit of a budget and the bytes taken of it.
struct State {
    limit: u64,
    held: u64,
}

/// How often a consumer waiting looks at the heap again, which tells nobody as it shrinks.
const LOOK: Duration = Duration::from_millis(10);

/// The limit of a budget, the bytes taken of it and the heap in use, as a consumer looks
/// ([`Budget::wait`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Room {
    pub limit: u64,
    pub held: u64,
    pub heap: u64,
}

impl Room {
    /// The bytes in use: those taken, or the heap where more.
    pub fn used(&self) -> u64 {
        self.held.max(self.heap)
    }

    /// Whether `n` bytes more fit in.
    pub fn fits(&self, n: u64) -> bool {
        self.used().saturating_add(n) <= self.limit
    }
}

/// Bytes taken of a budget, until dropped.
#[must_use]
pub struct Taken<'a> {
    budget: &'a Budget,
    n: u64,
}

impl Budget {
    /// A budget of what its units take alone.
    pub const fn new(limit: u64) -> Budget {
        Budget { state: Mutex::new(State { limit, held: 0 }), changed: Condvar::new(), heap: None }
    }

    /// A budget of what its units take, or of the heap in use that `heap` tells where more.
    pub const fn measuring(limit: u64, heap: fn() -> u64) -> Budget {
        Budget { state: Mutex::new(State { limit, held: 0 }), changed: Condvar::new(), heap: Some(heap) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The room as it is now.
    pub fn room(&self) -> Room {
        self.room_of(&self.lock())
    }

    fn room_of(&self, st: &State) -> Room {
        Room { limit: st.limit, held: st.held, heap: self.heap.map_or(0, |h| h()) }
    }

    pub fn limit(&self) -> u64 {
        self.lock().limit
    }

    /// Sets the limit (the units in keep what they took).
    pub fn set_limit(&self, limit: u64) {
        self.lock().limit = limit;
        self.changed.notify_all();
    }

    /// The bytes taken now.
    pub fn held(&self) -> u64 {
        self.lock().held
    }

    /// Waits until `f`, looking at the room (each time something is given back or the limit
    /// changes, and every `LOOK` where the heap is measured), takes a unit: what it gives, and
    /// the bytes taken for it. `f` runs with the budget locked; a lock it takes is taken after
    /// the budget's, never before.
    pub fn wait<R>(&self, mut f: impl FnMut(Room) -> Option<(R, u64)>) -> (R, Taken<'_>) {
        let mut st = self.lock();
        loop {
            let room = self.room_of(&st);
            if let Some((r, n)) = f(room) {
                st.held = st.held.saturating_add(n);
                return (r, Taken { budget: self, n });
            }
            st = match self.heap {
                None => self.changed.wait(st).unwrap_or_else(|e| e.into_inner()),
                Some(_) => self.changed.wait_timeout(st, LOOK).unwrap_or_else(|e| e.into_inner()).0,
            };
        }
    }

    /// Takes `n` bytes whatever is held.
    pub fn take(&self, n: u64) -> Taken<'_> {
        self.wait(|_| Some(((), n))).1
    }
}

impl Drop for Taken<'_> {
    fn drop(&mut self) {
        // (told of even of no bytes: a consumer may wait for its units to end)
        self.budget.lock().held -= self.n;
        self.budget.changed.notify_all();
    }
}

/// The budget of the process, of [`DEFAULT`] bytes until set, measuring the heap's large blocks
/// ([`crate::mem::large_in_use`]: none where the allocator is not of [`crate::mem`]).
pub fn global() -> &'static Budget {
    static GLOBAL: Budget = Budget::measuring(DEFAULT, crate::mem::large_in_use);
    &GLOBAL
}

/// The heap one recompression may take ([`attempt_limit`]) where set; 0: not set.
static ATTEMPT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The heap one recompression may take (with what it holds of its contents, and the
/// recompressions running at once together: [`crate::allowance`]): as set
/// ([`set_attempt_limit`]: what a user asked for, `aqr -M`), else the budget of the process or
/// half the system's memory where more, as the budget by default is about what the work takes
/// together, not a limit of one recompression.
pub fn attempt_limit() -> u64 {
    match ATTEMPT.load(std::sync::atomic::Ordering::Relaxed) {
        0 => global().limit().max(total().unwrap_or(0) / 2),
        n => n,
    }
}

/// Sets [`attempt_limit`] (0: as by default).
pub fn set_attempt_limit(n: u64) {
    ATTEMPT.store(n, std::sync::atomic::Ordering::Relaxed);
}

/// The memory of the system: physical, or the limit of the process's container (cgroup) where
/// lower. None where unknown.
pub fn total() -> Option<u64> {
    let phys = sys::physical()?;
    Some(sys::container().map_or(phys, |c| c.limit.min(phys)))
}

/// The memory that the process may take now without the system swapping: on Linux,
/// `MemAvailable` (which counts the page cache that can be dropped), or what the container's
/// limit leaves (its use but the inactive files) where less; on Windows, the physical memory
/// available, or the commit charge left where less; on macOS, the share of memory the kernel
/// tells available (`kern.memorystatus_level`, which takes its compressor into account). None
/// where unknown.
pub fn available() -> Option<u64> {
    let a = sys::available()?;
    Some(sys::container().map_or(a, |c| c.available.min(a)))
}

/// Of the memory cgroup of the process, the lowest limit of it and its ancestors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
struct Container {
    limit: u64,
    /// The limit, less what the cgroup uses but its inactive files.
    available: u64,
}

#[cfg(target_os = "linux")]
mod sys {
    use super::Container;
    use std::path::{Path, PathBuf};

    pub fn physical() -> Option<u64> {
        // SAFETY: of no preconditions
        let (pages, size) = unsafe { (libc::sysconf(libc::_SC_PHYS_PAGES), libc::sysconf(libc::_SC_PAGESIZE)) };
        (pages > 0 && size > 0).then(|| pages as u64 * size as u64)
    }

    pub fn available() -> Option<u64> {
        meminfo(&std::fs::read_to_string("/proc/meminfo").ok()?, "MemAvailable:")
    }

    /// The value of `key` (in kB) of `/proc/meminfo`'s text `s`, in bytes.
    pub(super) fn meminfo(s: &str, key: &str) -> Option<u64> {
        let line = s.lines().find(|l| l.starts_with(key))?;
        let kb: u64 = line[key.len()..].split_whitespace().next()?.parse().ok()?;
        kb.checked_mul(1024)
    }

    pub fn container() -> Option<Container> {
        let cgroup = std::fs::read_to_string("/proc/self/cgroup").ok()?;
        let mut found = None;
        for line in cgroup.lines() {
            let mut f = line.splitn(3, ':');
            let (_, controllers, path) = (f.next()?, f.next()?, f.next()?);
            let c = match controllers {
                // (v2: one hierarchy, but a hybrid mount has no memory controller in it)
                "" => walk(Path::new("/sys/fs/cgroup"), path, &V2),
                c if c.split(',').any(|c| c == "memory") => walk(Path::new("/sys/fs/cgroup/memory"), path, &V1),
                _ => None,
            };
            found = lower(found, c);
        }
        found
    }

    /// The files of a version of cgroups: the limit, the use and the stat of the inactive
    /// files; the limit's text of none.
    pub(super) struct Files {
        limit: &'static str,
        usage: &'static str,
        inactive: &'static str,
        none: &'static str,
    }

    pub(super) const V1: Files = Files {
        limit: "memory.limit_in_bytes",
        usage: "memory.usage_in_bytes",
        inactive: "total_inactive_file",
        none: "",
    };
    pub(super) const V2: Files =
        Files { limit: "memory.max", usage: "memory.current", inactive: "inactive_file", none: "max" };

    /// The lowest of the limits of cgroup `path` and its ancestors, under `root`: from where
    /// the path is, as with a namespace of cgroups it may be at the root.
    pub(super) fn walk(root: &Path, path: &str, files: &Files) -> Option<Container> {
        let mut dir: PathBuf = root.join(path.trim_start_matches('/'));
        if !dir.is_dir() {
            dir = root.to_path_buf();
        }
        let mut found = None;
        loop {
            found = lower(found, at(&dir, files));
            if dir == root || !dir.pop() || !dir.starts_with(root) {
                return found;
            }
        }
    }

    fn at(dir: &Path, files: &Files) -> Option<Container> {
        let read = |f: &str| std::fs::read_to_string(dir.join(f)).ok();
        let limit = read(files.limit)?;
        let limit = limit.trim();
        if limit == files.none {
            return None;
        }
        // (v1's "none" is a huge number, near 2^63 rounded to pages)
        let limit: u64 = limit.parse().ok().filter(|&l| l < 1 << 60)?;
        let usage: u64 = read(files.usage).and_then(|u| u.trim().parse().ok()).unwrap_or(0);
        let inactive = read("memory.stat")
            .and_then(|s| {
                let line = s.lines().find(|l| l.split_whitespace().next() == Some(files.inactive))?;
                line.split_whitespace().nth(1)?.parse::<u64>().ok()
            })
            .unwrap_or(0);
        Some(Container { limit, available: limit.saturating_sub(usage.saturating_sub(inactive)) })
    }

    fn lower(a: Option<Container>, b: Option<Container>) -> Option<Container> {
        match (a, b) {
            (Some(a), Some(b)) => {
                Some(Container { limit: a.limit.min(b.limit), available: a.available.min(b.available) })
            }
            (a, b) => a.or(b),
        }
    }
}

#[cfg(target_vendor = "apple")]
mod sys {
    use super::Container;

    /// Sysctl `name`'s value of type `T`.
    fn sysctl<T: Default>(name: &std::ffi::CStr) -> Option<T> {
        let mut v = T::default();
        let mut n = size_of::<T>();
        // SAFETY: a value of `n` bytes written to `v`, of that size
        let r = unsafe { libc::sysctlbyname(name.as_ptr(), (&raw mut v).cast(), &mut n, std::ptr::null_mut(), 0) };
        (r == 0 && n == size_of::<T>()).then_some(v)
    }

    pub fn physical() -> Option<u64> {
        sysctl::<u64>(c"hw.memsize").filter(|&m| m > 0)
    }

    pub fn available() -> Option<u64> {
        let level = sysctl::<i32>(c"kern.memorystatus_level").filter(|l| (0..=100).contains(l))?;
        Some(physical()? / 100 * level as u64)
    }

    pub fn container() -> Option<Container> {
        None
    }
}

#[cfg(windows)]
mod sys {
    use super::Container;

    #[repr(C)]
    #[derive(Default)]
    struct MemoryStatusEx {
        length: u32,
        load: u32,
        total_phys: u64,
        avail_phys: u64,
        total_page_file: u64,
        avail_page_file: u64,
        total_virtual: u64,
        avail_virtual: u64,
        avail_extended_virtual: u64,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalMemoryStatusEx(status: *mut MemoryStatusEx) -> i32;
    }

    fn status() -> Option<MemoryStatusEx> {
        let mut s = MemoryStatusEx { length: size_of::<MemoryStatusEx>() as u32, ..Default::default() };
        // SAFETY: a MEMORYSTATUSEX of its length
        (unsafe { GlobalMemoryStatusEx(&mut s) } != 0).then_some(s)
    }

    pub fn physical() -> Option<u64> {
        status().map(|s| s.total_phys).filter(|&m| m > 0)
    }

    pub fn available() -> Option<u64> {
        // (the commit charge left too: Windows does not overcommit, an allocation past it fails)
        status().map(|s| s.avail_phys.min(s.avail_page_file))
    }

    pub fn container() -> Option<Container> {
        None
    }
}

#[cfg(not(any(target_os = "linux", target_vendor = "apple", windows)))]
mod sys {
    use super::Container;

    pub fn physical() -> Option<u64> {
        None
    }

    pub fn available() -> Option<u64> {
        None
    }

    pub fn container() -> Option<Container> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering::SeqCst};

    /// Units wait for room, but one is taken alone whatever its size.
    #[test]
    fn waits_for_room() {
        let b = Budget::new(100);
        let big = b.wait(|r| (r.held == 0 || r.fits(150)).then_some(((), 150))).1;
        assert_eq!(b.held(), 150);
        let most = AtomicU64::new(0);
        std::thread::scope(|s| {
            for _ in 0..4 {
                s.spawn(|| {
                    let _t = b.wait(|r| (r.held == 0 || r.fits(60)).then_some(((), 60))).1;
                    most.fetch_max(b.held(), SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(5));
                });
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
            // (none let in beside the big one)
            assert_eq!(b.held(), 150);
            drop(big);
        });
        assert_eq!(b.held(), 0);
        assert_eq!(most.load(SeqCst), 60);
    }

    /// The heap in use counts as taken where more than what is, and a consumer waiting sees it
    /// shrink.
    #[test]
    fn heap_measured() {
        static HEAP: AtomicU64 = AtomicU64::new(0);
        let b = Budget::measuring(100, || HEAP.load(SeqCst));
        HEAP.store(80, SeqCst);
        let _a = b.take(10);
        assert_eq!(b.room(), Room { limit: 100, held: 10, heap: 80 });
        assert!(b.room().fits(20) && !b.room().fits(21));
        std::thread::scope(|s| {
            let t = s.spawn(|| drop(b.wait(|r| r.fits(50).then_some(((), 50)))));
            std::thread::sleep(std::time::Duration::from_millis(30));
            assert!(!t.is_finished());
            HEAP.store(30, SeqCst);
            t.join().unwrap();
        });
    }

    /// A raised limit lets in those waiting.
    #[test]
    fn limit_raised() {
        let b = Budget::new(10);
        let _a = b.take(10);
        std::thread::scope(|s| {
            let t = s.spawn(|| drop(b.wait(|r| r.fits(10).then_some(((), 10)))));
            std::thread::sleep(std::time::Duration::from_millis(10));
            b.set_limit(20);
            t.join().unwrap();
        });
        assert_eq!(b.held(), 10);
    }

    #[test]
    fn the_system() {
        if cfg!(any(target_os = "linux", target_vendor = "apple", windows)) {
            let (t, a) = (total().unwrap(), available().unwrap());
            assert!(t >= 64 << 20 && a <= t.max(sys::physical().unwrap()), "{t} {a}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn meminfo() {
        let s = "MemTotal:       16477348 kB\nMemFree:         1 kB\nMemAvailable:   16048384 kB\n";
        assert_eq!(sys::meminfo(s, "MemAvailable:"), Some(16048384 * 1024));
        assert_eq!(sys::meminfo(s, "Cached:"), None);
    }

    /// Of a hierarchy of cgroups, the lowest limit, and what it leaves but the inactive files.
    #[cfg(target_os = "linux")]
    #[test]
    fn cgroups() {
        let root = std::env::temp_dir().join(format!("aqr-budget-{}", std::process::id()));
        let leaf = root.join("a/b");
        std::fs::create_dir_all(&leaf).unwrap();
        let write = |d: &std::path::Path, max: &str, cur: u64, inactive: u64| {
            std::fs::write(d.join("memory.max"), max).unwrap();
            std::fs::write(d.join("memory.current"), cur.to_string()).unwrap();
            std::fs::write(d.join("memory.stat"), format!("anon 1\ninactive_file {inactive}\nactive_file 9\n"))
                .unwrap();
        };
        write(&root, "max\n", 0, 0);
        write(&root.join("a"), "8000\n", 5000, 1000);
        write(&leaf, "max\n", 100, 0);
        let c = sys::walk(&root, "/a/b", &sys::V2);
        assert_eq!(c, Some(Container { limit: 8000, available: 4000 }));
        // a namespace's path not under the root: the root's
        assert_eq!(sys::walk(&root.join("a"), "/x/y", &sys::V2).map(|c| c.limit), Some(8000));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
