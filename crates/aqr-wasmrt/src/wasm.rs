//! Modules run as WebAssembly, as the spec's "Module execution" defines (with the feature
//! `wasm`): by Wasmtime, linked (feature `wasmtime`, [`engine`]: always where the system is not
//! Windows), or by the interpreter blob, Wasmtime built of this same crate, that the program
//! places at run time ([`interp`], Windows x86-64: aqr carries the blob, and links no Wasmtime of
//! its own). What reads a module's bytes alone, and the bound of the applications' memories, is
//! here for both.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use aqr_formula::aqr::Failure;
use wasmparser as wp;

#[cfg(feature = "wasmtime")]
mod engine;
#[cfg(all(feature = "wasmtime", feature = "sandbox"))]
pub use engine::warm_up;
#[cfg(feature = "wasmtime")]
pub(crate) use engine::{Shared, compiled};
#[cfg(feature = "wasmtime")]
pub use engine::{load, load_bounded, load_released};

#[cfg(not(feature = "wasmtime"))]
mod interp;
#[cfg(all(not(feature = "wasmtime"), feature = "sandbox"))]
pub(crate) use interp::{interpreter_given, worker_main};
#[cfg(not(feature = "wasmtime"))]
pub use interp::{load, load_bounded, load_released, set_interpreter, warm_up};

pub(crate) type Decoded<T> = std::result::Result<T, Failure>;

/// The custom section of a module's task slots.
pub(crate) const TASK_SLOTS: &str = "aqr_task_slots";

/// A memory imported by a module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mem {
    Working,
    Params,
    Resource(u32),
    Input(u32),
    Output(u32),
}

/// `name` as `prefix` and an index without leading zeros.
fn indexed(name: &str, prefix: &str) -> Option<u32> {
    let s = name.strip_prefix(prefix)?;
    let ok = !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && (s == "0" || !s.starts_with('0'));
    if ok { s.parse().ok() } else { None }
}

pub(crate) fn memory_of(name: &str) -> Option<Mem> {
    Some(match name {
        "aqr_memory" => Mem::Working,
        "aqr_params" => Mem::Params,
        _ => {
            let name = name.strip_prefix("aqr_")?;
            if let Some(i) = indexed(name, "resource") {
                Mem::Resource(i)
            } else if let Some(k) = indexed(name, "input") {
                Mem::Input(k)
            } else {
                Mem::Output(indexed(name, "output")?)
            }
        }
    })
}

/// What an application is held to: the memory limit of its instance, the task limits of each
/// class of the module, and the concurrency chosen of each class.
#[derive(Clone, Copy)]
pub(crate) struct Bounds<'a> {
    pub memory_limit: u64,
    pub task_limits: &'a [u64],
    pub concurrency: &'a [usize],
}

/// The most the memories of an application may come to, for [`choose_concurrency`]: set from the
/// budget a user gave (`aqr -M`), none by default.
static BOUND: AtomicU64 = AtomicU64::new(u64::MAX);

/// Bounds the memories of the applications from now on to `bytes` (`u64::MAX`: no bound): the
/// instance's limit and, for each class of tasks, its concurrency times its task limit. A decoder
/// lowers the concurrency of the classes to fit, and fails an application that does not fit
/// even at 1 with a resource failure before it starts.
pub fn set_application_bound(bytes: u64) {
    BOUND.store(bytes, Ordering::Relaxed);
    #[cfg(not(feature = "wasmtime"))]
    interp::bound_set(bytes);
}

/// Where the interpreter blob comes from, for running modules without Wasmtime linked
/// ([`interp`]); nothing where it is.
#[cfg(feature = "wasmtime")]
pub fn set_interpreter(_: fn() -> Result<Vec<u8>, String>) {}

/// The bound set ([`set_application_bound`]).
pub(crate) fn application_bound_set() -> u64 {
    BOUND.load(Ordering::Relaxed)
}

/// The concurrency of each class of an application of the task `slots` of a module, its memory
/// limit and task limits: the slots, lowered by one for the class of the largest task limit
/// that is above 1 while the memory limit and the sum of concurrency times task limit exceed
/// `bound`; a resource failure if even 1 each exceeds it. A limit of [`aqr_formula::NO_LIMIT`]
/// (not known) counts for nothing, and the earlier Apply (no task limits) has the slots.
pub(crate) fn choose_concurrency(
    slots: &[usize],
    memory_limit: u64,
    task_limits: &[u64],
    bound: u64,
) -> Decoded<Vec<usize>> {
    let mut concurrency = slots.to_vec();
    if bound == u64::MAX || task_limits.len() != slots.len() {
        return Ok(concurrency);
    }
    let known = |l: u64| if l == aqr_formula::NO_LIMIT { 0 } else { l as u128 };
    let total = |c: &[usize]| {
        known(memory_limit) + c.iter().zip(task_limits).map(|(&n, &l)| n as u128 * known(l)).sum::<u128>()
    };
    while total(&concurrency) > bound as u128 {
        let lower = (0..slots.len()).filter(|&c| concurrency[c] > 1).max_by_key(|&c| known(task_limits[c]));
        let Some(c) = lower else {
            return Err(Failure::Resource(format!(
                "the memory of an application, at least {} bytes, is over the bound of {bound} bytes",
                total(&concurrency)
            )));
        };
        concurrency[c] -= 1;
    }
    Ok(concurrency)
}

/// The most the memories of an application may take: `memory_limit`, and with task limits, those
/// of as many units of each class at once as its `concurrency` ([`choose_concurrency`]). A limit
/// of [`aqr_formula::NO_LIMIT`] (not known) saturates it, where `choose_concurrency` counts it for
/// nothing.
pub fn application_bound(memory_limit: u64, task_limits: &[u64], concurrency: &[u64]) -> u64 {
    let tasks = task_limits.iter().zip(concurrency).fold(0u64, |t, (&l, &n)| t.saturating_add(l.saturating_mul(n)));
    memory_limit.saturating_add(tasks)
}

/// The pages of the working memory of `wasm` as it is instantiated: the minimum of its import
/// (the module's data and stack), for its memory plan ([`aqr_formula::Usage`]).
pub fn initial_pages(wasm: &[u8]) -> Option<u64> {
    for p in wp::Parser::new(0).parse_all(wasm) {
        if let wp::Payload::ImportSection(r) = p.ok()? {
            for i in r.into_imports() {
                let i = i.ok()?;
                if let (Some(Mem::Working), wp::TypeRef::Memory(m)) = (memory_of(i.name), i.ty) {
                    return Some(m.initial);
                }
            }
        }
    }
    None
}

/// The windows of the memories of `wasm`'s inputs and outputs whose import types have a
/// maximum (of a part module): whether each is an output's, its index, and its pages.
pub fn windows(wasm: &[u8]) -> Vec<(bool, u32, u64)> {
    let mut out = Vec::new();
    for p in wp::Parser::new(0).parse_all(wasm) {
        let Ok(wp::Payload::ImportSection(r)) = p else { continue };
        for i in r.into_imports().flatten() {
            let wp::TypeRef::Memory(m) = i.ty else { continue };
            match (memory_of(i.name), m.maximum) {
                (Some(Mem::Input(k)), Some(max)) => out.push((false, k, max)),
                (Some(Mem::Output(j)), Some(max)) => out.push((true, j, max)),
                _ => {}
            }
        }
        // (the imports are all there is to read: not the thousands of functions after them)
        break;
    }
    out
}

/// An application of a module, as [`record_applications`] keeps them: what it was given and
/// the largest size its memories reached.
#[derive(Debug, Clone)]
pub struct Applied {
    /// SHA-512/256 of the module.
    pub hash: [u8; 32],
    /// The lengths of the parameters, the resources' contents, the inputs and the outputs.
    pub params: u64,
    /// The parameters themselves, and the inputs.
    pub params_data: Vec<u8>,
    pub contents: Vec<Vec<u8>>,
    pub resources: Vec<u64>,
    pub inputs: Vec<u64>,
    pub sizes: Vec<u64>,
    /// The bytes of the memories imported but the working memory, which are made at the start.
    pub imported: u64,
    /// The memory limit declared.
    pub limit: u64,
    /// The peak of everything else in the memory limit's total: the working memories of the
    /// application's instance and of its tasks, and the messages queued in its channels.
    pub working: u64,
    /// Of that, the most the application's instance held (its working memory, the messages
    /// queued aside), and the most one task of each class did (its working memory).
    pub instance: u64,
    /// The most a unit of each class held at once: one task's memories, or a gang's with the
    /// messages queued in its channels; and the most the memories of the tasks of a unit came to
    /// added up as each ended (`task_memories`).
    pub tasks: Vec<u64>,
    pub task_memories: Vec<u64>,
    /// The concurrency of each class of tasks (`choose_concurrency`) it ran at.
    pub concurrency: Vec<usize>,
    pub ok: bool,
}

pub(crate) static APPLIED: Mutex<Option<Vec<Applied>>> = Mutex::new(None);

/// Starts keeping the applications run from now on (`true`), or stops (`false`); returns those
/// kept so far, for measuring the memory of modules.
pub fn record_applications(on: bool) -> Vec<Applied> {
    let mut a = APPLIED.lock().unwrap();
    let kept = a.take().unwrap_or_default();
    if on {
        *a = Some(Vec::new());
    }
    kept
}
