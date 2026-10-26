//! Archives of several files ([`compress_tree`]; spec, "Segments and entries"): the files of a
//! [`Tree`] are recompressed as the entries of a ZIP archive are, each by its kind by its name
//! ([`kind_by_name`]) and up to [`Options::depth`] formats deep from the file, the PNG files
//! alike among them coded as sets; each file's formula is then a leaf of the archive's one
//! segment.
//!
//! A file is to be read without the others, so what files share is bounded: a set of PNG files
//! takes at most [`Options::set_bytes`] bytes of them (as it is decoded whole for any of them),
//! and the streams of a class and kind of all the files are put one after another, solid, in
//! non-leaf blocks of at most [`Options::block`] bytes, in the order of the files (by
//! extension, then by name), but those that do not compress ([`aqr::Encoder::compresses`]).
//! Files of the same content are stored once.
//!
//! With [`Options::verify`], the archive is decoded (the applications' outputs taken from those
//! evaluated beforehand, as [`crate::compress`] does) and its files compared with the input:
//! those not given back, or all where the archive does not decode, are made again kept raw.

use std::collections::{HashMap, HashSet};

use aqr_decompress::{MODULES, Native};
use aqr_formula::aqr::{self, EntryNode, Leaf, Placed, Segment, Tree, Value};
use aqr_formula::{Formula, Kind, Module, Node};

use crate::{Cx, Options, detect, kind_by_name, nested, place_all, sets, verify};

/// Streams shorter than this are put together with those of their class and kind without
/// telling whether they compress.
const MERGE_PROBE: usize = 4 << 10;

/// A file's content to archive: its data, and a name telling its kind ([`kind_by_name`]).
#[derive(Debug, Clone, Copy)]
pub struct Input<'a> {
    pub name: &'a [u8],
    pub data: &'a [u8],
}

/// An archive of several files, and what became of them.
pub struct Archived {
    pub archive: Vec<u8>,
    pub report: Report,
}

/// What became of the files of an archive.
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// By content.
    pub files: Vec<FileReport>,
    /// The non-leaf blocks that several files slice.
    pub solid: Vec<Solid>,
    /// The contents coded against each other, a set each.
    pub sets: Vec<Vec<usize>>,
    /// Bytes of the archive but for the files': its header and footer, the module, resource and
    /// metadata blocks.
    pub overhead: usize,
    /// The contents kept raw because the archive did not give them back.
    pub unverified: Vec<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct FileReport {
    /// The format the data starts like ([`detect`]).
    pub format: Option<&'static str>,
    /// The modules its formula applies, in order (none: kept as it is).
    pub modules: Vec<Module>,
    /// Bytes of the archive that are the file's: its leaf block, and the part of the non-leaf
    /// blocks it slices by the bytes it slices of them (and of a set, by the sizes of its
    /// outputs). None for a file of the same content as another.
    pub archived: f64,
    /// The content it is the same as, stored once.
    pub same_as: Option<usize>,
}

/// A non-leaf block that several files slice.
#[derive(Debug, Clone)]
pub struct Solid {
    pub class: String,
    pub kind: Kind,
    pub len: usize,
    pub archived: usize,
    /// The contents that slice it, in order.
    pub contents: Vec<usize>,
}

/// The archive of `tree`, whose files are of `contents` (by [`EntryNode::File`]), with what
/// became of them. Panics on a tree that is not one the spec allows.
pub fn compress_tree(tree: &Tree, contents: &[Input], opt: &Options) -> Archived {
    // the same contents once, by the first of them
    let mut first: HashMap<(u128, usize), usize> = HashMap::new();
    let same_as: Vec<Option<usize>> = contents
        .iter()
        .enumerate()
        .map(|(c, i)| {
            let k = (xxhash_rust::xxh3::xxh3_128(i.data), i.data.len());
            let f = *first.entry(k).or_insert(c);
            (f != c).then_some(f)
        })
        .collect();
    let mut order: Vec<usize> = (0..contents.len()).filter(|&c| same_as[c].is_none()).collect();
    order.sort_by(|&a, &b| sort_key(contents[a].name).cmp(&sort_key(contents[b].name)).then(a.cmp(&b)));
    let mut raw = HashSet::new();
    loop {
        let (archive, mut report, bad) = attempt(tree, contents, &order, &same_as, &raw, opt);
        let bad: Vec<usize> = bad.into_iter().filter(|c| !raw.contains(c)).collect();
        if bad.is_empty() {
            report.unverified = raw.into_iter().collect();
            report.unverified.sort_unstable();
            return Archived { archive, report };
        }
        raw.extend(bad);
    }
}

/// What files are ordered by, to put those alike next to each other: the extension, then the
/// name, of the last component.
fn sort_key(name: &[u8]) -> (Vec<u8>, &[u8]) {
    let base = name.rsplit(|&b| b == b'/').next().unwrap_or(name);
    let ext = base.iter().rposition(|&b| b == b'.').filter(|&p| p > 0).map_or(&[][..], |p| &base[p + 1..]);
    (ext.to_ascii_lowercase(), base)
}

/// The archive of `tree`, its contents (those in `order`; the others `same_as` one of them)
/// recompressed but those in `raw`, with its report; and the contents it does not give back
/// (where verified).
fn attempt(
    tree: &Tree,
    contents: &[Input],
    order: &[usize],
    same_as: &[Option<usize>],
    raw: &HashSet<usize>,
    opt: &Options,
) -> (Vec<u8>, Report, Vec<usize>) {
    // a formula of the files as the contents of its streams, each a file of its own
    let mut f = Formula::default();
    let mut slices = Vec::with_capacity(order.len());
    for &c in order {
        let (kind, d) = (kind_by_name(contents[c].name), contents[c].data.to_vec());
        slices.push(if raw.contains(&c) { f.push_as("raw", kind, d) } else { f.push_content("raw", kind, d) });
    }
    f.root = Node::Concat(slices);

    // nested a format deeper, so that the files are as deep as a file alone is
    let nopt = Options { depth: opt.depth.saturating_add(1), ..opt.clone() };
    let total = contents.iter().map(|i| i.data.len()).sum();
    let mut cx = Cx::of_len(total, &nopt);
    cx.top_sets = Some(opt.set_bytes);
    // (the files known by where their copies are, and taken from the input)
    let mut regions: Vec<&[u8]> = vec![&[]; contents.len()];
    order.iter().zip(&f.streams).for_each(|(&c, s)| regions[c] = &s.data);
    cx.pre = opt.verify.then(|| verify::Prevalidator::spawn(opt.threads, &regions));
    let bases: Vec<usize> = f.streams.iter().map(|s| s.data.as_ptr() as usize).collect();
    let mut taken = HashSet::new();
    if let Some((mut children, plan)) = nested(&f, &nopt, 0, &cx) {
        let sets = sets::sets(&f, &mut children, &plan, &nopt, 0, &cx);
        for s in &sets {
            for &m in &s.members {
                if let &Node::Slice { stream, start, .. } = &children[m].0 {
                    taken.insert(bases[stream] + start as usize);
                }
            }
        }
        place_all(&mut f, children.into_iter().map(|c| c.1).collect(), sets, &nopt);
    }
    let deferred: Vec<_> = cx.deferred.borrow_mut().drain(..).collect();
    if let Some(pre) = &cx.pre {
        deferred.into_iter().filter(|(at, _)| !taken.contains(at)).for_each(|(_, j)| pre.send(j));
    }
    let memo = cx.pre.take().map(verify::Prevalidator::finish).unwrap_or_default();

    // a leaf for each file, or for each set of them
    let Node::Concat(nodes) = std::mem::replace(&mut f.root, Node::Concat(Vec::new())) else { unreachable!() };
    let mut leaves: Vec<Leaf> = Vec::new();
    let mut at = vec![(0, 0); contents.len()];
    let mut set_leaf: HashMap<usize, usize> = HashMap::new();
    for (&c, node) in order.iter().zip(nodes) {
        let sum = Some(aqr::checksum(contents[c].data));
        match node {
            Node::Output { shared, index, .. } => {
                let n = f.shared[shared].sizes.len();
                let l = *set_leaf.entry(shared).or_insert_with(|| {
                    leaves.push(Leaf { outputs: vec![Node::Concat(Vec::new()); n], sums: vec![None; n] });
                    leaves.len() - 1
                });
                (leaves[l].outputs[index], leaves[l].sums[index]) = (node, sum);
                at[c] = (l, index);
            }
            node => {
                leaves.push(Leaf { outputs: vec![node], sums: vec![sum] });
                at[c] = (leaves.len() - 1, 0);
            }
        }
    }
    for (c, s) in same_as.iter().enumerate() {
        if let Some(s) = s {
            at[c] = at[*s];
        }
    }

    // the streams of a class together, within a block each; but those that do not compress,
    // which would gain nothing and only make a file read alone read more
    for s in &mut f.streams {
        s.content = false;
        if s.kind != Kind::Compressed && s.data.len() >= MERGE_PROBE && !cx.enc.compresses(&s.data).expect("zstd") {
            s.kind = Kind::Compressed;
        }
    }
    let counts: Vec<usize> = leaves.iter().map(|l| l.outputs.len()).collect();
    f.root = Node::Concat(leaves.iter_mut().flat_map(|l| std::mem::take(&mut l.outputs)).collect());
    f.merge_classes_within(opt.block as u64);
    let Node::Concat(nodes) = std::mem::replace(&mut f.root, Node::Concat(Vec::new())) else { unreachable!() };
    let mut nodes = nodes.into_iter();
    leaves.iter_mut().zip(counts).for_each(|(l, n)| l.outputs = nodes.by_ref().take(n).collect());

    let seg = Segment { f: &f, leaves: &leaves, tree, contents: &at };
    let (archive, placed) =
        cx.enc.encode_segment(&seg).expect("a tree the spec allows, of slices within their streams");
    cx.enc.clear();
    let report = report(&f, &leaves, &placed, archive.len(), contents, &at, same_as);
    drop(f);

    let bad = if opt.verify {
        let m = verify::Cached {
            native: Native { threads: opt.threads },
            memo: memo.into(),
            hits: Default::default(),
            files: &contents.iter().map(|i| i.data).collect::<Vec<_>>(),
        };
        match aqr::decode_value(&archive, &aqr::natives(&MODULES, &m)) {
            Ok(Value::Tree(t, files)) => {
                let wanted =
                    tree.entries.iter().filter_map(|e| if let EntryNode::File(c) = e.node { Some(c) } else { None });
                let got = t.entries.iter().filter_map(|e| if let EntryNode::File(c) = e.node { Some(c) } else { None });
                let mut bad: Vec<usize> = wanted
                    .zip(got)
                    .filter(|&(c, g)| files[g] != contents[c].data)
                    .map(|(c, _)| same_as[c].unwrap_or(c))
                    .collect();
                bad.sort_unstable();
                bad.dedup();
                bad
            }
            // (which file is not known: all of them raw)
            _ => order.to_vec(),
        }
    } else {
        Vec::new()
    };
    (archive, report, bad)
}

/// The report of the archive of `len` bytes whose blocks are `placed`, of formula `f` and its
/// `leaves`, each content at its leaf and output in `at`.
fn report(
    f: &Formula,
    leaves: &[Leaf],
    placed: &[(Placed, usize)],
    len: usize,
    contents: &[Input],
    at: &[(usize, usize)],
    same_as: &[Option<usize>],
) -> Report {
    // by leaf, the bytes it slices of each stream, and the modules it applies
    let mut sliced: Vec<HashMap<usize, u64>> = vec![HashMap::new(); leaves.len()];
    let mut modules: Vec<Vec<Module>> = vec![Vec::new(); leaves.len()];
    fn visit(n: &Node, sliced: &mut HashMap<usize, u64>, modules: &mut Vec<Module>, shared: &mut [bool]) {
        n.visit(&mut |n| match n {
            &Node::Slice { stream, len: len @ 1.., .. } => *sliced.entry(stream).or_default() += len,
            Node::Apply(a) if !modules.contains(&a.module) => modules.push(a.module),
            &Node::Output { shared: s, .. } => shared[s] = true,
            _ => {}
        })
    }
    for (l, leaf) in leaves.iter().enumerate() {
        let mut shared = vec![false; f.shared.len()];
        leaf.outputs.iter().for_each(|n| visit(n, &mut sliced[l], &mut modules[l], &mut shared));
        for s in (0..f.shared.len()).rev() {
            if shared[s] {
                f.shared[s].apply.inputs.iter().for_each(|n| visit(n, &mut sliced[l], &mut modules[l], &mut shared));
                if !modules[l].contains(&f.shared[s].apply.module) {
                    modules[l].push(f.shared[s].apply.module);
                }
            }
        }
    }
    let mut by_stream: HashMap<usize, Vec<usize>> = HashMap::new();
    for (l, s) in sliced.iter().enumerate() {
        s.keys().for_each(|&st| by_stream.entry(st).or_default().push(l));
    }
    let mut bytes = vec![0f64; leaves.len()];
    let mut r = Report { overhead: len, ..Report::default() };
    // by leaf, its contents (the first of each content)
    let mut of_leaf: Vec<Vec<usize>> = vec![Vec::new(); leaves.len()];
    (0..contents.len()).filter(|&c| same_as[c].is_none()).for_each(|c| of_leaf[at[c].0].push(c));
    for &(p, n) in placed {
        match p {
            Placed::Formula(l) | Placed::Data(l) => bytes[l] += n as f64,
            Placed::NonLeaf(st) => {
                let ls = &by_stream[&st];
                let total: u64 = ls.iter().map(|&l| sliced[l][&st]).sum();
                ls.iter().for_each(|&l| bytes[l] += n as f64 * sliced[l][&st] as f64 / total.max(1) as f64);
                let mut cs: Vec<usize> = ls.iter().flat_map(|&l| of_leaf[l].iter().copied()).collect();
                cs.sort_unstable();
                if cs.len() > 1 {
                    let s = &f.streams[st];
                    r.solid.push(Solid {
                        class: s.class.to_string(),
                        kind: s.kind,
                        len: s.data.len(),
                        archived: n,
                        contents: cs,
                    });
                }
            }
            Placed::Module | Placed::Resource | Placed::Metadata => continue,
        }
        r.overhead -= n;
    }
    for (l, cs) in of_leaf.iter().enumerate() {
        if leaves[l].outputs.len() > 1 {
            let mut set = cs.clone();
            set.sort_by_key(|&c| at[c].1);
            r.sets.push(set);
        }
    }
    let out_len = |c: usize| leaves[at[c].0].outputs[at[c].1].len() as f64;
    let leaf_len: Vec<f64> = leaves.iter().map(|l| l.outputs.iter().map(|o| o.len() as f64).sum()).collect();
    r.files = (0..contents.len())
        .map(|c| {
            let l = at[c].0;
            FileReport {
                format: detect(contents[c].data).map(|f| f.name()),
                modules: modules[l].clone(),
                archived: match same_as[c] {
                    Some(_) => 0.0,
                    None if leaf_len[l] > 0.0 => bytes[l] * out_len(c) / leaf_len[l],
                    None => bytes[l] / leaves[l].outputs.len() as f64,
                },
                same_as: same_as[c],
            }
        })
        .collect();
    r
}
