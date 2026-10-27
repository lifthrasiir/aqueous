//! Modules run as WebAssembly by Wasmtime, as the spec's "Module execution" defines (with the
//! feature `wasm`): the module checked against the profile, the memories of an application
//! created and the entry point invoked, with the host functions.
//!
//! A trap, or a violation of a host function, is an invalid archive. Exceeding the memory
//! limit or the stack is a resource failure. The last `aqr_log` message before a failure is
//! part of it.
//!
//! Tasks (`aqr_spawn`) are instances of their own, of a copy of the working memory at the
//! spawn and empty other memories. The workers of aqr-common's pool free at the spawn run
//! them, each taking the next one not started, and so does the thread joining one not started.
//! The memory limit is over all the memories of the application.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, LazyLock, Mutex};

use aqr_common::pool;

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
    Engine::new(&c).expect("the configuration of Wasmtime")
});

/// The modules compiled, by their hashes, kept for the process.
static COMPILED: LazyLock<Mutex<HashMap<[u8; 32], Shared>>> = LazyLock::new(Default::default);

/// A module compiled, as [`load`] gives it.
#[derive(Clone)]
struct Shared(Arc<Compiled>);

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
    part: bool,
    /// The number of parameters of the entry point.
    arity: usize,
}

/// `wasm`, of SHA-512/256 `hash`, checked against the profile and compiled (once a process).
pub fn load(wasm: &[u8], hash: [u8; 32]) -> Decoded<Box<dyn Loaded>> {
    if let Some(c) = COMPILED.lock().unwrap().get(&hash) {
        return Ok(Box::new(c.clone()));
    }
    let c = Shared(Arc::new(compile(wasm)?));
    COMPILED.lock().unwrap().insert(hash, c.clone());
    Ok(Box::new(c))
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
    Ok(Compiled { module, imports, working, part, arity: ft.params().len() })
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

#[cfg(unix)]
use crate::cow::Snapshot;

/// The working memory of a spawner at a spawn (copied, without `cow`).
#[cfg(not(unix))]
struct Snapshot(Vec<u8>);

#[cfg(not(unix))]
impl Snapshot {
    fn new(mem: &[u8]) -> Snapshot {
        Snapshot(mem.to_vec())
    }

    fn as_slice(&self) -> &[u8] {
        &self.0
    }

    fn copy_to(&self, mem: &mut [u8]) {
        mem.copy_from_slice(&self.0)
    }
}

/// The failure of a task, which is the application's as the task is joined.
#[derive(Debug)]
struct TaskFailed(Failure);

impl std::fmt::Display for TaskFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "a task failed: {:?}", self.0)
    }
}

impl std::error::Error for TaskFailed {}

/// The memory limit of an application, and the size of the memories of all its instances.
struct Budget {
    limit: u64,
    total: Mutex<u64>,
}

/// What a task runs: `aqr_task(arg, i)` in an instance of `c` whose working memory is
/// `snapshot`.
struct Job {
    c: Shared,
    snapshot: Arc<Snapshot>,
    arg: u32,
    i: u32,
    budget: Arc<Budget>,
}

/// A task spawned, until its result is taken.
enum Slot {
    Pending(Job),
    Running,
    Done(Decoded<Vec<u8>>),
    Taken,
}

/// The tasks of a spawn.
struct Batch {
    slots: Mutex<Vec<Slot>>,
    done: Condvar,
}

impl Batch {
    /// Runs task `i` if it is not started yet, and returns whether it was.
    fn run(&self, i: usize) -> bool {
        let mut slots = self.slots.lock().unwrap();
        if !matches!(slots[i], Slot::Pending(_)) {
            return false;
        }
        let Slot::Pending(job) = std::mem::replace(&mut slots[i], Slot::Running) else { unreachable!() };
        drop(slots);
        let r = job.run();
        self.slots.lock().unwrap()[i] = Slot::Done(r);
        self.done.notify_all();
        true
    }

    /// What a worker does: the tasks not started, in order, until there is none.
    fn work(&self) {
        let mut i = 0;
        loop {
            let next = self.slots.lock().unwrap().iter().skip(i).position(|s| matches!(s, Slot::Pending(_)));
            let Some(k) = next else { break };
            i += k;
            self.run(i);
        }
    }
}

/// What the host functions of an instance work on.
struct State {
    c: Shared,
    budget: Arc<Budget>,
    /// The bytes of the memories of this instance, in `budget`.
    mine: u64,
    working: Option<Memory>,
    /// The data of the inputs of a part module, their memories and visible ranges.
    inputs: Vec<(&'static [u8], Option<Memory>, u64, u64)>,
    /// The outputs of a part module so far, with their memories and declared sizes.
    outputs: Vec<(Vec<u8>, Option<Memory>, u64)>,
    last_log: Option<Vec<u8>>,
    /// Whether the instance is a task, and its result.
    task: bool,
    result: Vec<u8>,
    /// The tasks spawned, by handle.
    spawned: Vec<Arc<Batch>>,
}

impl State {
    fn new(c: Shared, budget: Arc<Budget>, task: bool) -> State {
        State {
            c,
            budget,
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
        self.spawned.iter().any(|b| b.slots.lock().unwrap().iter().any(|s| !matches!(s, Slot::Taken)))
    }
}

impl Drop for State {
    fn drop(&mut self) {
        *self.budget.total.lock().unwrap() -= self.mine;
    }
}

impl ResourceLimiter for State {
    fn memory_growing(&mut self, current: usize, desired: usize, _: Option<usize>) -> wasmtime::Result<bool> {
        let mut total = self.budget.total.lock().unwrap();
        let t = (*total - current as u64).checked_add(desired as u64).filter(|&t| t <= self.budget.limit);
        *total = t.ok_or(OverLimit)?;
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
fn instantiate<'a>(
    store: &mut Store<State>,
    c: &Compiled,
    mem: &mut dyn FnMut(Mem) -> (MemoryType, Option<&'a [u8]>),
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
                let memory = Memory::new(&mut *store, ty).map_err(over)?;
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
                            o.1 = Some(memory);
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
    let instance =
        Instance::new(&mut *store, &c.module, &externs).map_err(|e| match e.downcast_ref::<OverLimit>() {
            Some(_) => Failure::Resource("memory limit exceeded".into()),
            None => Failure::Invalid(format!("not instantiated: {e}")),
        })?;
    Ok((instance, outputs))
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

        // SAFETY: the inputs outlive the store, which is dropped within this function, and so
        // do the references to them in it. (Tasks have none of them.)
        let data: Vec<&'static [u8]> =
            inputs.iter().map(|i| unsafe { std::slice::from_raw_parts(i.as_ptr(), i.len()) }).collect();
        let budget = Arc::new(Budget { limit: memory_limit, total: Mutex::new(0) });
        let mut state = State::new(self.clone(), budget, false);
        state.inputs = data.iter().map(|&d| (d, None, 0, 0)).collect();
        state.outputs = sizes.iter().map(|&s| (Vec::new(), None, s)).collect();
        let mut store = Store::new(&ENGINE, state);
        store.limiter(|s| s);

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
        })?;
        let entry = instance.get_func(&mut store, if c.part { PART } else { WHOLE }).expect("the entry point");
        let mut args = vec![Val::I64(params.len() as i64)];
        args.extend(resources.iter().map(|r| Val::I64(r.len() as i64)));
        args.extend(inputs.iter().map(|i| Val::I64(i.len() as i64)));
        args.extend(sizes.iter().map(|&s| Val::I64(s as i64)));
        if let Err(e) = entry.call(&mut store, &args, &mut []) {
            return Err(failure(&store, e));
        }
        if store.data().untaken() {
            return invalid("tasks not taken");
        }

        let outs: Vec<Vec<u8>> = if c.part {
            std::mem::take(&mut store.data_mut().outputs).into_iter().map(|o| o.0).collect()
        } else {
            outputs
                .iter()
                .zip(sizes)
                .map(|(m, &s)| match m {
                    Some(m) => m.data(&store)[..s as usize].to_vec(),
                    None => vec![0; s as usize],
                })
                .collect()
        };
        if outs.iter().zip(sizes).any(|(o, &s)| o.len() as u64 != s) {
            return invalid("outputs of other sizes than declared");
        }
        Ok(outs)
    }
}

impl Job {
    /// Runs the task, giving its result.
    fn run(self) -> Decoded<Vec<u8>> {
        let c = &*self.c.0;
        let mut store = Store::new(&ENGINE, State::new(self.c.clone(), self.budget.clone(), true));
        store.limiter(|s| s);
        let pages = (self.snapshot.as_slice().len() / 65536) as u32;
        // every memory but the working one empty
        let (instance, _) = instantiate(&mut store, c, &mut |m| match m {
            Mem::Working => (MemoryType::new(pages, c.working.1.map(|m| m as u32)), None),
            _ => (MemoryType::new64(0, Some(0)), None),
        })?;
        // the working memory a copy of the snapshot (on write where the system can), over
        // what the data segments wrote
        let w = store.data().working.expect("a working memory");
        self.snapshot.copy_to(w.data_mut(&mut store));
        drop(self.snapshot);
        let task = instance.get_typed_func::<(i32, i32), ()>(&mut store, TASK).expect("aqr_task");
        if let Err(e) = task.call(&mut store, (self.arg as i32, self.i as i32)) {
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
    (at + len <= w.data_size(c)).then_some((w, at..at + len))
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
                let Some((done, Some(mem), size)) = c.data().outputs.get(j).map(|o| (o.0.len() as u64, o.1, o.2))
                else {
                    return violation(format!("aqr_commit({j}) of an output not imported"));
                };
                let w = mem.data_size(&c) as u64;
                if !(done..=size).contains(&hi) || hi - done > w {
                    return violation(format!("aqr_commit({j}, {hi}) outside [{done}, {size}] or window {w}"));
                }
                let (m, state) = mem.data_and_store_mut(&mut c);
                let out = &mut state.outputs[j].0;
                for o in done..hi {
                    out.push(m[(o % w) as usize]);
                }
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
                let n = n as u32;
                let slots = (0..n)
                    .map(|i| {
                        let (c, budget) = (s.c.clone(), s.budget.clone());
                        Slot::Pending(Job { c, snapshot: snapshot.clone(), arg: arg as u32, i, budget })
                    })
                    .collect();
                let batch = Arc::new(Batch { slots: Mutex::new(slots), done: Condvar::new() });
                // (the thread joining them one of the workers)
                let mut workers = pool::reserve(n.saturating_sub(1) as usize);
                while !workers.is_empty() {
                    let b = batch.clone();
                    drop(workers.spawn(move || b.work()));
                }
                s.spawned.push(batch);
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
        HostFn::Join => Func::wrap(&mut *store, |c: Caller<'_, State>, h: i32, i: i32| -> wasmtime::Result<i32> {
            let (h, i) = (h as u32 as usize, i as u32 as usize);
            let Some(b) = c.data().spawned.get(h).filter(|b| i < b.slots.lock().unwrap().len()).cloned() else {
                return violation(format!("aqr_join({h}, {i}) of no task"));
            };
            b.run(i);
            let mut slots = b.slots.lock().unwrap();
            loop {
                match &slots[i] {
                    Slot::Running => slots = b.done.wait(slots).unwrap(),
                    Slot::Done(Ok(r)) => return Ok(r.len() as i32),
                    Slot::Done(Err(f)) => return Err(wasmtime::Error::new(TaskFailed(f.clone()))),
                    Slot::Taken => return violation(format!("aqr_join({h}, {i}) of a task taken")),
                    Slot::Pending(_) => unreachable!("run above"),
                }
            }
        }),
        HostFn::Take => Func::wrap(
            &mut *store,
            |mut c: Caller<'_, State>, h: i32, i: i32, dst: i32, len: i32| -> wasmtime::Result<()> {
                let (hu, iu) = (h as u32 as usize, i as u32 as usize);
                let Some(b) = c.data().spawned.get(hu).filter(|b| iu < b.slots.lock().unwrap().len()).cloned() else {
                    return violation(format!("aqr_take({h}, {i}) of no task"));
                };
                let mut slots = b.slots.lock().unwrap();
                if !matches!(slots[iu], Slot::Done(Ok(_))) {
                    return violation(format!("aqr_take({h}, {i}) of a task not joined, or taken"));
                }
                let Slot::Done(Ok(r)) = std::mem::replace(&mut slots[iu], Slot::Taken) else { unreachable!() };
                drop(slots);
                let Some((w, range)) = working_range(&c, dst, len).filter(|_| len as u32 as usize <= r.len()) else {
                    return violation(format!("aqr_take({h}, {i}, {dst}, {len}) out of bounds"));
                };
                w.data_mut(&mut c)[range].copy_from_slice(&r[..len as u32 as usize]);
                Ok(())
            },
        ),
    }
}
