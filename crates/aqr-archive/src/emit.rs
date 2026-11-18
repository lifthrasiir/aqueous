//! Formula blocks written ([`Emit`]): a formula's nodes as the instructions of the stack
//! machine, and the bound of their bytes ([`description_bound`]).

use super::*;

/// The end of an archive without a metadata block whose blocks take `blocks` bytes.
pub fn footer_bytes(blocks: usize) -> Vec<u8> {
    let mut out = Vec::new();
    footer(&mut out, blocks);
    out
}

/// [`Encoder::how`] of a stream of `class` and `kind` at `level`.
pub(crate) fn how_of(level: Level, class: &str, kind: Kind) -> How {
    let from = matches!(level, Level::Of(n) if n.clamp(1, Level::MAX) >= DENSE_FROM.get());
    let dense = from
        && DENSE_CLASSES.get().iter().any(|&c| match c {
            "text" => kind == Kind::Text,
            _ => match c.strip_suffix('/') {
                Some(_) => class.starts_with(c),
                None => class == c,
            },
        });
    match kind {
        Kind::Compressed => How::Light,
        _ if from && class == TOGETHER => How::Dense(TOGETHER_LEVEL.get()),
        _ if dense => How::Dense(DENSE_LEVEL.get()),
        _ => How::Usual,
    }
}

/// Instructions as nodes push their values, with the stack they leave: the outputs of
/// applications of several outputs not taken yet by (application, output), the rest `None`.
pub(crate) struct Emit<'a> {
    /// The index of each block of each stream in the formula block's list ([`listed`]).
    pub(crate) ordinals: &'a [Vec<Option<u64>>],
    /// The bytes of each stream whose slices are literals ([`literals`]), in pieces; `None` (or
    /// past the end) for those sliced.
    pub(crate) literals: &'a [Option<&'a [&'a [u8]]>],
    pub(crate) modules: &'a [Module],
    /// The classes of tasks of each module (by index; those past the end have none): a
    /// application declaring fewer task limits is written with [`aqr_formula::NO_LIMIT`] for the rest.
    pub(crate) classes: &'a [usize],
    pub(crate) resources: &'a [&'a Resource],
    pub(crate) out: &'a mut Vec<u8>,
    pub(crate) stack: Vec<Option<(usize, usize)>>,
}

impl Emit<'_> {
    /// The instructions pushing node `n`, its streams' blocks at `ordinals` and its modules and
    /// resources at their indices in `modules` and `resources`.
    pub(crate) fn node(&mut self, n: &Node) {
        match n {
            // (nothing of a block: an empty concatenation)
            Node::Slice { len: 0, .. } => {
                self.out.push(CONCAT);
                varint::put(self.out, 0);
                self.stack.push(None);
            }
            &Node::Slice { stream, start, len } if self.literals.get(stream).is_some_and(Option::is_some) => {
                let data = self.literals[stream].unwrap();
                self.out.push(LITERAL);
                varint::put(self.out, len);
                cut(data, start as usize, len as usize).into_iter().for_each(|p| self.out.extend_from_slice(p));
                self.stack.push(None);
            }
            &Node::Slice { stream, start, len } => {
                // (a slice of each block it is over, concatenated)
                let pieces = slice_blocks(start, len);
                let n = pieces.len();
                for (k, at, n) in pieces {
                    self.out.push(SLICE);
                    varint::put(self.out, self.ordinals[stream][k as usize].unwrap());
                    varint::put(self.out, at);
                    varint::put(self.out, n);
                    self.stack.push(None);
                }
                if n > 1 {
                    self.concat(CONCAT, n);
                }
            }
            Node::Concat(ns) => {
                // (before a part taking an output, the items so far put together as a counter
                // of base [`FOLD`] counts, so that the Swaps bringing the output up pass few
                // items of each concatenation open, and each byte is copied into few
                // concatenations: a concatenation of concatenations is the same)
                let mut levels: Vec<u32> = Vec::new();
                for n in ns {
                    if takes_output(n) {
                        self.fold(&mut levels);
                    }
                    self.node(n);
                    levels.push(0);
                }
                self.concat(CONCAT, levels.len());
            }
            Node::Pack(ns) => {
                ns.iter().for_each(|n| self.node(n));
                self.concat(PACK, ns.len());
            }
            Node::Apply(a) => {
                a.inputs.iter().for_each(|n| self.node(n));
                self.apply(a, &[a.len]);
            }
            &Node::Output { shared, index, .. } => {
                // up from where it is, the outputs not taken yet (all below the other items,
                // which are together at the top) going anywhere below the others: the other
                // items each a place down, the output nearest them to its place. The top swapped
                // with each of those items below it in turn, from the top down, then with that
                // output, then with the one taken.
                let at = self.stack.iter().rposition(|&o| o == Some((shared, index))).unwrap();
                let top = self.stack.len() - 1;
                if at < top {
                    let first = (at + 1..=top).find(|&p| self.stack[p].is_none());
                    debug_assert!(first.is_none_or(|f| self.stack[f..].iter().all(Option::is_none)));
                    let mut seq: Vec<usize> = first.map_or(Vec::new(), |f| (f..top).rev().collect());
                    if let Some(f) = first.filter(|&f| f > at + 1) {
                        seq.push(f - 1);
                    }
                    for p in seq.into_iter().chain([at]) {
                        self.out.push(SWAP);
                        varint::put(self.out, (top - p) as u64);
                        self.stack.swap(p, top);
                    }
                }
                debug_assert_eq!(self.stack[top], Some((shared, index)));
                self.stack[top] = None;
            }
            Node::ModuleBytes(m, d) => {
                self.out.push(MODULE_BYTES);
                varint::put(self.out, self.modules.iter().position(|n| n == m).unwrap() as u64);
                varint::put(self.out, d.len() as u64);
                self.stack.push(None);
            }
            Node::ResourceBytes(r) => {
                self.out.push(RESOURCE_BYTES);
                varint::put(self.out, self.resources.iter().position(|&s| s == r).unwrap() as u64);
                varint::put(self.out, n.len());
                self.stack.push(None);
            }
            Node::Patch(p) => {
                self.node(&p.source);
                self.node(&p.patch);
                self.out.push(PATCH);
                varint::put(self.out, p.len);
                self.stack.truncate(self.stack.len() - 2);
                self.stack.push(None);
            }
            Node::Either(_) => unreachable!("an alternative not resolved, refused by formula_data"),
        }
    }

    /// Puts the item at the top, the `k`-th output of a formula block of several, in its place
    /// `k`, below the outputs of applications not taken yet (where they are, the lowest of them
    /// brought up in its stead): so that taking one of them passes only the items of the output
    /// being made, not the outputs made before it (a formula block of a thousand files taking
    /// pieces of a Split, say, the Swaps would otherwise grow as their square).
    pub(crate) fn sink(&mut self, k: usize) {
        let top = self.stack.len() - 1;
        debug_assert!(self.stack[..k].iter().all(Option::is_none) && self.stack[top].is_none());
        if top > k {
            debug_assert!(self.stack[k].is_some(), "the outputs not taken yet right above those made");
            self.out.push(SWAP);
            varint::put(self.out, (top - k) as u64);
            self.stack.swap(k, top);
        }
    }

    /// Puts together the items of a concatenation at the top of the stack, of `levels` (each
    /// the number of times [`FOLD`] items were put together into it), [`FOLD`] of the same
    /// level at a time from the top.
    pub(crate) fn fold(&mut self, levels: &mut Vec<u32>) {
        while let Some(&top) = levels.last() {
            let n = levels.iter().rev().take_while(|&&l| l == top).count();
            if n < FOLD {
                break;
            }
            self.concat(CONCAT, n);
            levels.truncate(levels.len() - n);
            levels.push(top + 1);
        }
    }

    /// The Concat or Pack instruction `op` of the top `n` items.
    pub(crate) fn concat(&mut self, op: u8, n: usize) {
        self.out.push(op);
        varint::put(self.out, n as u64);
        self.stack.truncate(self.stack.len() - n);
        self.stack.push(None);
    }

    /// The instructions pushing the outputs of `s`, the `i`-th application of several outputs.
    pub(crate) fn shared(&mut self, i: usize, s: &aqr_formula::Shared) {
        s.inputs().iter().for_each(|n| self.node(n));
        if let aqr_formula::Of::Patch { len, .. } = s.of {
            self.out.push(PATCH);
            varint::put(self.out, len);
            self.stack.truncate(self.stack.len() - 2);
            self.stack.push(None);
        }
        if let Some(a) = s.apply().filter(|_| !s.split) {
            self.apply(a, &s.sizes);
        } else {
            if let Some(a) = s.apply() {
                self.apply(a, &[a.len]);
            }
            self.out.push(SPLIT);
            varint::put(self.out, s.sizes.len() as u64);
            s.sizes.iter().for_each(|&n| varint::put(self.out, n));
            self.stack.pop();
            self.stack.extend(s.sizes.iter().map(|_| None));
        }
        let k = self.stack.len() - s.sizes.len();
        (0..s.sizes.len()).for_each(|j| self.stack[k + j] = Some((i, j)));
    }

    /// The Apply instruction of `a` (its inputs pushed), of outputs of `sizes`.
    pub(crate) fn apply(&mut self, a: &aqr_formula::Apply, sizes: &[u64]) {
        let at = self.modules.iter().position(|&m| m == a.module).unwrap();
        let out = &mut *self.out;
        out.push(APPLY);
        varint::put(out, at as u64);
        varint::put(out, a.resources.len() as u64);
        for r in &a.resources {
            varint::put(out, self.resources.iter().position(|&s| s == r).unwrap() as u64);
        }
        varint::put(out, a.params.len() as u64);
        out.extend_from_slice(&a.params);
        varint::put(out, a.memory_limit);
        let own = self.classes.get(at).copied().unwrap_or(0);
        // (the bound counts the limits given and those of a module's classes up to its own; more
        // limits than the module's classes are a formula the decoder turns down)
        debug_assert!(own <= TASK_CLASSES, "{own} classes of tasks, past the description bound's");
        let classes = own.max(a.task_limits.len());
        varint::put(out, classes as u64);
        let limits = a.task_limits.iter().copied().chain(std::iter::repeat(aqr_formula::NO_LIMIT));
        limits.take(classes).for_each(|n| varint::put(out, n));
        varint::put(out, a.inputs.len() as u64);
        varint::put(out, sizes.len() as u64);
        sizes.iter().for_each(|&n| varint::put(out, n));
        self.stack.truncate(self.stack.len() - a.inputs.len());
        self.stack.extend(sizes.iter().map(|_| None));
    }
}

/// Items of a level of a concatenation put together at a time ([`Emit::fold`]).
pub(crate) const FOLD: usize = 8;

/// Whether node `n` takes an output of an application of several outputs.
pub(crate) fn takes_output(n: &Node) -> bool {
    match n {
        Node::Output { .. } => true,
        Node::Slice { .. } | Node::ModuleBytes(..) | Node::ResourceBytes(_) => false,
        Node::Concat(ns) | Node::Pack(ns) => ns.iter().any(takes_output),
        Node::Apply(a) => a.inputs.iter().any(takes_output),
        Node::Patch(p) => takes_output(&p.source) || takes_output(&p.patch),
        Node::Either(e) => takes_output(&e.kept) || takes_output(&e.made),
    }
}

/// The classes of tasks that [`description_bound`] allows an application's module, of task limits
/// not yet declared (each as large as a memory limit): no module has more (checked as
/// instructions are written).
pub(crate) const TASK_CLASSES: usize = 2;

/// Bytes of the instructions pushing node `n` at most, wherever the streams of its formula are
/// among others (up to 2^21 of them, and 2^14 modules and resources each): the bounds of
/// formulas put together ([`Formula::embed`]) add up, with the nodes around them. An output of
/// an application of several outputs ([`Node::Output`]) is counted as one Swap: the Swaps
/// bringing it up to the top of the stack depend on the formula around it.
pub fn description_bound(n: &Node) -> usize {
    match n {
        Node::Output { .. } => 1 + 3,
        Node::ModuleBytes(..) | Node::ResourceBytes(_) => 1 + 2 + varint::len(n.len()),
        Node::Patch(p) => description_bound(&p.source) + description_bound(&p.patch) + 1 + varint::len(p.len),
        // (whichever is chosen)
        Node::Either(e) => description_bound(&e.kept).max(description_bound(&e.made)),
        &Node::Slice { start, len, .. } => {
            let sliced = match slice_blocks(start, len)[..] {
                [(_, at, n)] => 1 + 3 + varint::len(at) + varint::len(n),
                ref pieces => {
                    let each = 1 + 3 + 2 * varint::len(BLOCK as u64);
                    pieces.len() * each + 1 + varint::len(pieces.len() as u64)
                }
            };
            // (or a literal: [`literals`])
            match len <= LITERAL_MAX.get() as u64 {
                true => sliced.max(1 + varint::len(len) + len as usize),
                false => sliced,
            }
        }
        Node::Concat(ns) | Node::Pack(ns) => {
            ns.iter().map(description_bound).sum::<usize>() + 1 + varint::len(ns.len() as u64)
        }
        Node::Apply(a) => {
            let inputs: usize = a.inputs.iter().map(description_bound).sum();
            inputs
                + 1
                + 2
                + varint::len(a.resources.len() as u64)
                + 2 * a.resources.len()
                + varint::len(a.params.len() as u64)
                + a.params.len()
                + varint::len(MEMORY_LIMIT)
                + varint::len(TASK_CLASSES as u64)
                + a.task_limits.iter().map(|&n| varint::len(n)).sum::<usize>()
                + varint::len(MEMORY_LIMIT) * TASK_CLASSES.saturating_sub(a.task_limits.len())
                + varint::len(a.inputs.len() as u64)
                + 1
                + varint::len(a.len)
        }
    }
}
