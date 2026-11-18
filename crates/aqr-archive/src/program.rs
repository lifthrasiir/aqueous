//! Formula blocks run: their programs read ([`Program`]), the applications admitted within the
//! memory of the decoder ([`Gate`]) and applied, the contents of leaves read.

use super::*;

/// What a formula block refers to.
pub(crate) struct Env<'a, 'n> {
    /// The ordinal of the first non-leaf block after the formula block.
    pub(crate) first: usize,
    pub(crate) modules: &'a [ModuleSrc],
    /// The data of the module and resource blocks as stored.
    pub(crate) module_data: &'a [&'a [u8]],
    pub(crate) resource_data: &'a [&'a [u8]],
    pub(crate) loaded: &'a [OnceLock<Decoded<Box<dyn Loaded + 'n>>>],
    pub(crate) resources: &'a [Resource],
    pub(crate) runner: &'n dyn Runner,
}

pub(crate) enum Insn<'a> {
    Nop,
    Literal(&'a [u8]),
    Swap(usize),
    Slice { block: usize, start: u64, len: u64 },
    Apply(Apply<'a>),
    Concat(usize),
    Pack(usize),
    Split(Vec<u64>),
    Patch(u64),
    Checksum(u64),
}

pub(crate) struct Apply<'a> {
    pub(crate) module: usize,
    pub(crate) resources: Vec<usize>,
    pub(crate) params: &'a [u8],
    pub(crate) memory_limit: u64,
    /// The task limits, one for each class of the module.
    pub(crate) task_limits: Vec<u64>,
    /// Whether it is of the opcode [`APPLY_OLD`], without task limits.
    old: bool,
    pub(crate) num_inputs: usize,
    pub(crate) output_sizes: Vec<u64>,
}

/// Reads formula data, checking what does not depend on the evaluation.
pub(crate) struct Reader<'a> {
    pub(crate) d: &'a [u8],
    pub(crate) at: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn num(&mut self) -> Decoded<u64> {
        leb(self.d, &mut self.at)
    }

    /// A number less than `bound`.
    pub(crate) fn index(&mut self, bound: usize) -> Decoded<usize> {
        let n = self.num()?;
        usize::try_from(n)
            .ok()
            .filter(|&n| n < bound)
            .ok_or_else(|| Failure::Invalid(format!("index {n} out of range")))
    }

    /// A count of items of at least a byte each.
    pub(crate) fn count(&mut self) -> Decoded<usize> {
        let n = self.num()?;
        usize::try_from(n)
            .ok()
            .filter(|&n| n <= self.d.len() - self.at)
            .ok_or_else(|| Failure::Invalid(format!("count {n} out of range")))
    }

    pub(crate) fn nums(&mut self) -> Decoded<Vec<u64>> {
        let n = self.count()?;
        (0..n).map(|_| self.num()).collect()
    }

    pub(crate) fn bytes(&mut self) -> Decoded<&'a [u8]> {
        let n = self.count()?;
        self.at += n;
        Ok(&self.d[self.at - n..self.at])
    }
}

/// A formula block read: its instructions, and the non-leaf blocks, resources and modules they
/// refer to (`blocks` by their ordinals among all, `resources` and `modules` among those of its
/// segment).
pub(crate) struct Program<'a> {
    pub(crate) insns: Vec<Insn<'a>>,
    pub(crate) blocks: Vec<usize>,
    pub(crate) resources: Vec<usize>,
    pub(crate) modules: Vec<usize>,
}

impl Program<'_> {
    /// Whether a Checksum instruction covers each output: the item on top of the stack as it
    /// runs, the same item as Swap instructions move it (spec, "Checksum instruction").
    pub(crate) fn outputs_checked(&self) -> bool {
        let mut st: Vec<bool> = Vec::new();
        let replace = |st: &mut Vec<bool>, n: usize, m: usize| {
            st.truncate(st.len() - n);
            st.resize(st.len() + m, false);
        };
        for insn in &self.insns {
            match insn {
                Insn::Nop => {}
                Insn::Literal(_) | Insn::Slice { .. } => st.push(false),
                &Insn::Swap(d) => {
                    let top = st.len() - 1;
                    st.swap(top, top - d);
                }
                Insn::Apply(a) => replace(&mut st, a.num_inputs, a.output_sizes.len()),
                &Insn::Concat(n) | &Insn::Pack(n) => replace(&mut st, n, 1),
                Insn::Split(sizes) => replace(&mut st, 1, sizes.len()),
                Insn::Patch(_) => replace(&mut st, 2, 1),
                Insn::Checksum(_) => *st.last_mut().expect("checked when read") = true,
            }
        }
        st.iter().all(|&c| c)
    }

    /// The most bytes on the stack at once (with the inputs and outputs of an instruction as it
    /// runs both, or an application's memory limit where larger: the working memory of its module,
    /// which a native one takes too), as the sizes the program tells: the weight of its leaf
    /// ([`Archive::set_budget`]); and the bytes on it at the end, its outputs. Of a module of
    /// `slots[m]` task slots (by module block of its segment), the limit counts the tasks' copies of the working
    /// memory as memories of their own, which a native one does not make and Wasm ones share
    /// (copying on write): an instance's share of it is weighed, as of the instance and as many
    /// tasks. Third, the bytes of the inputs and outputs of its applications of modules of no
    /// tasks: of the work that takes a thread, its longest.
    pub(crate) fn weight(&self, slots: &[u64]) -> (u64, u64, u64) {
        let (mut st, mut on, mut most, mut serial) = (Vec::<u64>::new(), 0u64, 0u64, 0u64);
        let pop = |st: &mut Vec<u64>, on: &mut u64, n: usize| -> u64 {
            let taken = st.drain(st.len().saturating_sub(n)..).fold(0u64, u64::saturating_add);
            *on = on.saturating_sub(taken);
            taken
        };
        for insn in &self.insns {
            let limit = match insn {
                // (the instance's share: of all of the application's where there are no task limits)
                Insn::Apply(a) if a.memory_limit != aqr_formula::NO_LIMIT && a.task_limits.is_empty() => {
                    a.memory_limit / (1 + slots[self.modules[a.module]])
                }
                Insn::Apply(a) if a.memory_limit != aqr_formula::NO_LIMIT => a.memory_limit,
                _ => 0,
            };
            let (ins, outs): (u64, Vec<u64>) = match insn {
                Insn::Literal(l) => (0, vec![l.len() as u64]),
                &Insn::Slice { len, .. } => (0, vec![len]),
                Insn::Apply(a) => (pop(&mut st, &mut on, a.num_inputs), a.output_sizes.clone()),
                &Insn::Concat(n) | &Insn::Pack(n) => {
                    let taken = pop(&mut st, &mut on, n);
                    (taken, vec![taken])
                }
                Insn::Split(sizes) => (pop(&mut st, &mut on, 1), sizes.clone()),
                &Insn::Patch(len) => (pop(&mut st, &mut on, 2), vec![len]),
                Insn::Nop | Insn::Swap(_) | Insn::Checksum(_) => continue,
            };
            let out = outs.iter().fold(0u64, |a, &b| a.saturating_add(b));
            if let Insn::Apply(a) = insn
                && slots[self.modules[a.module]] == 0
            {
                serial = serial.saturating_add(ins).saturating_add(out);
            }
            most = most.max(on.saturating_add(ins.saturating_add(out).max(limit)));
            on = on.saturating_add(out);
            st.extend(outs);
        }
        (most, on, serial)
    }
}

/// The size of the data of a stored leaf block `d` (where it is Zstandard data, by its header;
/// `d`'s own where that is not read).
pub(crate) fn stored_len(d: &[u8], zstd: bool, checked: bool) -> u64 {
    let raw = d.len().saturating_sub(if checked { 8 } else { 0 }) as u64;
    if !zstd {
        return raw;
    }
    let mut at = 0;
    let Ok(delta) = leb(d, &mut at) else { return raw };
    let shrink = if delta == 0 { leb(d, &mut at).unwrap_or(0) } else { 0 };
    raw.saturating_sub(at as u64 + 1).saturating_add(delta).saturating_sub(shrink)
}

aqr_common::zopt! {
    /// The bytes of work on a thread from which a leaf is evaluated first ([`Program::weight`]).
    pub(crate) static LONG_SERIAL: Opt<u64> = Opt::value("eval:long-serial", 64 << 20, "leaves with this much work on a thread are evaluated first");
}

/// The leaves let in by [`Archive::set_budget`] ([`aqr_common::pool::dispatch_any`]): in an
/// order (of the leaves' indices), each once there is room in the budget for it (or none of
/// this evaluation is in); but while the first not let in waits, the leaves after it that fit
/// go before it, up to the limit's bytes of weights (after which it is let in as soon as it
/// fits, or alone), and past `eval:passing` leaves that do not at most.
pub(crate) struct Gate<'w> {
    budget: Option<&'w Budget>,
    weights: &'w [u64],
    order: Vec<usize>,
    /// (locked within the budget's lock, never the other way)
    state: Mutex<GateState>,
    /// The leaves in (changed within the budget's lock, or as a leaf ends, which then tells the
    /// budget).
    on: AtomicUsize,
}

aqr_common::zopt! {
    /// How many leaves that do not fit in the leaves going before the first waiting pass at most
    /// ([`Gate`]).
    pub(crate) static PASSING: Opt<usize> = Opt::value("eval:passing", 64, "most leaves that do not fit that the leaves passing a waiting one look past");
}

struct GateState {
    /// The leaves let in, and the first not (by their places in the order, as below).
    taken: Vec<bool>,
    first: usize,
    /// While it waits: the leaves after it looked at and not let in, the first not looked at,
    /// and the weights of those let in.
    skipped: Vec<usize>,
    scan: usize,
    passed: u64,
}

impl GateState {
    /// A leaf after the first that may go before it, of those that `fits`.
    pub(crate) fn passing(&mut self, fits: impl Fn(usize) -> bool) -> Option<usize> {
        let (first, taken) = (self.first, &self.taken);
        self.skipped.retain(|&i| !taken[i] && i != first);
        if let Some(k) = self.skipped.iter().position(|&i| fits(i)) {
            return Some(self.skipped.remove(k));
        }
        self.scan = self.scan.max(first + 1);
        let passing = PASSING.get();
        while self.skipped.len() < passing && self.scan < taken.len() {
            let i = self.scan;
            self.scan += 1;
            if fits(i) {
                return Some(i);
            }
            self.skipped.push(i);
        }
        None
    }
}

/// A leaf let in, until dropped, and what it takes of the budget.
pub(crate) struct Admitted<'g>(&'g Gate<'g>, Option<Taken<'g>>);

impl<'w> Gate<'w> {
    pub(crate) fn new(budget: Option<&'w Budget>, weights: &'w [u64], order: Vec<usize>) -> Self {
        let taken = vec![false; weights.len()];
        let state = GateState { taken, first: 0, skipped: Vec::new(), scan: 0, passed: 0 };
        Gate { budget, weights, order, state: Mutex::new(state), on: AtomicUsize::new(0) }
    }

    /// Waits for a leaf to be let in, and gives its index: the first not let in yet if it fits,
    /// alone (of this evaluation) or beside what the budget holds, else one after it that does as
    /// it may pass.
    pub(crate) fn next(&self) -> (usize, Admitted<'_>) {
        let take = |s: &mut GateState| {
            while s.taken.get(s.first) == Some(&true) {
                s.first += 1;
            }
        };
        let Some(budget) = self.budget else {
            let mut s = self.state.lock().unwrap();
            take(&mut s);
            let first = s.first;
            s.taken[first] = true;
            self.on.fetch_add(1, Relaxed);
            return (self.order[first], Admitted(self, None));
        };
        let (i, t) = budget.wait(|room| {
            let mut s = self.state.lock().unwrap();
            take(&mut s);
            let first = s.first;
            let alone = self.on.load(Relaxed) == 0;
            let fits = |i: usize| alone || room.fits(self.weights[self.order[i]]);
            let i = if fits(first) {
                s.passed = 0;
                first
            } else if s.passed < room.limit {
                s.passing(fits)?
            } else {
                return None;
            };
            let w = self.weights[self.order[i]];
            s.taken[i] = true;
            if i != first {
                s.passed = s.passed.saturating_add(w);
            }
            self.on.fetch_add(1, Relaxed);
            Some((self.order[i], w))
        });
        (i, Admitted(self, Some(t)))
    }
}

impl Drop for Admitted<'_> {
    fn drop(&mut self) {
        // (before the budget is given back, which tells those waiting)
        self.0.on.fetch_sub(1, Relaxed);
        drop(self.1.take());
    }
}

/// Formula data `d` of `outputs` outputs read, among `non_leaf` non-leaf blocks, marking those it
/// refers to in `used`: everything checked that does not depend on the evaluation.
pub(crate) fn read<'a>(
    d: &'a [u8],
    outputs: u64,
    env: &Env<'a, '_>,
    non_leaf: usize,
    used: &mut [bool],
) -> Decoded<Program<'a>> {
    let mut r = Reader { d, at: 0 };
    // the non-leaf blocks referenced, by their ordinals among all
    let (mut blocks, mut next) = (Vec::new(), env.first);
    for delta in r.nums()? {
        let o = usize::try_from(delta)
            .ok()
            .and_then(|d| next.checked_add(d))
            .filter(|&o| o < non_leaf)
            .ok_or_else(|| Failure::Invalid("a referenced block that does not exist".into()))?;
        blocks.push(o);
        next = o + 1;
    }
    let n = r.count()?;
    let resources = (0..n).map(|_| r.index(env.resources.len())).collect::<Decoded<Vec<_>>>()?;
    let n = r.count()?;
    let modules = (0..n).map(|_| r.index(env.modules.len())).collect::<Decoded<Vec<_>>>()?;

    // a pass checking the stack and the lists
    let (mut insns, mut depth) = (Vec::new(), 0usize);
    let (mut b_used, mut r_used, mut m_used) =
        (vec![false; blocks.len()], vec![false; resources.len()], vec![false; modules.len()]);
    let pop = |depth: &mut usize, n: usize| match depth.checked_sub(n) {
        Some(d) => {
            *depth = d;
            Ok(())
        }
        None => invalid("stack underflow"),
    };
    while r.at < d.len() {
        let op = d[r.at];
        r.at += 1;
        let insn = match op {
            NOP => Insn::Nop,
            LITERAL => Insn::Literal(r.bytes()?),
            // the data of a block as stored, pushed as a literal is; the block must be a module
            // block in full, not a shorthand block (spec, "`module_bytes`")
            MODULE_BYTES => {
                let module = r.index(modules.len())?;
                m_used[module] = true;
                if matches!(env.modules[modules[module]], ModuleSrc::Registered(..)) {
                    return invalid("a ModuleBytes instruction of a shorthand module block");
                }
                stored_bytes(env.module_data[modules[module]], r.num()?)?
            }
            RESOURCE_BYTES => {
                let resource = r.index(resources.len())?;
                r_used[resource] = true;
                stored_bytes(env.resource_data[resources[resource]], r.num()?)?
            }
            SWAP => Insn::Swap(r.index(depth)?),
            SLICE => {
                let block = r.index(blocks.len())?;
                b_used[block] = true;
                Insn::Slice { block, start: r.num()?, len: r.num()? }
            }
            APPLY | APPLY_OLD => {
                let module = r.index(modules.len())?;
                m_used[module] = true;
                let n = r.count()?;
                let res = (0..n).map(|_| r.index(resources.len())).collect::<Decoded<Vec<_>>>()?;
                res.iter().for_each(|&i| r_used[i] = true);
                let params = r.bytes()?;
                let mut memory_limit = r.num()?;
                let old = op == APPLY_OLD;
                let mut task_limits = if old { Vec::new() } else { r.nums()? };
                if !DECLARED_LIMIT.get() {
                    memory_limit = aqr_formula::NO_LIMIT;
                    task_limits.iter_mut().for_each(|l| *l = aqr_formula::NO_LIMIT);
                }
                let num_inputs = usize::try_from(r.num()?).unwrap_or(usize::MAX);
                let output_sizes = r.nums()?;
                if output_sizes.is_empty() {
                    return invalid("an application without outputs");
                }
                Insn::Apply(Apply {
                    module,
                    resources: res,
                    params,
                    memory_limit,
                    task_limits,
                    old,
                    num_inputs,
                    output_sizes,
                })
            }
            CONCAT => Insn::Concat(r.index(depth + 1)?),
            PACK => Insn::Pack(r.index(depth + 1)?),
            SPLIT => {
                let sizes = r.nums()?;
                if sizes.is_empty() {
                    return invalid("a split into no items");
                }
                Insn::Split(sizes)
            }
            PATCH => Insn::Patch(r.num()?),
            CHECKSUM => {
                let v = d.get(r.at..r.at + 8).ok_or_else(|| Failure::Invalid("truncated checksum".into()))?;
                r.at += 8;
                Insn::Checksum(u64::from_le_bytes(v.try_into().unwrap()))
            }
            _ => return invalid(format!("reserved opcode {op:#04x}")),
        };
        match &insn {
            Insn::Nop | Insn::Swap(_) => {}
            Insn::Literal(_) | Insn::Slice { .. } => depth += 1,
            Insn::Apply(a) => {
                pop(&mut depth, a.num_inputs)?;
                depth += a.output_sizes.len();
            }
            &Insn::Concat(n) | &Insn::Pack(n) => {
                pop(&mut depth, n)?;
                depth += 1;
            }
            Insn::Split(sizes) => {
                pop(&mut depth, 1)?;
                depth += sizes.len();
            }
            Insn::Patch(_) => {
                pop(&mut depth, 2)?;
                depth += 1;
            }
            Insn::Checksum(_) if depth == 0 => return invalid("a checksum of an empty stack"),
            Insn::Checksum(_) => {}
        }
        insns.push(insn);
    }
    if [b_used, r_used, m_used].iter().any(|u| u.contains(&false)) {
        return invalid("a block listed but not used");
    }
    if depth as u64 != outputs {
        return invalid(format!("{depth} outputs, not {outputs}"));
    }
    blocks.iter().for_each(|&o| used[o] = true);
    Ok(Program { insns, blocks, resources, modules })
}

/// A ModuleBytes or ResourceBytes instruction pushing `data`, the data of a block as stored,
/// which its operand says is of `size` bytes.
fn stored_bytes(data: &[u8], size: u64) -> Decoded<Insn<'_>> {
    if data.len() as u64 != size {
        return invalid(format!("a block of {} bytes, not {size}", data.len()));
    }
    Ok(Insn::Literal(data))
}

/// The non-leaf blocks as formula blocks are evaluated (several at once), each with the slices
/// of it yet to be evaluated or on a stack: once there are none, it is dropped.
pub(crate) struct Contents<'c, 't> {
    /// Each told of its block decoded ahead ([`Contents::ahead`]).
    pub(crate) data: Vec<Waited<Slot<'c>>>,
    pub(crate) dictionaries: &'c [dict::Dictionary],
    pub(crate) refs: Vec<AtomicUsize>,
    /// Told of the blocks as they are decoded.
    pub(crate) tally: &'t Tally<'t>,
    /// Whether the pages of a block are let go of once it is read ([`Archive::let_go_read`]).
    pub(crate) let_go: bool,
    /// The payload decrypted as the blocks are read, and let go of once read ([`open_decrypting`]).
    pub(crate) sealed: Option<&'c envelope::Decrypting<'c>>,
}

/// A value under a lock with a condition variable of its own: one condition variable waited on
/// with several mutexes is an error to POSIX (and panics in std on macOS, which checks it).
pub(crate) struct Waited<T> {
    value: Mutex<T>,
    changed: std::sync::Condvar,
}

impl<T> Waited<T> {
    pub(crate) fn new(v: T) -> Self {
        Waited { value: Mutex::new(v), changed: std::sync::Condvar::new() }
    }

    pub(crate) fn lock(&self) -> std::sync::LockResult<std::sync::MutexGuard<'_, T>> {
        self.value.lock()
    }

    pub(crate) fn wait<'g>(
        &self,
        g: std::sync::MutexGuard<'g, T>,
    ) -> std::sync::LockResult<std::sync::MutexGuard<'g, T>> {
        self.changed.wait(g)
    }

    pub(crate) fn notify_all(&self) {
        self.changed.notify_all();
    }
}

/// How far the leaves' evaluation is, told as it goes where it is to be ([`Archive::on_progress`]).
pub(crate) struct Tally<'t> {
    pub(crate) tell: Option<&'t (dyn Fn(Step) + Send + Sync)>,
    pub(crate) decompressed: AtomicU64,
    pub(crate) evaluated: AtomicU64,
    /// Of all the data blocks and of all the outputs.
    pub(crate) totals: (u64, u64),
}

impl Tally<'_> {
    /// So many more bytes of the data blocks decompressed and of the outputs evaluated, told.
    pub(crate) fn add(&self, decompressed: u64, evaluated: u64) {
        let Some(tell) = self.tell else { return };
        let d = self.decompressed.fetch_add(decompressed, Ordering::Relaxed) + decompressed;
        let e = self.evaluated.fetch_add(evaluated, Ordering::Relaxed) + evaluated;
        tell(Step::Reached(Reached { decompressed: (d, self.totals.0), evaluated: (e, self.totals.1) }));
    }

    pub(crate) fn step(&self, s: Step) {
        if let Some(tell) = self.tell {
            tell(s);
        }
    }
}

/// A non-leaf block: its data as it is in the archive (and whether it is Zstandard data) until
/// first sliced (or being decoded ahead), then decoded until no slice of it is left.
pub(crate) enum Slot<'c> {
    Raw(&'c [u8], bool, Option<usize>),
    Ahead,
    Decoded(Arc<Vec<u8>>),
    Gone,
}

impl<'c> Contents<'c, '_> {
    /// The data of block `o`, while any slice of it is to be evaluated: decoded the first time.
    pub(crate) fn block(&self, o: usize) -> Decoded<Arc<Vec<u8>>> {
        let mut s = self.data[o].lock().unwrap();
        while let Slot::Ahead = *s {
            s = self.data[o].wait(s).unwrap();
        }
        if let Slot::Raw(d, zstd, dict) = *s {
            self.ensure(d)?;
            let decoded = decoded_with(d, zstd, true, dict.map(|d| &self.dictionaries[d]));
            self.tally.add(d.len() as u64, 0);
            *s = Slot::Decoded(Arc::new(decoded?));
            self.read(d);
        }
        match &*s {
            Slot::Decoded(d) => Ok(d.clone()),
            _ => unreachable!("a block with slices left"),
        }
    }

    /// The data of block `o` as it is stored in the archive, where it is not Zstandard data and
    /// is not decoded yet: checked, and let go of as read, not copied. None for any other.
    pub(crate) fn stored(&self, o: usize) -> Option<Decoded<&'c [u8]>> {
        let Slot::Raw(d, false, _) = *self.data[o].lock().unwrap() else { return None };
        let d = self.ensure(d).and_then(|()| stored_checked(d));
        if let Ok(d) = d {
            self.tally.add(d.len() as u64, 0);
        }
        Some(d)
    }

    /// Decodes block `o` ahead of its first slice, where it is not yet (and does not fail: one
    /// that does is left to be decoded, and fail, as it is sliced).
    pub(crate) fn ahead(&self, o: usize) {
        let mut s = self.data[o].lock().unwrap();
        let Slot::Raw(d, zstd, dict) = *s else { return };
        *s = Slot::Ahead;
        drop(s);
        let decoded = self.ensure(d).and_then(|()| decoded_with(d, zstd, true, dict.map(|d| &self.dictionaries[d])));
        let mut s = self.data[o].lock().unwrap();
        *s = match decoded {
            Ok(b) => {
                self.tally.add(d.len() as u64, 0);
                self.read(d);
                Slot::Decoded(Arc::new(b))
            }
            Err(_) => Slot::Raw(d, zstd, dict),
        };
        drop(s);
        self.data[o].notify_all();
    }

    /// Block `d` of the archive read (decoded), not to be read again: its pages let go of where
    /// they are to be ([`Archive::let_go_read`]), or its chunks decrypted ([`open_decrypting`]).
    pub(crate) fn read(&self, d: &[u8]) {
        if self.let_go {
            // SAFETY: of the archive, a file mapped read only (as `let_go_read` was told)
            unsafe { aqr_common::pages::let_go(d.as_ptr(), d.len()) };
        }
        if let Some(p) = self.sealed
            && let Some(r) = p.range_of(d)
        {
            // SAFETY: the block read, its bytes not used again (got again where it is read again)
            unsafe { p.release(r) };
        }
    }

    /// Block `d` of the archive to be read: its chunks decrypted where the payload is decrypted
    /// as it is read ([`open_decrypting`]); invalid where they do not authenticate.
    pub(crate) fn ensure(&self, d: &[u8]) -> Decoded<()> {
        match self.sealed.zip(self.sealed.and_then(|p| p.range_of(d))) {
            Some((p, r)) => p.get(r).map(|_| ()),
            None => Ok(()),
        }
    }

    pub(crate) fn release(&self, o: usize) {
        if self.refs[o].fetch_sub(1, Ordering::Relaxed) == 1 {
            *self.data[o].lock().unwrap() = Slot::Gone;
        }
    }
}

/// What takes the bytes of a file as it is decoded ([`Archive::write_file`]).
pub type Sink<'a> = dyn FnMut(&[u8]) -> Decoded<()> + Send + 'a;

/// What takes the bytes of an output as it is evaluated ([`eval::run`]): a [`Sink`], not needing to
/// be sent.
pub(crate) type Taker<'a> = dyn FnMut(&[u8]) -> Decoded<()> + 'a;

/// Whether Wasm module `w` is of a release (SHA-512/256 in [`crate::released::RELEASED`]), the
/// only modules that the reserved Apply instruction ([`APPLY_OLD`]) is valid for.
fn released(w: &[u8]) -> bool {
    use sha2::Digest;
    let sum = sha2::Sha512_256::digest(w);
    let hex: String = sum.iter().map(|b| format!("{b:02x}")).collect();
    crate::released::RELEASED.contains(&hex.as_str()) || cfg!(test) && tests_released(&hex)
}

/// Whether a Wasm module of SHA-512/256 `hash` is of a release ([`crate::released::RELEASED`]):
/// the modules the reserved Apply instruction is valid for, which a host runs by the rules
/// before the current ones (an `aqr_spawn` of two parameters, no task limits).
pub fn is_released(hash: &[u8; 32]) -> bool {
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    crate::released::RELEASED.contains(&hex.as_str())
}

#[cfg(test)]
pub(crate) static TEST_RELEASED: Mutex<Vec<String>> = Mutex::new(Vec::new());

#[cfg(test)]
fn tests_released(hex: &str) -> bool {
    TEST_RELEASED.lock().unwrap().iter().any(|h| h == hex)
}

#[cfg(not(test))]
fn tests_released(_: &str) -> bool {
    false
}

aqr_common::zopt! {
    /// Whether the decoder holds applications to the memory limits their instructions declare
    /// (`no-declared-limit`: read as no limit, [`aqr_formula::NO_LIMIT`], the instance's and each task
    /// class's alike), so that an archive declaring too little decodes still: for development.
    static DECLARED_LIMIT: Opt<bool> =
        Opt::flag("declared-limit", true, "hold applications to their declared memory limits (no-declared-limit: none)");
}

/// Fails (a resource failure) where the minimal bound of application `a` of module block
/// `module` is over the limit of the process ([`aqr_formula::set_application_limit`], 512 MiB by
/// default), before its module is applied. Not for the earlier Apply, whose limit was an estimate.
pub(crate) fn within_limit(a: &Apply, module: usize) -> Decoded<()> {
    if a.old {
        return Ok(());
    }
    let (needed, limit) =
        (aqr_formula::minimal_bound(a.memory_limit, &a.task_limits), aqr_formula::application_limit());
    if needed > limit {
        let mib = needed.div_ceil(1 << 20);
        return Err(Failure::Resource(format!(
            "module block {module}: an application needs at least {needed} bytes of memory ({mib} MiB) \
             but is limited to {limit}; allow it with --application-bound={mib}M"
        )));
    }
    Ok(())
}

/// The outputs of application `a` of module block `module`.
pub(crate) fn apply(
    env: &Env,
    module: usize,
    a: &Apply,
    inputs: &[&[u8]],
    resources: &[Resource],
) -> Decoded<Vec<Vec<u8>>> {
    let wasm = env.modules[module].wasm(env.runner)?;
    let loaded = env.loaded[module].get_or_init(|| env.runner.load(wasm));
    let loaded = loaded.as_ref().map_err(Failure::clone)?;
    let classes = task_classes(wasm);
    if a.old {
        // (the memory limit was an estimate of all of the application's memories: not enforced)
        if !released(wasm) {
            return invalid(format!("module block {module}: the reserved Apply instruction of a module not released"));
        }
        let outputs = loaded.apply(a.params, resources, inputs, &a.output_sizes, aqr_formula::NO_LIMIT, &[])?;
        return checked_outputs(outputs, a, module);
    }
    if a.task_limits.len() != classes {
        return invalid(format!(
            "module block {module}: {} task limits for {classes} classes of tasks",
            a.task_limits.len()
        ));
    }
    within_limit(a, module)?;
    let outputs = loaded.apply(a.params, resources, inputs, &a.output_sizes, a.memory_limit, &a.task_limits)?;
    checked_outputs(outputs, a, module)
}

/// `outputs` of application `a` of module block `module`, if they are of the sizes declared.
fn checked_outputs(outputs: Vec<Vec<u8>>, a: &Apply, module: usize) -> Decoded<Vec<Vec<u8>>> {
    if outputs.len() != a.output_sizes.len() || outputs.iter().zip(&a.output_sizes).any(|(o, &n)| o.len() as u64 != n) {
        return invalid(format!("module block {module}: outputs of other sizes than declared"));
    }
    Ok(outputs)
}
