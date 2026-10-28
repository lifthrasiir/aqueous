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
use aqr_formula::aqr::{Failure, Loaded};
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
    // the working memories (as defined by `Compiled::working_module`) copied on write over
    #[cfg(windows)]
    c.with_host_memory(Arc::new(crate::cow::Creator));
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
    /// The number of parameters of the entry point.
    arity: usize,
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
    let c = Shared(Arc::new(compile(wasm)?));
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
    let mut grows_other = false;
    let mut clean = 0u64;
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
                            } else if !m.memory64 || m.initial != 0 || m.maximum.is_some() {
                                return invalid(format!("{} not 64-bit of minimum 0 and no maximum", i.name));
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
    if imports.iter().any(|i| matches!(i, Import::Func(HostFn::Spawn))) {
        let ft = task.and_then(|f| funcs.get(f as usize)).and_then(|&t| types.get(t as usize));
        let Some(ft) = ft else { return invalid(format!("tasks spawned without {TASK}")) };
        if ft.params() != [wp::ValType::I32, wp::ValType::I32] || !ft.results().is_empty() {
            return invalid(format!("{TASK} of another type"));
        }
    }
    let module = Module::new(&ENGINE, wasm).map_err(|e| Failure::Invalid(format!("not compiled: {e}")))?;
    let working_module = if cfg!(windows) {
        let m = Module::new(&ENGINE, memory_module(working));
        Some(m.map_err(|e| Failure::Resource(format!("the working memory not compiled: {e}")))?)
    } else {
        None
    };
    Ok(Compiled { module, imports, working, clean, working_module, part, arity: ft.params().len() })
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
    /// The size of the memories of all the instances and of the messages queued.
    total: Mutex<u64>,
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

/// What a task runs: `aqr_task(arg, i)` in an instance of `c` whose working memory is
/// `snapshot`.
struct Job {
    c: Shared,
    app: Arc<App>,
    snapshot: Arc<Snapshot>,
    arg: u32,
    i: u32,
}

/// An output of an application: the caller's buffer, of the size declared.
struct Output {
    buf: &'static mut [u8],
    memory: Option<Memory>,
    done: u64,
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
    inputs: Vec<(&'static [u8], Option<Memory>, u64, u64)>,
    /// The outputs, their memories and (of a part module) the bytes committed so far.
    outputs: Vec<Output>,
    last_log: Option<Vec<u8>>,
    /// Whether the instance is a task, and its result.
    task: bool,
    result: Vec<u8>,
    /// The tasks spawned, by handle; taken where `None`.
    spawned: Vec<Vec<Option<Arc<Cell>>>>,
}

impl State {
    fn new(c: Shared, app: Arc<App>, task: bool) -> State {
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
            result: Vec::new(),
            spawned: Vec::new(),
        }
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
        let _ = self.app.account(0, self.mine);
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
        self.app.account(desired as u64, current as u64)?;
        self.mine = self.mine - current as u64 + desired as u64;
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

impl Loaded for Shared {
    fn apply(
        &self,
        params: &[u8],
        resources: &[Resource],
        inputs: &[&[u8]],
        sizes: &[u64],
        memory_limit: u64,
    ) -> Decoded<Vec<Vec<u8>>> {
        self.check(params, resources, inputs, sizes, memory_limit)?;
        // (zeroed by the system as they are written, as a copy out of the memories would be)
        let mut outs: Vec<Vec<u8>> = sizes.iter().map(|&s| vec![0; s as usize]).collect();
        let mut bufs: Vec<&mut [u8]> = outs.iter_mut().map(|o| &mut o[..]).collect();
        self.run(params, resources, inputs, &mut bufs, memory_limit)?;
        Ok(outs)
    }
}

impl Shared {
    /// [`Loaded::apply`], the outputs written into `outs` (of the sizes declared), every byte
    /// of them where it succeeds.
    pub(crate) fn apply_into(
        &self,
        params: &[u8],
        resources: &[Resource],
        inputs: &[&[u8]],
        outs: &mut [&mut [u8]],
        memory_limit: u64,
    ) -> Decoded<()> {
        let sizes: Vec<u64> = outs.iter().map(|o| o.len() as u64).collect();
        self.check(params, resources, inputs, &sizes, memory_limit)?;
        self.run(params, resources, inputs, outs, memory_limit)
    }

    /// The application checked against the module and the memory limit, before anything of it
    /// is made.
    fn check(
        &self,
        params: &[u8],
        resources: &[Resource],
        inputs: &[&[u8]],
        sizes: &[u64],
        memory_limit: u64,
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
        let pages = |len: u64| len.div_ceil(65536);
        let total: u128 = c
            .imports
            .iter()
            .map(|i| match *i {
                Import::Memory(Mem::Working) => c.working.0 as u128,
                Import::Memory(Mem::Params) => pages(params.len() as u64) as u128,
                Import::Memory(Mem::Resource(i)) => pages(resources[i as usize].len() as u64) as u128,
                Import::Memory(Mem::Input(k)) => pages(inputs[k as usize].len() as u64) as u128,
                Import::Memory(Mem::Output(j)) => pages(sizes[j as usize]) as u128,
                Import::Func(_) => 0,
            })
            .sum::<u128>()
            * 65536;
        if total > memory_limit as u128 {
            return Err(Failure::Resource("memory limit exceeded".into()));
        }
        Ok(())
    }

    /// The application [`check`](Shared::check)ed, its outputs into `outs`.
    fn run(
        &self,
        params: &[u8],
        resources: &[Resource],
        inputs: &[&[u8]],
        outs: &mut [&mut [u8]],
        memory_limit: u64,
    ) -> Decoded<()> {
        let c = &*self.0;
        let sizes: Vec<u64> = outs.iter().map(|o| o.len() as u64).collect();
        let sizes = &sizes[..];

        // SAFETY: the inputs and outputs outlive the store, which is dropped within this
        // function, and so do the references to them in it; the outputs are not reached but by
        // it meanwhile. (Tasks have none of them.)
        let data: Vec<&'static [u8]> =
            inputs.iter().map(|i| unsafe { std::slice::from_raw_parts(i.as_ptr(), i.len()) }).collect();
        let bufs: Vec<&'static mut [u8]> =
            outs.iter_mut().map(|o| unsafe { std::slice::from_raw_parts_mut(o.as_mut_ptr(), o.len()) }).collect();
        let exec = Exec::new();
        let app = Arc::new(App {
            limit: memory_limit,
            total: Mutex::new(0),
            exec: exec.clone(),
            chans: Mutex::default(),
            next: AtomicU64::new(1),
            failed: Mutex::default(),
        });
        let mut state = State::new(self.clone(), app.clone(), false);
        state.inputs = data.iter().map(|&d| (d, None, 0, 0)).collect();
        state.outputs = bufs.into_iter().map(|buf| Output { buf, memory: None, done: 0 }).collect();
        let mut store = Store::new(&ENGINE, state);
        store.limiter(|s| s);

        let run = async move {
            // the memories, holding their data
            let (instance, outputs) = instantiate(&mut store, c, &mut |m| match m {
                Mem::Working => (MemoryType::new(c.working.0 as u32, c.working.1.map(|m| m as u32)), None),
                Mem::Params => sized(params.len() as u64, Some(params)),
                Mem::Resource(i) => sized(resources[i as usize].len() as u64, Some(&resources[i as usize])),
                Mem::Input(k) => {
                    let d = inputs[k as usize];
                    sized(d.len() as u64, (!c.part).then_some(d))
                }
                Mem::Output(j) => sized(sizes[j as usize], None),
            })
            .await?;
            let entry = instance.get_func(&mut store, if c.part { PART } else { WHOLE }).expect("the entry point");
            let mut args = vec![Val::I64(params.len() as i64)];
            args.extend(resources.iter().map(|r| Val::I64(r.len() as i64)));
            args.extend(inputs.iter().map(|i| Val::I64(i.len() as i64)));
            args.extend(sizes.iter().map(|&s| Val::I64(s as i64)));
            if let Err(e) = entry.call_async(&mut store, &args, &mut []).await {
                return Err(failure(&store, e));
            }
            if store.data().untaken() {
                return invalid("tasks not taken");
            }
            if c.part {
                if store.data().outputs.iter().any(|o| o.done != o.buf.len() as u64) {
                    return invalid("outputs of other sizes than declared");
                }
            } else {
                for (j, m) in outputs.iter().enumerate() {
                    match m {
                        Some(m) => {
                            let (m, state) = m.data_and_store_mut(&mut store);
                            let buf = &mut *state.outputs[j].buf;
                            buf.copy_from_slice(&m[..buf.len()]);
                        }
                        None => store.data_mut().outputs[j].buf.fill(0),
                    }
                }
            }
            Ok(())
        };
        match exec.block_on(run) {
            Ok(r) => r,
            Err(Stopped::Stopped) => Err(app.failed.lock().unwrap().take().expect("a task failed")),
            Err(Stopped::Deadlock) => invalid("a deadlock: every instance waits"),
        }
    }
}

impl Job {
    /// Runs the task, giving its result.
    async fn run(self) -> Decoded<Vec<u8>> {
        let c = &*self.c.0;
        let mut store = Store::new(&ENGINE, State::new(self.c.clone(), self.app.clone(), true));
        store.limiter(|s| s);
        let pages = (self.snapshot.as_slice().len() / 65536) as u32;
        // every memory but the working one empty
        let (instance, _) = instantiate(&mut store, c, &mut |m| match m {
            Mem::Working => (MemoryType::new(pages, c.working.1.map(|m| m as u32)), None),
            _ => (MemoryType::new64(0, Some(0)), None),
        })
        .await?;
        // the working memory a copy of the snapshot (on write where the system can), over
        // what the data segments wrote
        let w = store.data().working.expect("a working memory");
        self.snapshot.copy_to(w.data_mut(&mut store), usize::try_from(c.clean).unwrap_or(usize::MAX));
        drop(self.snapshot);
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
                let Some(&(data, Some(mem), lo_k, hi_k)) = c.data().inputs.get(k) else {
                    return violation(format!("aqr_request({k}) of an input not imported"));
                };
                let w = mem.data_size(&c) as u64;
                if !(hi_k..=data.len() as u64).contains(&hi) || hi - lo_k > w {
                    return violation(format!("aqr_request({k}, {hi}) outside [{hi_k}, {}] or window {w}", data.len()));
                }
                let m = mem.data_mut(&mut c);
                for o in hi_k..hi {
                    m[(o % w) as usize] = data[o as usize];
                }
                c.data_mut().inputs[k].3 = hi;
                Ok(())
            })
        }
        HostFn::Release => {
            Func::wrap(&mut *store, move |mut c: Caller<'_, State>, k: i32, lo: i64| -> wasmtime::Result<()> {
                part(&c, "aqr_release")?;
                let (k, lo) = (k as u32 as usize, lo as u64);
                let Some(&(_, Some(_), lo_k, hi_k)) = c.data().inputs.get(k) else {
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
                let Some((done, Some(mem), size)) =
                    c.data().outputs.get(j).map(|o| (o.done, o.memory, o.buf.len() as u64))
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
                    out.buf[at as usize..][..n].copy_from_slice(&m[from..from + n]);
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
                let w = c.data().working.expect("a working memory");
                let snapshot = Arc::new(Snapshot::new(w.data(&c)));
                let s = c.data_mut();
                let cells: Vec<Option<Arc<Cell>>> = (0..n as u32)
                    .map(|i| {
                        let cell = Arc::new(Cell::default());
                        let job =
                            Job { c: s.c.clone(), app: s.app.clone(), snapshot: snapshot.clone(), arg: arg as u32, i };
                        let (done, app) = (cell.clone(), s.app.clone());
                        s.app.exec.spawn(async move {
                            let r = job.run().await;
                            // (a task failing fails the application at once: those waiting for
                            // it through channels would wait for ever)
                            if let Err(f) = &r {
                                app.failed.lock().unwrap().get_or_insert_with(|| f.clone());
                                app.exec.stop();
                            }
                            done.finish(r)
                        });
                        Some(cell)
                    })
                    .collect();
                s.spawned.push(cells);
                Ok((s.spawned.len() - 1) as i32)
            })
        }
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
                let r = match cell.0.lock().unwrap().result.take() {
                    Some(Ok(r)) => r,
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
