//! Rewrites the `aqr_api` imports (see `aqr-api/src/wasm.rs`) into a module of the spec's profile:
//!
//! - the memory defined by the linker becomes the working memory `aqr.aqr_memory`;
//! - `size_M`, `read_M`, `write_O` and `copy_O_M` become `memory.size M`, `memory.copy working M`,
//!   `memory.copy O working` and `memory.copy O M` (`O` an output), with `aqr.aqr_M` imported
//!   (64-bit, minimum 0, no maximum) for each memory `M` used;
//! - `request`, `release`, `commit`, `heartbeat`, `log`, `spawn`, `result`, `join` and `take`
//!   become `aqr.aqr_request` and so on;
//! - where the module spawns tasks, `aqr_task` is exported: it sets the stack pointer below the
//!   spawning frame (the record its first argument points to, aligned down to 16) and calls
//!   `aqr_task_body` (`aqr-api`'s);
//! - every other export but the entry point goes, and so does whatever only they used.

use std::collections::HashMap;

use walrus::ir::{BinaryOp, Instr, InstrLocId, MemoryCopy, MemorySize, VisitorMut, dfs_pre_order_mut};
use walrus::{
    ExportItem, FunctionBuilder, FunctionId, GlobalKind, ImportKind, MemoryId, Module, ModuleConfig, ValType,
};
use wasmparser::{Validator, WasmFeatures};

const API: &str = "aqr_api";
const WHOLE: &str = "aqr_apply_whole";
const PART: &str = "aqr_apply_part";
const TASK: &str = "aqr_task";
const TASK_BODY: &str = "aqr_task_body";

fn parse(wasm: &[u8]) -> Result<Module, String> {
    ModuleConfig::new().parse(wasm).map_err(|e| e.to_string())
}

/// Whether the module imports `aqr_api` or exports an entry point.
pub fn is_aqr_module(wasm: &[u8]) -> Result<bool, String> {
    let m = parse(wasm)?;
    Ok(m.imports.iter().any(|i| i.module == API) || m.exports.iter().any(|e| e.name == WHOLE || e.name == PART))
}

/// `params`, `output`j, `input`k or `resource`i, without leading zeros.
fn is_memory_name(name: &str) -> bool {
    name == "params" || is_output(name) || indexed(name, "input") || indexed(name, "resource")
}

/// `output`j, without leading zeros.
fn is_output(name: &str) -> bool {
    indexed(name, "output")
}

/// `prefix` and an index without leading zeros.
fn indexed(name: &str, prefix: &str) -> bool {
    let index = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && (s == "0" || !s.starts_with('0'));
    name.strip_prefix(prefix).is_some_and(index)
}

enum Op {
    Host,
    Size,
    Read,
    Write,
    /// Into an output, from a memory.
    Copy(String),
}

struct Calls<'a> {
    calls: &'a HashMap<FunctionId, Instr>,
    working: MemoryId,
    grows_other: bool,
}

impl VisitorMut for Calls<'_> {
    fn visit_instr_mut(&mut self, instr: &mut Instr, _: &mut InstrLocId) {
        match instr {
            Instr::Call(call) => {
                if let Some(new) = self.calls.get(&call.func) {
                    *instr = new.clone();
                }
            }
            Instr::MemoryGrow(grow) => self.grows_other |= grow.memory != self.working,
            _ => {}
        }
    }
}

pub fn rewrite(wasm: &[u8]) -> Result<Vec<u8>, String> {
    use ValType::{I32, I64};

    let mut m = parse(wasm)?;

    let working = match m.memories.iter().map(|mem| mem.id()).collect::<Vec<_>>()[..] {
        [w] => w,
        _ => return Err("not exactly one memory from the linker".into()),
    };
    let mem = m.memories.get(working);
    if mem.import.is_some() || mem.memory64 || mem.shared {
        return Err("the memory from the linker is not a defined 32-bit unshared one (--import-memory?)".into());
    }
    let import = m.imports.add("aqr", "aqr_memory", working);
    m.memories.get_mut(working).import = Some(import);

    let mut memories = HashMap::new();
    let mut memory = |m: &mut Module, name: &str| {
        *memories
            .entry(name.to_owned())
            .or_insert_with(|| m.add_import_memory("aqr", &format!("aqr_{name}"), false, true, 0, None, None).0)
    };
    let mut calls = HashMap::new();
    let mut part_funcs = Vec::new();
    let imports: Vec<_> =
        m.imports.iter().map(|i| (i.id(), i.module.clone(), i.name.clone(), i.kind.clone())).collect();
    for (id, module, name, kind) in imports {
        if id == import {
            continue;
        }
        if module != API {
            return Err(format!("import {module}.{name} other than from {API}"));
        }
        let ImportKind::Function(func) = kind else {
            return Err(format!("{API}.{name} is not a function"));
        };
        let (op, mem, params, results): (_, _, &[_], &[_]) = match name.as_str() {
            "request" | "release" | "commit" => (Op::Host, "", &[I32, I64], &[]),
            "heartbeat" => (Op::Host, "", &[], &[]),
            "log" | "result" => (Op::Host, "", &[I32, I32], &[]),
            "spawn" | "join" => (Op::Host, "", &[I32, I32], &[I32]),
            "take" => (Op::Host, "", &[I32, I32, I32, I32], &[]),
            _ => match name.split_once('_') {
                Some(("size", mem)) => (Op::Size, mem, &[], &[I64]),
                Some(("read", mem)) => (Op::Read, mem, &[I32, I64, I32], &[]),
                Some(("write", mem)) if is_output(mem) => (Op::Write, mem, &[I64, I32, I32], &[]),
                Some(("copy", ms)) => match ms.split_once('_') {
                    Some((out, mem)) if is_output(out) => (Op::Copy(out.to_owned()), mem, &[I64, I64, I64], &[]),
                    _ => return Err(format!("unknown {API}.{name}")),
                },
                _ => return Err(format!("unknown {API}.{name}")),
            },
        };
        let ty = m.types.get(m.funcs.get(func).ty());
        if ty.params() != params || ty.results() != results {
            return Err(format!("{API}.{name} has the type {:?} -> {:?}", ty.params(), ty.results()));
        }
        if !matches!(op, Op::Host) && !is_memory_name(mem) {
            return Err(format!("unknown memory in {API}.{name}"));
        }
        let instr: Instr = match op {
            Op::Host => {
                let i = m.imports.get_mut(id);
                i.module = "aqr".into();
                i.name = format!("aqr_{name}");
                if matches!(name.as_str(), "request" | "release" | "commit") {
                    part_funcs.push(i.name.clone());
                }
                continue;
            }
            Op::Size => MemorySize { memory: memory(&mut m, mem) }.into(),
            Op::Read => MemoryCopy { dst: working, src: memory(&mut m, mem) }.into(),
            Op::Write => MemoryCopy { dst: memory(&mut m, mem), src: working }.into(),
            Op::Copy(out) => MemoryCopy { dst: memory(&mut m, &out), src: memory(&mut m, mem) }.into(),
        };
        calls.insert(func, instr);
    }

    let mut visitor = Calls { calls: &calls, working, grows_other: false };
    for (_, func) in m.funcs.iter_local_mut() {
        let entry = func.entry_block();
        dfs_pre_order_mut(&mut visitor, func, entry);
    }
    if visitor.grows_other {
        return Err("memory.grow on a memory other than the working memory".into());
    }

    let mut entry = None;
    let mut task_body = None;
    let exports: Vec<_> = m.exports.iter().map(|e| (e.id(), e.name.clone(), e.item)).collect();
    for (id, name, item) in exports {
        match item {
            ExportItem::Function(func) if name == TASK_BODY => {
                task_body = Some(func);
                m.exports.delete(id);
            }
            ExportItem::Function(func) if name == WHOLE || name == PART => {
                if entry.replace((name.clone(), func)).is_some() {
                    return Err(format!("both {WHOLE} and {PART} exported"));
                }
            }
            _ => m.exports.delete(id),
        }
    }
    let Some((entry, func)) = entry else {
        return Err(format!("neither {WHOLE} nor {PART} exported"));
    };
    let ty = m.types.get(m.funcs.get(func).ty());
    if !ty.params().iter().all(|&t| t == I64) || !ty.results().is_empty() {
        return Err(format!("{entry} has the type {:?} -> {:?}", ty.params(), ty.results()));
    }
    if entry == WHOLE && !part_funcs.is_empty() {
        return Err(format!("whole module imports {}", part_funcs.join(", ")));
    }
    if m.imports.iter().any(|i| i.module == "aqr" && i.name == "aqr_spawn") {
        task(&mut m, task_body)?;
    }

    walrus::passes::gc::run(&mut m);
    if let Some(i) = m.imports.iter().find(|i| i.module == API) {
        return Err(format!("{API}.{} used other than by a call", i.name));
    }
    if m.imports.iter().any(|i| matches!(i.kind, ImportKind::Table(_))) {
        return Err("table imported".into());
    }

    let wasm = m.emit_wasm();
    Validator::new_with_features(WasmFeatures::WASM2 | WasmFeatures::MULTI_MEMORY | WasmFeatures::MEMORY64)
        .validate_all(&wasm)
        .map_err(|e| format!("invalid output: {e}"))?;
    Ok(wasm)
}

/// Exports `aqr_task`, calling `body` (`aqr_task_body`) on the stack below the record of the
/// spawn: the frames of the module that spawned the task, which what it runs borrows from, are
/// above it.
fn task(m: &mut Module, body: Option<FunctionId>) -> Result<(), String> {
    use ValType::I32;
    let body = body.ok_or(format!("tasks spawned without {TASK_BODY}"))?;
    let ty = m.types.get(m.funcs.get(body).ty());
    if ty.params() != [I32, I32] || !ty.results().is_empty() {
        return Err(format!("{TASK_BODY} has the type {:?} -> {:?}", ty.params(), ty.results()));
    }
    // the stack pointer: the one mutable global of the linker's output
    let sp = match m.globals.iter().filter(|g| g.mutable).map(|g| (g.id(), g.ty, &g.kind)).collect::<Vec<_>>()[..] {
        [(sp, I32, GlobalKind::Local(_))] => sp,
        _ => return Err("not exactly one mutable global, the stack pointer".into()),
    };
    let (rec, i) = (m.locals.add(I32), m.locals.add(I32));
    let mut b = FunctionBuilder::new(&mut m.types, &[I32, I32], &[]);
    b.func_body()
        .local_get(rec)
        .i32_const(-16)
        .binop(BinaryOp::I32And)
        .global_set(sp)
        .local_get(rec)
        .local_get(i)
        .call(body);
    let f = b.finish(vec![rec, i], &mut m.funcs);
    m.exports.add(TASK, f);
    Ok(())
}
