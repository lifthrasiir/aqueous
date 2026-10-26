//! A scoped work queue: tasks run on up to `threads` threads (the caller included, the others
//! workers of aqr-common's pool), each worker with its own state, and the results come back by
//! task number.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

/// Runs `f(state, i)` for every `i` in `order` (a list of task numbers below `n`, taken in
/// that order by whichever worker is free); `init` makes the state of a worker. The result
/// of task `i` is at index `i` (`None` for those not in `order`). A panic in any task
/// reaches the caller once the others are done.
pub fn map<S, R: Send>(
    threads: usize,
    n: usize,
    order: &[usize],
    init: impl Fn() -> S + Sync,
    f: impl Fn(&mut S, usize) -> R + Sync,
) -> Vec<Option<R>> {
    let slots: Vec<Mutex<Option<R>>> = (0..n).map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    let work = || {
        let mut st = init();
        while let Some(&i) = order.get(next.fetch_add(1, Relaxed)) {
            let r = f(&mut st, i);
            *slots[i].lock().unwrap() = Some(r);
        }
    };
    let workers = threads.clamp(1, order.len().max(1));
    aqr_common::pool::scope(workers - 1, |sc| {
        for _ in 0..sc.workers() {
            sc.spawn(work);
        }
        work();
    });
    slots.into_iter().map(|m| m.into_inner().unwrap()).collect()
}
