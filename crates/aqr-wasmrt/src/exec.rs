//! The executor of the instances of an application ([`Exec`]): futures (instances of a module
//! running on fibers, `call_async`) polled by the thread applying the module and by workers of
//! aqr-common's pool where some are free. A future waits only for what another future gives
//! (a result, a message), so where no future can be polled and none is being polled, the
//! application can never go on: a deadlock, as [`Exec::block_on`] tells.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::task::{Context, Poll, Wake, Waker};

use aqr_common::pool;

type Boxed = Pin<Box<dyn Future<Output = ()> + Send>>;

/// The futures of an application but the first, and the threads polling them.
pub struct Exec {
    inner: Mutex<Inner>,
    changed: Condvar,
}

#[derive(Default)]
struct Inner {
    queue: VecDeque<Arc<Job>>,
    /// The threads polling a future now.
    polling: usize,
    /// The workers of the pool helping.
    helpers: usize,
    /// Whether the first future is to be polled again.
    woken: bool,
    /// Whether the application is over (the futures left are dropped).
    over: bool,
    /// Every future spawned, to drop those left (a future waiting is held only by the wakers it
    /// left, which what it holds may hold: a cycle).
    all: Vec<std::sync::Weak<Job>>,
}

/// A future spawned, to be polled as it is woken.
struct Job {
    exec: Arc<Exec>,
    fut: Mutex<Option<Boxed>>,
    queued: AtomicBool,
}

impl Wake for Job {
    fn wake(self: Arc<Self>) {
        if !self.queued.swap(true, Ordering::AcqRel) {
            let exec = self.exec.clone();
            exec.push(self);
        }
    }
}

/// Wakes the first future.
struct First(Arc<Exec>);

impl Wake for First {
    fn wake(self: Arc<Self>) {
        self.0.inner.lock().unwrap().woken = true;
        self.0.changed.notify_all();
    }
}

/// Why the first future was not done: no future could ever go on, or [`Exec::stop`].
#[derive(Debug, PartialEq, Eq)]
pub enum Stopped {
    Deadlock,
    Stopped,
}

impl Exec {
    pub fn new() -> Arc<Exec> {
        Arc::new(Exec { inner: Mutex::default(), changed: Condvar::new() })
    }

    /// Polls `fut` as it is woken, until it is done (or the application is over).
    pub fn spawn(self: &Arc<Self>, fut: impl Future<Output = ()> + Send + 'static) {
        let job = Arc::new(Job { exec: self.clone(), fut: Mutex::new(Some(Box::pin(fut))), queued: true.into() });
        self.inner.lock().unwrap().all.push(Arc::downgrade(&job));
        self.push(job);
    }

    fn push(self: &Arc<Self>, job: Arc<Job>) {
        let mut inner = self.inner.lock().unwrap();
        if inner.over {
            return;
        }
        inner.queue.push_back(job);
        // a worker more where one is free, while more is queued than there are workers
        if inner.queue.len() > inner.helpers {
            let mut w = pool::reserve(1);
            if !w.is_empty() {
                inner.helpers += 1;
                let exec = self.clone();
                drop(w.spawn(move || exec.help()));
            }
        }
        drop(inner);
        self.changed.notify_all();
    }

    /// Polls the next future queued, if any (unlocked meanwhile); whether there was one.
    fn run_next<'a>(&'a self, mut inner: MutexGuard<'a, Inner>) -> (MutexGuard<'a, Inner>, bool) {
        let Some(job) = inner.queue.pop_front() else { return (inner, false) };
        inner.polling += 1;
        drop(inner);
        job.queued.store(false, Ordering::Release);
        {
            let mut fut = job.fut.lock().unwrap();
            if let Some(f) = fut.as_mut() {
                let waker = Waker::from(job.clone());
                if f.as_mut().poll(&mut Context::from_waker(&waker)).is_ready() {
                    *fut = None;
                }
            }
        }
        // (let go of unlocked: where the future kept no waker of itself, this drops it, and
        // dropping a future may wake others)
        drop(job);
        let mut inner = self.inner.lock().unwrap();
        inner.polling -= 1;
        self.changed.notify_all();
        (inner, true)
    }

    /// What a worker does: the futures queued, until there is none.
    fn help(&self) {
        let mut inner = self.inner.lock().unwrap();
        while !inner.over {
            let (i, ran) = self.run_next(inner);
            inner = i;
            if !ran {
                break;
            }
        }
        inner.helpers -= 1;
    }

    /// Ends the application, as [`Exec::block_on`] returns.
    pub fn stop(&self) {
        self.inner.lock().unwrap().over = true;
        self.changed.notify_all();
    }

    /// Polls `first` until it is done, polling the futures spawned meanwhile; a deadlock where
    /// it is not done and no future can go on. The futures left are dropped then.
    pub fn block_on<R>(self: &Arc<Self>, first: impl Future<Output = R>) -> Result<R, Stopped> {
        let mut first = std::pin::pin!(first);
        let waker = Waker::from(Arc::new(First(self.clone())));
        let r = loop {
            if let Poll::Ready(r) = first.as_mut().poll(&mut Context::from_waker(&waker)) {
                break Ok(r);
            }
            let mut inner = self.inner.lock().unwrap();
            let mut deadlock = false;
            loop {
                if inner.over {
                    break;
                }
                if std::mem::take(&mut inner.woken) {
                    break;
                }
                let (i, ran) = self.run_next(inner);
                inner = i;
                if ran {
                    continue;
                }
                if inner.polling == 0 {
                    // nothing queued nor polled, so nothing can wake anything
                    (inner.over, deadlock) = (true, true);
                    break;
                }
                inner = self.changed.wait(inner).unwrap();
            }
            if inner.over {
                break Err(if deadlock { Stopped::Deadlock } else { Stopped::Stopped });
            }
        };
        self.end();
        r
    }

    /// The application over: the futures left dropped, once no thread polls one.
    fn end(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.over = true;
        let queued = std::mem::take(&mut inner.queue);
        while inner.polling > 0 {
            inner = self.changed.wait(inner).unwrap();
        }
        let all = std::mem::take(&mut inner.all);
        drop(inner);
        // (unlocked: dropping a future may wake others, those queued held by the queue alone too)
        drop(queued);
        for job in all.iter().filter_map(std::sync::Weak::upgrade) {
            let fut = job.fut.lock().unwrap().take();
            drop(fut);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::poll_fn;

    /// A value set once, waited for by futures.
    #[derive(Default)]
    struct Cell(Mutex<(Option<u32>, Vec<Waker>)>);

    impl Cell {
        fn set(&self, v: u32) {
            let mut c = self.0.lock().unwrap();
            c.0 = Some(v);
            c.1.drain(..).for_each(Waker::wake);
        }

        async fn get(&self) -> u32 {
            poll_fn(|cx| {
                let mut c = self.0.lock().unwrap();
                match c.0 {
                    Some(v) => Poll::Ready(v),
                    None => {
                        c.1.push(cx.waker().clone());
                        Poll::Pending
                    }
                }
            })
            .await
        }
    }

    /// The futures left at a deadlock, waiting on each other through their wakers, are dropped.
    #[test]
    fn cycles_dropped() {
        struct Dropped(Arc<AtomicBool>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Relaxed)
            }
        }
        let exec = Exec::new();
        let (a, b) = (Arc::new(Cell::default()), Arc::new(Cell::default()));
        let dropped = Arc::new(AtomicBool::new(false));
        let d = Dropped(dropped.clone());
        let (a2, b2) = (a.clone(), b.clone());
        // (the future holds `b2`, whose wakers will hold the future)
        exec.spawn(async move {
            let _d = d;
            let v = b2.get().await;
            a2.set(v)
        });
        let b3 = b.clone();
        exec.spawn(async move {
            b3.get().await;
        });
        drop(b);
        assert!(exec.block_on(async move { a.get().await }).is_err());
        assert!(dropped.load(Ordering::Relaxed));
    }

    /// A future dropped as the application ends, or as it is let go of once polled (no waker
    /// of it kept), that wakes another future as it is dropped: the executor not locked then
    /// (the decoder hung so on a FLAC archive, a task's state waking the tasks of its gang).
    #[test]
    fn dropping_wakes() {
        /// Wakes the waker it holds as it is dropped.
        struct WakeOnDrop(Arc<Mutex<Option<Waker>>>);
        impl Drop for WakeOnDrop {
            fn drop(&mut self) {
                if let Some(w) = self.0.lock().unwrap().take() {
                    w.wake();
                }
            }
        }
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for _ in 0..100 {
                let exec = Exec::new();
                let (first, other) = (Arc::new(Mutex::new(None::<Waker>)), Arc::new(Mutex::new(None::<Waker>)));
                let mut spawned = 0;
                let r = exec.block_on(poll_fn(|cx| {
                    spawned += 1;
                    match spawned {
                        // a future waiting forever, its waker kept, waking the first once polled
                        1 => {
                            *first.lock().unwrap() = Some(cx.waker().clone());
                            let (first, other) = (first.clone(), other.clone());
                            exec.spawn(poll_fn(move |cx| {
                                *other.lock().unwrap() = Some(cx.waker().clone());
                                if let Some(w) = first.lock().unwrap().take() {
                                    w.wake();
                                }
                                Poll::<()>::Pending
                            }));
                            Poll::Pending
                        }
                        // a future that keeps no waker of itself, waking the other as it is dropped
                        _ => {
                            let w = WakeOnDrop(other.clone());
                            exec.spawn(poll_fn(move |_| {
                                let _ = &w;
                                Poll::<()>::Pending
                            }));
                            Poll::Ready(())
                        }
                    }
                }));
                assert_eq!(r, Ok(()));

                // (the future keeping no waker polled, and let go of, while the first waits)
                let exec = Exec::new();
                let (first, other) = (Arc::new(Mutex::new(None::<Waker>)), Arc::new(Mutex::new(None::<Waker>)));
                let mut polled = 0;
                let r = exec.block_on(poll_fn(|cx| {
                    polled += 1;
                    if polled > 1 {
                        return Poll::Ready(());
                    }
                    *first.lock().unwrap() = Some(cx.waker().clone());
                    // waiting, its waker kept, waking the other once polled; woken again (by the
                    // other dropped), waking the first
                    let later = Arc::new(Mutex::new(None::<Waker>));
                    let (f, o, l) = (first.clone(), other.clone(), later.clone());
                    let mut times = 0;
                    exec.spawn(poll_fn(move |cx| {
                        times += 1;
                        *o.lock().unwrap() = Some(cx.waker().clone());
                        let w = match times {
                            1 => l.lock().unwrap().take(),
                            _ => f.lock().unwrap().take(),
                        };
                        w.map(Waker::wake);
                        Poll::<()>::Pending
                    }));
                    // waiting for the other to be polled (its waker kept), then keeping none
                    let (w, o) = (WakeOnDrop(other.clone()), other.clone());
                    exec.spawn(poll_fn(move |cx| {
                        let _ = &w;
                        if o.lock().unwrap().is_none() {
                            *later.lock().unwrap() = Some(cx.waker().clone());
                        }
                        Poll::<()>::Pending
                    }));
                    Poll::Pending
                }));
                assert_eq!(r, Ok(()));
            }
            tx.send(()).unwrap();
        });
        rx.recv_timeout(std::time::Duration::from_secs(30)).expect("the executor deadlocked");
    }

    /// A chain of futures each waiting for the one before, the first waiting for the last: done;
    /// and two waiting for each other: a deadlock.
    #[test]
    fn chains_and_deadlocks() {
        let exec = Exec::new();
        let cells: Vec<Arc<Cell>> = (0..50).map(|_| Arc::default()).collect();
        for i in 1..cells.len() {
            let (from, to) = (cells[i - 1].clone(), cells[i].clone());
            exec.spawn(async move { to.set(from.get().await + 1) });
        }
        let (first, last) = (cells[0].clone(), cells[cells.len() - 1].clone());
        assert_eq!(
            exec.block_on(async move {
                first.set(1);
                last.get().await
            })
            .unwrap(),
            50
        );

        let exec = Exec::new();
        let (a, b) = (Arc::new(Cell::default()), Arc::new(Cell::default()));
        let (a2, b2) = (a.clone(), b.clone());
        exec.spawn(async move { b2.set(a2.get().await) });
        assert!(exec.block_on(async move { b.get().await + a.get().await }).is_err());
    }
}
