//! What a listing of an archive tells of each file's content ([`Archive::listing`]): the bytes
//! of the blocks it takes, of data and of the rest, its share of them (of blocks taken by
//! several, a part), and the formula making it, summarized.
//!
//! A formula block is a program of a stack machine: read again as a graph of the instructions,
//! each item of the stack a node (an application or a split of several outputs a node for each,
//! of the one instruction), the content of output `o` is what the item `o` from the bottom at
//! the end is made of, apart from the other outputs of the leaf.

use std::collections::BTreeSet;

use super::{Archive, Decoded, Env, Insn, LeafBlock, decoded_with, invalid, read, stored_len};

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
}

/// The most characters of a summary, but for its first node.
const SUMMARY: usize = 48;
/// The most groups of alike children shown of a node.
const BREADTH: usize = 4;

#[derive(Clone)]
enum Shape {
    Literal,
    /// Of the non-leaf block (by its ordinal among all).
    Slice(usize),
    /// Of the module block (by index), its resources (by index) and the output taken of how many.
    Apply {
        module: usize,
        resources: Vec<usize>,
        out: usize,
        outs: usize,
    },
    Concat,
    Pack,
    Split {
        out: usize,
        outs: usize,
    },
    Patch,
    /// A leaf data block, Zstandard data or not.
    Stored(bool),
}

struct Node {
    shape: Shape,
    kids: Vec<usize>,
    /// The instruction it is of (by its index among all of the graph's).
    insn: usize,
    /// The bytes of its output.
    size: u64,
}

/// The graph of one or more formulas, and module names given.
struct Graph<'f> {
    nodes: Vec<Node>,
    insns: usize,
    name: &'f dyn Fn(usize) -> String,
}

impl Graph<'_> {
    fn add(&mut self, shape: Shape, kids: Vec<usize>, insn: usize, size: u64) -> usize {
        self.nodes.push(Node { shape, kids, insn, size });
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
            Shape::Literal => "lit".into(),
            Shape::Slice(_) => "slice".into(),
            Shape::Apply { module, out, outs, .. } => nth((self.name)(*module), *out, *outs),
            Shape::Concat => "cat".into(),
            Shape::Pack => "pack".into(),
            Shape::Split { out, outs } => nth("split".into(), *out, *outs),
            Shape::Patch => "patch".into(),
            Shape::Stored(false) => "stored".into(),
            Shape::Stored(true) => "zstd".into(),
        }
    }

    /// Node `n` summarized down to `depth` levels below it (`*` past them).
    fn render(&self, n: usize, depth: usize) -> String {
        let label = self.label(n);
        let kids = &self.nodes[n].kids;
        if kids.is_empty() {
            return label;
        }
        if depth == 0 {
            return format!("{label}(*)");
        }
        let mut groups: Vec<(String, usize)> = Vec::new();
        for &k in kids {
            let s = self.render(k, depth - 1);
            match groups.last_mut() {
                Some((last, count)) if *last == s => *count += 1,
                _ => groups.push((s, 1)),
            }
        }
        let mut parts: Vec<String> =
            groups.iter().take(BREADTH).map(|(s, c)| if *c > 1 { format!("{s} x{c}") } else { s.clone() }).collect();
        if groups.len() > BREADTH {
            parts.push("...".into());
        }
        format!("{label}({})", parts.join(", "))
    }

    /// Node `n` summarized as deep as fits in [`SUMMARY`] characters past its label.
    fn summary(&self, n: usize) -> String {
        let label = self.label(n).chars().count();
        let mut depth = 64;
        loop {
            let s = self.render(n, depth);
            if depth == 0 || s.chars().count() <= label + SUMMARY {
                return s;
            }
            depth = depth.min(s.len()) - 1;
        }
    }
}

impl Archive<'_, '_> {
    /// The bytes of the data blocks: the leaf data blocks and the non-leaf blocks, framed.
    pub fn data_bytes(&self) -> u64 {
        let b = &self.blocks;
        let stored = b.leaves.iter().zip(&b.sizes.leaves).filter(|(l, _)| matches!(l, LeafBlock::Stored(..)));
        stored.map(|(_, &n)| n).sum::<u64>() + b.sizes.non_leaf.iter().sum::<u64>()
    }

    /// Of each content of the tree (by [`super::EntryNode::File`]), what a listing tells
    /// ([`Listed`]); of an archive of a single file, of the file alone. `name` names the module of
    /// Wasm module `wasm` (`None`: not known).
    pub fn listing(&self, name: &dyn Fn(&[u8]) -> Option<String>) -> Decoded<Vec<Listed>> {
        let blocks = &self.blocks;
        let sizes = &blocks.sizes;
        let names: Vec<String> = blocks
            .modules
            .iter()
            .enumerate()
            .map(|(i, w)| name(w).unwrap_or_else(|| format!("module{}", i + 1)))
            .collect();
        let name_of = |m: usize| names[m].clone();
        let mut g = Graph { nodes: Vec::new(), insns: 0, name: &name_of };
        // by leaf, its outputs (nodes)
        let mut leaves: Vec<Vec<usize>> = Vec::new();
        let mut used = vec![false; blocks.non_leaf.len()];
        for leaf in &blocks.leaves {
            let outputs = match *leaf {
                LeafBlock::Stored(d, zstd, checked, _) => {
                    let insn = g.insns;
                    g.insns += 1;
                    vec![g.add(Shape::Stored(zstd), Vec::new(), insn, stored_len(d, zstd, checked))]
                }
                LeafBlock::Formula { outputs, data, modules, resources, non_leaf } => {
                    let (d, dict) = blocks.formulas[data];
                    let d = decoded_with(d, true, true, dict.map(|d| &blocks.dictionaries[d]))?;
                    let env = Env {
                        first: non_leaf,
                        modules: &blocks.modules[..modules],
                        module_data: &blocks.module_data[..modules],
                        resource_data: &blocks.resource_data[..resources],
                        loaded: &self.loaded[..modules],
                        resources: &blocks.resources[..resources],
                        runner: self.runner,
                    };
                    let p = read(&d, outputs, &env, blocks.non_leaf.len(), &mut used)?;
                    let mut st: Vec<usize> = Vec::new();
                    for insn in &p.insns {
                        let at = g.insns;
                        g.insns += 1;
                        let mut pop = |n: usize| st.split_off(st.len() - n);
                        let sum = |g: &Graph, kids: &[usize]| kids.iter().map(|&k| g.nodes[k].size).sum::<u64>();
                        match insn {
                            Insn::Nop | Insn::Checksum(_) | Insn::Concat(1) => {}
                            &Insn::Swap(depth) => {
                                let top = st.len() - 1;
                                st.swap(top, top - depth);
                            }
                            Insn::Literal(l) => st.push(g.add(Shape::Literal, Vec::new(), at, l.len() as u64)),
                            &Insn::Slice { block, len, .. } => {
                                st.push(g.add(Shape::Slice(p.blocks[block]), Vec::new(), at, len))
                            }
                            Insn::Apply(a) => {
                                let kids = pop(a.num_inputs);
                                let (module, outs) = (p.modules[a.module], a.output_sizes.len());
                                let resources: Vec<usize> = a.resources.iter().map(|&r| p.resources[r]).collect();
                                for (out, &size) in a.output_sizes.iter().enumerate() {
                                    let shape = Shape::Apply { module, resources: resources.clone(), out, outs };
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
        // a content, of the outputs (leaf, output) given: its size, the blocks it takes, its formula
        let taken = |of: &[(usize, usize)]| -> (u64, BTreeSet<Taken>, String, usize) {
            let mut blocks_of = BTreeSet::new();
            let (mut insns, mut dicts) = (BTreeSet::new(), BTreeSet::new());
            for &(l, _) in of {
                blocks_of.insert(Taken::Leaf(l));
                dicts.extend(blocks.leaf_dictionary[l]);
            }
            let mut nodes = 0;
            for &(l, o) in of {
                for n in g.reach(leaves[l][o]) {
                    let node = &g.nodes[n];
                    if !matches!(node.shape, Shape::Stored(_)) && insns.insert(node.insn) {
                        nodes += 1;
                    }
                    match &node.shape {
                        &Shape::Slice(b) => {
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
            (size, blocks_of, formula, nodes)
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
            .map(|(share, (size, taken, formula, nodes))| {
                let (mut data, mut overhead) = (0, 0);
                for t in taken {
                    let (n, is_data) = bytes(t);
                    *if is_data { &mut data } else { &mut overhead } += n;
                }
                Listed { size, data, overhead, share, formula, nodes }
            })
            .collect())
    }
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
