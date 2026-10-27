//! Modules run as WebAssembly by Wasmtime, as the spec's "Module execution" defines (with the
//! feature `wasm`): the module checked against the profile, the memories of an application
//! created and the entry point invoked, with the host functions.
//!
//! A trap, or a violation of a host function, is an invalid archive. Exceeding the memory
//! limit or the stack is a resource failure. The last `aqr_log` message before a failure is
//! part of it.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

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

fn func_of(name: &str) -> Option<(HostFn, &'static [wp::ValType])> {
    use wp::ValType::{I32, I64};
    Some(match name {
        "aqr_request" => (HostFn::Request, &[I32, I64]),
        "aqr_release" => (HostFn::Release, &[I32, I64]),
        "aqr_commit" => (HostFn::Commit, &[I32, I64]),
        "aqr_heartbeat" => (HostFn::Heartbeat, &[]),
        "aqr_log" => (HostFn::Log, &[I32, I32]),
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
    let (mut imports, mut working, mut entry) = (Vec::new(), None, None);
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
                            let Some((f, params)) = func_of(i.name) else {
                                return invalid(format!("an import {}", i.name));
                            };
                            let ft = types.get(t as usize).ok_or_else(|| Failure::Invalid("a bad type".into()))?;
                            if ft.params() != params || !ft.results().is_empty() {
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
            wp::Payload::ExportSection(r) => {
                for e in r {
                    let e = e.map_err(bad)?;
                    if (e.name == WHOLE || e.name == PART) && e.kind == wp::ExternalKind::Func {
                        if entry.is_some() {
                            return invalid("two entry points");
                        }
                        entry = Some((e.name == PART, e.index));
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

/// What the host functions of an application work on.
struct State {
    limit: u64,
    total: u64,
    working: Option<Memory>,
    /// The data of the inputs of a part module, their memories and visible ranges.
    inputs: Vec<(&'static [u8], Option<Memory>, u64, u64)>,
    /// The outputs of a part module so far, with their memories and declared sizes.
    outputs: Vec<(Vec<u8>, Option<Memory>, u64)>,
    last_log: Option<Vec<u8>>,
}

impl ResourceLimiter for State {
    fn memory_growing(&mut self, current: usize, desired: usize, _: Option<usize>) -> wasmtime::Result<bool> {
        let total = (self.total - current as u64).checked_add(desired as u64).filter(|&t| t <= self.limit);
        self.total = total.ok_or(OverLimit)?;
        Ok(true)
    }

    fn table_growing(&mut self, _: usize, _: usize, _: Option<usize>) -> wasmtime::Result<bool> {
        Ok(true)
    }
}

fn violation<T>(msg: String) -> wasmtime::Result<T> {
    Err(wasmtime::Error::new(Violation(msg)))
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
        // do the references to them in it.
        let data: Vec<&'static [u8]> =
            inputs.iter().map(|i| unsafe { std::slice::from_raw_parts(i.as_ptr(), i.len()) }).collect();
        let state = State {
            limit: memory_limit,
            total: 0,
            working: None,
            inputs: data.iter().map(|&d| (d, None, 0, 0)).collect(),
            outputs: sizes.iter().map(|&s| (Vec::new(), None, s)).collect(),
            last_log: None,
        };
        let mut store = Store::new(&ENGINE, state);
        store.limiter(|s| s);
        let failed = |store: &Store<State>, f: Failure| match (f, &store.data().last_log) {
            (Failure::Invalid(m), Some(log)) => Failure::Invalid(format!("{m}: {}", String::from_utf8_lossy(log))),
            (f, _) => f,
        };
        let over = |e: wasmtime::Error| match e.downcast_ref::<OverLimit>() {
            Some(_) => Failure::Resource("memory limit exceeded".into()),
            None => Failure::Resource(format!("{e}")),
        };

        // the memories, holding their data
        let mut externs = Vec::with_capacity(c.imports.len());
        let mut outputs = vec![None; sizes.len()];
        for i in &c.imports {
            externs.push(match *i {
                Import::Memory(m) => {
                    let (ty, init): (MemoryType, Option<&[u8]>) = match m {
                        Mem::Working => (MemoryType::new(c.working.0 as u32, c.working.1.map(|m| m as u32)), None),
                        Mem::Params => sized(params.len() as u64, Some(params)),
                        Mem::Resource(i) => sized(resources[i as usize].len() as u64, Some(&resources[i as usize])),
                        Mem::Input(k) => {
                            let d = inputs[k as usize];
                            sized(d.len() as u64, (!c.part).then_some(d))
                        }
                        Mem::Output(j) => sized(sizes[j as usize], None),
                    };
                    let mem = Memory::new(&mut store, ty).map_err(over)?;
                    if let Some(d) = init {
                        mem.data_mut(&mut store)[..d.len()].copy_from_slice(d);
                    }
                    match m {
                        Mem::Working => store.data_mut().working = Some(mem),
                        Mem::Input(k) => store.data_mut().inputs[k as usize].1 = Some(mem),
                        Mem::Output(j) => {
                            store.data_mut().outputs[j as usize].1 = Some(mem);
                            outputs[j as usize] = Some(mem);
                        }
                        _ => {}
                    }
                    Extern::Memory(mem)
                }
                Import::Func(f) => Extern::Func(host(&mut store, f)),
            });
        }

        let instance =
            Instance::new(&mut store, &c.module, &externs).map_err(|e| match e.downcast_ref::<OverLimit>() {
                Some(_) => Failure::Resource("memory limit exceeded".into()),
                None => Failure::Invalid(format!("not instantiated: {e}")),
            })?;
        let entry = instance.get_func(&mut store, if c.part { PART } else { WHOLE }).expect("the entry point");
        let mut args = vec![Val::I64(params.len() as i64)];
        args.extend(resources.iter().map(|r| Val::I64(r.len() as i64)));
        args.extend(inputs.iter().map(|i| Val::I64(i.len() as i64)));
        args.extend(sizes.iter().map(|&s| Val::I64(s as i64)));
        if let Err(e) = entry.call(&mut store, &args, &mut []) {
            let f = if e.is::<OverLimit>() {
                Failure::Resource("memory limit exceeded".into())
            } else if let Some(v) = e.downcast_ref::<Violation>() {
                Failure::Invalid(v.0.clone())
            } else {
                match e.downcast_ref::<Trap>() {
                    Some(Trap::StackOverflow) => Failure::Resource("call stack exhausted".into()),
                    Some(t) => Failure::Invalid(format!("trap: {t}")),
                    None => Failure::Invalid(format!("{e}")),
                }
            };
            return Err(failed(&store, f));
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

/// The type of a memory sized to fit `len` bytes, with its data.
fn sized(len: u64, data: Option<&[u8]>) -> (MemoryType, Option<&[u8]>) {
    let pages = len.div_ceil(65536);
    (MemoryType::new64(pages, Some(pages)), data)
}

/// The host function `f`.
fn host(store: &mut Store<State>, f: HostFn) -> Func {
    match f {
        HostFn::Request => {
            Func::wrap(&mut *store, |mut c: Caller<'_, State>, k: i32, hi: i64| -> wasmtime::Result<()> {
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
            Func::wrap(&mut *store, |mut c: Caller<'_, State>, k: i32, lo: i64| -> wasmtime::Result<()> {
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
            Func::wrap(&mut *store, |mut c: Caller<'_, State>, j: i32, hi: i64| -> wasmtime::Result<()> {
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
    }
}
