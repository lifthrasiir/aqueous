//! The threads of the process: a pool of a fixed number of them ([`init`]), on which every
//! thread of the recompressors, their modules and the archive encoder runs.
//!
//! Work does not wait for threads: it reserves what workers are free ([`reserve`], [`scope`]),
//! as many as it would take at most, and runs on those it gets, alone where it gets none (the
//! recompressors' outputs do not depend on their threads). A reserved worker runs only what
//! it is reserved for, so that work handing data from thread to thread over channels that
//! block (a pipeline of stages) runs its stages at once, never waiting for a worker: nothing
//! waits for a worker but [`run`], which moves work onto the pool from outside it.
//!
//! Threads that libraries make on their own count in the pool by the workers reserved for them
//! and left idle: Zstandard's (aqr-formula) and rayon's ([`rayon_pool`], whose threads are the
//! workers themselves, and one standing for the thread that waits on it).

use std::any::Any;
use std::collections::VecDeque;
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

/// Bytes of stack of a worker: as much as a main thread has, for the work [`run`] moves there.
const STACK: usize = 16 << 20;

type Job = Box<dyn FnOnce() + Send + 'static>;

struct Pool {
    size: usize,
    /// Workers neither reserved nor running anything.
    free: Mutex<usize>,
    freed: Condvar,
    jobs: Mutex<VecDeque<Job>>,
    queued: Condvar,
}

static POOL: OnceLock<Pool> = OnceLock::new();
/// The size [`init`] asked for, until the pool is made (0: the machine's threads).
static SIZE: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static WORKER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Makes the pool of `n` workers (at least one), unless it is made already: before anything
/// else uses it. Returns whether it is of `n`.
pub fn init(n: usize) -> bool {
    SIZE.store(n.max(1), Ordering::Relaxed);
    pool().size == n.max(1)
}

/// The number of workers of the pool: those that [`init`] asked for, else as many as the
/// machine has threads.
pub fn size() -> usize {
    pool().size
}

/// Whether the calling thread is a worker of the pool.
pub fn is_worker() -> bool {
    WORKER.with(|w| w.get())
}

fn pool() -> &'static Pool {
    POOL.get_or_init(|| {
        let size = match SIZE.load(Ordering::Relaxed) {
            0 => std::thread::available_parallelism().map_or(1, |n| n.get()),
            n => n,
        };
        for i in 0..size {
            std::thread::Builder::new()
                .name(format!("aqr-pool-{i}"))
                .stack_size(STACK)
                .spawn(work)
                .expect("a worker of the pool");
        }
        Pool {
            size,
            free: Mutex::new(size),
            freed: Condvar::new(),
            jobs: Mutex::new(VecDeque::new()),
            queued: Condvar::new(),
        }
    })
}

/// What a worker does: the jobs queued, one after another. (A job is queued only for a worker
/// reserved for it, so that one is idle for each.)
fn work() {
    WORKER.with(|w| w.set(true));
    let p = pool();
    loop {
        let job = {
            let mut jobs = p.jobs.lock().unwrap();
            loop {
                match jobs.pop_front() {
                    Some(j) => break j,
                    None => jobs = p.queued.wait(jobs).unwrap(),
                }
            }
        };
        job();
    }
}

fn release(n: usize) {
    if n > 0 {
        let p = pool();
        *p.free.lock().unwrap() += n;
        p.freed.notify_all();
    }
}

/// Workers reserved: free until spawned on ([`Reservation::spawn`]), given back as what runs on
/// them returns, and those not spawned on as the reservation is dropped.
pub struct Reservation {
    n: usize,
}

/// Reserves up to `want` workers of those free, without waiting for any.
pub fn reserve(want: usize) -> Reservation {
    if want == 0 {
        return Reservation { n: 0 };
    }
    let mut free = pool().free.lock().unwrap();
    let n = want.min(*free);
    *free -= n;
    Reservation { n }
}

impl Reservation {
    /// The workers left to spawn on.
    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// Runs `f` on a worker reserved, which is given back once it returns. Panics where none
    /// is left.
    pub fn spawn<T: Send + 'static>(&mut self, f: impl FnOnce() -> T + Send + 'static) -> Handle<'static, T> {
        // SAFETY: it borrows nothing
        unsafe { self.spawn_unchecked(f, None) }
    }

    /// [`Reservation::spawn`] of `f` borrowing for `'a`, of `scope` if any.
    ///
    /// # Safety
    /// `f` has to have returned before `'a` ends ([`Handle::join`], or as [`scope`] waits).
    unsafe fn spawn_unchecked<'a, T: Send + 'a>(
        &mut self,
        f: impl FnOnce() -> T + Send + 'a,
        scope: Option<Arc<ScopeState>>,
    ) -> Handle<'a, T> {
        assert!(self.n > 0, "no worker reserved left");
        self.n -= 1;
        let slot = Arc::new(Slot { value: Mutex::new(None), done: Condvar::new(), scope: scope.clone() });
        let s = slot.clone();
        let job: Box<dyn FnOnce() + Send + 'a> = Box::new(move || {
            let r = catch_unwind(AssertUnwindSafe(f));
            *s.value.lock().unwrap() = Some(r);
            s.done.notify_all();
        });
        // SAFETY: as the caller makes sure; the slot outlives the job
        let job: Job = unsafe { std::mem::transmute::<Box<dyn FnOnce() + Send + 'a>, Job>(job) };
        let p = pool();
        p.jobs.lock().unwrap().push_back(Box::new(move || {
            // (the job's part of the slot dropped with it, before the scope is told)
            job();
            match scope {
                // (back to the scope, for it to spawn on again)
                Some(s) => {
                    let mut w = s.workers.lock().unwrap();
                    (w.idle, w.running) = (w.idle + 1, w.running - 1);
                    s.finished.notify_all();
                }
                None => release(1),
            }
        }));
        p.queued.notify_one();
        Handle { slot, _borrow: PhantomData }
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        release(self.n);
    }
}

struct Slot<T> {
    value: Mutex<Option<std::thread::Result<T>>>,
    done: Condvar,
    scope: Option<Arc<ScopeState>>,
}

impl<T> Drop for Slot<T> {
    /// A panic nobody joined goes to the scope, which resumes it.
    fn drop(&mut self) {
        if let (Some(s), Some(Err(p))) = (&self.scope, self.value.get_mut().unwrap().take()) {
            s.panic.lock().unwrap().get_or_insert(p);
        }
    }
}

/// What runs on a worker, to wait for.
pub struct Handle<'a, T> {
    slot: Arc<Slot<T>>,
    _borrow: PhantomData<&'a ()>,
}

impl<T> Handle<'_, T> {
    /// What it returned, or its panic, once it has.
    pub fn join(self) -> std::thread::Result<T> {
        let mut v = self.slot.value.lock().unwrap();
        loop {
            match v.take() {
                Some(r) => return r,
                None => v = self.slot.done.wait(v).unwrap(),
            }
        }
    }
}

/// The workers of a [`Scope`], and a panic of what was spawned on them that nobody joined.
#[derive(Default)]
struct ScopeState {
    workers: Mutex<Workers>,
    finished: Condvar,
    panic: Mutex<Option<Box<dyn Any + Send>>>,
}

#[derive(Default)]
struct Workers {
    all: usize,
    idle: usize,
    running: usize,
}

/// Workers of the pool for work borrowing what is outside it, as [`std::thread::scope`] gives
/// threads: what is spawned has returned once [`scope`] does. The workers are the scope's
/// until then: what is spawned once all of them run something waits for one.
pub struct Scope<'scope, 'env: 'scope> {
    state: Arc<ScopeState>,
    _scope: PhantomData<&'scope mut &'scope ()>,
    _env: PhantomData<&'env mut &'env ()>,
}

/// Runs `f` with up to `want` workers of the pool reserved (those free, without waiting for
/// any), to spawn as many things on ([`Scope::spawn`]); returns once they all have. A panic of
/// one not joined is resumed here, as is one of `f`.
pub fn scope<'env, R>(want: usize, f: impl for<'scope> FnOnce(&'scope Scope<'scope, 'env>) -> R) -> R {
    scope_in(reserve(want), f)
}

/// [`scope`] of the workers of `r`: for work that tells first whether it has enough of them
/// (a pipeline, of a thread per stage).
pub fn scope_in<'env, R>(mut r: Reservation, f: impl for<'scope> FnOnce(&'scope Scope<'scope, 'env>) -> R) -> R {
    let n = std::mem::take(&mut r.n);
    let s = Scope {
        state: Arc::new(ScopeState {
            workers: Mutex::new(Workers { all: n, idle: n, running: 0 }),
            ..Default::default()
        }),
        _scope: PhantomData,
        _env: PhantomData,
    };
    let r = catch_unwind(AssertUnwindSafe(|| f(&s)));
    // what is spawned waited for, then the workers given back
    let mut w = s.state.workers.lock().unwrap();
    while w.running > 0 {
        w = s.state.finished.wait(w).unwrap();
    }
    release(w.idle);
    drop(w);
    let panic = s.state.panic.lock().unwrap().take();
    match (r, panic) {
        (Err(p), _) | (Ok(_), Some(p)) => resume_unwind(p),
        (Ok(r), None) => r,
    }
}

impl<'scope> Scope<'scope, '_> {
    /// The workers of the scope: so many things spawned run at once.
    pub fn workers(&self) -> usize {
        self.state.workers.lock().unwrap().all
    }

    /// Runs `f` on a worker of the scope, once one is idle. Panics where the scope has none.
    pub fn spawn<T: Send + 'scope>(&'scope self, f: impl FnOnce() -> T + Send + 'scope) -> Handle<'scope, T> {
        let mut w = self.state.workers.lock().unwrap();
        assert!(w.all > 0, "a scope of no workers");
        while w.idle == 0 {
            w = self.state.finished.wait(w).unwrap();
        }
        (w.idle, w.running) = (w.idle - 1, w.running + 1);
        drop(w);
        // SAFETY: the scope waits for it before it returns
        unsafe { Reservation { n: 1 }.spawn_unchecked(f, Some(self.state.clone())) }
    }
}

/// `f` on a worker of the pool, waiting for one to be free; on the calling thread itself where
/// it is a worker already. Its panic is resumed here.
pub fn run<R: Send>(f: impl FnOnce() -> R + Send) -> R {
    if is_worker() {
        return f();
    }
    let p = pool();
    let mut free = p.free.lock().unwrap();
    while *free == 0 {
        free = p.freed.wait(free).unwrap();
    }
    *free -= 1;
    drop(free);
    let mut r = Reservation { n: 1 };
    // SAFETY: joined before `f`'s borrows end
    let h = unsafe { r.spawn_unchecked(f, None) };
    h.join().unwrap_or_else(|p| resume_unwind(p))
}

/// `f(0..n)` on up to `threads` threads (the calling one and workers reserved), in order. A
/// panic of `f` is resumed here. Each worker is given back as soon as there is nothing left
/// for it to start, for what is still running (a long item's own work) to reserve.
pub fn par_map<R: Send>(n: usize, threads: usize, f: impl Fn(usize) -> R + Sync) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let out: Vec<Mutex<Option<R>>> = (0..n).map(|_| Mutex::new(None)).collect();
    let work = || {
        loop {
            let i = next.fetch_add(1, Ordering::Relaxed);
            if i >= n {
                break;
            }
            *out[i].lock().unwrap() = Some(f(i));
        }
    };
    let mut r = reserve(threads.min(n).saturating_sub(1));
    // SAFETY: each joined below, before what `work` borrows goes, a panic of the calling
    // thread's part caught for that
    let hs: Vec<_> = (0..r.len()).map(|_| unsafe { r.spawn_unchecked(work, None) }).collect();
    // (another's panic stops the rest: the next index is past them)
    let mine = catch_unwind(AssertUnwindSafe(work));
    if mine.is_err() {
        next.store(n, Ordering::Relaxed);
    }
    let mut panic = mine.err();
    for h in hs {
        if let Err(p) = h.join() {
            next.store(n, Ordering::Relaxed);
            panic.get_or_insert(p);
        }
    }
    if let Some(p) = panic {
        resume_unwind(p);
    }
    out.into_iter().map(|m| m.into_inner().unwrap().expect("every index done")).collect()
}

/// `f(i, admit(i))` for each `i` of `0..n`, each on a worker of its own, up to `threads − 1`
/// at once: the calling thread lets them in, in order, as `admit` does (which may wait, as for
/// memory), each on a worker as one is free, and the worker is given back as `f` returns. So
/// no thread waits holding a worker, which work still running can reserve (an item's own
/// threads). On the calling thread alone where `threads` is 1. A panic of `f` stops what is
/// not let in yet and is resumed here.
pub fn dispatch<G: Send, R: Send>(
    n: usize,
    threads: usize,
    mut admit: impl FnMut(usize) -> G,
    f: impl Fn(usize, G) -> R + Sync,
) -> Vec<R> {
    if threads <= 1 {
        return (0..n).map(|i| f(i, admit(i))).collect();
    }
    let out: Vec<Mutex<Option<R>>> = (0..n).map(|_| Mutex::new(None)).collect();
    let (running, done, failed) = (Mutex::new(0usize), Condvar::new(), std::sync::atomic::AtomicBool::new(false));
    let (out, running, done, failed, f) = (&out, &running, &done, &failed, &f);
    let p = pool();
    let mut hs = Vec::new();
    for (i, slot) in out.iter().enumerate() {
        if failed.load(Ordering::Relaxed) {
            break;
        }
        let g = admit(i);
        {
            let mut r = running.lock().unwrap();
            while *r >= threads - 1 {
                r = done.wait(r).unwrap();
            }
            *r += 1;
        }
        let mut free = p.free.lock().unwrap();
        while *free == 0 {
            free = p.freed.wait(free).unwrap();
        }
        *free -= 1;
        drop(free);
        let job = move || {
            let r = catch_unwind(AssertUnwindSafe(|| f(i, g)));
            if r.is_err() {
                failed.store(true, Ordering::Relaxed);
            }
            *running.lock().unwrap() -= 1;
            done.notify_all();
            match r {
                Ok(r) => *slot.lock().unwrap() = Some(r),
                Err(p) => resume_unwind(p),
            }
        };
        // SAFETY: each joined below, before what `job` borrows goes
        hs.push(unsafe { Reservation { n: 1 }.spawn_unchecked(job, None) });
    }
    let mut panic = None;
    for h in hs {
        if let Err(p) = h.join() {
            panic.get_or_insert(p);
        }
    }
    if let Some(p) = panic {
        resume_unwind(p);
    }
    out.iter().map(|m| m.lock().unwrap().take().expect("every index done")).collect()
}

/// A rayon thread pool of up to `threads` threads: as many workers of the pool as are free, up
/// to `threads − 1`, and a thread of its own standing for the calling one, which waits as it
/// gives the pool work (`install`); `None` where the calling thread is one of a rayon pool
/// already, which is then to be used as it is. Its workers stay reserved until it is dropped.
/// Work given to rayon outside such a pool goes to rayon's global pool, of threads of its own.
///
/// (The calling thread is not taken over, rayon's `use_current_thread`: rayon leaves a thread
/// it takes over a worker of the pool for good, which then fails as a thread of another pool,
/// and runs alone what it gives rayon.)
#[cfg(feature = "rayon")]
pub fn rayon_pool(threads: usize) -> Option<rayon::ThreadPool> {
    if rayon::current_thread_index().is_some() {
        return None;
    }
    let mut r = reserve(threads.saturating_sub(1));
    let mut caller = true;
    rayon::ThreadPoolBuilder::new()
        .num_threads(r.len() + 1)
        .spawn_handler(|t| {
            if std::mem::take(&mut caller) {
                std::thread::Builder::new().name("aqr-rayon".into()).stack_size(STACK).spawn(move || t.run())?;
            } else {
                r.spawn(move || t.run());
            }
            Ok(())
        })
        .build()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::sync_channel;

    /// Work that borrows, a pipeline of stages blocking on each other, panics; never more
    /// than the pool's workers reserved.
    #[test]
    fn scopes() {
        let n = size();
        let data: Vec<usize> = (0..1000).collect();
        let sums = par_map(10, 4, |i| data[i * 100..(i + 1) * 100].iter().sum::<usize>());
        assert_eq!(sums.iter().sum::<usize>(), data.iter().sum());

        // stages on their workers, or none where there are not enough
        let got = scope(2, |s| {
            if s.workers() < 2 {
                return None;
            }
            let (tx, rx) = sync_channel::<usize>(0);
            let (tx2, rx2) = sync_channel::<usize>(0);
            s.spawn(move || (0..100).for_each(|i| tx.send(i).unwrap()));
            s.spawn(move || rx.iter().for_each(|i| tx2.send(i * 2).unwrap()));
            Some(rx2.iter().sum::<usize>())
        });
        assert!(got.is_none_or(|g| g == 9900));

        // a worker spawned on again once what ran on it returns
        let twice = scope(1, |s| {
            (s.workers() == 1).then(|| (0..3).map(|i| s.spawn(move || i * 2).join().unwrap()).sum::<usize>())
        });
        assert!(twice.is_none_or(|t| t == 6));

        // all reserved: none is left to nested work
        scope(n, |s| {
            assert!(s.workers() <= n);
            if s.workers() == n {
                assert_eq!(reserve(1).len(), 0);
            }
        });
        assert!(
            catch_unwind(|| scope(1, |s| {
                if s.workers() == 1 {
                    s.spawn(|| panic!("in a worker"));
                } else {
                    panic!("here");
                }
            }))
            .is_err()
        );
        assert!(catch_unwind(|| par_map(8, 8, |i| if i == 5 { panic!("five") } else { i })).is_err());
        assert!(run(is_worker));
    }

    /// The workers of [`par_map`] are given back as they run out of work, for what is still
    /// running to take (a long item's own threads).
    #[test]
    fn par_map_gives_back() {
        let n = size();
        if n < 2 {
            return;
        }
        let done = AtomicUsize::new(0);
        // (on a worker, as everything of aqr is: all of them then the par_map's)
        let got = run(|| par_map(n, n, |i| {
            if i > 0 {
                done.fetch_add(1, Ordering::Relaxed);
                return 0;
            }
            let start = std::time::Instant::now();
            while start.elapsed().as_secs() < 5 {
                if done.load(Ordering::Relaxed) == n - 1 {
                    let r = reserve(1);
                    if !r.is_empty() {
                        return r.len();
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            0
        }));
        assert_eq!(got[0], 1);
    }

    /// [`dispatch`]: every item, let in in order and never more than `threads − 1` at once,
    /// while what runs can reserve the workers left; and a panic resumed.
    #[test]
    fn dispatches() {
        let n = size();
        let (admitted, at_once, most) = (Mutex::new(Vec::new()), AtomicUsize::new(0), AtomicUsize::new(0));
        let got = run(|| {
            dispatch(
                100,
                4,
                |i| admitted.lock().unwrap().push(i),
                |i, ()| {
                    let k = at_once.fetch_add(1, Ordering::Relaxed) + 1;
                    most.fetch_max(k, Ordering::Relaxed);
                    std::thread::sleep(std::time::Duration::from_micros(200));
                    at_once.fetch_sub(1, Ordering::Relaxed);
                    i * 2
                },
            )
        });
        assert_eq!(got, (0..100).map(|i| i * 2).collect::<Vec<_>>());
        assert_eq!(*admitted.lock().unwrap(), (0..100).collect::<Vec<_>>());
        assert!(most.load(Ordering::Relaxed) <= 3.min(n - 1).max(1));
        // one long item takes the workers the others leave
        if n >= 3 {
            let left = AtomicUsize::new(0);
            let got = run(|| {
                dispatch(
                    n,
                    n,
                    |_| (),
                    |i, ()| {
                        if i > 0 {
                            left.fetch_add(1, Ordering::Relaxed);
                            return 0;
                        }
                        let start = std::time::Instant::now();
                        while start.elapsed().as_secs() < 5 {
                            if left.load(Ordering::Relaxed) == n - 1 {
                                let r = reserve(n);
                                if r.len() >= n - 2 {
                                    return r.len();
                                }
                            }
                            std::thread::sleep(std::time::Duration::from_millis(1));
                        }
                        0
                    },
                )
            });
            assert!(got[0] >= n - 2, "{}", got[0]);
        }
        assert!(catch_unwind(|| run(|| dispatch(8, 4, |_| (), |i, ()| if i == 5 { panic!("five") } else { i }))).is_err());
    }

    /// A rayon pool leaves no thread a worker of it once dropped: the calling thread, or a
    /// worker of the pool that made one, makes others, or is one of another's.
    #[cfg(feature = "rayon")]
    #[test]
    fn rayon_pools() {
        use rayon::prelude::*;
        let sum = |p: Option<rayon::ThreadPool>| {
            let f = || (0..100_000u64).into_par_iter().sum::<u64>();
            p.map_or_else(f, |p| p.install(f))
        };
        let pool = rayon_pool(2);
        assert!(pool.is_some());
        assert_eq!(sum(pool), 4_999_950_000);
        assert_eq!(rayon::current_thread_index(), None);
        for _ in 0..20 {
            let n = size();
            let left = par_map(n, n, |_| {
                let pool = rayon_pool(2);
                let made = pool.is_some();
                assert_eq!(sum(pool), 4_999_950_000);
                made && rayon::current_thread_index().is_none()
            });
            assert!(left.into_iter().all(|l| l));
        }
    }
}
