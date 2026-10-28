//! A scoped work queue: tasks run on up to `threads` threads (the caller included, the others
//! workers of aqr-common's pool), each worker with its own state, and the results come back by
//! task number.

use aqr_common::wire::Wire;
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

/// [`map`] of work whose tasks, on WebAssembly, run as tasks of the module
/// (`aqr_common::pool::par_map_apart`), each with a state of its own.
pub fn map_apart<S, R: Send + Wire>(
    threads: usize,
    n: usize,
    order: &[usize],
    init: impl Fn() -> S + Sync,
    f: impl Fn(&mut S, usize) -> R + Sync,
) -> Vec<Option<R>> {
    if !cfg!(target_family = "wasm") {
        return map(threads, n, order, init, f);
    }
    let mut slots: Vec<Option<R>> = (0..n).map(|_| None).collect();
    let done = aqr_common::pool::par_map_apart(order.len(), threads, |k| f(&mut init(), order[k]));
    for (&i, r) in order.iter().zip(done) {
        slots[i] = Some(r);
    }
    slots
}
