//! The order formulas are sealed into the sink in (`aqr_formula::aqr::sink`,
//! `docs/streaming-encoder-design.md`): that of the contents, depth first, whatever the threads,
//! as the archive's bytes depend on it. Each content made apart has a slot ([`Order::slots`])
//! under that of the formula it is of; a slot's job (sealing the content's formula, what of it
//! is not sealed already) runs once the slot before it is done, and that of a formula once its
//! contents' slots are. A job submitted before its turn waits, held, and runs on the thread
//! that ends the job before it.

use std::sync::{Arc, Condvar, Mutex};

use aqr_formula::aqr::sink::Sink;

/// A slot by its index; the root's is [`ROOT`].
pub(crate) type Slot = usize;

/// The slot of the formula of the file itself, open from the start.
pub(crate) const ROOT: Slot = 0;

/// A job of a slot: what is sealed then.
pub(crate) type Job = Box<dyn FnOnce(&Sink) + Send>;

pub(crate) struct Order {
    sink: Sink,
    state: Mutex<State>,
    changed: Condvar,
}

struct State {
    slots: Vec<SlotState>,
    /// Whether a job runs (one at a time: they write to the sink in order).
    running: bool,
}

#[derive(Default)]
struct SlotState {
    parent: Option<Slot>,
    children: Vec<Slot>,
    open: bool,
    done: bool,
    job: Option<Job>,
}

impl Order {
    pub fn new(sink: Sink) -> Order {
        let root = SlotState { open: true, ..SlotState::default() };
        Order { sink, state: Mutex::new(State { slots: vec![root], running: false }), changed: Condvar::new() }
    }

    /// `n` slots under `parent`, in order (once for a slot).
    pub fn slots(&self, parent: Slot, n: usize) -> Vec<Slot> {
        let mut st = self.state.lock().unwrap();
        debug_assert!(st.slots[parent].children.is_empty(), "a slot's slots made once");
        let first = st.slots.len();
        st.slots.extend((0..n).map(|_| SlotState { parent: Some(parent), ..SlotState::default() }));
        st.slots[parent].children = (first..first + n).collect();
        self.progress(st, parent);
        (first..first + n).collect()
    }

    /// Has `job` run in the turn of slot `s` (now, where it is its turn).
    pub fn submit(&self, s: Slot, job: Job) {
        let mut st = self.state.lock().unwrap();
        debug_assert!(st.slots[s].job.is_none() && !st.slots[s].done, "a job of a slot once");
        st.slots[s].job = Some(job);
        self.progress(st, s);
    }

    /// Waits until the slots under `s` are done.
    pub fn wait(&self, s: Slot) {
        let _w = aqr_common::trace::wait("seal");
        let mut st = self.state.lock().unwrap();
        while !st.slots[s].children.iter().all(|&c| st.slots[c].done) {
            st = self.changed.wait(st).unwrap();
        }
    }

    /// Waits until slot `s` is done.
    pub fn wait_done(&self, s: Slot) {
        let _w = aqr_common::trace::wait("seal");
        let mut st = self.state.lock().unwrap();
        while !st.slots[s].done {
            st = self.changed.wait(st).unwrap();
        }
    }

    /// The sink, to seal into it outside the slots' order: only what is of the root's turn
    /// (the parts of the formula of the file itself, where it has no contents made apart).
    pub fn sink(&self) -> &Sink {
        &self.sink
    }

    /// The sink, once every job has run.
    pub fn into_sink(self) -> Sink {
        debug_assert!(!self.state.into_inner().unwrap().running);
        self.sink
    }

    /// Runs the jobs whose turn it is, from slot `s` on (which has just changed): into the first
    /// of its slots not done, opening it, else its own job, then on to its parent.
    fn progress<'a>(&'a self, mut st: std::sync::MutexGuard<'a, State>, mut s: Slot) {
        loop {
            if st.running || !st.slots[s].open || st.slots[s].done {
                return;
            }
            if let Some(&c) = st.slots[s].children.iter().find(|&&c| !st.slots[c].done) {
                if st.slots[c].open {
                    return;
                }
                st.slots[c].open = true;
                s = c;
                continue;
            }
            let Some(job) = st.slots[s].job.take() else { return };
            st.running = true;
            drop(st);
            job(&self.sink);
            st = self.state.lock().unwrap();
            st.running = false;
            st.slots[s].done = true;
            self.changed.notify_all();
            match st.slots[s].parent {
                Some(p) => s = p,
                None => return,
            }
        }
    }
}

/// What a context seals into, where its contents are sealed: the order and its slot.
#[derive(Clone)]
pub(crate) struct Sealing {
    pub order: Arc<Order>,
    pub slot: Slot,
}
