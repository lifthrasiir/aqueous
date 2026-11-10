//! Tasks: work run apart from the module, each on a copy of the working memory as it is when
//! they are spawned, giving back bytes (`aqr_spawn`, `aqr_result`, `aqr_join`, `aqr_take`).
//!
//! [`spawn`] runs `f(i, out)` for each `i` of `0..n`, and [`Tasks::join`] gives what each put in
//! `out`. A task sees nothing of the module but the working memory: every other memory is
//! empty to it, and it cannot request inputs or commit outputs. So nothing that `f` does
//! reaches the module but `out`: not even what it changes through shared references (atomics,
//! cells), which natively it would.
//!
//! Tasks may run at once, and talk through channels ([`crate::chan`]).
//!
//! On WebAssembly a task is a new instance of the module, which `aqr-ld`'s `aqr_task` starts on
//! the stack below the spawning frame, so that what `f` borrows from it is there. Natively
//! ([`emu`](crate::emu)) each task runs on a thread of its own from its spawn.

use alloc::vec::Vec;
use core::marker::PhantomData;

/// The function of the tasks, as [`spawn`] gives it to them.
#[doc(hidden)]
#[repr(C)]
#[derive(Clone)]
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
    threads: Vec<Option<std::thread::JoinHandle<Vec<u8>>>>,
    taken: Vec<bool>,
    _f: PhantomData<&'a ()>,
}

/// Spawns `n` tasks running `f(i, out)` for `i` in `0..n`, all on one copy of the working
/// memory. Trapping in one fails the application.
///
/// Natively the tasks are threads borrowing `f`, joined as the [`Tasks`] are dropped: leaking
/// them (`mem::forget`, a cycle of `Rc`s) leaves the threads running after `f` is gone, so
/// spawning is unsafe, as nothing in safe code stops it.
///
/// # Safety
/// The [`Tasks`] are dropped (or each joined) before `'a` ends: not leaked.
///
/// ```compile_fail,E0133
/// let f = |_: usize, _: &mut Vec<u8>| {};
/// core::mem::forget(aqr_api::task::spawn(1, &f));
/// ```
pub unsafe fn spawn<'a, F: Fn(usize, &mut Vec<u8>) + Sync + 'a>(n: usize, f: &'a F) -> Tasks<'a> {
    // SAFETY: as the caller makes sure
    unsafe { spawn_class(0, n, f) }
}

/// [`spawn`] of tasks of class `class` of the module (the bytes of its custom section
/// `aqr_task_slots`; the application's instance spawns any class, a task one above its own), a
/// class that is not a gang class. The channels made since the instance's last spawn belong to
/// a gang (see [`spawn_gang`]): there are none here. Natively the class is of no matter: every
/// task has a thread.
///
/// # Safety
/// As [`spawn`].
pub unsafe fn spawn_class<'a, F: Fn(usize, &mut Vec<u8>) + Sync + 'a>(class: u32, n: usize, f: &'a F) -> Tasks<'a> {
    // SAFETY: as the caller makes sure
    unsafe { spawn_of(class, false, n, f) }
}

/// [`spawn_class`] of a gang class: the `n` tasks are a gang, which the channels made since the
/// instance's last spawn are given to (and no other instance's tasks may take their ends). The
/// class is a gang class in the module's `aqr_task_slots` (the top bit of its byte). The gang
/// is started together, so its tasks may wait for each other as channels make them; its memory
/// limit is of all its tasks and the messages queued in its channels.
///
/// # Safety
/// As [`spawn`].
pub unsafe fn spawn_gang<'a, F: Fn(usize, &mut Vec<u8>) + Sync + 'a>(class: u32, n: usize, f: &'a F) -> Tasks<'a> {
    // SAFETY: as the caller makes sure
    unsafe { spawn_of(class, true, n, f) }
}

unsafe fn spawn_of<'a, F: Fn(usize, &mut Vec<u8>) + Sync + 'a>(
    class: u32,
    gang: bool,
    n: usize,
    f: &'a F,
) -> Tasks<'a> {
    #[cfg(not(target_family = "wasm"))]
    let _ = class;
    #[cfg(target_family = "wasm")]
    let _ = gang;
    #[cfg(target_family = "wasm")]
    let t = Tasks {
        handle: spawn_record(class, run::<F>, f as *const F as *const (), n),
        taken: alloc::vec![false; n],
        _f: PhantomData,
    };
    #[cfg(not(target_family = "wasm"))]
    let rec = Record { run: run::<F>, data: f as *const F as *const () };
    // SAFETY: `f` outlives the threads, joined as the tasks are dropped (as the caller makes sure)
    #[cfg(not(target_family = "wasm"))]
    let t =
        Tasks { threads: unsafe { crate::sys::spawn(&rec, n, gang) }, taken: alloc::vec![false; n], _f: PhantomData };
    t
}

/// What `aqr_spawn` is given: the record of the function, which the tasks get as their
/// argument, and the region record that makes the tasks start from all of the working memory
/// (source and destination 0, its length).
#[cfg(target_family = "wasm")]
#[repr(C)]
pub(crate) struct Frame {
    /// First: the tasks' stack starts below it.
    pub(crate) rec: Record,
    pub(crate) region: [u32; 3],
}

/// `aqr_spawn` of the record of `run` and `data`, which this frame holds: never inlined, so
/// that the frame is below those of its callers, whose data the tasks (whose stack starts
/// below the record) find as they were. (The record within a caller's frame could be above
/// some of its data, which the tasks' frames would then overwrite.)
#[cfg(target_family = "wasm")]
#[inline(never)]
fn spawn_record(class: u32, run: unsafe fn(*const (), usize, &mut Vec<u8>), data: *const (), n: usize) -> u32 {
    let len = (core::arch::wasm32::memory_size::<0>() as u32) << 16;
    let frame = Frame { rec: Record { run, data }, region: [0, len, 0] };
    crate::sys::spawn(class, core::hint::black_box(&frame), n)
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
        // (a trap resumed here)
        #[cfg(not(target_family = "wasm"))]
        {
            let _w = crate::emu::waiting("tasks");
            self.threads[i].take().expect("a task").join().unwrap_or_else(|p| std::panic::resume_unwind(p))
        }
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
                // (a trap resumed here, unless already unwinding)
                #[cfg(not(target_family = "wasm"))]
                {
                    let _w = crate::emu::waiting("tasks");
                    if let Err(p) = self.threads[i].take().expect("a task").join()
                        && !std::thread::panicking()
                    {
                        std::panic::resume_unwind(p)
                    }
                }
            }
        }
    }
}
