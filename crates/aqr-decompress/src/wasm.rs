//! Modules run as WebAssembly by Wasmtime, as the spec's "Module execution" defines (with the
//! feature `wasm`): the module checked against the profile, the memories of an application
//! created and the entry point invoked, with the host functions.
//!
//! A trap, or a violation of a host function, is an invalid archive. Exceeding the memory
//! limit or the stack is a resource failure. The last `aqr_log` message before a failure is
//! part of it.
//!
//! Tasks (`aqr_spawn`) are instances of their own, of a copy of the working memory at the
//! spawn and empty other memories, which channels (`aqr_chan`) link. Every instance runs on a
//! fiber (`call_async`), and the instances waiting (joining a task, sending to a full channel or
//! receiving from an empty one) leave their threads to the others: those of [`Exec`], the thread
//! applying the module and workers of aqr-common's pool. Where every instance waits, the
//! application is an invalid archive (a deadlock). The memory limit is over all the memories of
//! the application, and the messages queued in its channels.

use std::collections::{HashMap, VecDeque};
use std::future::poll_fn;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::task::{Poll, Waker};

use crate::exec::{Exec, Stopped};

use aqr_formula::Resource;
use aqr_formula::aqr::{Failure, Loaded, PartIn, PartOut};
use wasmparser::{
    Validator, WasmFeatures, {self as wp},
};
use wasmtime::{
    Caller, Config, Engine, Extern, Func, Instance, Memory, MemoryType, Module, ResourceLimiter, Store, Trap, Val,
};

type Decoded<T> = std::result::Result<T, Failure>;

const WHOLE: &str = "aqr_apply_whole";
const PART: &str = "aqr_apply_part";
const TASK: &str = "aqr_task";
/// The custom section of a module's task slots.
const TASK_SLOTS: &str = "aqr_task_slots";

fn invalid<T>(msg: impl Into<String>) -> Decoded<T> {
    Err(Failure::Invalid(msg.into()))
}

static ENGINE: LazyLock<Engine> = LazyLock::new(|| {
    let mut c = Config::new();
    // WebAssembly 2.0, with multiple memories and 64-bit addresses, deterministic (the rest of
    // 3.0 is not built in, or turned down by `compile`)
    c.wasm_multi_memory(true)
        .wasm_memory64(true)
        .wasm_simd(true)
        .wasm_relaxed_simd(false)
        .wasm_tail_call(false)
        .wasm_extended_const(false)
        .wasm_gc(false)
        .wasm_wide_arithmetic(false)
        .wasm_custom_page_sizes(false)
        .wasm_stack_switching(false)
        .cranelift_nan_canonicalization(true);
    // the working memories (as defined by `Compiled::working_module`) copied on write over,
    // where the system has placeholders (else copied)
    #[cfg(windows)]
    if crate::cow::placeholders() {
        c.with_host_memory(Arc::new(crate::cow::Creator));
    }
    Engine::new(&c).expect("the configuration of Wasmtime")
});

/// The engine made, and a module compiled: what Wasmtime makes or loads once a process (its
/// threads of compilation), made before a worker of the sandbox is locked down.
#[cfg(feature = "sandbox")]
pub fn warm_up() {
    // a module of a function of no parameters and results, doing nothing
    const MODULE: &[u8] = b"\0asm\x01\0\0\0\x01\x04\x01\x60\0\0\x03\x02\x01\0\x0a\x04\x01\x02\0\x0b";
    Module::new(&ENGINE, MODULE).expect("a module of a function compiled");
}

/// The modules compiled, by their hashes, kept for the process.
static COMPILED: LazyLock<Mutex<HashMap<[u8; 32], Shared>>> = LazyLock::new(Default::default);

/// A module compiled, as [`load`] gives it.
#[derive(Clone)]
pub(crate) struct Shared(Arc<Compiled>);

/// A memory imported by a module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mem {
    Working,
    Params,
    Resource(u32),
    Input(u32),
    Output(u32),
}

/// A function imported by a module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostFn {
    Request,
    Release,
    Commit,
    Heartbeat,
    Log,
    Spawn,
    SpawnV2,
    Result,
    Join,
    Take,
    Chan,
    Send,
    Recv,
    Close,
}

enum Import {
    Memory(Mem),
    Func(HostFn),
}

/// A module of the profile, compiled.
struct Compiled {
    module: Module,
    /// The imports, in order.
    imports: Vec<Import>,
    /// The working memory's minimum and maximum, in pages.
    working: (u64, Option<u64>),
    /// Where the working memory is of zeros from once instantiated: the end of the data
    /// segments written into it (`u64::MAX` where not known).
    clean: u64,
    /// A module defining a memory of the working memory's type (exported as `m`), the working
    /// memories made as it defines them where they are to be made so (of `cow::Creator`).
    working_module: Option<Module>,
    part: bool,
    /// The windows of the memories of the inputs and outputs whose import types have a maximum
    /// (of a part module), in pages.
    windows: Vec<(Mem, u64)>,
    /// The number of parameters of the entry point.
    arity: usize,
    /// The task slots of each class of its tasks (none without tasks).
    slots: Vec<usize>,
    /// SHA-512/256 of the module.
    hash: [u8; 32],
}

/// `wasm`, of SHA-512/256 `hash`, checked against the profile and compiled (once a process).
pub fn load(wasm: &[u8], hash: [u8; 32]) -> Decoded<Box<dyn Loaded>> {
    Ok(Box::new(compiled(wasm, hash)?))
}

/// [`load`], of the module itself (for [`Shared::apply_into`]).
pub(crate) fn compiled(wasm: &[u8], hash: [u8; 32]) -> Decoded<Shared> {
    if let Some(c) = COMPILED.lock().unwrap().get(&hash) {
        return Ok(c.clone());
    }
    let c = Shared(Arc::new(Compiled { hash, ..compile(wasm)? }));
    COMPILED.lock().unwrap().insert(hash, c.clone());
    Ok(c)
}

/// `name` as `prefix` and an index without leading zeros.
fn indexed(name: &str, prefix: &str) -> Option<u32> {
    let s = name.strip_prefix(prefix)?;
    let ok = !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && (s == "0" || !s.starts_with('0'));
    if ok { s.parse().ok() } else { None }
}

fn memory_of(name: &str) -> Option<Mem> {
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

/// The host function `name`, of its parameters and results.
#[allow(clippy::type_complexity)]
fn func_of(name: &str) -> Option<(HostFn, &'static [wp::ValType], &'static [wp::ValType])> {
    use wp::ValType::{I32, I64};
    Some(match name {
        "aqr_request" => (HostFn::Request, &[I32, I64], &[]),
        "aqr_release" => (HostFn::Release, &[I32, I64], &[]),
        "aqr_commit" => (HostFn::Commit, &[I32, I64], &[]),
        "aqr_heartbeat" => (HostFn::Heartbeat, &[], &[]),
        "aqr_log" => (HostFn::Log, &[I32, I32], &[]),
        "aqr_spawn" => (HostFn::Spawn, &[I32, I32], &[I32]),
        "aqr_spawn_v2" => (HostFn::SpawnV2, &[I32, I32, I32, I32, I32], &[I32]),
        "aqr_result" => (HostFn::Result, &[I32, I32], &[]),
        "aqr_join" => (HostFn::Join, &[I32, I32], &[I32]),
        "aqr_take" => (HostFn::Take, &[I32, I32, I32, I32], &[]),
        "aqr_chan" => (HostFn::Chan, &[I32], &[I32]),
        "aqr_send" => (HostFn::Send, &[I32, I32, I32], &[]),
        "aqr_recv" => (HostFn::Recv, &[I32, I32, I32], &[I32]),
        "aqr_close" => (HostFn::Close, &[I32], &[]),
        _ => return None,
    })
}

/// Checks `wasm` against the profile (the spec, "Module execution"), and compiles it.
fn compile(wasm: &[u8]) -> Decoded<Compiled> {
    let bad = |e: wp::BinaryReaderError| Failure::Invalid(format!("not a module of the profile: {e}"));
    Validator::new_with_features(WasmFeatures::WASM2 | WasmFeatures::MULTI_MEMORY | WasmFeatures::MEMORY64)
        .validate_all(wasm)
        .map_err(bad)?;
    let (mut types, mut funcs) = (Vec::new(), Vec::new());
    let (mut imports, mut working, mut entry, mut task) = (Vec::new(), None, None, None);
    let mut memories: Vec<Mem> = Vec::new();
    let mut windows: Vec<(Mem, u64)> = Vec::new();
    let mut grows_other = false;
    let mut clean = 0u64;
    let mut slots = Vec::new();
    for payload in wp::Parser::new(0).parse_all(wasm) {
        match payload.map_err(bad)? {
            wp::Payload::TypeSection(r) => {
                for t in r.into_iter_err_on_gc_types() {
                    types.push(t.map_err(bad)?);
                }
            }
            wp::Payload::ImportSection(r) => {
                for i in r.into_imports() {
                    let i = i.map_err(bad)?;
                    if i.module != "aqr" {
                        return invalid(format!("an import from {}", i.module));
                    }
                    match i.ty {
                        wp::TypeRef::Memory(m) => {
                            let Some(mem) = memory_of(i.name) else { return invalid(format!("an import {}", i.name)) };
                            if memories.contains(&mem) {
                                return invalid(format!("{} imported twice", i.name));
                            }
                            if m.shared {
                                return invalid("a shared memory");
                            }
                            if mem == Mem::Working {
                                if m.memory64 {
                                    return invalid("a 64-bit working memory");
                                }
                                working = Some((m.initial, m.maximum));
                            } else if !m.memory64 || m.initial != 0 {
                                return invalid(format!("{} not 64-bit of minimum 0", i.name));
                            } else if let Some(max) = m.maximum {
                                if !matches!(mem, Mem::Input(_) | Mem::Output(_)) || max == 0 {
                                    return invalid(format!("{} of a maximum", i.name));
                                }
                                windows.push((mem, max));
                            }
                            memories.push(mem);
                            imports.push(Import::Memory(mem));
                        }
                        wp::TypeRef::Func(t) => {
                            let Some((f, params, results)) = func_of(i.name) else {
                                return invalid(format!("an import {}", i.name));
                            };
                            let ft = types.get(t as usize).ok_or_else(|| Failure::Invalid("a bad type".into()))?;
                            if ft.params() != params || ft.results() != results {
                                return invalid(format!("{} of another type", i.name));
                            }
                            funcs.push(t);
                            imports.push(Import::Func(f));
                        }
                        _ => return invalid(format!("an import {} of a table, global or tag", i.name)),
                    }
                }
            }
            wp::Payload::FunctionSection(r) => {
                for t in r {
                    funcs.push(t.map_err(bad)?);
                }
            }
            wp::Payload::MemorySection(r) if r.count() > 0 => return invalid("a memory defined"),
            wp::Payload::StartSection { .. } => return invalid("a start function"),
            wp::Payload::ExportSection(r) => {
                for e in r {
                    let e = e.map_err(bad)?;
                    if (e.name == WHOLE || e.name == PART) && e.kind == wp::ExternalKind::Func {
                        if entry.is_some() {
                            return invalid("two entry points");
                        }
                        entry = Some((e.name == PART, e.index));
                    } else if e.name == TASK && e.kind == wp::ExternalKind::Func {
                        task = Some(e.index);
                    }
                }
            }
            wp::Payload::DataSection(r) => {
                for d in r {
                    let d = d.map_err(bad)?;
                    let wp::DataKind::Active { memory_index, offset_expr } = d.kind else { continue };
                    if memories.get(memory_index as usize) != Some(&Mem::Working) {
                        continue;
                    }
                    let mut ops = offset_expr.get_operators_reader();
                    let end = match (ops.read(), ops.read()) {
                        (Ok(wp::Operator::I32Const { value }), Ok(wp::Operator::End)) => {
                            value as u32 as u64 + d.data.len() as u64
                        }
                        _ => u64::MAX,
                    };
                    clean = clean.max(end);
                }
            }
            wp::Payload::CustomSection(c) if c.name() == TASK_SLOTS => slots.push(c.data().to_vec()),
            wp::Payload::CodeSectionEntry(body) => {
                let mut ops = body.get_operators_reader().map_err(bad)?;
                while !ops.eof() {
                    if let wp::Operator::MemoryGrow { mem } = ops.read().map_err(bad)? {
                        grows_other |= memories.get(mem as usize) != Some(&Mem::Working);
                    }
                }
            }
            _ => {}
        }
    }
    let Some(working) = working else { return invalid("no working memory") };
    if grows_other {
        return invalid("memory.grow of a memory other than the working memory");
    }
    let Some((part, f)) = entry else { return invalid("no entry point") };
    let ft = funcs.get(f as usize).and_then(|&t| types.get(t as usize));
    let ft = ft.ok_or_else(|| Failure::Invalid("a bad entry point".into()))?;
    if ft.params().iter().any(|&p| p != wp::ValType::I64) || !ft.results().is_empty() {
        return invalid("an entry point of other than i64 parameters, or of results");
    }
    let parts = imports.iter().any(|i| matches!(i, Import::Func(HostFn::Request | HostFn::Release | HostFn::Commit)));
    if parts && !part {
        return invalid("a whole module importing functions of part modules");
    }
    if !windows.is_empty() && !part {
        return invalid("a whole module of a memory of a maximum");
    }
    let spawns = imports.iter().any(|i| matches!(i, Import::Func(HostFn::Spawn | HostFn::SpawnV2)));
    if spawns {
        let ft = task.and_then(|f| funcs.get(f as usize)).and_then(|&t| types.get(t as usize));
        let Some(ft) = ft else { return invalid(format!("tasks spawned without {TASK}")) };
        if ft.params() != [wp::ValType::I32, wp::ValType::I32] || !ft.results().is_empty() {
            return invalid(format!("{TASK} of another type"));
        }
    }
    let slots: Vec<usize> = match (spawns, &slots[..]) {
        (true, [d]) if !d.is_empty() && d.iter().all(|n| (1..=64).contains(n)) => {
            d.iter().map(|&n| n as usize).collect()
        }
        (true, [_]) => return invalid(format!("{TASK_SLOTS} not bytes from 1 to 64")),
        (true, []) => return invalid(format!("tasks spawned without {TASK_SLOTS}")),
        (true, _) => return invalid(format!("{TASK_SLOTS} more than once")),
        (false, []) => Vec::new(),
        (false, _) => return invalid(format!("{TASK_SLOTS} without tasks")),
    };
    let module = Module::new(&ENGINE, wasm).map_err(|e| Failure::Invalid(format!("not compiled: {e}")))?;
    let working_module = if cfg!(windows) {
        let m = Module::new(&ENGINE, memory_module(working));
        Some(m.map_err(|e| Failure::Resource(format!("the working memory not compiled: {e}")))?)
    } else {
        None
    };
    let arity = ft.params().len();
    Ok(Compiled { module, imports, working, clean, working_module, part, windows, arity, slots, hash: [0; 32] })
}

/// A module defining a memory of 32 bits of `limits` (in pages), exported as `m`.
fn memory_module((min, max): (u64, Option<u64>)) -> Vec<u8> {
    fn leb(v: &mut Vec<u8>, mut n: u64) {
        loop {
            let b = (n & 0x7f) as u8;
            n >>= 7;
            v.push(if n == 0 { b } else { b | 0x80 });
            if n == 0 {
                return;
            }
        }
    }
    let mut limits = vec![max.is_some() as u8];
    leb(&mut limits, min);
    if let Some(max) = max {
        leb(&mut limits, max);
    }
    let mut m = b"\0asm\x01\0\0\0".to_vec();
    // the memory section: one memory
    m.push(5);
    leb(&mut m, 1 + limits.len() as u64);
    m.push(1);
    m.extend(limits);
    // the export section: memory 0 as `m`
    m.extend([7, 5, 1, 1, b'm', 2, 0]);
    m
}

/// The memory limit of an application, as the memories are made and grown.
#[derive(Debug)]
struct OverLimit;

impl std::fmt::Display for OverLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("memory limit exceeded")
    }
}

impl std::error::Error for OverLimit {}

/// A violation of a host function.
#[derive(Debug)]
struct Violation(String);

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Violation {}

use crate::cow::Snapshot;

/// The failure of a task, which is the application's as the task is joined.
#[derive(Debug)]
struct TaskFailed(Failure);

impl std::fmt::Display for TaskFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "a task failed: {:?}", self.0)
    }
}

impl std::error::Error for TaskFailed {}

/// What the instances of an application share.
struct App {
    limit: u64,
    /// The task limits (none: all of the application under `limit`), and then the slots of each
    /// class over the application.
    task_limits: Vec<u64>,
    classes: Vec<Arc<Slots>>,
    /// The size of the memories of all the instances and of the messages queued.
    total: Mutex<u64>,
    /// The largest `total` so far, and the most the application's instance and any one task of
    /// each class held (their working memories, the instance's other memories with it).
    peak: AtomicU64,
    instance_peak: AtomicU64,
    task_peaks: Vec<AtomicU64>,
    exec: Arc<Exec>,
    chans: Mutex<Vec<Chan>>,
    /// The identity of the next task (0 is the application's instance).
    next: AtomicU64,
    /// The failure of a task, which ends the application.
    failed: Mutex<Option<Failure>>,
}

impl App {
    /// Adds `add` bytes to the total and takes `sub` from it, unless it would exceed the limit.
    fn account(&self, add: u64, sub: u64) -> wasmtime::Result<()> {
        let mut total = self.total.lock().unwrap();
        let t = (*total - sub).checked_add(add).filter(|&t| t <= self.limit);
        *total = t.ok_or(OverLimit)?;
        self.peak.fetch_max(*total, Ordering::Relaxed);
        Ok(())
    }
}

/// A channel: its messages queued, and the instances at its ends (by identity), as they first
/// use them.
struct Chan {
    cap: usize,
    msgs: VecDeque<Vec<u8>>,
    closed: bool,
    sender: Option<u64>,
    receiver: Option<u64>,
    /// The instances waiting to send (a message taken) and to receive (one sent, or closing).
    sending: Vec<Waker>,
    receiving: Vec<Waker>,
}

/// A task, as the instance that spawned it sees it.
#[derive(Default)]
struct Cell(Mutex<Spawned>);

#[derive(Default)]
struct Spawned {
    /// Its result, once it is done (taken with it).
    result: Option<Decoded<Vec<u8>>>,
    joined: bool,
    /// The instances waiting for it.
    waiting: Vec<Waker>,
}

impl Cell {
    /// The task done, with `r`.
    fn finish(&self, r: Decoded<Vec<u8>>) {
        let mut c = self.0.lock().unwrap();
        c.result = Some(r);
        let waiting = std::mem::take(&mut c.waiting);
        drop(c);
        wake(waiting);
    }
}

/// Wakes `wakers`, with no lock held: waking or dropping a waker may drop an instance, which
/// closes its channels.
fn wake(wakers: Vec<Waker>) {
    wakers.into_iter().for_each(Waker::wake);
}

/// The tasks an instance spawned: how many are running, and those to start as they end, in the
/// order spawned. A task to start is not an instance yet: it has no memory, and is not one that
/// waits (so where those running all wait, it is a deadlock).
#[derive(Default)]
struct Slots(Mutex<(usize, VecDeque<Boxed>)>);

type Boxed = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;

impl Slots {
    /// Runs `task` on `exec` once one of the `slots` is free.
    fn start(self: &Arc<Self>, exec: &Arc<Exec>, slots: usize, task: Boxed) {
        let mut s = self.0.lock().unwrap();
        if s.0 < slots {
            s.0 += 1;
            drop(s);
            self.run(exec, task);
        } else {
            s.1.push_back(task);
        }
    }

    /// Runs `task`, which holds a slot, and then the next to start in its slot.
    fn run(self: &Arc<Self>, exec: &Arc<Exec>, task: Boxed) {
        let (slots, ex) = (self.clone(), exec.clone());
        exec.spawn(async move {
            task.await;
            let next = {
                let mut s = slots.0.lock().unwrap();
                let next = s.1.pop_front();
                if next.is_none() {
                    s.0 -= 1;
                }
                next
            };
            if let Some(next) = next {
                slots.run(&ex, next);
            }
        });
    }
}

/// `aqr_spawn_v2`'s work for the instance of `c`: `n` tasks of class `class` starting as `start`
/// gives, appended to the handles; the index of their handle.
fn spawn(c: &mut Caller<'_, State>, class: usize, start: Start, arg: u32, n: u32) -> wasmtime::Result<i32> {
    let s = c.data_mut();
    let classes = s.c.0.slots.len();
    if class >= classes {
        return violation(format!("a task of class {class} of {classes}"));
    }
    // (with task limits, a task spawns only classes above its own: none waits holding a slot)
    if s.own_limit() && class <= s.class {
        return violation(format!("a task of class {} spawning one of class {class}", s.class));
    }
    let slots = match s.app.task_limits.is_empty() {
        true => {
            if s.slots.len() < classes {
                s.slots.resize_with(classes, Arc::default);
            }
            s.slots[class].clone()
        }
        false => s.app.classes[class].clone(),
    };
    let most = s.c.0.slots[class];
    let cells: Vec<Option<Arc<Cell>>> = (0..n)
        .map(|i| {
            let cell = Arc::new(Cell::default());
            let job = Job { c: s.c.clone(), app: s.app.clone(), class, start: start.clone(), arg, i };
            let (done, app) = (cell.clone(), s.app.clone());
            let task = Box::pin(async move {
                let r = job.run().await;
                // (a task failing fails the application at once: those waiting for it through
                // channels would wait for ever)
                if let Err(f) = &r {
                    app.failed.lock().unwrap().get_or_insert_with(|| f.clone());
                    app.exec.stop();
                }
                done.finish(r)
            });
            slots.start(&s.app.exec, most, task);
            Some(cell)
        })
        .collect();
    s.spawned.push(cells);
    Ok((s.spawned.len() - 1) as i32)
}

/// What a task runs: `aqr_task(arg, i)` in an instance of `c`, of class `class`, whose working
/// memory starts as `start` gives.
struct Job {
    c: Shared,
    app: Arc<App>,
    class: usize,
    start: Start,
    arg: u32,
    i: u32,
}

/// What a task's working memory starts as: a copy of the spawning instance's (`aqr_spawn`), or
/// regions of it, each written at its offset (`aqr_spawn_v2`).
#[derive(Clone)]
enum Start {
    Copy(Arc<Snapshot>),
    Regions(Arc<Vec<(u64, Vec<u8>)>>),
}

/// An output of an application: where it goes, its memory, and (of a part module) the bytes
/// committed so far.
struct Output {
    buf: Drain,
    memory: Option<Memory>,
    done: u64,
}

/// The data of an input of an application: whole, or pulled as a part module requests it.
enum Feed {
    Bytes(&'static [u8]),
    /// (under a lock only to be shared with the state, which is taken but by its owner)
    Pull(Mutex<&'static mut dyn PartIn>),
}

impl Feed {
    fn len(&self) -> u64 {
        match self {
            Feed::Bytes(d) => d.len() as u64,
            Feed::Pull(p) => p.lock().unwrap().len(),
        }
    }
}

/// Where an output of an application goes: the caller's buffer (of the size declared), or
/// pushed as a part module commits it (of so many bytes).
enum Drain {
    Buf(&'static mut [u8]),
    Push(Mutex<&'static mut dyn PartOut>, u64),
}

impl Drain {
    fn len(&self) -> u64 {
        match self {
            Drain::Buf(b) => b.len() as u64,
            Drain::Push(_, n) => *n,
        }
    }
}

/// Writes `d` to the ring buffer `m` of `w` bytes from offset `at`.
fn ring_write(m: &mut [u8], w: u64, mut at: u64, mut d: &[u8]) {
    while !d.is_empty() {
        let from = (at % w) as usize;
        let n = d.len().min(w as usize - from);
        m[from..from + n].copy_from_slice(&d[..n]);
        (at, d) = (at + n as u64, &d[n..]);
    }
}

/// What the host functions of an instance work on.
struct State {
    c: Shared,
    app: Arc<App>,
    id: u64,
    /// The bytes of the memories of this instance, in the total of `app`.
    mine: u64,
    working: Option<Memory>,
    /// The data of the inputs of a part module, their memories and visible ranges.
    inputs: Vec<(Feed, Option<Memory>, u64, u64)>,
    /// The outputs, their memories and (of a part module) the bytes committed so far.
    outputs: Vec<Output>,
    last_log: Option<Vec<u8>>,
    /// Whether the instance is a task (and its class), and its result.
    task: bool,
    class: usize,
    result: Vec<u8>,
    /// The tasks spawned, by handle; taken where `None`.
    spawned: Vec<Vec<Option<Arc<Cell>>>>,
    /// Of each class, the tasks spawned running and those to start (without task limits; with
    /// them, the application's `classes`).
    slots: Vec<Arc<Slots>>,
}

impl State {
    fn new(c: Shared, app: Arc<App>, task: Option<usize>) -> State {
        let (class, task) = (task.unwrap_or(0), task.is_some());
        let id = if task { app.next.fetch_add(1, Ordering::Relaxed) } else { 0 };
        State {
            c,
            app,
            id,
            mine: 0,
            working: None,
            inputs: Vec::new(),
            outputs: Vec::new(),
            last_log: None,
            task,
            class,
            result: Vec::new(),
            spawned: Vec::new(),
            slots: Vec::new(),
        }
    }

    /// Whether its memories are under a limit of their own (a task's, with task limits) rather
    /// than in the application's total.
    fn own_limit(&self) -> bool {
        self.task && !self.app.task_limits.is_empty()
    }

    /// Whether a task spawned is yet to be taken.
    fn untaken(&self) -> bool {
        self.spawned.iter().flatten().any(Option::is_some)
    }

    /// The task `i` of handle `h`, not taken.
    fn cell(&self, h: i32, i: i32) -> Option<Arc<Cell>> {
        self.spawned.get(h as u32 as usize)?.get(i as u32 as usize)?.clone()
    }
}

impl Drop for State {
    /// The memories given back, and the channels this instance sends to closed.
    fn drop(&mut self) {
        if !self.own_limit() {
            let _ = self.app.account(0, self.mine);
        }
        let mut waiting = Vec::new();
        for ch in self.app.chans.lock().unwrap().iter_mut().filter(|ch| ch.sender == Some(self.id)) {
            ch.closed = true;
            waiting.append(&mut ch.receiving);
        }
        wake(waiting);
    }
}

impl ResourceLimiter for State {
    fn memory_growing(&mut self, current: usize, desired: usize, _: Option<usize>) -> wasmtime::Result<bool> {
        let mine = self.mine - current as u64 + desired as u64;
        match self.own_limit() {
            // (a task under its class's limit on its own)
            true if mine > self.app.task_limits[self.class] => return Err(wasmtime::Error::new(OverLimit)),
            true => {}
            false => self.app.account(desired as u64, current as u64)?,
        }
        self.mine = mine;
        match self.task {
            true => self.app.task_peaks[self.class].fetch_max(self.mine, Ordering::Relaxed),
            false => self.app.instance_peak.fetch_max(self.mine, Ordering::Relaxed),
        };
        Ok(true)
    }

    fn table_growing(&mut self, _: usize, _: usize, _: Option<usize>) -> wasmtime::Result<bool> {
        Ok(true)
    }
}

fn violation<T>(msg: String) -> wasmtime::Result<T> {
    Err(wasmtime::Error::new(Violation(msg)))
}

/// The failure of an invocation of the instance of `store`, with its last `aqr_log` message.
fn failure(store: &Store<State>, e: wasmtime::Error) -> Failure {
    let f = if e.is::<OverLimit>() {
        Failure::Resource("memory limit exceeded".into())
    } else if let Some(v) = e.downcast_ref::<Violation>() {
        Failure::Invalid(v.0.clone())
    } else if let Some(TaskFailed(f)) = e.downcast_ref() {
        // (with the task's message already)
        return f.clone();
    } else {
        match e.downcast_ref::<Trap>() {
            Some(Trap::StackOverflow) => Failure::Resource("call stack exhausted".into()),
            Some(t) => Failure::Invalid(format!("trap: {t}")),
            None => Failure::Invalid(format!("{e}")),
        }
    };
    match (f, &store.data().last_log) {
        (Failure::Invalid(m), Some(log)) => Failure::Invalid(format!("{m}: {}", String::from_utf8_lossy(log))),
        (f, _) => f,
    }
}

/// Instantiates `c` in `store` with the memories of the types and data `mem` gives, and the
/// host functions; returns the instance and the memories of the outputs.
async fn instantiate<'a>(
    store: &mut Store<State>,
    c: &Compiled,
    mem: &mut (dyn FnMut(Mem) -> (MemoryType, Option<&'a [u8]>) + Send),
) -> Decoded<(Instance, Vec<Option<Memory>>)> {
    let over = |e: wasmtime::Error| match e.downcast_ref::<OverLimit>() {
        Some(_) => Failure::Resource("memory limit exceeded".into()),
        None => Failure::Resource(format!("{e}")),
    };
    let mut externs = Vec::with_capacity(c.imports.len());
    let mut outputs = vec![None; store.data().outputs.len()];
    for i in &c.imports {
        externs.push(match *i {
            Import::Memory(m) => {
                let (ty, init) = mem(m);
                let memory = match (m, &c.working_module) {
                    (Mem::Working, Some(wm)) => defined(store, wm, ty.minimum()).await.map_err(over)?,
                    _ => Memory::new(&mut *store, ty).map_err(over)?,
                };
                if let Some(d) = init {
                    memory.data_mut(&mut *store)[..d.len()].copy_from_slice(d);
                }
                let s = store.data_mut();
                match m {
                    Mem::Working => s.working = Some(memory),
                    Mem::Input(k) => {
                        if let Some(i) = s.inputs.get_mut(k as usize) {
                            i.1 = Some(memory);
                        }
                    }
                    Mem::Output(j) => {
                        if let Some(o) = s.outputs.get_mut(j as usize) {
                            o.memory = Some(memory);
                            outputs[j as usize] = Some(memory);
                        }
                    }
                    _ => {}
                }
                Extern::Memory(memory)
            }
            Import::Func(f) => Extern::Func(host(store, f)),
        });
    }
    let instance = Instance::new_async(&mut *store, &c.module, &externs).await.map_err(|e| {
        match e.downcast_ref::<OverLimit>() {
            Some(_) => Failure::Resource("memory limit exceeded".into()),
            None => Failure::Invalid(format!("not instantiated: {e}")),
        }
    })?;
    Ok((instance, outputs))
}

/// A memory of `pages` pages, as the module `wm` ([`memory_module`]) defines it.
async fn defined(store: &mut Store<State>, wm: &Module, pages: u64) -> wasmtime::Result<Memory> {
    let i = Instance::new_async(&mut *store, wm, &[]).await?;
    let m = i.get_memory(&mut *store, "m").expect("the memory defined");
    let have = m.size(&*store);
    if pages > have {
        m.grow(&mut *store, pages - have)?;
    }
    Ok(m)
}

/// The lengths of `inputs`.
fn lens(inputs: &[&[u8]]) -> Vec<u64> {
    inputs.iter().map(|i| i.len() as u64).collect()
}

impl Loaded for Shared {
    fn is_part(&self) -> bool {
        self.0.part
    }

    fn apply_part(
        &self,
        params: &[u8],
        resources: &[Resource],
        inputs: &mut [&mut dyn PartIn],
        outputs: &mut [&mut dyn PartOut],
        sizes: &[u64],
        memory_limit: u64,
        task_limits: &[u64],
    ) -> Decoded<()> {
        let _s = aqr_common::trace::span("wasm");
        if !self.0.part {
            return Err(Failure::Resource("a whole module applied as a part module".into()));
        }
        if outputs.len() != sizes.len() {
            return invalid("outputs of other sizes than declared");
        }
        let lens: Vec<u64> = inputs.iter().map(|i| i.len()).collect();
        self.check(params, resources, &lens, sizes, memory_limit, task_limits)?;
        // SAFETY: the inputs and outputs outlive the store, which is dropped within `run_fed`,
        // and are not reached but by it meanwhile
        let feeds = (inputs.iter_mut()).map(|i| {
            Feed::Pull(Mutex::new(unsafe { std::mem::transmute::<&mut dyn PartIn, &'static mut dyn PartIn>(&mut **i) }))
        });
        let drains = outputs.iter_mut().zip(sizes).map(|(o, &n)| {
            Drain::Push(
                Mutex::new(unsafe { std::mem::transmute::<&mut dyn PartOut, &'static mut dyn PartOut>(&mut **o) }),
                n,
            )
        });
        self.run_fed(params, resources, feeds.collect(), drains.collect(), memory_limit, task_limits)
    }

    fn apply(
        &self,
        params: &[u8],
        resources: &[Resource],
        inputs: &[&[u8]],
        sizes: &[u64],
        memory_limit: u64,
        task_limits: &[u64],
    ) -> Decoded<Vec<Vec<u8>>> {
        let _s = aqr_common::trace::span("wasm");
        self.check(params, resources, &lens(inputs), sizes, memory_limit, task_limits)?;
        // (zeroed by the system as they are written, as a copy out of the memories would be)
        let mut outs: Vec<Vec<u8>> = sizes.iter().map(|&s| vec![0; s as usize]).collect();
        let mut bufs: Vec<&mut [u8]> = outs.iter_mut().map(|o| &mut o[..]).collect();
        self.run(params, resources, inputs, &mut bufs, memory_limit, task_limits)?;
        Ok(outs)
    }
}

impl Shared {
    /// [`Loaded::apply`], the outputs written into `outs` (of the sizes declared), every byte
    /// of them where it succeeds.
    #[cfg(feature = "sandbox")]
    pub(crate) fn apply_into(
        &self,
        params: &[u8],
        resources: &[Resource],
        inputs: &[&[u8]],
        outs: &mut [&mut [u8]],
        memory_limit: u64,
        task_limits: &[u64],
    ) -> Decoded<()> {
        let sizes: Vec<u64> = outs.iter().map(|o| o.len() as u64).collect();
        self.check(params, resources, &lens(inputs), &sizes, memory_limit, task_limits)?;
        self.run(params, resources, inputs, outs, memory_limit, task_limits)
    }

    /// The application checked against the module and the memory limit, before anything of it
    /// is made.
    fn check(
        &self,
        params: &[u8],
        resources: &[Resource],
        inputs: &[u64],
        sizes: &[u64],
        memory_limit: u64,
        task_limits: &[u64],
    ) -> Decoded<()> {
        let c = &*self.0;
        if sizes.is_empty() {
            return invalid("no output");
        }
        if c.arity != 1 + resources.len() + inputs.len() + sizes.len() {
            return invalid(format!("an entry point of {} parameters", c.arity));
        }
        for i in &c.imports {
            let n = match *i {
                Import::Memory(Mem::Resource(i)) => i as usize >= resources.len(),
                Import::Memory(Mem::Input(k)) => k as usize >= inputs.len(),
                Import::Memory(Mem::Output(j)) => j as usize >= sizes.len(),
                _ => false,
            };
            if n {
                return invalid("a memory imported beyond those given");
            }
        }
        if !task_limits.is_empty() && task_limits.len() != c.slots.len() {
            return invalid(format!("{} task limits for {} classes of tasks", task_limits.len(), c.slots.len()));
        }
        let total = self.imported(params, resources, inputs, sizes) + c.working.0 as u128 * 65536;
        if total > memory_limit as u128 {
            return Err(Failure::Resource("memory limit exceeded".into()));
        }
        Ok(())
    }

    /// The bytes of the memories imported but the working memory, as the application makes them.
    fn imported(&self, params: &[u8], resources: &[Resource], inputs: &[u64], sizes: &[u64]) -> u128 {
        let pages = |len: u64| len.div_ceil(65536) as u128;
        let pages: u128 = (self.0.imports.iter())
            .map(|i| match *i {
                Import::Memory(Mem::Params) => pages(params.len() as u64),
                Import::Memory(Mem::Resource(i)) => pages(resources[i as usize].len() as u64),
                Import::Memory(m @ Mem::Input(k)) => self.0.pages(m, inputs[k as usize]) as u128,
                Import::Memory(m @ Mem::Output(j)) => self.0.pages(m, sizes[j as usize]) as u128,
                Import::Memory(Mem::Working) | Import::Func(_) => 0,
            })
            .sum();
        pages * 65536
    }

    /// The application [`check`](Shared::check)ed, its outputs into `outs`.
    fn run(
        &self,
        params: &[u8],
        resources: &[Resource],
        inputs: &[&[u8]],
        outs: &mut [&mut [u8]],
        memory_limit: u64,
        task_limits: &[u64],
    ) -> Decoded<()> {
        // SAFETY: the inputs and outputs outlive the store, which is dropped within `run_fed`,
        // and so do the references to them in it; the outputs are not reached but by it
        // meanwhile. (Tasks have none of them.)
        let feeds = inputs.iter().map(|i| Feed::Bytes(unsafe { std::slice::from_raw_parts(i.as_ptr(), i.len()) }));
        let drains =
            (outs.iter_mut()).map(|o| Drain::Buf(unsafe { std::slice::from_raw_parts_mut(o.as_mut_ptr(), o.len()) }));
        self.run_fed(params, resources, feeds.collect(), drains.collect(), memory_limit, task_limits)
    }

    /// [`Shared::run`] of the inputs and outputs as the instance takes them, which outlive it.
    fn run_fed(
        &self,
        params: &[u8],
        resources: &[Resource],
        feeds: Vec<Feed>,
        drains: Vec<Drain>,
        memory_limit: u64,
        task_limits: &[u64],
    ) -> Decoded<()> {
        let c = &*self.0;
        let sizes: Vec<u64> = drains.iter().map(Drain::len).collect();
        let sizes = &sizes[..];
        let lens: Vec<u64> = feeds.iter().map(Feed::len).collect();
        // (the data of the inputs given whole, for a whole module's memories, and to record)
        let data: Vec<Option<&'static [u8]>> = (feeds.iter())
            .map(|f| match f {
                Feed::Bytes(d) => Some(*d),
                Feed::Pull(_) => None,
            })
            .collect();
        let (lens, data) = (&lens, &data);
        let exec = Exec::new();
        let app = Arc::new(App {
            limit: memory_limit,
            task_limits: task_limits.to_vec(),
            classes: c.slots.iter().map(|_| Arc::default()).collect(),
            total: Mutex::new(0),
            peak: AtomicU64::new(0),
            instance_peak: AtomicU64::new(0),
            task_peaks: c.slots.iter().map(|_| AtomicU64::new(0)).collect(),
            exec: exec.clone(),
            chans: Mutex::default(),
            next: AtomicU64::new(1),
            failed: Mutex::default(),
        });
        let mut state = State::new(self.clone(), app.clone(), None);
        state.inputs = feeds.into_iter().map(|f| (f, None, 0, 0)).collect();
        state.outputs = drains.into_iter().map(|buf| Output { buf, memory: None, done: 0 }).collect();
        let mut store = Store::new(&ENGINE, state);
        store.limiter(|s| s);

        let run = async move {
            // the memories, holding their data
            let (instance, outputs) = instantiate(&mut store, c, &mut |m| match m {
                Mem::Working => (MemoryType::new(c.working.0 as u32, c.working.1.map(|m| m as u32)), None),
                Mem::Params => sized(params.len() as u64, Some(params)),
                Mem::Resource(i) => sized(resources[i as usize].len() as u64, Some(&resources[i as usize])),
                Mem::Input(k) => {
                    let pages = c.pages(m, lens[k as usize]);
                    (MemoryType::new64(pages, Some(pages)), data[k as usize].filter(|_| !c.part))
                }
                Mem::Output(j) => {
                    let pages = c.pages(m, sizes[j as usize]);
                    (MemoryType::new64(pages, Some(pages)), None)
                }
            })
            .await?;
            let entry = instance.get_func(&mut store, if c.part { PART } else { WHOLE }).expect("the entry point");
            let mut args = vec![Val::I64(params.len() as i64)];
            args.extend(resources.iter().map(|r| Val::I64(r.len() as i64)));
            args.extend(lens.iter().map(|&n| Val::I64(n as i64)));
            args.extend(sizes.iter().map(|&s| Val::I64(s as i64)));
            if let Err(e) = entry.call_async(&mut store, &args, &mut []).await {
                return Err(failure(&store, e));
            }
            if store.data().untaken() {
                return invalid("tasks not taken");
            }
            if c.part {
                if store.data().outputs.iter().any(|o| o.done != o.buf.len()) {
                    return invalid("outputs of other sizes than declared");
                }
            } else {
                for (j, m) in outputs.iter().enumerate() {
                    match m {
                        Some(m) => {
                            let (m, state) = m.data_and_store_mut(&mut store);
                            let Drain::Buf(buf) = &mut state.outputs[j].buf else { unreachable!("pushed") };
                            buf.copy_from_slice(&m[..buf.len()]);
                        }
                        None => {
                            let Drain::Buf(buf) = &mut store.data_mut().outputs[j].buf else { unreachable!("pushed") };
                            buf.fill(0)
                        }
                    }
                }
            }
            Ok(())
        };
        let r = match exec.block_on(run) {
            Ok(r) => r,
            Err(Stopped::Stopped) => Err(app.failed.lock().unwrap().take().expect("a task failed")),
            Err(Stopped::Deadlock) => invalid("a deadlock: every instance waits"),
        };
        if let Some(log) = APPLIED.lock().unwrap().as_mut() {
            let imported = self.imported(params, resources, lens, sizes) as u64;
            log.push(Applied {
                hash: c.hash,
                params: params.len() as u64,
                params_data: params.to_vec(),
                resources: resources.iter().map(|r| r.len() as u64).collect(),
                inputs: lens.clone(),
                contents: data.iter().map(|d| d.map_or_else(Vec::new, <[u8]>::to_vec)).collect(),
                sizes: sizes.to_vec(),
                imported,
                limit: memory_limit,
                working: app.peak.load(Ordering::Relaxed).saturating_sub(imported),
                instance: app.instance_peak.load(Ordering::Relaxed).saturating_sub(imported),
                tasks: app.task_peaks.iter().map(|p| p.load(Ordering::Relaxed)).collect(),
                ok: r.is_ok(),
            });
        }
        r
    }
}

/// The pages of the working memory of `wasm` as it is instantiated: the minimum of its import
/// (the module's data and stack), for its memory plan ([`aqr_formula::Usage`]).
/// The task slots of each class of tasks of `wasm` (its custom section `aqr_task_slots`; none
/// where it has none), unchecked.
pub fn task_slots(wasm: &[u8]) -> Vec<u64> {
    for p in wp::Parser::new(0).parse_all(wasm) {
        if let Ok(wp::Payload::CustomSection(c)) = p
            && c.name() == TASK_SLOTS
        {
            return c.data().iter().map(|&n| n as u64).collect();
        }
    }
    Vec::new()
}

/// The most the memories of an application may take: `memory_limit`, and with task limits, those
/// of as many tasks of each class at once as its task `slots` ([`task_slots`]).
pub fn application_bound(memory_limit: u64, task_limits: &[u64], slots: &[u64]) -> u64 {
    let tasks = task_limits.iter().zip(slots).fold(0u64, |t, (&l, &n)| t.saturating_add(l.saturating_mul(n)));
    memory_limit.saturating_add(tasks)
}

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
    pub tasks: Vec<u64>,
    pub ok: bool,
}

static APPLIED: Mutex<Option<Vec<Applied>>> = Mutex::new(None);

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

impl Job {
    /// Runs the task, giving its result.
    async fn run(self) -> Decoded<Vec<u8>> {
        let c = &*self.c.0;
        let mut store = Store::new(&ENGINE, State::new(self.c.clone(), self.app.clone(), Some(self.class)));
        store.limiter(|s| s);
        let pages = match &self.start {
            Start::Copy(s) => s.as_slice().len() / 65536,
            // (the minimum, and the regions within it)
            Start::Regions(r) => {
                let end = r.iter().map(|(d, b)| d + b.len() as u64).max().unwrap_or(0);
                (end.div_ceil(65536) as usize).max(c.working.0 as usize)
            }
        } as u32;
        // every memory but the working one empty
        let (instance, _) = instantiate(&mut store, c, &mut |m| match m {
            Mem::Working => (MemoryType::new(pages, c.working.1.map(|m| m as u32)), None),
            _ => (MemoryType::new64(0, Some(0)), None),
        })
        .await?;
        // the working memory a copy of the snapshot (on write where the system can), over
        // what the data segments wrote
        let w = store.data().working.expect("a working memory");
        match self.start {
            Start::Copy(s) => s.copy_to(w.data_mut(&mut store), usize::try_from(c.clean).unwrap_or(usize::MAX)),
            Start::Regions(r) => {
                let m = w.data_mut(&mut store);
                for (d, b) in r.iter() {
                    m[*d as usize..][..b.len()].copy_from_slice(b);
                }
            }
        }
        let task = instance.get_typed_func::<(i32, i32), ()>(&mut store, TASK).expect("aqr_task");
        if let Err(e) = task.call_async(&mut store, (self.arg as i32, self.i as i32)).await {
            return Err(failure(&store, e));
        }
        if store.data().untaken() {
            return invalid("tasks not taken");
        }
        Ok(std::mem::take(&mut store.data_mut().result))
    }
}

impl Compiled {
    /// The pages of memory `m` sized to fit `len` bytes, within its window where it has one.
    fn pages(&self, m: Mem, len: u64) -> u64 {
        let window = self.windows.iter().find(|w| w.0 == m).map_or(u64::MAX, |w| w.1);
        len.div_ceil(65536).min(window)
    }
}

/// The type of a memory sized to fit `len` bytes, with its data.
fn sized(len: u64, data: Option<&[u8]>) -> (MemoryType, Option<&[u8]>) {
    let pages = len.div_ceil(65536);
    (MemoryType::new64(pages, Some(pages)), data)
}

/// The range `at..at + len` of the working memory of `c`, if within it.
fn working_range(c: &Caller<'_, State>, at: i32, len: i32) -> Option<(Memory, std::ops::Range<usize>)> {
    let w = c.data().working?;
    let (at, len) = (at as u32 as usize, len as u32 as usize);
    (at.checked_add(len)? <= w.data_size(c)).then_some((w, at..at + len))
}

/// The channel `c` of the application, with `f`, which leaves the wakers to wake once the
/// channels are unlocked in its second argument: a violation if there is none.
fn with_chan<R>(
    c: &Caller<'_, State>,
    ch: i32,
    f: impl FnOnce(&mut Chan, &mut Vec<Waker>) -> wasmtime::Result<R>,
) -> wasmtime::Result<R> {
    let mut waiting = Vec::new();
    let mut chans = c.data().app.chans.lock().unwrap();
    let r = match chans.get_mut(ch as u32 as usize) {
        Some(chan) => f(chan, &mut waiting),
        None => violation(format!("channel {ch} not made")),
    };
    drop(chans);
    wake(waiting);
    r
}

/// Takes the end `end` of a channel for the instance `id`: a violation if another has it.
fn end_of(end: &mut Option<u64>, id: u64, what: &str) -> wasmtime::Result<()> {
    match *end.get_or_insert(id) == id {
        true => Ok(()),
        false => violation(format!("{what} by two instances")),
    }
}

/// The host function `f`.
fn host(store: &mut Store<State>, f: HostFn) -> Func {
    let part = |c: &Caller<'_, State>, name: &str| match c.data().task {
        true => violation(format!("{name} in a task")),
        false => Ok(()),
    };
    match f {
        HostFn::Request => {
            Func::wrap(&mut *store, move |mut c: Caller<'_, State>, k: i32, hi: i64| -> wasmtime::Result<()> {
                part(&c, "aqr_request")?;
                let (k, hi) = (k as u32 as usize, hi as u64);
                let Some((len, Some(mem), lo_k, hi_k)) = c.data().inputs.get(k).map(|i| (i.0.len(), i.1, i.2, i.3))
                else {
                    return violation(format!("aqr_request({k}) of an input not imported"));
                };
                let w = mem.data_size(&c) as u64;
                if !(hi_k..=len).contains(&hi) || hi - lo_k > w {
                    return violation(format!("aqr_request({k}, {hi}) outside [{hi_k}, {len}] or window {w}"));
                }
                let (m, state) = mem.data_and_store_mut(&mut c);
                match &mut state.inputs[k].0 {
                    Feed::Bytes(data) => ring_write(m, w, hi_k, &data[hi_k as usize..hi as usize]),
                    Feed::Pull(p) => {
                        let mut at = hi_k;
                        p.get_mut()
                            .unwrap()
                            .pull(hi, &mut |d| {
                                ring_write(m, w, at, d);
                                at += d.len() as u64;
                            })
                            .map_err(|f| wasmtime::Error::new(TaskFailed(f)))?;
                    }
                }
                state.inputs[k].3 = hi;
                Ok(())
            })
        }
        HostFn::Release => {
            Func::wrap(&mut *store, move |mut c: Caller<'_, State>, k: i32, lo: i64| -> wasmtime::Result<()> {
                part(&c, "aqr_release")?;
                let (k, lo) = (k as u32 as usize, lo as u64);
                let Some((Some(_), lo_k, hi_k)) = c.data().inputs.get(k).map(|i| (i.1, i.2, i.3)) else {
                    return violation(format!("aqr_release({k}) of an input not imported"));
                };
                if !(lo_k..=hi_k).contains(&lo) {
                    return violation(format!("aqr_release({k}, {lo}) outside [{lo_k}, {hi_k}]"));
                }
                c.data_mut().inputs[k].2 = lo;
                Ok(())
            })
        }
        HostFn::Commit => {
            Func::wrap(&mut *store, move |mut c: Caller<'_, State>, j: i32, hi: i64| -> wasmtime::Result<()> {
                part(&c, "aqr_commit")?;
                let (j, hi) = (j as u32 as usize, hi as u64);
                let Some((done, Some(mem), size)) = c.data().outputs.get(j).map(|o| (o.done, o.memory, o.buf.len()))
                else {
                    return violation(format!("aqr_commit({j}) of an output not imported"));
                };
                let w = mem.data_size(&c) as u64;
                if !(done..=size).contains(&hi) || hi - done > w {
                    return violation(format!("aqr_commit({j}, {hi}) outside [{done}, {size}] or window {w}"));
                }
                let (m, state) = mem.data_and_store_mut(&mut c);
                let out = &mut state.outputs[j];
                // the window's bytes from `done`, wrapping at its end
                let mut at = done;
                while at < hi {
                    let from = (at % w) as usize;
                    let n = (hi - at).min(w - from as u64) as usize;
                    match &mut out.buf {
                        Drain::Buf(buf) => buf[at as usize..][..n].copy_from_slice(&m[from..from + n]),
                        Drain::Push(p, _) => p
                            .get_mut()
                            .unwrap()
                            .push(&m[from..from + n])
                            .map_err(|f| wasmtime::Error::new(TaskFailed(f)))?,
                    }
                    at += n as u64;
                }
                out.done = hi;
                Ok(())
            })
        }
        HostFn::Heartbeat => Func::wrap(&mut *store, || {}),
        HostFn::Log => Func::wrap(&mut *store, |mut c: Caller<'_, State>, ptr: i32, len: i32| {
            let (ptr, len) = (ptr as u32 as usize, len as u32 as usize);
            if let Some(w) = c.data().working {
                let msg = w.data(&c).get(ptr..ptr.saturating_add(len)).map(<[u8]>::to_vec);
                c.data_mut().last_log = msg;
            }
        }),
        HostFn::Spawn => {
            Func::wrap(&mut *store, |mut c: Caller<'_, State>, arg: i32, n: i32| -> wasmtime::Result<i32> {
                // (aqr_spawn_v2 of class 0 and all of the working memory at offset 0)
                let w = c.data().working.expect("a working memory");
                let start = Start::Copy(Arc::new(Snapshot::new(w.data(&c))));
                spawn(&mut c, 0, start, arg as u32, n as u32)
            })
        }
        HostFn::SpawnV2 => Func::wrap(
            &mut *store,
            |mut c: Caller<'_, State>,
             class: i32,
             count: i32,
             records: i32,
             arg: i32,
             n: i32|
             -> wasmtime::Result<i32> {
                let (count, records) = (count as u32 as usize, records as u32 as usize);
                let w = c.data().working.expect("a working memory");
                let m = w.data(&c);
                let Some(recs) = count.checked_mul(12).and_then(|l| m.get(records..records.checked_add(l)?)) else {
                    return violation(format!("aqr_spawn_v2's {count} records at {records} out of bounds"));
                };
                let mut regions = Vec::with_capacity(count);
                for r in recs.chunks_exact(12) {
                    let u = |k: usize| u32::from_le_bytes(r[k..k + 4].try_into().unwrap()) as usize;
                    let (src, len, dst) = (u(0), u(4), u(8));
                    let Some(b) = src.checked_add(len).and_then(|e| m.get(src..e)) else {
                        return violation(format!("aqr_spawn_v2's region {src}+{len} out of bounds"));
                    };
                    if (dst + len) as u64 > 1 << 32 {
                        return violation(format!("aqr_spawn_v2's region to {dst}+{len} past 4 GiB"));
                    }
                    regions.push((dst as u64, b.to_vec()));
                }
                spawn(&mut c, class as u32 as usize, Start::Regions(Arc::new(regions)), arg as u32, n as u32)
            },
        ),
        HostFn::Result => Func::wrap(&mut *store, |mut c: Caller<'_, State>, ptr: i32, len: i32| {
            if !c.data().task {
                return violation("aqr_result outside a task".into());
            }
            let Some((w, r)) = working_range(&c, ptr, len) else {
                return violation(format!("aqr_result({ptr}, {len}) out of bounds"));
            };
            let bytes = w.data(&c)[r].to_vec();
            c.data_mut().result = bytes;
            Ok(())
        }),
        HostFn::Join => Func::wrap_async(&mut *store, |c: Caller<'_, State>, (h, i): (i32, i32)| {
            Box::new(async move {
                let Some(cell) = c.data().cell(h, i) else {
                    return violation(format!("aqr_join({h}, {i}) of no task, or one taken"));
                };
                poll_fn(|cx| {
                    let mut st = cell.0.lock().unwrap();
                    match &st.result {
                        None => {
                            st.waiting.push(cx.waker().clone());
                            Poll::Pending
                        }
                        Some(Ok(r)) => {
                            let len = r.len() as i32;
                            st.joined = true;
                            Poll::Ready(Ok(len))
                        }
                        Some(Err(f)) => Poll::Ready(Err(wasmtime::Error::new(TaskFailed(f.clone())))),
                    }
                })
                .await
            })
        }),
        HostFn::Take => Func::wrap(
            &mut *store,
            |mut c: Caller<'_, State>, h: i32, i: i32, dst: i32, len: i32| -> wasmtime::Result<()> {
                let joined = c.data().cell(h, i).filter(|cell| cell.0.lock().unwrap().joined);
                let Some(cell) = joined else {
                    return violation(format!("aqr_take({h}, {i}) of a task not joined, or taken"));
                };
                let r = match &mut *cell.0.lock().unwrap() {
                    Spawned { result: r @ Some(Ok(_)), .. } => r.take().unwrap().unwrap(),
                    _ => unreachable!("joined"),
                };
                let Some((w, range)) = working_range(&c, dst, len).filter(|_| len as u32 as usize <= r.len()) else {
                    return violation(format!("aqr_take({h}, {i}, {dst}, {len}) out of bounds"));
                };
                w.data_mut(&mut c)[range].copy_from_slice(&r[..len as u32 as usize]);
                c.data_mut().spawned[h as u32 as usize][i as u32 as usize] = None;
                Ok(())
            },
        ),
        HostFn::Chan => Func::wrap(&mut *store, |c: Caller<'_, State>, cap: i32| -> wasmtime::Result<i32> {
            if cap <= 0 {
                return violation(format!("aqr_chan({cap})"));
            }
            let mut chans = c.data().app.chans.lock().unwrap();
            chans.push(Chan {
                cap: cap as usize,
                msgs: VecDeque::new(),
                closed: false,
                sender: None,
                receiver: None,
                sending: Vec::new(),
                receiving: Vec::new(),
            });
            Ok((chans.len() - 1) as i32)
        }),
        HostFn::Send => Func::wrap_async(&mut *store, |c: Caller<'_, State>, (ch, ptr, len): (i32, i32, i32)| {
            Box::new(async move {
                let Some((w, r)) = working_range(&c, ptr, len) else {
                    return violation(format!("aqr_send({ch}, {ptr}, {len}) out of bounds"));
                };
                let mut msg = Some(w.data(&c)[r].to_vec());
                let (id, app) = (c.data().id, c.data().app.clone());
                poll_fn(|cx| {
                    let r = with_chan(&c, ch, |chan, waiting| {
                        end_of(&mut chan.sender, id, "sending")?;
                        if chan.closed {
                            return violation(format!("aqr_send({ch}) after closing"));
                        }
                        if chan.msgs.len() >= chan.cap {
                            chan.sending.push(cx.waker().clone());
                            return Ok(false);
                        }
                        let m = msg.take().unwrap();
                        app.account(m.len() as u64, 0)?;
                        chan.msgs.push_back(m);
                        waiting.append(&mut chan.receiving);
                        Ok(true)
                    });
                    match r {
                        Ok(false) => Poll::Pending,
                        Ok(true) => Poll::Ready(Ok(())),
                        Err(e) => Poll::Ready(Err(e)),
                    }
                })
                .await
            })
        }),
        HostFn::Recv => Func::wrap_async(&mut *store, |mut c: Caller<'_, State>, (ch, dst, cap): (i32, i32, i32)| {
            Box::new(async move {
                let (id, app) = (c.data().id, c.data().app.clone());
                let msg = poll_fn(|cx| {
                    let r = with_chan(&c, ch, |chan, waiting| {
                        end_of(&mut chan.receiver, id, "receiving")?;
                        let Some(m) = chan.msgs.front() else {
                            if chan.closed {
                                return Ok(Some(Err(-1)));
                            }
                            chan.receiving.push(cx.waker().clone());
                            return Ok(None);
                        };
                        if m.len() > cap as u32 as usize {
                            return Ok(Some(Err(m.len() as i32)));
                        }
                        let m = chan.msgs.pop_front().unwrap();
                        app.account(0, m.len() as u64)?;
                        waiting.append(&mut chan.sending);
                        Ok(Some(Ok(m)))
                    });
                    match r {
                        Ok(None) => Poll::Pending,
                        Ok(Some(m)) => Poll::Ready(Ok(m)),
                        Err(e) => Poll::Ready(Err(e)),
                    }
                })
                .await?;
                // (too long a message left queued, or none to come)
                let m = match msg {
                    Ok(m) => m,
                    Err(n) => return Ok(n),
                };
                let Some((w, range)) = working_range(&c, dst, m.len() as i32) else {
                    return violation(format!("aqr_recv({ch}, {dst}) out of bounds"));
                };
                w.data_mut(&mut c)[range].copy_from_slice(&m);
                Ok(m.len() as i32)
            })
        }),
        HostFn::Close => Func::wrap(&mut *store, |c: Caller<'_, State>, ch: i32| -> wasmtime::Result<()> {
            let id = c.data().id;
            with_chan(&c, ch, |chan, waiting| {
                end_of(&mut chan.sender, id, "sending")?;
                chan.closed = true;
                waiting.append(&mut chan.receiving);
                Ok(())
            })
        }),
    }
}
