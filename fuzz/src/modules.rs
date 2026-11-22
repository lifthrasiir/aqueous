//! `modules`' body ([`run`]): a WebAssembly module, as an archive may carry any, checked against
//! the profile and run by aqr-wasmrt (Wasmtime, in this process) as the decoder runs one that is
//! not known: of the input's bytes (the seeds' real modules, mutated), or made by wasm-smith of
//! the profile's imports and of an entry point, its loops bounded. Its application's parameters,
//! inputs, outputs' sizes and limits are of the input too. Nothing is checked of what it gives
//! (the decoder checks the outputs' sizes): that it fails or ends, and does not crash.

use aqr_archive::{Decoded, PartIn, PartOut};
use aqr_formula::Resource;
use libfuzzer_sys::arbitrary::{Result, Unstructured};
use sha2::Digest;

/// The host functions of the profile, by name and type (spec, "Module execution"), of whole
/// modules (`W`), part modules (`P`) and modules of tasks (`T`).
const FUNCS: [(&str, &str, char); 13] = [
    ("aqr_heartbeat", "", 'W'),
    ("aqr_log", "(param i32 i32)", 'W'),
    ("aqr_request", "(param i32 i64)", 'P'),
    ("aqr_release", "(param i32 i64)", 'P'),
    ("aqr_commit", "(param i32 i64)", 'P'),
    ("aqr_spawn", "(param i32 i32 i32 i32 i32) (result i32)", 'T'),
    ("aqr_result", "(param i32 i32)", 'T'),
    ("aqr_join", "(param i32 i32) (result i32)", 'T'),
    ("aqr_take", "(param i32 i32 i32 i32)", 'T'),
    ("aqr_chan", "(param i32) (result i32)", 'T'),
    ("aqr_send", "(param i32 i32 i32)", 'T'),
    ("aqr_recv", "(param i32 i32 i32) (result i32)", 'T'),
    ("aqr_close", "(param i32)", 'T'),
];

/// The fuel of a module made: its loops and calls, each taking one, trap once it is spent.
const FUEL: u32 = 100_000;

/// The shape of a module made of `u` (its imports and exports, as wasm-smith's `module_shape`
/// takes it), and how many memories it imports: a whole module, a part module, or a whole module
/// of tasks (`kind` 0, 1, 2); the working memory, the parameters', a resource's, inputs' and
/// outputs' now and then, and the host functions of its kind now and then.
fn shape(u: &mut Unstructured, kind: usize, (resources, inputs, outputs): Counts) -> Result<(String, u32)> {
    let mut m = String::from("(module\n  (import \"aqr\" \"aqr_memory\" (memory 0))\n");
    let mut memories = 1;
    let mut mem = |m: &mut String, name: &str| {
        m.push_str(&format!("  (import \"aqr\" \"{name}\" (memory i64 0))\n"));
        memories += 1;
    };
    if u.arbitrary()? {
        mem(&mut m, "aqr_params");
    }
    // (of the resources, inputs and outputs given, now and then each)
    for (prefix, n) in [("aqr_resource", resources), ("aqr_input", inputs), ("aqr_output", outputs)] {
        for k in 0..n {
            if u.ratio(2, 3)? {
                mem(&mut m, &format!("{prefix}{k}"));
            }
        }
    }
    let of = match kind {
        0 => "W",
        1 => "WP",
        _ => "WT",
    };
    for (name, ty, k) in FUNCS {
        // (a module of tasks spawning them)
        if of.contains(k) && (u.arbitrary()? || kind == 2 && name == "aqr_spawn") {
            m.push_str(&format!("  (import \"aqr\" \"{name}\" (func {ty}))\n"));
        }
    }
    // (the entry point's parameters: the parameters', then each resource's, input's and output's)
    let entry = if kind == 1 { "aqr_apply_part" } else { "aqr_apply_whole" };
    let params = " i64".repeat(1 + resources + inputs + outputs);
    m.push_str(&format!("  (func (export \"{entry}\") (param{params}))\n"));
    if kind == 2 {
        m.push_str("  (func (export \"aqr_task\") (param i32 i32))\n");
    }
    m.push(')');
    Ok((m, memories))
}

/// A module made by wasm-smith of `u`, of kind `kind` ([`shape`]), its loops bounded; of tasks,
/// a custom section of task slots after it (one byte a class: slots in the low 7 bits, a gang
/// with the top bit).
fn made(u: &mut Unstructured, kind: usize, counts: Counts) -> Result<Vec<u8>> {
    let (shape, memories) = shape(u, kind, counts)?;
    let mut c = wasm_smith::Config::default();
    c.module_shape = Some(wat::parse_str(&shape).expect("a shape"));
    (c.memory64_enabled, c.multi_value_enabled, c.bulk_memory_enabled) = (true, true, true);
    (c.threads_enabled, c.gc_enabled, c.exceptions_enabled, c.tail_call_enabled) = (false, false, false, false);
    (c.relaxed_simd_enabled, c.custom_page_sizes_enabled, c.wide_arithmetic_enabled) = (false, false, false);
    // (reference types as wasm-smith makes them need GC's types, which Wasmtime refuses here)
    c.reference_types_enabled = false;
    // (imports and constant expressions as WebAssembly 2 has them; no memory but those imported)
    (c.allow_start_export, c.compact_imports_enabled, c.extended_const_enabled) = (false, false, false);
    (c.max_memories, c.max_tables, c.max_funcs, c.max_instructions) = (memories as usize, 2, 16, 256);
    (c.max_memory32_bytes, c.max_memory64_bytes) = (1 << 26, 1 << 26);
    let mut m = wasm_smith::Module::new(c, u)?;
    let _ = m.ensure_termination(FUEL);
    let Some(mut wasm) = growing_working(&m.to_bytes()) else {
        return Err(libfuzzer_sys::arbitrary::Error::IncorrectFormat);
    };
    if kind == 2 {
        let n = u.int_in_range(1..=3)?;
        // (now and then any byte, else of 1 to 4 slots, a gang now and then)
        let slot = |u: &mut Unstructured| -> Result<u8> {
            Ok(match u.ratio(1, 8)? {
                true => u.arbitrary()?,
                false => u.int_in_range(1..=4u8)? | if u.ratio(1, 4)? { 0x80 } else { 0 },
            })
        };
        let slots: Vec<u8> = (0..n).map(|_| slot(u)).collect::<Result<_>>()?;
        let mut body = vec![14u8];
        body.extend_from_slice(b"aqr_task_slots");
        body.extend_from_slice(&slots);
        wasm.push(0);
        let mut n = body.len();
        while n >= 0x80 {
            wasm.push(n as u8 | 0x80);
            n >>= 7;
        }
        wasm.push(n as u8);
        wasm.extend_from_slice(&body);
    }
    Ok(wasm)
}

/// What makes a module's `memory.grow` of a memory other than the working memory (index 0, the
/// first imported; the others 64-bit), which the profile refuses, a grow that fails: its delta
/// dropped, -1.
struct GrowingWorking;

impl wasm_encoder::reencode::Reencode for GrowingWorking {
    type Error = std::convert::Infallible;

    fn parse_function_body(
        &mut self,
        code: &mut wasm_encoder::CodeSection,
        func: wasmparser::FunctionBody<'_>,
    ) -> std::result::Result<(), wasm_encoder::reencode::Error<Self::Error>> {
        let mut f = self.new_function_with_parsed_locals(&func)?;
        let mut r = func.get_operators_reader()?;
        while !r.eof() {
            match r.read()? {
                wasmparser::Operator::MemoryGrow { mem } if mem != 0 => {
                    f.instruction(&wasm_encoder::Instruction::Drop);
                    f.instruction(&wasm_encoder::Instruction::I64Const(-1));
                }
                op => {
                    f.instruction(&self.instruction(op)?);
                }
            }
        }
        code.function(&f);
        Ok(())
    }
}

/// Module `wasm` growing the working memory alone ([`GrowingWorking`]).
fn growing_working(wasm: &[u8]) -> Option<Vec<u8>> {
    use wasm_encoder::reencode::Reencode;
    let mut m = wasm_encoder::Module::new();
    GrowingWorking.parse_core_module(&mut m, wasmparser::Parser::new(0), wasm).ok()?;
    Some(m.finish())
}

/// An input of a part module's application: its bytes, pulled.
struct In(Vec<u8>, usize);

impl PartIn for In {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }

    fn pull(&mut self, hi: u64, put: &mut dyn FnMut(&[u8])) -> Decoded<()> {
        let hi = (hi as usize).min(self.0.len());
        if hi > self.1 {
            put(&self.0[self.1..hi]);
            self.1 = hi;
        }
        Ok(())
    }
}

/// An output of a part module's application: its bytes, up to `.1` of them.
struct Out(Vec<u8>, u64);

impl PartOut for Out {
    fn push(&mut self, d: &[u8]) -> Decoded<()> {
        match self.0.len() as u64 + d.len() as u64 <= self.1 {
            true => Ok(self.0.extend_from_slice(d)),
            false => Err(aqr_archive::Failure::Invalid("an output past its size".into())),
        }
    }
}

/// How many resources, inputs and outputs an application has.
type Counts = (usize, usize, usize);

fn counts(u: &mut Unstructured) -> Result<Counts> {
    Ok((u.int_in_range(0..=1)?, u.int_in_range(0..=2)?, u.int_in_range(1..=2)?))
}

/// The application's parameters, resources, inputs, outputs' sizes (of `counts`), memory limit
/// and the limit of a task of each of `classes`, of `u`.
#[allow(clippy::type_complexity)]
fn application(
    u: &mut Unstructured,
    (r, i, o): Counts,
    classes: usize,
) -> Result<(Vec<u8>, Vec<Resource>, Vec<Vec<u8>>, Vec<u64>, u64, Vec<u64>)> {
    let bytes = |u: &mut Unstructured, most: usize| -> Result<Vec<u8>> {
        let n = u.int_in_range(0..=most)?;
        Ok(u.bytes(n.min(u.len()))?.to_vec())
    };
    let params = bytes(u, 16)?;
    let resources = (0..r).map(|_| bytes(u, 64).map(Resource::new)).collect::<Result<_>>()?;
    let inputs = (0..i).map(|_| bytes(u, 512)).collect::<Result<_>>()?;
    let sizes = (0..o).map(|_| u.int_in_range(0..=4096)).collect::<Result<_>>()?;
    let memory_limit = u.int_in_range(1..=1024u64)? << 16;
    let task_limits = (0..classes).map(|_| u.int_in_range(1..=256u64).map(|p| p << 16)).collect::<Result<_>>()?;
    Ok((params, resources, inputs, sizes, memory_limit, task_limits))
}

/// What became of an input, for `replay`.
pub fn run(data: &[u8]) -> String {
    let mut u = Unstructured::new(data);
    let Ok(how) = u.int_in_range(0..=3u8) else { return "empty".into() };
    // (the application's sizes first, of a few bytes, then the module: what is left, or made)
    let mut head = Unstructured::new(u.bytes(u.len().min(64)).unwrap_or_default());
    let Ok(counts) = counts(&mut head) else { return "no application".into() };
    let wasm = match how {
        0 => u.take_rest().to_vec(),
        e => match made(&mut u, e as usize - 1, counts) {
            Ok(w) => w,
            Err(_) => return "not made".into(),
        },
    };
    let hash: [u8; 32] = sha2::Sha512_256::digest(&wasm).into();
    let l = match aqr_wasmrt::wasm::load(&wasm, hash) {
        Ok(l) => l,
        Err(e) => return format!("refused: {e}"),
    };
    let classes = aqr_archive::task_classes(&wasm);
    let Ok((params, resources, inputs, sizes, memory_limit, task_limits)) = application(&mut head, counts, classes)
    else {
        return "no application".into();
    };
    if l.is_part_of(&sizes) {
        let mut ins: Vec<In> = inputs.into_iter().map(|d| In(d, 0)).collect();
        let mut outs: Vec<Out> = sizes.iter().map(|&n| Out(Vec::new(), n)).collect();
        let mut ins_dyn: Vec<&mut dyn PartIn> = ins.iter_mut().map(|i| i as &mut dyn PartIn).collect();
        let mut outs_dyn: Vec<&mut dyn PartOut> = outs.iter_mut().map(|o| o as &mut dyn PartOut).collect();
        let r = l.apply_part(&params, &resources, &mut ins_dyn, &mut outs_dyn, &sizes, memory_limit, &task_limits);
        format!("applied as a part module: {:?}", r.map_err(|e| e.to_string()))
    } else {
        let inputs: Vec<&[u8]> = inputs.iter().map(|d| &d[..]).collect();
        let r = l.apply(&params, &resources, &inputs, &sizes, memory_limit, &task_limits);
        format!("applied: {:?}", r.map(|o| o.len()).map_err(|e| e.to_string()))
    }
}
