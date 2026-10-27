//! Tasks: work run apart from the module, each on a copy of the working memory as it is when
//! they are spawned, giving back bytes (`aqr_spawn`, `aqr_result`, `aqr_join`, `aqr_take`).
//!
//! [`spawn`] runs `f(i, out)` for each `i` of `0..n`, and [`Tasks::join`] gives what each put in
//! `out`. A task sees nothing of the module but the working memory: every other memory is
//! empty to it, and it cannot request inputs or commit outputs. So nothing that `f` does
//! reaches the module but `out`: not even what it changes through shared references (atomics,
//! cells), which natively it would.
//!
//! On WebAssembly a task is a new instance of the module, which `aqr-ld`'s `aqr_task` starts on
//! the stack below the spawning frame, so that what `f` borrows from it is there. Natively
//! ([`emu`](crate::emu)) the tasks run as they are spawned, on the calling thread.

use alloc::vec::Vec;
use core::marker::PhantomData;

/// The function of the tasks, as [`spawn`] gives it to them.
#[doc(hidden)]
#[repr(C)]
pub struct Record {
    run: unsafe fn(*const (), usize, &mut Vec<u8>),
    data: *const (),
}

unsafe fn run<F: Fn(usize, &mut Vec<u8>)>(data: *const (), i: usize, out: &mut Vec<u8>) {
    // SAFETY: `data` is the `&F` of `spawn`, alive as long as the tasks
    unsafe { (*(data as *const F))(i, out) }
}

impl Record {
    /// What task `i` gives.
    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn call(&self, i: usize) -> Vec<u8> {
        let mut out = Vec::new();
        // SAFETY: a record lives no longer than what it was made of (`spawn`)
        unsafe { (self.run)(self.data, i, &mut out) };
        out
    }
}

/// Runs the task `i` of `rec` and gives its result: the body of `aqr_task` (on WebAssembly).
///
/// # Safety
/// `rec` is the record of a [`spawn`] of the module, in the copy of its working memory.
#[cfg(target_family = "wasm")]
#[doc(hidden)]
pub unsafe fn body(rec: *const Record, i: usize) {
    let mut out = Vec::new();
    // SAFETY: as the caller makes sure
    unsafe { ((*rec).run)((*rec).data, i, &mut out) };
    crate::sys::result(&out);
}

/// Tasks spawned, each of whose results is to be taken once ([`Tasks::join`]); those not
/// taken are when the tasks are dropped.
#[must_use = "tasks are joined as they are dropped"]
pub struct Tasks<'a> {
    #[cfg(target_family = "wasm")]
    handle: u32,
    #[cfg(not(target_family = "wasm"))]
    results: Vec<Option<Vec<u8>>>,
    taken: Vec<bool>,
    _f: PhantomData<&'a ()>,
}

/// Spawns `n` tasks running `f(i, out)` for `i` in `0..n`, all on one copy of the working
/// memory. Trapping in one fails the application.
pub fn spawn<'a, F: Fn(usize, &mut Vec<u8>) + Sync + 'a>(n: usize, f: &'a F) -> Tasks<'a> {
    let rec = Record { run: run::<F>, data: f as *const F as *const () };
    #[cfg(target_family = "wasm")]
    let t = Tasks { handle: crate::sys::spawn(&rec, n), taken: alloc::vec![false; n], _f: PhantomData };
    #[cfg(not(target_family = "wasm"))]
    let t = Tasks { results: crate::sys::spawn(&rec, n), taken: alloc::vec![false; n], _f: PhantomData };
    t
}

impl Tasks<'_> {
    /// The number of tasks.
    pub fn len(&self) -> usize {
        self.taken.len()
    }

    pub fn is_empty(&self) -> bool {
        self.taken.is_empty()
    }

    /// The result of task `i`, once it is done. Panics if it was taken already.
    pub fn join(&mut self, i: usize) -> Vec<u8> {
        assert!(!core::mem::replace(&mut self.taken[i], true), "task {i} joined twice");
        #[cfg(target_family = "wasm")]
        {
            let mut out = alloc::vec![0; crate::sys::join(self.handle, i)];
            crate::sys::take(self.handle, i, &mut out);
            out
        }
        #[cfg(not(target_family = "wasm"))]
        self.results[i].take().expect("a result")
    }
}

impl Drop for Tasks<'_> {
    fn drop(&mut self) {
        for i in 0..self.taken.len() {
            if !self.taken[i] {
                self.taken[i] = true;
                #[cfg(target_family = "wasm")]
                {
                    crate::sys::join(self.handle, i);
                    crate::sys::take(self.handle, i, &mut []);
                }
            }
        }
    }
}
