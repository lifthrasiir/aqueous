//! What the threads of the process are doing, sampled now and then, and the heap by what
//! allocated it: the statistics of `aqr --stats`.
//!
//! Work tells what it is doing by spans ([`span`]): each thread has a stack of labels (static
//! names), and the item it works for (a content of the archive, [`item`]). Waiting is a span
//! too ([`wait`]), of a label of its own. What a thread moves onto a worker of the pool runs
//! there under the spans and item of the thread that moved it ([`context`], [`adopt`]).
//!
//! Once [`enable`]d (before the pool is made, for its workers to be known from the start), the
//! global allocator ([`crate::mem::Mapped`]) counts the bytes of the heap, and those of its
//! blocks of at least [`LARGE`] bytes by the label that allocated them (the innermost span of
//! work of the thread). A [`Recording`] samples every thread's stack and item, the heap and the
//! resident set every [`INTERVAL`] (or less often, as it grows long) until it is finished.
//! Off (the default), a span costs an atomic load.
//!
//! What is out of sight: memory allocated outside the Rust heap (Zstandard's own, mapped
//! files, WebAssembly memories), threads that libraries make (Zstandard's workers: the workers
//! of the pool reserved for them stay idle, [`Sample::reserved`]), and processes of a sandbox.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicUsize, Ordering::*};
use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::time::{Duration, Instant};

use crate::blocks::Blocks;

static ON: AtomicBool = AtomicBool::new(false);

/// Starts counting the heap and telling the spans of the threads that run from now on.
pub fn enable() {
    LazyLock::force(&START);
    LazyLock::force(&LABELS);
    ON.store(true, Release);
}

/// Whether [`enable`]d. (On WebAssembly, none of this is: `trace_none.rs`.)
#[inline]
pub fn on() -> bool {
    ON.load(Relaxed)
}

static START: LazyLock<Instant> = LazyLock::new(Instant::now);

/// Blocks of the heap of at least this many bytes are counted by what allocated them.
pub const LARGE: usize = 64 << 10;

/// Labels at most (those after them are all [`OTHER`]).
const LABELS_MAX: usize = 1024;

/// Spans of a thread kept at most (those deeper are not told).
const DEPTH: usize = 32;

/// No label: what no span is of.
pub const NONE: u32 = 0;
/// The label of what is past [`LABELS_MAX`].
const OTHER: u32 = 1;

/// No item ([`Sample::threads`]).
pub const NO_ITEM: u32 = u32::MAX;

/// The labels: by id, the name and whether it is of waiting.
struct Labels {
    names: Vec<(&'static str, bool)>,
    ids: HashMap<(&'static str, bool), u32>,
}

static LABELS: LazyLock<RwLock<Labels>> = LazyLock::new(|| {
    let names = vec![("", false), ("other", false)];
    let ids = names.iter().enumerate().map(|(i, &n)| (n, i as u32)).collect();
    RwLock::new(Labels { names, ids })
});

fn label(name: &'static str, wait: bool) -> u32 {
    if let Some(&id) = LABELS.read().unwrap().ids.get(&(name, wait)) {
        return id;
    }
    let mut l = LABELS.write().unwrap();
    if let Some(&id) = l.ids.get(&(name, wait)) {
        return id;
    }
    if l.names.len() >= LABELS_MAX {
        return OTHER;
    }
    let id = l.names.len() as u32;
    l.names.push((name, wait));
    l.ids.insert((name, wait), id);
    id
}

/// What a thread is doing, as the sampler reads it.
struct Thread {
    name: String,
    worker: bool,
    frames: [AtomicU32; DEPTH],
    depth: AtomicUsize,
    item: AtomicU32,
    alive: AtomicBool,
}

static THREADS: Mutex<Vec<Arc<Thread>>> = Mutex::new(Vec::new());

/// A thread's own [`Thread`], told dead as the thread ends.
struct Local(Arc<Thread>);

impl Drop for Local {
    fn drop(&mut self) {
        self.0.alive.store(false, Relaxed);
    }
}

thread_local! {
    static LOCAL: RefCell<Option<Local>> = const { RefCell::new(None) };
    /// The innermost label of work of the thread, which the allocator counts blocks by.
    static OWNER: Cell<u32> = const { Cell::new(NONE) };
}

/// `f` of the calling thread's [`Thread`], made where it has none and `register` (none on a
/// thread ending).
fn with_thread<R>(register: bool, f: impl FnOnce(&Thread) -> R) -> Option<R> {
    LOCAL
        .try_with(|l| {
            let mut l = l.try_borrow_mut().ok()?;
            if l.is_none() {
                if !register {
                    return None;
                }
                let t = Arc::new(Thread {
                    name: std::thread::current().name().unwrap_or("thread").to_owned(),
                    worker: crate::pool::is_worker(),
                    frames: [const { AtomicU32::new(NONE) }; DEPTH],
                    depth: AtomicUsize::new(0),
                    item: AtomicU32::new(NO_ITEM),
                    alive: AtomicBool::new(true),
                });
                THREADS.lock().unwrap().push(t.clone());
                *l = Some(Local(t));
            }
            Some(f(&l.as_ref()?.0))
        })
        .ok()
        .flatten()
}

/// Tells of the calling thread (a worker of the pool, as it starts), so that it is sampled even
/// where it never runs anything.
pub fn register() {
    if on() {
        with_thread(true, |_| ());
    }
}

/// A span of the calling thread, until dropped.
#[must_use = "a span lasts until it is dropped"]
pub struct Span {
    /// The owner of blocks before it; `None` where nothing was pushed.
    prev: Option<u32>,
    _here: PhantomData<*const ()>,
}

const NOTHING: Span = Span { prev: None, _here: PhantomData };

/// The calling thread at work on `name`, under the spans it is in.
#[inline]
pub fn span(name: &'static str) -> Span {
    if !on() { NOTHING } else { push(label(name, false), false) }
}

/// The calling thread waiting, for what `name` tells (another's work, a worker, memory). Only
/// of threads told of already: a thread that only waits is not sampled.
#[inline]
pub fn wait(name: &'static str) -> Span {
    if !on() { NOTHING } else { push(label(name, true), true) }
}

fn push(id: u32, wait: bool) -> Span {
    // (a thread registers by what it does, not by what it waits for)
    let pushed = with_thread(!wait, |t| {
        let d = t.depth.load(Relaxed);
        if d < DEPTH {
            t.frames[d].store(id, Relaxed);
        }
        t.depth.store(d + 1, Release);
    });
    if pushed.is_none() {
        return NOTHING;
    }
    let prev = OWNER.try_with(|o| if wait { o.get() } else { o.replace(id) }).unwrap_or(NONE);
    Span { prev: Some(prev), _here: PhantomData }
}

impl Drop for Span {
    fn drop(&mut self) {
        if let Some(prev) = self.prev {
            with_thread(false, |t| t.depth.store(t.depth.load(Relaxed).saturating_sub(1), Release));
            _ = OWNER.try_with(|o| o.set(prev));
        }
    }
}

/// The item the calling thread works for, until dropped.
#[must_use = "an item lasts until it is dropped"]
pub struct Item {
    prev: Option<u32>,
    _here: PhantomData<*const ()>,
}

/// The calling thread at work for item `i` (a content of the archive; `None`: none).
#[inline]
pub fn item(i: Option<usize>) -> Item {
    let none = Item { prev: None, _here: PhantomData };
    if !on() {
        return none;
    }
    let i = i.map_or(NO_ITEM, |i| i.min(NO_ITEM as usize - 1) as u32);
    let prev = with_thread(true, |t| t.item.swap(i, Relaxed));
    Item { prev, _here: PhantomData }
}

impl Drop for Item {
    fn drop(&mut self) {
        if let Some(prev) = self.prev {
            with_thread(false, |t| t.item.store(prev, Relaxed));
        }
    }
}

/// The spans of work and the item of a thread, for work it moves to another ([`adopt`]).
#[derive(Clone)]
pub struct Context {
    frames: Vec<u32>,
    item: u32,
}

/// The spans of work (not those of waiting, nor the `work` of [`adopt`]) and the item of the
/// calling thread; `None` where off or of none.
#[inline]
pub fn context() -> Option<Context> {
    if !on() {
        return None;
    }
    let work = label("work", false);
    with_thread(false, |t| {
        let names = LABELS.read().unwrap();
        let d = t.depth.load(Relaxed).min(DEPTH);
        let frames: Vec<u32> = (0..d)
            .map(|i| t.frames[i].load(Relaxed))
            .filter(|&id| id != work && !names.names.get(id as usize).is_some_and(|n| n.1))
            .collect();
        Context { frames, item: t.item.load(Relaxed) }
    })
    .filter(|c| !c.frames.is_empty() || c.item != NO_ITEM)
}

/// The calling thread under the spans and item of `c`, until dropped.
#[must_use = "the context lasts until it is dropped"]
pub struct Adopted {
    /// The depth, item and owner before.
    prev: Option<(usize, u32, u32)>,
    _here: PhantomData<*const ()>,
}

/// (Without a context, of a span of label `work`: what runs on a worker of no span of its own is
/// not idle.)
pub fn adopt(c: Option<Context>) -> Adopted {
    let none = Adopted { prev: None, _here: PhantomData };
    if !on() {
        return none;
    }
    let c = c.unwrap_or_else(|| Context { frames: vec![label("work", false)], item: NO_ITEM });
    let prev = with_thread(true, |t| {
        let d = t.depth.load(Relaxed);
        for (k, &id) in c.frames.iter().enumerate() {
            if d + k < DEPTH {
                t.frames[d + k].store(id, Relaxed);
            }
        }
        t.depth.store(d + c.frames.len(), Release);
        (d, t.item.swap(c.item, Relaxed))
    });
    let Some((d, item)) = prev else { return none };
    let owner = OWNER.try_with(|o| match c.frames.last() {
        Some(&id) => o.replace(id),
        None => o.get(),
    });
    Adopted { prev: Some((d, item, owner.unwrap_or(NONE))), _here: PhantomData }
}

impl Drop for Adopted {
    fn drop(&mut self) {
        if let Some((d, item, owner)) = self.prev {
            with_thread(false, |t| {
                t.depth.store(d, Release);
                t.item.store(item, Relaxed);
            });
            _ = OWNER.try_with(|o| o.set(owner));
        }
    }
}

// Work done in other processes (the sandbox's workers).

/// Work a worker process did, as the worker told it: when it ended, how long it took, the CPU
/// time it used, and the spans and item of the thread that had it done.
#[derive(Clone)]
struct Remote {
    end: Instant,
    wall: f64,
    cpu: f64,
    frames: Vec<u32>,
    item: u32,
}

static REMOTE: Mutex<Vec<Remote>> = Mutex::new(Vec::new());

/// Tells that the calling thread had a process of its own do work, which ended now: `wall` is
/// how long it took, `cpu` the CPU time all of the process's threads used meanwhile. The sampler
/// cannot see inside the process; the trace shows `cpu / wall` threads at work through that
/// time, outside the pool, under the calling thread's spans and for its item.
pub fn remote(wall: Duration, cpu: Duration) {
    if !on() || wall.is_zero() {
        return;
    }
    let (mut frames, item) = match context() {
        Some(c) => (c.frames, c.item),
        None => (Vec::new(), NO_ITEM),
    };
    frames.push(label("sandbox worker", false));
    REMOTE.lock().unwrap().push(Remote {
        end: Instant::now(),
        wall: wall.as_secs_f64(),
        cpu: cpu.as_secs_f64(),
        frames,
        item,
    });
}

/// Adds to `t` (a recording that started at `start`) the threads of work told by [`remote`]: at
/// each sample within the time of a piece of work, as many busy threads as its CPU time over
/// its wall time makes (the fraction spread over the samples).
fn add_remote(t: &mut Trace, start: Instant) {
    let mut work: Vec<(f64, f64, f64, u32, u32)> = REMOTE
        .lock()
        .unwrap()
        .iter()
        .filter_map(|r| {
            let end = r.end.checked_duration_since(start)?.as_secs_f64();
            // (the stack of the work, looked up or made)
            let stack = match t.stacks.iter().position(|s| *s == r.frames) {
                Some(i) => i,
                None => {
                    t.stacks.push(r.frames.clone());
                    t.stacks.len() - 1
                }
            };
            Some((end - r.wall, end, r.cpu / r.wall, stack as u32, r.item))
        })
        .collect();
    if work.is_empty() {
        return;
    }
    work.sort_by(|a, b| a.0.total_cmp(&b.0));
    let columns = t.threads.len();
    let mut most = 0;
    for (m, s) in t.samples.iter_mut().enumerate() {
        // (a low-discrepancy sequence: the fractions add up over the samples)
        let phase = (m as f64 * 0.618_033_988_749_895).fract();
        let mut extra = Vec::new();
        for &(from, to, threads, stack, item) in &work {
            if s.at >= from && s.at < to {
                let n = (threads + phase).floor() as usize;
                extra.extend(std::iter::repeat_n((stack, item), n));
            }
        }
        if !extra.is_empty() {
            s.threads.resize(columns, (0, NO_ITEM));
            most = most.max(extra.len());
            s.threads.extend(extra);
        }
    }
    t.threads.extend((0..most).map(|i| (format!("sandbox worker {}", i + 1), false)));
    t.labels = LABELS.read().unwrap().names.clone();
}

// The heap, as the allocator tells of it.

static HEAP: AtomicI64 = AtomicI64::new(0);
static BYTES: [AtomicI64; LABELS_MAX] = [const { AtomicI64::new(0) }; LABELS_MAX];
/// Bytes held outside the heap ([`outside`]) by label.
static OUTSIDE: [AtomicI64; LABELS_MAX] = [const { AtomicI64::new(0) }; LABELS_MAX];

/// Bytes held outside the heap (regions of aqr-compress's spill, say), counted by the label of
/// work of the calling thread (as a large block of the heap is) until the guard is dropped.
pub fn outside(n: usize) -> Outside {
    if !on() || n == 0 {
        return Outside { label: NONE, n: 0 };
    }
    let label = OWNER.try_with(Cell::get).unwrap_or(NONE);
    OUTSIDE[label as usize].fetch_add(n as i64, Relaxed);
    Outside { label, n: n as i64 }
}

/// Bytes counted by [`outside`].
pub struct Outside {
    label: u32,
    n: i64,
}

impl Drop for Outside {
    fn drop(&mut self) {
        if self.n != 0 {
            OUTSIDE[self.label as usize].fetch_sub(self.n, Relaxed);
        }
    }
}

/// The large blocks by address: their owner and size. (A map of no allocation of its own
/// large enough to be counted, so never taken again within itself.)
// (blocks of at least 64 KiB: the bits above their alignment)
static BLOCKS: Blocks<(u32, usize), 16> = Blocks::new();

#[inline]
pub(crate) fn allocated(p: *mut u8, n: usize) {
    if !on() || p.is_null() {
        return;
    }
    HEAP.fetch_add(n as i64, Relaxed);
    if n >= LARGE {
        large(p as usize, n, None);
    }
}

#[cold]
fn large(p: usize, n: usize, owner: Option<u32>) {
    let o = owner.unwrap_or_else(|| OWNER.try_with(Cell::get).unwrap_or(NONE));
    BYTES[o as usize].fetch_add(n as i64, Relaxed);
    BLOCKS.insert(p, (o, n));
}

#[inline]
pub(crate) fn freed(p: *mut u8, n: usize) {
    if !on() {
        return;
    }
    HEAP.fetch_sub(n as i64, Relaxed);
    if n >= LARGE {
        gone(p as usize);
    }
}

/// The owner of large block `p`, no longer counted.
#[cold]
fn gone(p: usize) -> Option<u32> {
    let (o, n) = BLOCKS.remove(p)?;
    BYTES[o as usize].fetch_sub(n as i64, Relaxed);
    Some(o)
}

/// Block `p` of `n` bytes, now `q` of `m` (of the same owner, where counted).
#[inline]
pub(crate) fn reallocated(p: *mut u8, n: usize, q: *mut u8, m: usize) {
    if !on() || q.is_null() {
        return;
    }
    HEAP.fetch_add(m as i64 - n as i64, Relaxed);
    if n >= LARGE || m >= LARGE {
        let o = if n >= LARGE { gone(p as usize) } else { None };
        if m >= LARGE {
            large(q as usize, m, o);
        }
    }
}

/// The resident set of the process, and what of it is of files mapped (0 where unknown).
#[derive(Debug, Clone, Copy, Default)]
pub struct Resident {
    pub all: u64,
    /// Of files mapped (the inputs, the spill): pages the system may write out (where changed)
    /// and drop as memory runs short, rather than swap. On Windows, the working set but its
    /// private part; on macOS, the pages of external objects; on Linux, `RssFile` and
    /// `RssShmem`.
    pub mapped: u64,
}

impl Resident {
    /// Of a working set of `all` bytes, `private` of them private (Windows): of files mapped
    /// unknown where `private` is 0, as Windows before 10 1809 leaves PrivateWorkingSetSize
    /// (taking the counters of PROCESS_MEMORY_COUNTERS_EX2 but filling those before), and a
    /// process always has private pages.
    #[cfg_attr(not(any(windows, test)), allow(dead_code))]
    fn of_working_set(all: u64, private: u64) -> Resident {
        Resident { all, mapped: if private == 0 { 0 } else { all.saturating_sub(private) } }
    }
}

/// [`Resident`] now.
pub fn resident() -> Resident {
    #[cfg(windows)]
    {
        /// PROCESS_MEMORY_COUNTERS_EX2 (Windows 10 1809 on), whose first fields are those of
        /// PROCESS_MEMORY_COUNTERS: of `sizes`, 1 is WorkingSetSize, 9 PrivateWorkingSetSize.
        #[repr(C)]
        struct Counters {
            cb: u32,
            page_faults: u32,
            sizes: [usize; 11],
        }
        unsafe extern "system" {
            fn GetCurrentProcess() -> isize;
            fn K32GetProcessMemoryInfo(process: isize, counters: *mut Counters, cb: u32) -> i32;
        }
        let mut c = Counters { cb: size_of::<Counters>() as u32, page_faults: 0, sizes: [0; 11] };
        // SAFETY: the process's own pseudo-handle, and counters of the size told
        if unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) } != 0 {
            return Resident::of_working_set(c.sizes[1] as u64, c.sizes[9] as u64);
        }
        // (older systems: PROCESS_MEMORY_COUNTERS alone, of 8 sizes)
        c.cb = (8 + size_of::<[usize; 8]>()) as u32;
        // SAFETY: as above, of fewer bytes than the struct's
        match unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) } {
            0 => Resident::default(),
            _ => Resident { all: c.sizes[1] as u64, mapped: 0 },
        }
    }
    #[cfg(target_vendor = "apple")]
    {
        /// task_vm_info (TASK_VM_INFO), up to `external` and the fields after it that every
        /// revision has: sizes of 8 bytes but `region_count` and `page_size`, of 4.
        #[repr(C)]
        struct VmInfo {
            virtual_size: u64,
            region_count: i32,
            page_size: i32,
            resident_size: u64,
            resident_size_peak: u64,
            device: u64,
            device_peak: u64,
            internal: u64,
            internal_peak: u64,
            external: u64,
            external_peak: u64,
            rest: [u64; 10],
        }
        const TASK_VM_INFO: u32 = 22;
        unsafe extern "C" {
            /// The task's own port (what `mach_task_self()` reads).
            static mach_task_self_: libc::mach_port_t;
        }
        // SAFETY: a struct of the size told (in 4-byte units), written by the call
        unsafe {
            let mut info: VmInfo = std::mem::zeroed();
            let mut count = (size_of::<VmInfo>() / 4) as libc::mach_msg_type_number_t;
            let r = libc::task_info(mach_task_self_, TASK_VM_INFO, (&raw mut info).cast(), &mut count);
            match r == libc::KERN_SUCCESS {
                true => Resident { all: info.resident_size, mapped: info.external },
                false => Resident::default(),
            }
        }
    }
    #[cfg(all(unix, not(target_vendor = "apple")))]
    {
        // (pages: of all, resident, and resident of files and shared memory)
        let statm = std::fs::read_to_string("/proc/self/statm").unwrap_or_default();
        let mut pages = statm.split_whitespace().skip(1).map(|p| p.parse::<u64>().unwrap_or(0));
        let (all, mapped) = (pages.next().unwrap_or(0), pages.next().unwrap_or(0));
        // SAFETY: of no preconditions
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(0) as u64;
        Resident { all: all * page, mapped: mapped * page }
    }
    #[cfg(not(any(windows, unix)))]
    Resident::default()
}

// The sampler.

/// How often a recording samples at first.
pub const INTERVAL: Duration = Duration::from_millis(10);

/// Samples a recording keeps at most: past them, every other one is dropped and the interval
/// doubled.
const SAMPLES_MAX: usize = 1 << 16;

/// What was sampled at a time.
#[derive(Debug, Clone)]
pub struct Sample {
    /// Seconds since the recording started.
    pub at: f64,
    /// Bytes of the heap, and the resident set and of it what is of files mapped
    /// ([`Resident`]).
    pub heap: i64,
    pub rss: u64,
    pub mapped: u64,
    /// Workers of the pool reserved or running something.
    pub reserved: u32,
    /// Bytes taken of the budget of the process ([`crate::budget::global`]).
    pub held: u64,
    /// By thread ([`Trace::threads`]; those made later missing): its stack ([`Trace::stacks`];
    /// [`DEAD`] where it has ended) and its item ([`NO_ITEM`]: none).
    pub threads: Vec<(u32, u32)>,
    /// Bytes of the large blocks by the label that allocated them, those of any.
    pub bytes: Vec<(u32, i64)>,
    /// Bytes held outside the heap ([`outside`]) by label, those of any.
    pub outside: Vec<(u32, i64)>,
}

/// The stack of a thread ended ([`Sample::threads`]).
pub const DEAD: u32 = u32::MAX;

/// What a recording sampled.
#[derive(Debug, Clone, Default)]
pub struct Trace {
    /// The interval between the samples (as it ended).
    pub interval: Duration,
    /// Seconds the recording took.
    pub seconds: f64,
    /// By label id: its name, and whether it is of waiting.
    pub labels: Vec<(&'static str, bool)>,
    /// By stack id: its labels, the outermost first; stack 0 is none (idle).
    pub stacks: Vec<Vec<u32>>,
    /// The threads: name, and whether a worker of the pool.
    pub threads: Vec<(String, bool)>,
    pub samples: Vec<Sample>,
    /// The workers of the pool.
    pub workers: usize,
}

/// A sampler running, until finished (or dropped).
pub struct Recording {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
    watch: Watch,
}

/// What a recording has sampled so far, to be seen as it goes on ([`Recording::watch`]).
#[derive(Clone)]
pub struct Watch(Arc<Sampled>);

struct Sampled {
    start: Instant,
    /// The trace so far (its labels, seconds and workers filled in once ended, else as seen),
    /// and whether it has ended.
    trace: Mutex<(Trace, bool)>,
}

/// Starts sampling (none where not [`enable`]d).
pub fn record() -> Option<Recording> {
    if !on() {
        return None;
    }
    let stop = Arc::new(AtomicBool::new(false));
    let watch = Watch(Arc::new(Sampled {
        start: Instant::now(),
        trace: Mutex::new((Trace { interval: INTERVAL, stacks: vec![Vec::new()], ..Trace::default() }, false)),
    }));
    let (s, w) = (stop.clone(), watch.clone());
    let handle = std::thread::Builder::new().name("aqr-trace".into()).spawn(move || sample(&s, &w.0)).ok()?;
    Some(Recording { stop, handle: Some(handle), watch })
}

impl Recording {
    /// What was sampled, the sampler stopped.
    pub fn finish(mut self) -> Trace {
        self.stop();
        self.watch.trace()
    }

    /// A view of what is sampled, as it goes on (and once finished or dropped, all of it).
    pub fn watch(&self) -> Watch {
        self.watch.clone()
    }

    fn stop(&mut self) {
        self.stop.store(true, Relaxed);
        if let Some(h) = self.handle.take() {
            h.thread().unpark();
            _ = h.join();
        }
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Watch {
    /// What was sampled so far.
    pub fn trace(&self) -> Trace {
        let (mut t, ended) = self.0.trace.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if !ended {
            t.seconds = self.0.start.elapsed().as_secs_f64();
            t.labels = LABELS.read().unwrap().names.clone();
            t.workers = crate::pool::usage().1;
        }
        add_remote(&mut t, self.0.start);
        t
    }
}

fn sample(stop: &AtomicBool, to: &Sampled) {
    let start = to.start;
    let mut interval = INTERVAL;
    let mut threads: Vec<Arc<Thread>> = Vec::new();
    let mut stacks: HashMap<Vec<u32>, u32> = HashMap::from([(Vec::new(), 0)]);
    let mut next = start;
    let mut frames = Vec::with_capacity(DEPTH);
    loop {
        let done = stop.load(Relaxed);
        let first = threads.len();
        {
            let all = THREADS.lock().unwrap();
            if all.len() > threads.len() {
                threads.extend(all[threads.len()..].iter().cloned());
            }
        }
        let r = resident();
        let mut s = Sample {
            at: start.elapsed().as_secs_f64(),
            heap: HEAP.load(Relaxed),
            rss: r.all,
            mapped: r.mapped,
            reserved: crate::pool::usage().0 as u32,
            held: crate::budget::global().held(),
            threads: Vec::with_capacity(threads.len()),
            bytes: Vec::new(),
            outside: Vec::new(),
        };
        let mut new_stacks = Vec::new();
        for t in &threads {
            if !t.alive.load(Relaxed) {
                s.threads.push((DEAD, NO_ITEM));
                continue;
            }
            let d = t.depth.load(Acquire).min(DEPTH);
            frames.clear();
            frames.extend((0..d).map(|i| t.frames[i].load(Relaxed)));
            let n = stacks.len() as u32;
            let id = match stacks.get(&frames) {
                Some(&id) => id,
                None => {
                    stacks.insert(frames.clone(), n);
                    new_stacks.push(frames.clone());
                    n
                }
            };
            s.threads.push((id, t.item.load(Relaxed)));
        }
        let labels = LABELS.read().unwrap().names.len();
        s.bytes = (0..labels).map(|l| (l as u32, BYTES[l].load(Relaxed))).filter(|b| b.1 != 0).collect();
        s.outside = (0..labels).map(|l| (l as u32, OUTSIDE[l].load(Relaxed))).filter(|b| b.1 != 0).collect();
        {
            let (t, ended) = &mut *to.trace.lock().unwrap_or_else(|e| e.into_inner());
            t.threads.extend(threads[first..].iter().map(|t| (t.name.clone(), t.worker)));
            t.stacks.extend(new_stacks);
            t.samples.push(s);
            if t.samples.len() >= SAMPLES_MAX {
                let mut k = 0;
                t.samples.retain(|_| {
                    k += 1;
                    k % 2 == 1
                });
                interval *= 2;
            }
            t.interval = interval;
            if done {
                t.seconds = start.elapsed().as_secs_f64();
                t.labels = LABELS.read().unwrap().names.clone();
                t.workers = crate::pool::usage().1;
                *ended = true;
                break;
            }
        }
        next += interval;
        let now = Instant::now();
        if next > now {
            std::thread::park_timeout(next - now);
        } else {
            next = now;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A working set's part of files mapped is what is not private; unknown (0) where the
    /// private part is not told (0: Windows before 10 1809 leaves PrivateWorkingSetSize so, and
    /// a process always has private pages), not all of it.
    #[test]
    fn working_set_mapped() {
        let r = Resident::of_working_set(8 << 30, 3 << 30);
        assert_eq!((r.all, r.mapped), (8 << 30, 5 << 30));
        let r = Resident::of_working_set(8 << 30, 0);
        assert_eq!((r.all, r.mapped), (8 << 30, 0));
    }

    /// What a recording sampled so far is seen as it goes on, and all of it once finished.
    #[test]
    fn watched() {
        enable();
        let r = record().unwrap();
        let w = r.watch();
        let _s = span("watched");
        std::thread::sleep(Duration::from_millis(50));
        let so_far = w.trace();
        assert!(!so_far.samples.is_empty());
        assert!(so_far.seconds >= 0.05);
        assert!(so_far.labels.contains(&("watched", false)));
        assert!(so_far.stacks.len() > 1, "the stack of this thread");
        std::thread::sleep(Duration::from_millis(30));
        let t = r.finish();
        assert!(t.samples.len() > so_far.samples.len());
        assert_eq!(w.trace().samples.len(), t.samples.len());
        assert_eq!(w.trace().seconds, t.seconds);
    }

    /// Spans, waits and items as sampled, work moved to another thread under them, and the
    /// heap's large blocks by their label.
    #[test]
    fn sampled() {
        enable();
        let r = record().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        {
            let _s = span("outer");
            let _i = item(Some(7));
            let _s = span("inner");
            // (by the allocator of the binaries, which counts it)
            use std::alloc::{GlobalAlloc, Layout};
            let l = Layout::from_size_align(4 << 20, 8).unwrap();
            // SAFETY: a block of this layout, freed below
            let block = unsafe { crate::mem::Mapped.alloc(l) };
            let cx = context();
            std::thread::spawn(move || {
                let _a = adopt(cx);
                let _s = span("moved");
                std::thread::sleep(Duration::from_millis(60));
                tx.send(()).unwrap();
            });
            let _w = wait("waiting");
            rx.recv().unwrap();
            std::thread::sleep(Duration::from_millis(30));
            // SAFETY: allocated above, of `l`
            unsafe { crate::mem::Mapped.dealloc(block, l) };
        }
        std::thread::sleep(Duration::from_millis(30));
        let t = r.finish();
        let id = |n: &str, w: bool| t.labels.iter().position(|&l| l == (n, w)).unwrap() as u32;
        let (outer, inner, moved, waiting) =
            (id("outer", false), id("inner", false), id("moved", false), id("waiting", true));
        let seen = |stack: &[u32], item: u32| {
            t.samples
                .iter()
                .any(|s| s.threads.iter().any(|&(st, it)| st != DEAD && t.stacks[st as usize] == stack && it == item))
        };
        assert!(seen(&[outer, inner, waiting], 7));
        assert!(seen(&[outer, inner, moved], 7));
        assert!(t.samples.iter().any(|s| s.bytes.iter().any(|&(l, n)| l == inner && n >= 4 << 20)));
        // (the block gone)
        assert!(t.samples.last().unwrap().bytes.iter().all(|&(l, _)| l != inner));
    }
}
