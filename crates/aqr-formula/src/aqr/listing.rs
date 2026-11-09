//! What a listing of an archive tells of each file's content ([`Archive::listing`]): the bytes
//! of the blocks it takes, of data and of the rest, its share of them (of blocks taken by
//! several, a part), and the formula making it, summarized.
//!
//! A formula block is a program of a stack machine: read again as a graph of the instructions,
//! each item of the stack a node (an application or a split of several outputs a node for each,
//! of the one instruction), the content of output `o` is what the item `o` from the bottom at
//! the end is made of, apart from the other outputs of the leaf.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::Write;
use std::sync::OnceLock;

use super::{Archive, Blocks, Decoded, Insn, LeafBlock, NoModules, decoded_with, invalid, read, stored_len};

/// Of a content of an archive ([`Archive::listing`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    /// Its bytes.
    pub size: u64,
    /// The bytes of the data blocks it is made of: its leaf data block, or the non-leaf blocks
    /// its formula slices for it.
    pub data: u64,
    /// The bytes of the other blocks it takes: its formula block, and the module, resource and
    /// dictionary blocks its formula and data blocks take.
    pub overhead: u64,
    /// Its share of the blocks of `data` and `overhead`: a block taken by several contents shared
    /// among them by their sizes, so that contents taking the same blocks have the same ratio of
    /// share to size, and the shares of a block add up to it.
    pub share: u64,
    /// The formula making it, summarized: modules by their names, `lit` (a literal), `slice`,
    /// `cat`, `pack`, `split` and `patch`; `#k` the `k`-th output (from 1) of several; `NAME xN`
    /// `N` alike in a row; `*` what is left out at depth, `...` in breadth. A leaf data block
    /// is `stored` (or `zstd`, of Zstandard data).
    pub formula: String,
    /// The instructions of its formula making it (none for a leaf data block).
    pub nodes: usize,
    /// The segment (from 0) of its leaf block (of the first, of a file of several).
    pub segment: usize,
}

/// An application of a formula ([`Graph::applications`]).
#[derive(Debug, Clone)]
pub struct Applied {
    /// Its module block (by index among all).
    pub module: usize,
    /// The memory limit of its instance ([`crate::NO_LIMIT`]: none declared) and the task limits.
    pub instance: u64,
    pub tasks: Vec<u64>,
    /// The bytes of its outputs.
    pub output: u64,
    /// The applications (by index in [`Graph::applications`]'s list) whose outputs are directly its inputs.
    pub from: Vec<usize>,
}

impl Applied {
    /// The least memory it needs: its instance and a unit of each class (none where undeclared).
    pub fn minimal(&self) -> Option<u64> {
        (self.instance != crate::NO_LIMIT).then(|| self.tasks.iter().fold(self.instance, |a, &t| a.saturating_add(t)))
    }

    /// The most memory it needs: its instance and every slot of each class (`slots`) a unit.
    pub fn full(&self, slots: &[u64]) -> Option<u64> {
        (self.instance != crate::NO_LIMIT).then(|| {
            let units = self.tasks.iter().zip(slots).map(|(&t, &n)| t.saturating_mul(n));
            units.fold(self.instance, u64::saturating_add)
        })
    }
}

/// The most characters of a summary, but for its first node.
const SUMMARY: usize = 48;
/// The most groups of alike children shown of a node.
const BREADTH: usize = 4;
/// The most characters kept of a node rendered for a summary (past them `...`), as one far
/// longer than [`SUMMARY`] is shown of no node.
const RENDERED: usize = 4 * SUMMARY;

/// The most bytes of a literal or parameters kept to be shown ([`Graph::dump`]).
const SHOWN: usize = 32;

#[derive(Clone)]
enum Shape {
    /// Its first bytes ([`SHOWN`]).
    Literal(Vec<u8>),
    /// Of the non-leaf block (by its ordinal among all), from where.
    Slice(usize, u64),
    /// Of the module block (by index), its resources (by index) and the output taken of how many;
    /// its parameters (the first [`SHOWN`] bytes, and how many) and memory limit.
    Apply {
        module: usize,
        resources: Vec<usize>,
        out: usize,
        outs: usize,
        params: Vec<u8>,
        params_len: usize,
        memory_limit: u64,
        task_limits: Vec<u64>,
    },
    Concat,
    Pack,
    Split {
        out: usize,
        outs: usize,
    },
    Patch,
    /// A leaf data block, Zstandard data or not, and checked or not.
    Stored(bool, bool),
}

struct Node {
    shape: Shape,
    kids: Vec<usize>,
    /// The instruction it is of (by its index among all of the graph's).
    insn: usize,
    /// The bytes of its output.
    size: u64,
    /// The checksum a Checksum instruction gives it.
    checksum: Option<u64>,
}

/// The graph of one or more formulas, and module names given.
pub(super) struct Graph<'f> {
    nodes: Vec<Node>,
    insns: usize,
    name: &'f dyn Fn(usize) -> String,
    /// By instruction, the first of the graph's of its leaf.
    first_insn: Vec<usize>,
}

impl Graph<'_> {
    fn add(&mut self, shape: Shape, kids: Vec<usize>, insn: usize, size: u64) -> usize {
        self.nodes.push(Node { shape, kids, insn, size, checksum: None });
        self.nodes.len() - 1
    }

    /// The nodes made from node `n` (itself too), each once.
    fn reach(&self, n: usize) -> Vec<usize> {
        let (mut seen, mut todo, mut out) = (vec![false; self.nodes.len()], vec![n], Vec::new());
        while let Some(k) = todo.pop() {
            if !std::mem::replace(&mut seen[k], true) {
                out.push(k);
                todo.extend(&self.nodes[k].kids);
            }
        }
        out
    }

    fn label(&self, n: usize) -> String {
        let nth = |s: String, out: usize, outs: usize| if outs > 1 { format!("{s}#{}", out + 1) } else { s };
        match &self.nodes[n].shape {
            Shape::Literal(_) => "lit".into(),
            Shape::Slice(..) => "slice".into(),
            Shape::Apply { module, out, outs, .. } => nth((self.name)(*module), *out, *outs),
            Shape::Concat => "cat".into(),
            Shape::Pack => "pack".into(),
            Shape::Split { out, outs } => nth("split".into(), *out, *outs),
            Shape::Patch => "patch".into(),
            Shape::Stored(false, _) => "stored".into(),
            Shape::Stored(true, _) => "zstd".into(),
        }
    }

    /// Node `n` in full, a line of each node under it indented `indent` (two spaces a level
    /// down): its label, bytes and what it is of. The inputs of an instruction of several outputs
    /// are shown once, where it is first (`seen`, by instruction); `non_leaf_at` tells where
    /// each non-leaf block is (by its ordinal among all).
    pub(super) fn dump(
        &self,
        n: usize,
        indent: usize,
        non_leaf_at: &[usize],
        seen: &mut HashSet<usize>,
        out: &mut String,
    ) {
        let node = &self.nodes[n];
        let hex = |b: &[u8], len: usize| {
            let mut s: String = b.iter().map(|x| format!("{x:02x}")).collect();
            if len > b.len() {
                s += "...";
            }
            s
        };
        let _ = write!(out, "{:indent$}{}  {} B", "", self.label(n), node.size);
        let several = match &node.shape {
            Shape::Apply { outs, .. } | Shape::Split { outs, .. } => *outs > 1,
            _ => false,
        };
        // (the instruction, within its leaf from 1, of several outputs)
        if several {
            let _ = write!(out, "  @{}", node.insn - self.first_insn[node.insn] + 1);
        }
        match &node.shape {
            Shape::Literal(b) if node.size > 0 => _ = write!(out, "  {}", hex(b, node.size as usize)),
            Shape::Literal(_) => {}
            &Shape::Slice(b, start) => {
                let _ = write!(out, "  of non-leaf block #{} at {}, from {start}", b + 1, non_leaf_at[b]);
            }
            Shape::Apply { module, resources, params, params_len, memory_limit, task_limits, .. } => {
                let _ = write!(out, "  module #{}", module + 1);
                if !resources.is_empty() {
                    let r: Vec<String> = resources.iter().map(|r| format!("#{}", r + 1)).collect();
                    let _ = write!(out, ", resource {}", r.join(" "));
                }
                if *params_len > 0 {
                    let _ = write!(out, ", params {params_len} B {}", hex(params, *params_len));
                }
                if *memory_limit != crate::NO_LIMIT {
                    let _ = write!(out, ", memory limit {memory_limit} B");
                    if !task_limits.is_empty() {
                        let t: Vec<String> = task_limits.iter().map(|t| format!("{t}")).collect();
                        let _ = write!(out, ", task limits {} B", t.join(" "));
                    }
                }
            }
            Shape::Stored(_, true) => out.push_str("  checked"),
            _ => {}
        }
        if let Some(c) = node.checksum {
            let _ = write!(out, "  checksum {c:016x}");
        }
        if several && !seen.insert(node.insn) {
            out.push_str("  (inputs as above)\n");
            return;
        }
        out.push('\n');
        for &k in &node.kids {
            self.dump(k, indent + 2, non_leaf_at, seen, out);
        }
    }

    /// Each application (an instruction of several outputs once): its module block, memory limit and
    /// task limits, and the bytes of its outputs.
    pub(super) fn applications(&self) -> Vec<Applied> {
        let mut out: Vec<Applied> = Vec::new();
        let mut index: std::collections::HashMap<usize, usize> = Default::default();
        for n in &self.nodes {
            let Shape::Apply { module, memory_limit, task_limits, .. } = &n.shape else { continue };
            match index.get(&n.insn) {
                Some(&k) => out[k].output += n.size,
                None => {
                    // (the inputs are nodes made before it)
                    let mut from: Vec<usize> =
                        n.kids.iter().filter_map(|&k| index.get(&self.nodes[k].insn).copied()).collect();
                    from.sort_unstable();
                    from.dedup();
                    index.insert(n.insn, out.len());
                    let (instance, tasks) = (*memory_limit, task_limits.clone());
                    out.push(Applied { module: *module, instance, tasks, output: n.size, from });
                }
            }
        }
        out
    }

    /// Node `n` summarized down to `depth` levels below it (`*` past them), each node rendered once
    /// at each depth (`memo`: nodes are taken by several, an image's Split by thousands), and at
    /// most [`RENDERED`] characters of each kept.
    fn render(&self, n: usize, depth: usize, memo: &mut HashMap<(usize, usize), String>) -> String {
        if let Some(s) = memo.get(&(n, depth)) {
            return s.clone();
        }
        let label = self.label(n);
        let kids = &self.nodes[n].kids;
        let s = if kids.is_empty() {
            label
        } else if depth == 0 {
            format!("{label}(*)")
        } else {
            let mut groups: Vec<(String, usize)> = Vec::new();
            for &k in kids {
                let s = self.render(k, depth - 1, memo);
                match groups.last_mut() {
                    Some((last, count)) if *last == s => *count += 1,
                    _ => groups.push((s, 1)),
                }
            }
            let mut parts: Vec<String> = (groups.iter().take(BREADTH))
                .map(|(s, c)| if *c > 1 { format!("{s} x{c}") } else { s.clone() })
                .collect();
            if groups.len() > BREADTH {
                parts.push("...".into());
            }
            format!("{label}({})", parts.join(", "))
        };
        let s = match s.char_indices().nth(RENDERED) {
            Some((at, _)) => format!("{}...", &s[..at]),
            None => s,
        };
        memo.insert((n, depth), s.clone());
        s
    }

    /// Node `n` summarized as deep as fits in [`SUMMARY`] characters past its label (one level
    /// deeper at a time, until it does not or nothing is left out).
    fn summary(&self, n: usize) -> String {
        let label = self.label(n).chars().count();
        let mut memo = HashMap::new();
        let mut best = self.render(n, 0, &mut memo);
        for depth in 1..=64 {
            let s = self.render(n, depth, &mut memo);
            if s.chars().count() > label + SUMMARY || s == best {
                break;
            }
            best = s;
        }
        best
    }
}

impl Archive<'_, '_> {
    /// The bytes of the data blocks: the leaf data blocks and the non-leaf blocks, framed.
    pub fn data_bytes(&self) -> Decoded<u64> {
        let b = self.parsed()?;
        let stored = b.leaves.iter().zip(&b.sizes.leaves).filter(|(l, _)| matches!(l, LeafBlock::Stored(..)));
        Ok(stored.map(|(_, &n)| n).sum::<u64>() + b.sizes.non_leaf.iter().sum::<u64>())
    }

    /// Of the contents of the tree marked in `only` (by [`super::EntryNode::File`]), their sizes
    /// and the instructions of their formulas ([`Listed::size`], [`Listed::nodes`]): of an archive
    /// opened by its index, the blocks of their leaves alone read. None of a content not marked.
    pub fn sized(&self, only: &[bool]) -> Decoded<Vec<Option<(u64, usize)>>> {
        let Some((_, refs)) = &self.tree else { return invalid("an archive of a single file") };
        let taken = |c: usize| only.get(c) == Some(&true);
        let partial;
        let blocks = match &self.kept {
            super::Archived::Indexed(i) => {
                let mut leaves = vec![false; i.leaves_len()];
                refs.iter().enumerate().filter(|&(c, _)| taken(c)).for_each(|(_, r)| leaves[r.0] = true);
                let picked: Vec<usize> = (0..leaves.len()).filter(|&l| leaves[l]).collect();
                partial = i.blocks_for(&picked);
                &partial
            }
            super::Archived::Parsed(b) => b,
        };
        let name = |m: usize| format!("module{}", m + 1);
        let (g, leaves) = graph(blocks, &name)?;
        Ok(refs
            .iter()
            .enumerate()
            .map(|(c, &(l, o, _))| {
                taken(c).then(|| (g.nodes[leaves[l][o]].size, instructions(&g, leaves[l][o], &mut BTreeSet::new())))
            })
            .collect())
    }

    /// Of each content of the tree (by [`super::EntryNode::File`]), what a listing tells
    /// ([`Listed`]); of an archive of a single file, of the file alone. `name` names the module of
    /// Wasm module `wasm` (`None`: not known).
    pub fn listing(&self, name: &dyn Fn(&[u8]) -> Option<String>) -> Decoded<Vec<Listed>> {
        let blocks = self.parsed()?;
        let sizes = &blocks.sizes;
        let names = module_names(blocks, name);
        let name_of = |m: usize| names[m].clone();
        let (g, leaves) = graph(blocks, &name_of)?;
        // a content, of the outputs (leaf, output) given: its size, the blocks it takes, its formula
        // (the segment of a leaf; the last of one lost)
        let segment_of = |l: usize| {
            let s = blocks.segments.iter().position(|s| (s.first.leaves..s.end.leaves).contains(&l));
            s.unwrap_or(blocks.segments.len().saturating_sub(1))
        };
        let taken = |of: &[(usize, usize)]| -> (u64, BTreeSet<Taken>, String, usize, usize) {
            let mut blocks_of = BTreeSet::new();
            let (mut insns, mut dicts) = (BTreeSet::new(), BTreeSet::new());
            for &(l, _) in of {
                blocks_of.insert(Taken::Leaf(l));
                dicts.extend(blocks.leaf_dictionary[l]);
            }
            let mut nodes = 0;
            for &(l, o) in of {
                nodes += instructions(&g, leaves[l][o], &mut insns);
                for n in g.reach(leaves[l][o]) {
                    let node = &g.nodes[n];
                    match &node.shape {
                        &Shape::Slice(b, _) => {
                            blocks_of.insert(Taken::NonLeaf(b));
                            dicts.extend(blocks.non_leaf[b].2);
                        }
                        Shape::Apply { module, resources: r, .. } => {
                            blocks_of.insert(Taken::Module(*module));
                            blocks_of.extend(r.iter().map(|&r| Taken::Resource(r)));
                        }
                        _ => {}
                    }
                }
            }
            blocks_of.extend(dicts.into_iter().map(Taken::Dictionary));
            let formula = match of {
                [(l, o)] => g.summary(leaves[*l][*o]),
                _ => {
                    // the parts of a file of several leaves, as their concatenation
                    let parts: Vec<String> = of.iter().map(|&(l, o)| g.summary(leaves[l][o])).collect();
                    let mut groups: Vec<(String, usize)> = Vec::new();
                    for s in parts {
                        match groups.last_mut() {
                            Some((last, c)) if *last == s => *c += 1,
                            _ => groups.push((s, 1)),
                        }
                    }
                    let mut shown: Vec<String> = groups
                        .iter()
                        .take(BREADTH)
                        .map(|(s, c)| if *c > 1 { format!("{s} x{c}") } else { s.clone() })
                        .collect();
                    if groups.len() > BREADTH {
                        shown.push("...".into());
                    }
                    format!("cat({})", shown.join(", "))
                }
            };
            let size = of.iter().map(|&(l, o)| g.nodes[leaves[l][o]].size).sum();
            (size, blocks_of, formula, nodes, of.first().map_or(0, |&(l, _)| segment_of(l)))
        };
        let contents: Vec<_> = match &self.tree {
            Some((_, refs)) => refs.iter().map(|&(l, o, _)| taken(&[(l, o)])).collect(),
            None => {
                let all: Vec<(usize, usize)> =
                    leaves.iter().enumerate().flat_map(|(l, outs)| (0..outs.len()).map(move |o| (l, o))).collect();
                vec![taken(&all)]
            }
        };
        // a block's bytes and whether they are data
        let bytes = |t: Taken| match t {
            Taken::Leaf(l) => (sizes.leaves[l], matches!(blocks.leaves[l], LeafBlock::Stored(..))),
            Taken::NonLeaf(b) => (sizes.non_leaf[b], true),
            Taken::Module(m) => (sizes.modules[m], false),
            Taken::Resource(r) => (sizes.resources[r], false),
            Taken::Dictionary(d) => (sizes.dictionaries[d], false),
        };
        let shares = apportion(&contents.iter().map(|(size, t, ..)| (*size, t)).collect::<Vec<_>>(), &|t| bytes(t).0);
        Ok(shares
            .into_iter()
            .zip(contents)
            .map(|(share, (size, taken, formula, nodes, segment))| {
                let (mut data, mut overhead) = (0, 0);
                for t in taken {
                    let (n, is_data) = bytes(t);
                    *if is_data { &mut data } else { &mut overhead } += n;
                }
                Listed { size, data, overhead, share, formula, nodes, segment }
            })
            .collect())
    }
}

/// The instructions of `g` that output `n` and what it is of, but those of leaf data blocks and
/// those in `seen` (to which they are added).
fn instructions(g: &Graph, n: usize, seen: &mut BTreeSet<usize>) -> usize {
    g.reach(n)
        .into_iter()
        .filter(|&k| !matches!(g.nodes[k].shape, Shape::Stored(..)) && seen.insert(g.nodes[k].insn))
        .count()
}

/// The names of the module blocks of `blocks`, as `name` names their Wasm modules (`moduleN`
/// where it does not).
pub(super) fn module_names(blocks: &Blocks, name: &dyn Fn(&[u8]) -> Option<String>) -> Vec<String> {
    blocks.modules.iter().enumerate().map(|(i, w)| name(w).unwrap_or_else(|| format!("module{}", i + 1))).collect()
}

/// The graph of the formulas of the leaf blocks of `blocks`, the modules named by `name`; and by
/// leaf, its outputs (nodes).
pub(super) fn graph<'f>(blocks: &Blocks, name: &'f dyn Fn(usize) -> String) -> Decoded<(Graph<'f>, Vec<Vec<usize>>)> {
    let mut g = Graph { nodes: Vec::new(), insns: 0, name, first_insn: Vec::new() };
    let loaded: Vec<OnceLock<_>> = blocks.modules.iter().map(|_| OnceLock::new()).collect();
    // by leaf, its outputs (nodes)
    let mut leaves: Vec<Vec<usize>> = Vec::new();
    let mut used = vec![false; blocks.non_leaf.len()];
    for leaf in &blocks.leaves {
        let outputs = match *leaf {
            LeafBlock::Stored(d, zstd, checked, _) => {
                let insn = g.insns;
                g.insns += 1;
                g.first_insn.push(insn);
                vec![g.add(Shape::Stored(zstd, checked), Vec::new(), insn, stored_len(d, zstd, checked))]
            }
            LeafBlock::Lost => return invalid("a file whose block is lost"),
            // (of a leaf not taken alone, nothing)
            LeafBlock::Unread => Vec::new(),
            LeafBlock::Bad(ref e) => return Err(e.clone()),
            LeafBlock::Formula { outputs, data, segment, .. } => {
                let (d, dict) = blocks.formulas[data];
                let d = decoded_with(d, true, true, dict.map(|d| &blocks.dictionaries[d]))?;
                let (env, end) = blocks.env(leaf, &loaded, &NoModules);
                let p = read(&d, outputs, &env, end, &mut used)?;
                // (the program's modules and resources by their indices in the segment)
                let seg = blocks.segments[segment].first;
                let mut st: Vec<usize> = Vec::new();
                let first = g.insns;
                for insn in &p.insns {
                    let at = g.insns;
                    g.insns += 1;
                    g.first_insn.push(first);
                    let mut pop = |n: usize| st.split_off(st.len() - n);
                    let sum = |g: &Graph, kids: &[usize]| kids.iter().map(|&k| g.nodes[k].size).sum::<u64>();
                    match insn {
                        Insn::Nop | Insn::Concat(1) => {}
                        &Insn::Checksum(c) => g.nodes[*st.last().expect("checked when read")].checksum = Some(c),
                        &Insn::Swap(depth) => {
                            let top = st.len() - 1;
                            st.swap(top, top - depth);
                        }
                        Insn::Literal(l) => {
                            let shown = l[..l.len().min(SHOWN)].to_vec();
                            st.push(g.add(Shape::Literal(shown), Vec::new(), at, l.len() as u64))
                        }
                        &Insn::Slice { block, start, len } => {
                            st.push(g.add(Shape::Slice(p.blocks[block], start), Vec::new(), at, len))
                        }
                        Insn::Apply(a) => {
                            let kids = pop(a.num_inputs);
                            let (module, outs) = (seg.modules + p.modules[a.module], a.output_sizes.len());
                            let resources: Vec<usize> =
                                a.resources.iter().map(|&r| seg.resources + p.resources[r]).collect();
                            let params = a.params[..a.params.len().min(SHOWN)].to_vec();
                            for (out, &size) in a.output_sizes.iter().enumerate() {
                                let shape = Shape::Apply {
                                    module,
                                    resources: resources.clone(),
                                    out,
                                    outs,
                                    params: params.clone(),
                                    params_len: a.params.len(),
                                    memory_limit: a.memory_limit,
                                    task_limits: a.task_limits.clone(),
                                };
                                st.push(g.add(shape, kids.clone(), at, size));
                            }
                        }
                        &Insn::Concat(n) => {
                            let kids = pop(n);
                            let size = sum(&g, &kids);
                            st.push(g.add(Shape::Concat, kids, at, size));
                        }
                        &Insn::Pack(n) => {
                            let kids = pop(n);
                            let size = 8 + 8 * n as u64 + sum(&g, &kids);
                            st.push(g.add(Shape::Pack, kids, at, size));
                        }
                        Insn::Split(sizes) => {
                            let kids = pop(1);
                            for (out, &size) in sizes.iter().enumerate() {
                                st.push(g.add(Shape::Split { out, outs: sizes.len() }, kids.clone(), at, size));
                            }
                        }
                        &Insn::Patch(len) => {
                            let kids = pop(2);
                            st.push(g.add(Shape::Patch, kids, at, len));
                        }
                    }
                }
                if st.len() as u64 != outputs {
                    return invalid("a formula of other outputs than its block's");
                }
                st
            }
        };
        leaves.push(outputs);
    }
    Ok((g, leaves))
}

/// A block a content takes.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Taken {
    Leaf(usize),
    NonLeaf(usize),
    Module(usize),
    Resource(usize),
    Dictionary(usize),
}

/// Of contents (their sizes and the blocks they take), their shares of the blocks (`bytes`: a
/// block's bytes), each block's bytes shared among the contents taking it by their sizes (alike
/// where they are all empty), rounded.
fn apportion<T: Copy + Ord>(contents: &[(u64, &BTreeSet<T>)], bytes: &dyn Fn(T) -> u64) -> Vec<u64> {
    let mut takers: std::collections::BTreeMap<T, (u64, usize)> = Default::default();
    for &(size, taken) in contents {
        for &t in taken {
            let e = takers.entry(t).or_default();
            *e = (e.0 + size, e.1 + 1);
        }
    }
    contents
        .iter()
        .map(|&(size, taken)| {
            let mut share = 0.0;
            for &t in taken {
                let (n, (sum, count)) = (bytes(t) as f64, takers[&t]);
                share += if sum == 0 { n / count as f64 } else { n * size as f64 / sum as f64 };
            }
            share.round() as u64
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A summary of a graph whose nodes take one another's many times over (each a concatenation of
    /// both outputs of a Split of the one before: 2^40 paths) is made in moments: each node
    /// rendered once at each depth, as deep as fits.
    #[test]
    fn summary_of_shared_nodes() {
        let name = |_: usize| String::from("m");
        let mut g = Graph { nodes: Vec::new(), insns: 0, name: &name, first_insn: Vec::new() };
        let mut n = g.add(Shape::Slice(0, 0), Vec::new(), 0, 2);
        for i in 0..40 {
            let a = g.add(Shape::Split { out: 0, outs: 2 }, vec![n], 2 * i + 1, 1);
            let b = g.add(Shape::Split { out: 1, outs: 2 }, vec![n], 2 * i + 1, 1);
            n = g.add(Shape::Concat, vec![a, b], 2 * i + 2, 2);
        }
        let t = std::time::Instant::now();
        let s = g.summary(n);
        assert!(t.elapsed().as_secs_f64() < 1.0, "{:?}", t.elapsed());
        assert!(s.starts_with("cat(split#1(cat(") && s.chars().count() <= 3 + SUMMARY, "{s}");
    }

    #[test]
    fn shared_blocks_apportioned() {
        // blocks of 900, 30 and 60 bytes
        let bytes = |t: usize| [900, 30, 60][t];
        let (a, b, c, empty) =
            (BTreeSet::from([0, 2]), BTreeSet::from([0, 2]), BTreeSet::from([1, 2]), BTreeSet::from([1]));
        let got = apportion(&[(100, &a), (200, &b), (300, &c), (0, &empty)], &bytes);
        // a and b share block 0 alike, their ratio the same
        assert_eq!(got, [310, 620, 60, 0]);
        let got = apportion(&[(0, &empty), (0, &empty)], &bytes);
        assert_eq!(got, [15, 15]);
    }
}
