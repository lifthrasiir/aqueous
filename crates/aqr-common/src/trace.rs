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
use std::collections::{BTreeMap, HashMap};
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicUsize, Ordering::*};
use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::time::{Duration, Instant};

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
    if !on() { NOTHING } else { push(label(name, false), false, true) }
}

/// The calling thread waiting, for what `name` tells (another's work, a worker, memory). Only
/// of threads told of already: a thread that only waits is not sampled.
#[inline]
pub fn wait(name: &'static str) -> Span {
    if !on() { NOTHING } else { push(label(name, true), true, false) }
}

fn push(id: u32, wait: bool, register: bool) -> Span {
    let pushed = with_thread(register, |t| {
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

// The heap, as the allocator tells of it.

static HEAP: AtomicI64 = AtomicI64::new(0);
static BYTES: [AtomicI64; LABELS_MAX] = [const { AtomicI64::new(0) }; LABELS_MAX];

const SHARDS: usize = 16;

/// The large blocks by address: their owner and size. (A map of no allocation of its own
/// large enough to be counted, so never taken again within itself.)
static BLOCKS: [Mutex<BTreeMap<usize, (u32, usize)>>; SHARDS] = [const { Mutex::new(BTreeMap::new()) }; SHARDS];

fn shard(p: usize) -> &'static Mutex<BTreeMap<usize, (u32, usize)>> {
    // (blocks of at least 64 KiB: the bits above their alignment)
    &BLOCKS[((p >> 16) ^ (p >> 20)) % SHARDS]
}

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
    if let Ok(mut b) = shard(p).lock() {
        b.insert(p, (o, n));
    }
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
    let (o, n) = shard(p).lock().ok()?.remove(&p)?;
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

/// The resident set of the process, in bytes (0 where unknown).
pub fn rss() -> u64 {
    #[cfg(windows)]
    {
        #[repr(C)]
        struct Counters {
            cb: u32,
            page_faults: u32,
            sizes: [usize; 8],
        }
        unsafe extern "system" {
            fn GetCurrentProcess() -> isize;
            fn K32GetProcessMemoryInfo(process: isize, counters: *mut Counters, cb: u32) -> i32;
        }
        let mut c = Counters { cb: size_of::<Counters>() as u32, page_faults: 0, sizes: [0; 8] };
        // SAFETY: the process's own pseudo-handle, and a PROCESS_MEMORY_COUNTERS of its size
        if unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) } == 0 {
            return 0;
        }
        // (WorkingSetSize)
        c.sizes[1] as u64
    }
    #[cfg(target_vendor = "apple")]
    {
        // SAFETY: a proc_taskinfo of its size, written by the call
        unsafe {
            let mut info: libc::proc_taskinfo = std::mem::zeroed();
            let size = size_of::<libc::proc_taskinfo>() as i32;
            let n = libc::proc_pidinfo(libc::getpid(), libc::PROC_PIDTASKINFO, 0, (&raw mut info).cast(), size);
            if n == size { info.pti_resident_size } else { 0 }
        }
    }
    #[cfg(all(unix, not(target_vendor = "apple")))]
    {
        let statm = std::fs::read_to_string("/proc/self/statm").unwrap_or_default();
        let pages: u64 = statm.split_whitespace().nth(1).and_then(|p| p.parse().ok()).unwrap_or(0);
        // SAFETY: of no preconditions
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(0) as u64;
        pages * page
    }
    #[cfg(not(any(windows, unix)))]
    0
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
    /// Bytes of the heap, and the resident set.
    pub heap: i64,
    pub rss: u64,
    /// Workers of the pool reserved or running something.
    pub reserved: u32,
    /// By thread ([`Trace::threads`]; those made later missing): its stack ([`Trace::stacks`];
    /// [`DEAD`] where it has ended) and its item ([`NO_ITEM`]: none).
    pub threads: Vec<(u32, u32)>,
    /// Bytes of the large blocks by the label that allocated them, those of any.
    pub bytes: Vec<(u32, i64)>,
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

/// A sampler running, until finished.
pub struct Recording {
    stop: Arc<AtomicBool>,
    handle: std::thread::JoinHandle<Trace>,
}

/// Starts sampling (none where not [`enable`]d).
pub fn record() -> Option<Recording> {
    if !on() {
        return None;
    }
    let stop = Arc::new(AtomicBool::new(false));
    let s = stop.clone();
    let handle = std::thread::Builder::new().name("aqr-trace".into()).spawn(move || sample(&s)).ok()?;
    Some(Recording { stop, handle })
}

impl Recording {
    /// What was sampled, the sampler stopped.
    pub fn finish(self) -> Trace {
        self.stop.store(true, Relaxed);
        self.handle.thread().unpark();
        self.handle.join().unwrap_or_default()
    }
}

fn sample(stop: &AtomicBool) -> Trace {
    let start = Instant::now();
    let mut interval = INTERVAL;
    let mut threads: Vec<Arc<Thread>> = Vec::new();
    let mut stacks: HashMap<Vec<u32>, u32> = HashMap::from([(Vec::new(), 0)]);
    let mut samples: Vec<Sample> = Vec::new();
    let mut next = start;
    let mut frames = Vec::with_capacity(DEPTH);
    loop {
        let done = stop.load(Relaxed);
        {
            let all = THREADS.lock().unwrap();
            if all.len() > threads.len() {
                threads.extend(all[threads.len()..].iter().cloned());
            }
        }
        let mut s = Sample {
            at: start.elapsed().as_secs_f64(),
            heap: HEAP.load(Relaxed),
            rss: rss(),
            reserved: crate::pool::usage().0 as u32,
            threads: Vec::with_capacity(threads.len()),
            bytes: Vec::new(),
        };
        for t in &threads {
            if !t.alive.load(Relaxed) {
                s.threads.push((DEAD, NO_ITEM));
                continue;
            }
            let d = t.depth.load(Acquire).min(DEPTH);
            frames.clear();
            frames.extend((0..d).map(|i| t.frames[i].load(Relaxed)));
            let n = stacks.len() as u32;
            let id = *stacks.entry(frames.clone()).or_insert(n);
            s.threads.push((id, t.item.load(Relaxed)));
        }
        let labels = LABELS.read().unwrap().names.len();
        s.bytes = (0..labels).map(|l| (l as u32, BYTES[l].load(Relaxed))).filter(|b| b.1 != 0).collect();
        samples.push(s);
        if samples.len() >= SAMPLES_MAX {
            let mut k = 0;
            samples.retain(|_| {
                k += 1;
                k % 2 == 1
            });
            interval *= 2;
        }
        if done {
            break;
        }
        next += interval;
        let now = Instant::now();
        if next > now {
            std::thread::park_timeout(next - now);
        } else {
            next = now;
        }
    }
    let mut by_id = vec![Vec::new(); stacks.len()];
    stacks.into_iter().for_each(|(s, id)| by_id[id as usize] = s);
    Trace {
        interval,
        seconds: start.elapsed().as_secs_f64(),
        labels: LABELS.read().unwrap().names.clone(),
        stacks: by_id,
        threads: threads.iter().map(|t| (t.name.clone(), t.worker)).collect(),
        samples,
        workers: crate::pool::usage().1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
