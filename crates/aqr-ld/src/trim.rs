//! Takes out of a module what only panics and their messages need:
//!
//! - the panic handler traps at once unless it logs (std's has nowhere to print), and so does
//!   every function that surely reaches a trap with no effect but on memories (`panic_fmt`,
//!   `panic_bounds_check` and so on, as they make the message); a call to one becomes a trap,
//!   and what only made its arguments before it goes;
//! - with the relocations of the linker's output (`--emit-relocs`), the data and the table
//!   entries that no code left refers to go: the data zeroed, and out of the data segments where
//!   long enough; the table renumbered, and the functions only it held gone (`fmt` impls and so
//!   on, held by the vtables of the messages).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;

use walrus::ir::{
    Instr, InstrLocId, InstrSeq, InstrSeqId, Unreachable, Value, Visitor, VisitorMut, dfs_in_order, dfs_pre_order_mut,
};
use walrus::{
    ConstExpr, DataKind, ElementItems, ElementKind, FunctionId, FunctionKind, ImportKind, LocalFunction, MemoryId,
    Module,
};
use wasmparser::{KnownCustom, Linking, Operator, Parser, Payload, RelocationType, SymbolInfo};

/// What a relocated value refers to.
#[derive(Clone)]
enum Ref {
    /// The data at these addresses (a data symbol).
    Data(Range<u32>),
    /// A table entry, the value itself.
    Table,
}

/// The relocations of the linker's output, by where they are.
pub struct Relocs {
    /// By the offset of the value in the file.
    code: Vec<(u32, Ref)>,
    /// By the address of the value in memory.
    data: Vec<(u32, Ref)>,
}

/// The relocations of `wasm`, if it has them.
pub fn relocs(wasm: &[u8]) -> Result<Option<Relocs>, String> {
    let e = |e: wasmparser::BinaryReaderError| e.to_string();
    let mut code_start = 0;
    let mut data_start = 0;
    // (offset in the file of the bytes, their address), by segment
    let mut segments = Vec::new();
    let mut symbols = Vec::new();
    let mut code = Vec::new();
    let mut data = Vec::new();
    let mut linking = false;
    for payload in Parser::new(0).parse_all(wasm) {
        match payload.map_err(e)? {
            Payload::CodeSectionStart { range, .. } => code_start = range.start,
            Payload::DataSection(r) => {
                data_start = r.range().start;
                for d in r {
                    let d = d.map_err(e)?;
                    let base = match d.kind {
                        wasmparser::DataKind::Active { memory_index: 0, offset_expr } => {
                            match offset_expr.get_operators_reader().read().map_err(e)? {
                                Operator::I32Const { value } => Some(value as u32),
                                _ => return Err("data segment at no constant address".into()),
                            }
                        }
                        _ => None,
                    };
                    segments.push((d.data.as_ptr() as usize - wasm.as_ptr() as usize, base));
                }
            }
            Payload::CustomSection(c) => match c.as_known() {
                KnownCustom::Linking(l) => {
                    linking = true;
                    for sub in l.subsections() {
                        let Linking::SymbolTable(map) = sub.map_err(e)? else { continue };
                        for s in map {
                            symbols.push(match s.map_err(e)? {
                                SymbolInfo::Data { symbol: Some(d), .. } => {
                                    let base = segments.get(d.index as usize).and_then(|s| s.1);
                                    base.map(|b| b + d.offset..b + d.offset + d.size)
                                }
                                _ => None,
                            });
                        }
                    }
                }
                KnownCustom::Reloc(r) if c.name() == "reloc.CODE" || c.name() == "reloc.DATA" => {
                    let is_code = c.name() == "reloc.CODE";
                    for r in r.entries() {
                        let r = r.map_err(e)?;
                        let to = match r.ty {
                            RelocationType::MemoryAddrLeb
                            | RelocationType::MemoryAddrSleb
                            | RelocationType::MemoryAddrI32 => match symbols.get(r.index as usize) {
                                Some(Some(range)) => Ref::Data(range.clone()),
                                // (not in a data segment: zeros, or the linker's)
                                _ => continue,
                            },
                            RelocationType::TableIndexSleb | RelocationType::TableIndexI32 => Ref::Table,
                            RelocationType::FunctionIndexLeb
                            | RelocationType::TypeIndexLeb
                            | RelocationType::GlobalIndexLeb
                            | RelocationType::TableNumberLeb => continue,
                            ty => return Err(format!("relocation {ty:?}")),
                        };
                        if is_code {
                            code.push(((code_start + r.offset as usize) as u32, to));
                        } else {
                            let at = data_start + r.offset as usize;
                            let (bytes, base) = segments
                                .iter()
                                .rev()
                                .find(|s| s.0 <= at)
                                .ok_or("relocation before the data segments")?;
                            let base = base.ok_or("relocation in a passive data segment")?;
                            data.push((base + (at - bytes) as u32, to));
                        }
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
    data.sort_by_key(|r| r.0);
    Ok(linking.then_some(Relocs { code, data }))
}

/// The position of the instruction at `loc` (none for one made here).
fn pos(loc: &InstrLocId) -> u32 {
    if loc.is_default() { u32::MAX } else { loc.data() }
}

/// The ids of the sequences of `func`.
fn seqs(func: &LocalFunction) -> Vec<InstrSeqId> {
    struct V(Vec<InstrSeqId>);
    impl<'i> Visitor<'i> for V {
        fn start_instr_seq(&mut self, seq: &'i InstrSeq) {
            self.0.push(seq.id());
        }
    }
    let mut v = V(Vec::new());
    dfs_in_order(&mut v, func, func.entry_block());
    v.0
}

/// How running a sequence from its start may end.
enum End {
    /// Maybe at its end.
    Falls,
    /// Never at its end, and only by a trap or a branch out of it (or never: a loop that may
    /// not end counts, a hang made a trap).
    Not,
    /// Maybe otherwise: by a return, a call that may do anything.
    Unknown,
}

/// What is known of the functions of a module.
struct Facts {
    /// Those that surely trap.
    doomed: HashSet<FunctionId>,
    /// Those with no effect out of the instance: they call no import, nothing indirectly.
    quiet: HashSet<FunctionId>,
}

/// How `seq` may end, adding to `targets` the blocks that it may branch to.
fn end(func: &LocalFunction, seq: InstrSeqId, facts: &Facts, targets: &mut HashSet<InstrSeqId>) -> End {
    for (instr, _) in &func.block(seq).instrs {
        match instr {
            Instr::Unreachable(_) => return End::Not,
            Instr::Call(c) if facts.doomed.contains(&c.func) => return End::Not,
            Instr::Call(c) if facts.quiet.contains(&c.func) => {}
            Instr::Br(b) => {
                targets.insert(b.block);
                return End::Not;
            }
            Instr::BrIf(b) => {
                targets.insert(b.block);
            }
            Instr::BrTable(b) => {
                targets.extend(b.blocks.iter().copied());
                targets.insert(b.default);
                return End::Not;
            }
            Instr::Block(b) => match end(func, b.seq, facts, targets) {
                End::Unknown => return End::Unknown,
                End::Not if !targets.contains(&b.seq) => return End::Not,
                _ => {}
            },
            // (a branch to it stays in it)
            Instr::Loop(l) => match end(func, l.seq, facts, targets) {
                End::Unknown => return End::Unknown,
                End::Not => return End::Not,
                End::Falls => {}
            },
            Instr::IfElse(b) => {
                let ends = [end(func, b.consequent, facts, targets), end(func, b.alternative, facts, targets)];
                if ends.iter().any(|e| matches!(e, End::Unknown)) {
                    return End::Unknown;
                }
                let falls = |e: &End, s| matches!(e, End::Falls) || targets.contains(&s);
                if !falls(&ends[0], b.consequent) && !falls(&ends[1], b.alternative) {
                    return End::Not;
                }
            }
            Instr::Call(_)
            | Instr::CallIndirect(_)
            | Instr::Return(_)
            | Instr::ReturnCall(_)
            | Instr::ReturnCallIndirect(_) => return End::Unknown,
            _ => {}
        }
    }
    End::Falls
}

/// Whether `instr`, just before a trap, may go: it has no effect but on the instance.
fn idle(instr: &Instr, facts: &Facts) -> bool {
    match instr {
        Instr::Call(c) => facts.quiet.contains(&c.func),
        _ => matches!(
            instr,
            Instr::Const(_)
                | Instr::LocalGet(_)
                | Instr::LocalSet(_)
                | Instr::LocalTee(_)
                | Instr::GlobalGet(_)
                | Instr::GlobalSet(_)
                | Instr::Binop(_)
                | Instr::Unop(_)
                | Instr::Select(_)
                | Instr::Drop(_)
                | Instr::Load(_)
                | Instr::Store(_)
                | Instr::MemoryCopy(_)
                | Instr::MemoryFill(_)
        ),
    }
}

/// The functions that call `targets`, directly or not.
fn reaches(m: &Module, targets: &HashSet<FunctionId>) -> HashSet<FunctionId> {
    let mut callers: HashMap<FunctionId, Vec<FunctionId>> = HashMap::new();
    for (id, func) in m.funcs.iter_local() {
        for seq in seqs(func) {
            for (instr, _) in &func.block(seq).instrs {
                match instr {
                    Instr::Call(c) => callers.entry(c.func).or_default().push(id),
                    Instr::ReturnCall(c) => callers.entry(c.func).or_default().push(id),
                    _ => {}
                }
            }
        }
    }
    let mut found = HashSet::new();
    let mut work: Vec<_> = targets.iter().copied().collect();
    while let Some(f) = work.pop() {
        for &c in callers.get(&f).into_iter().flatten() {
            if found.insert(c) {
                work.push(c);
            }
        }
    }
    found
}

/// Makes the panics traps, then (with `relocs`) takes out what is no longer referred to.
pub fn trim(m: &mut Module, relocs: Option<&Relocs>, working: MemoryId) -> Result<(), String> {
    // the position of every instruction, for the relocations to find theirs
    let mut locs: Vec<u32> = Vec::new();
    for (_, func) in m.funcs.iter_local() {
        for seq in seqs(func) {
            locs.extend(func.block(seq).instrs.iter().filter(|(_, loc)| !loc.is_default()).map(|(_, loc)| loc.data()));
        }
    }
    locs.sort_unstable();
    let mut code_refs: HashMap<u32, Vec<Ref>> = HashMap::new();
    for (at, to) in relocs.map(|r| &r.code[..]).unwrap_or_default() {
        let i = locs.partition_point(|&l| l <= *at);
        let loc = *locs[..i].last().ok_or("relocation before the code")?;
        code_refs.entry(loc).or_default().push(to.clone());
    }

    doom(m);

    if let Some(relocs) = relocs {
        collect(m, relocs, &code_refs, working)?;
    }
    Ok(())
}

/// Makes the panic handler, unless it logs, and every function surely reaching a trap traps.
fn doom(m: &mut Module) {
    let log: HashSet<_> = m
        .imports
        .iter()
        .filter_map(|i| match i.kind {
            ImportKind::Function(f) if i.name == "aqr_log" => Some(f),
            _ => None,
        })
        .collect();
    let logging = reaches(m, &log);
    let mut loud: HashSet<_> =
        m.funcs.iter().filter(|f| matches!(f.kind, FunctionKind::Import(_))).map(|f| f.id()).collect();
    for (id, func) in m.funcs.iter_local() {
        let indirect = seqs(func).into_iter().any(|seq| {
            func.block(seq)
                .instrs
                .iter()
                .any(|(i, _)| matches!(i, Instr::CallIndirect(_) | Instr::ReturnCallIndirect(_)))
        });
        if indirect {
            loud.insert(id);
        }
    }
    loud.extend(reaches(m, &loud));
    let quiet = m.funcs.iter().map(|f| f.id()).filter(|f| !loud.contains(f)).collect();
    let handler: HashSet<FunctionId> = m
        .funcs
        .iter()
        .filter(|f| f.name.as_deref().is_some_and(|n| n.ends_with("rust_begin_unwind")))
        .map(|f| f.id())
        .filter(|f| !logging.contains(f))
        .collect();
    // all doomed at first, then those that may return or do more taken out, until none is: the
    // functions left surely trap (or never end), those calling each other too
    let mut facts =
        Facts { doomed: m.funcs.iter_local().map(|(id, _)| id).chain(handler.iter().copied()).collect(), quiet };
    loop {
        let out: Vec<_> = m
            .funcs
            .iter_local()
            .filter(|(id, _)| facts.doomed.contains(id) && !handler.contains(id))
            .filter(|(_, func)| {
                let mut targets = HashSet::new();
                !matches!(end(func, func.entry_block(), &facts, &mut targets), End::Not)
                    || targets.contains(&func.entry_block())
            })
            .map(|(id, _)| id)
            .collect();
        if out.is_empty() {
            break;
        }
        for id in out {
            facts.doomed.remove(&id);
        }
    }
    let doomed = &facts.doomed;

    for (id, func) in m.funcs.iter_local_mut() {
        if doomed.contains(&id) {
            let entry = func.entry_block();
            // (of no position: what was there no longer refers to anything)
            func.block_mut(entry).instrs = vec![(Unreachable {}.into(), InstrLocId::default())];
            continue;
        }
        for seq in seqs(func) {
            let instrs = &mut func.block_mut(seq).instrs;
            let trap = instrs.iter().position(|(instr, _)| match instr {
                Instr::Unreachable(_) => true,
                Instr::Call(c) => doomed.contains(&c.func),
                _ => false,
            });
            let Some(trap) = trap else { continue };
            instrs.truncate(trap);
            while instrs.last().is_some_and(|(instr, _)| idle(instr, &facts)) {
                instrs.pop();
            }
            instrs.push((Unreachable {}.into(), InstrLocId::default()));
        }
    }
}

/// What a live function or data refers to.
struct Live<'a> {
    m: &'a Module,
    code_refs: &'a HashMap<u32, Vec<Ref>>,
    relocs: &'a Relocs,
    /// The initial bytes of memory, by address of the segment.
    image: &'a BTreeMap<u32, Vec<u8>>,
    funcs: HashSet<FunctionId>,
    func_work: Vec<FunctionId>,
    /// Live data, as ranges of addresses (overlapping maybe).
    data: Vec<Range<u32>>,
    data_seen: HashSet<(u32, u32)>,
    data_work: Vec<Range<u32>>,
    /// Live table entries, by index.
    table: HashSet<u32>,
    table_funcs: &'a HashMap<u32, FunctionId>,
}

impl Live<'_> {
    fn func(&mut self, f: FunctionId) {
        if self.funcs.insert(f) {
            self.func_work.push(f);
        }
    }

    fn data(&mut self, r: &Range<u32>) {
        if self.data_seen.insert((r.start, r.end)) {
            self.data.push(r.clone());
            self.data_work.push(r.clone());
        }
    }

    fn table(&mut self, index: u32) -> Result<(), String> {
        if index == 0 {
            return Ok(());
        }
        let f = *self.table_funcs.get(&index).ok_or(format!("table index {index} of no function"))?;
        self.table.insert(index);
        self.func(f);
        Ok(())
    }

    fn byte4(&self, at: u32) -> Option<u32> {
        let (base, bytes) = self.image.range(..=at).next_back()?;
        let i = (at - base) as usize;
        Some(u32::from_le_bytes(bytes.get(i..i + 4)?.try_into().ok()?))
    }

    fn run(&mut self) -> Result<(), String> {
        loop {
            if let Some(f) = self.func_work.pop() {
                let FunctionKind::Local(func) = &self.m.funcs.get(f).kind else { continue };
                for seq in seqs(func) {
                    for (instr, loc) in &func.block(seq).instrs {
                        match instr {
                            Instr::Call(c) => self.func(c.func),
                            Instr::ReturnCall(c) => self.func(c.func),
                            Instr::RefFunc(r) => self.func(r.func),
                            _ => {}
                        }
                        for to in self.code_refs.get(&pos(loc)).into_iter().flatten() {
                            match (to, instr) {
                                (Ref::Data(r), _) => self.data(r),
                                (Ref::Table, Instr::Const(c)) => match c.value {
                                    Value::I32(i) => self.table(i as u32)?,
                                    _ => return Err("table index not in an i32.const".into()),
                                },
                                (Ref::Table, _) => return Err("table index not in an i32.const".into()),
                            }
                        }
                    }
                }
            } else if let Some(r) = self.data_work.pop() {
                let relocs = self.relocs;
                let from = relocs.data.partition_point(|d| d.0 < r.start);
                for (at, to) in relocs.data[from..].iter().take_while(|d| d.0 < r.end) {
                    match to {
                        Ref::Data(r) => self.data(r),
                        Ref::Table => {
                            let i = self.byte4(*at).ok_or("table index out of the data segments")?;
                            self.table(i)?;
                        }
                    }
                }
            } else {
                return Ok(());
            }
        }
    }
}

/// Data segments shorter apart than this are one.
const GAP: u32 = 8;

/// Takes out the data and the table entries that no live code or data refers to.
fn collect(
    m: &mut Module,
    relocs: &Relocs,
    code_refs: &HashMap<u32, Vec<Ref>>,
    working: MemoryId,
) -> Result<(), String> {
    let mut image = BTreeMap::new();
    for d in m.data.iter() {
        match d.kind {
            DataKind::Active { memory, offset: ConstExpr::Value(Value::I32(a)) } if memory == working => {
                image.insert(a as u32, d.value.clone());
            }
            _ => return Err("data segment other than active in the working memory".into()),
        }
    }
    let (elem, table, table_funcs) = match &m.elements.iter().collect::<Vec<_>>()[..] {
        [] => (None, None, HashMap::new()),
        [e] => match (&e.kind, &e.items) {
            (ElementKind::Active { table, offset: ConstExpr::Value(Value::I32(o)) }, ElementItems::Functions(fs)) => {
                let funcs = fs.iter().enumerate().map(|(i, &f)| (*o as u32 + i as u32, f)).collect();
                (Some(e.id()), Some(*table), funcs)
            }
            _ => return Err("element segment other than active functions".into()),
        },
        _ => return Err("element segments other than one".into()),
    };

    let mut live = Live {
        m,
        code_refs,
        relocs,
        image: &image,
        funcs: HashSet::new(),
        func_work: Vec::new(),
        data: Vec::new(),
        data_seen: HashSet::new(),
        data_work: Vec::new(),
        table: HashSet::new(),
        table_funcs: &table_funcs,
    };
    for e in m.exports.iter() {
        if let walrus::ExportItem::Function(f) = e.item {
            live.func(f);
        }
    }
    if let Some(f) = m.start {
        live.func(f);
    }
    live.run()?;
    let (live_funcs, mut live_data, live_table) = (live.funcs, live.data, live.table);

    // the table: the live entries from 1 on, in their order
    let mut renumber = HashMap::new();
    if let (Some(elem), Some(table)) = (elem, table) {
        let mut kept: Vec<_> = live_table.iter().copied().collect();
        kept.sort_unstable();
        renumber = kept.iter().enumerate().map(|(i, &old)| (old, i as u32 + 1)).collect();
        let funcs = kept.iter().map(|i| table_funcs[i]).collect();
        let e = m.elements.get_mut(elem);
        e.kind = ElementKind::Active { table, offset: ConstExpr::Value(Value::I32(1)) };
        e.items = ElementItems::Functions(funcs);
        let t = m.tables.get_mut(table);
        t.initial = kept.len() as u64 + 1;
        t.maximum = Some(t.initial);
    }
    struct Patch<'a> {
        code_refs: &'a HashMap<u32, Vec<Ref>>,
        renumber: &'a HashMap<u32, u32>,
    }
    impl VisitorMut for Patch<'_> {
        fn visit_instr_mut(&mut self, instr: &mut Instr, loc: &mut InstrLocId) {
            let table = self.code_refs.get(&pos(loc)).is_some_and(|r| r.iter().any(|r| matches!(r, Ref::Table)));
            if let (true, Instr::Const(c)) = (table, instr)
                && let Value::I32(i) = &mut c.value
                && *i != 0
            {
                *i = self.renumber[&(*i as u32)] as i32;
            }
        }
    }
    let mut patch = Patch { code_refs, renumber: &renumber };
    for (id, func) in m.funcs.iter_local_mut() {
        if live_funcs.contains(&id) {
            let entry = func.entry_block();
            dfs_pre_order_mut(&mut patch, func, entry);
        }
    }

    // the data: the live bytes (table indices renumbered), dead ones zeroed, long dead runs out
    live_data.sort_by_key(|r| r.start);
    for (at, to) in &relocs.data {
        let Ref::Table = to else { continue };
        if !live_data.iter().any(|r| r.contains(at)) {
            continue;
        }
        let (base, bytes) = image.range_mut(..=*at).next_back().ok_or("table index out of the data segments")?;
        let i = (at - base) as usize;
        let old = u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap());
        if old != 0 {
            bytes[i..i + 4].copy_from_slice(&renumber[&old].to_le_bytes());
        }
    }
    let ids: Vec<_> = m.data.iter().map(|d| d.id()).collect();
    for id in ids {
        m.data.delete(id);
    }
    m.memories.get_mut(working).data_segments.clear();
    for (base, bytes) in image {
        let end = base + bytes.len() as u32;
        // the live runs within the segment, merged across short gaps
        let mut runs: Vec<Range<u32>> = Vec::new();
        for r in &live_data {
            let r = r.start.max(base)..r.end.min(end);
            if r.is_empty() {
                continue;
            }
            match runs.last_mut() {
                Some(last) if r.start <= last.end + GAP => last.end = last.end.max(r.end),
                _ => runs.push(r),
            }
        }
        let mut kept = vec![0; bytes.len()];
        for r in &live_data {
            let r = r.start.max(base)..r.end.min(end);
            if !r.is_empty() {
                let r = (r.start - base) as usize..(r.end - base) as usize;
                kept[r.clone()].copy_from_slice(&bytes[r]);
            }
        }
        for r in runs {
            let mut v = kept[(r.start - base) as usize..(r.end - base) as usize].to_vec();
            // (zeros at either end are as the memory is)
            let lead = v.iter().take_while(|&&b| b == 0).count();
            let trail = v[lead..].iter().rev().take_while(|&&b| b == 0).count();
            v.truncate(v.len() - trail);
            v.drain(..lead);
            if !v.is_empty() {
                let offset = ConstExpr::Value(Value::I32((r.start + lead as u32) as i32));
                let id = m.data.add(DataKind::Active { memory: working, offset }, v);
                m.memories.get_mut(working).data_segments.insert(id);
            }
        }
    }
    Ok(())
}
