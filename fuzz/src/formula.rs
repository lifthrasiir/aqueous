//! Formulas made of a fuzzer's bytes ([`formula`]), for `roundtrip`, and what they evaluate to
//! by a reference evaluator of their own ([`reference`]), apart from aqr-archive's (the
//! decoder's): a recursive walk of the nodes, the toy modules as `src/lib.rs` has them.

use aqr_archive::{
    ContentAt, Encoder, Entry, EntryNode, Extra, Leaf, Level, SegmentWriter, Status, Time, Tree, Value, checksum, kill,
};
use aqr_formula::{Formula, Kind, Node, Of, Resource, packed, patch};
use libfuzzer_sys::arbitrary::{Result, Unstructured};

use crate::{COPY, CUT, REV};

/// The longest stream made.
const STREAM_MAX: usize = 4096;
/// The depth of nodes at most.
const DEPTH: u32 = 4;

/// What makes a formula of `u`: the formula so far and the outputs of its applications of
/// several not placed yet. Of a batch, all the outputs of an application, and of those whose
/// outputs it takes, are placed in one leaf (`SegmentWriter::batch` refuses them otherwise): by
/// application, its group (those taking of one another), and by group, the leaf it is bound to.
struct Gen<'a, 'b> {
    u: &'a mut Unstructured<'b>,
    f: Formula,
    outputs: Vec<Node>,
    group: Vec<usize>,
    bound: Vec<Option<usize>>,
    /// The leaf being made, of a batch.
    leaf: Option<usize>,
    /// The applications whose outputs were taken while an application's inputs were made.
    took: Vec<usize>,
}

/// A formula made of `u`: up to 3 streams, up to 3 applications of several outputs (of `cut`,
/// or a node or a patch split), and a root of slices, concatenations, packs, applications of
/// `rev` and `copy`, outputs and patches; each output placed once. Now and then a slice out of
/// its stream or an application of a size not its own: the formula then fails to evaluate.
pub fn formula(u: &mut Unstructured) -> Result<Formula> {
    let mut g = Gen::new(u)?;
    let mut root = vec![g.node(DEPTH)?];
    // (the outputs left, placed anywhere in the root's concatenation)
    while !g.outputs.is_empty() {
        let o = g.outputs.swap_remove(g.u.choose_index(g.outputs.len())?);
        root.insert(g.u.int_in_range(0..=root.len())?, o);
    }
    let mut f = g.f;
    f.root = if root.len() == 1 { root.pop().unwrap() } else { Node::Concat(root) };
    Ok(f)
}

/// A batch of a tree's leaves made of `u`: a formula's streams and applications of several
/// outputs as [`formula`] makes them (its root not used), and 1 to 4 leaves of 1 to 3 outputs
/// each over them, the outputs left placed as outputs of their own or in one.
pub fn batch(u: &mut Unstructured) -> Result<(Formula, Vec<Vec<Node>>)> {
    let mut g = Gen::new(u)?;
    let mut leaves: Vec<Vec<Node>> = Vec::new();
    for l in 0..g.u.int_in_range(1..=4)? {
        g.leaf = Some(l);
        let n = g.u.int_in_range(1..=3)?;
        leaves.push((0..n).map(|_| g.node(DEPTH)).collect::<Result<_>>()?);
    }
    while !g.outputs.is_empty() {
        let o = g.outputs.swap_remove(g.u.choose_index(g.outputs.len())?);
        let k = g.group_of(&o);
        let l = match g.bound[k] {
            Some(l) => l,
            None => g.u.choose_index(leaves.len())?,
        };
        g.bound[k] = Some(l);
        let at = g.u.int_in_range(0..=leaves[l].len())?;
        match g.u.ratio(1, 2)? {
            true => leaves[l].insert(at, o),
            false => {
                let at = at.min(leaves[l].len() - 1);
                let n = std::mem::replace(&mut leaves[l][at], Node::Concat(Vec::new()));
                leaves[l][at] = Node::Concat(vec![n, o]);
            }
        }
    }
    Ok((g.f, leaves))
}

impl<'a, 'b> Gen<'a, 'b> {
    /// Up to 3 streams, and up to 3 applications of several outputs.
    fn new(u: &'a mut Unstructured<'b>) -> Result<Gen<'a, 'b>> {
        let mut g = Gen {
            u,
            f: Formula::default(),
            outputs: Vec::new(),
            group: Vec::new(),
            bound: Vec::new(),
            leaf: None,
            took: Vec::new(),
        };
        for _ in 0..g.u.int_in_range(1..=3)? {
            g.stream()?;
        }
        for _ in 0..g.u.int_in_range(0..=3)? {
            g.shared()?;
        }
        Ok(g)
    }

    /// Up to 2 resources of an application, now and then.
    fn resources(&mut self) -> Result<Vec<Resource>> {
        let n = if self.u.ratio(1, 4)? { self.u.int_in_range(1..=2)? } else { 0 };
        (0..n)
            .map(|_| {
                let k = self.u.int_in_range(0..=64)?;
                Ok(Resource::new(self.take(k)?))
            })
            .collect()
    }

    fn stream(&mut self) -> Result<()> {
        let n = self.u.int_in_range(0..=STREAM_MAX)?;
        // (as it comes, or of a pattern repeated: what compresses)
        let data: Vec<u8> = match self.u.ratio(1, 2)? {
            true => self.take(n)?,
            false => {
                let k = self.u.int_in_range(1..=16)?;
                let p = self.take(k)?;
                p.iter().copied().cycle().take(if p.is_empty() { 0 } else { n }).collect()
            }
        };
        let class = ["a", "b", "c"][self.u.choose_index(3)?];
        match self.u.int_in_range(0..=3)? {
            0 => self.f.push_as(class, Kind::Compressed, data),
            1 => self.f.push_apart(class, data),
            2 => self.f.push_as(class, Kind::Text, data),
            _ => self.f.push(class, data),
        };
        Ok(())
    }

    /// Sizes of 1 to 4 outputs adding up to `total`.
    fn sizes(&mut self, total: u64) -> Result<Vec<u64>> {
        let n = self.u.int_in_range(1..=4)?;
        let mut cuts: Vec<u64> = (1..n).map(|_| self.u.int_in_range(0..=total)).collect::<Result<_>>()?;
        cuts.sort_unstable();
        let (mut at, mut sizes) = (0, Vec::with_capacity(n));
        for c in cuts.into_iter().chain([total]) {
            sizes.push(c - at);
            at = c;
        }
        Ok(sizes)
    }

    /// The group of the application of output `o`.
    fn group_of(&self, o: &Node) -> usize {
        match o {
            &Node::Output { shared, .. } => self.group[shared],
            _ => unreachable!("an output"),
        }
    }

    /// Of the outputs not placed, those to be placed in the leaf being made.
    fn placeable(&self) -> Vec<usize> {
        (0..self.outputs.len())
            .filter(|&i| {
                self.leaf.is_none() || self.bound[self.group_of(&self.outputs[i])].is_none_or(|l| Some(l) == self.leaf)
            })
            .collect()
    }

    fn shared(&mut self) -> Result<()> {
        let took = std::mem::take(&mut self.took);
        let outputs = match self.u.int_in_range(0..=2)? {
            0 => {
                let inputs = self.nodes(DEPTH - 1)?;
                let total = inputs.iter().map(Node::len).sum();
                let sizes = self.sizes(total)?;
                let (resources, params) = (self.resources()?, self.params()?);
                let a = match Node::apply_with(CUT, resources, params, inputs, 0) {
                    Node::Apply(a) => *a,
                    _ => unreachable!(),
                };
                self.f.share(a, sizes)
            }
            1 => {
                let n = self.node(DEPTH - 1)?;
                let sizes = self.sizes(n.len())?;
                self.f.split_node(n, sizes)
            }
            _ => {
                let source = self.node(DEPTH - 1)?;
                let (patch, len) = self.patch_of(&source)?;
                let sizes = self.sizes(len)?;
                self.f.split_patch(source, patch, sizes)
            }
        };
        self.outputs.extend(outputs);
        // (a group of its own, merged with those of the applications it took outputs of)
        let i = self.group.len();
        self.group.push(i);
        self.bound.push(None);
        let mut merged: Vec<usize> = self.took.iter().map(|&j| self.group[j]).collect();
        merged.push(i);
        let k = *merged.iter().min().unwrap();
        self.group.iter_mut().filter(|g| merged.contains(g)).for_each(|g| *g = k);
        self.took = took;
        Ok(())
    }

    fn params(&mut self) -> Result<Vec<u8>> {
        let n = self.u.int_in_range(0..=8)?;
        self.take(n)
    }

    /// `n` bytes of `u`, or as many as are left.
    fn take(&mut self, n: usize) -> Result<Vec<u8>> {
        let n = n.min(self.u.len());
        Ok(self.u.bytes(n)?.to_vec())
    }

    fn nodes(&mut self, depth: u32) -> Result<Vec<Node>> {
        (0..self.u.int_in_range(0..=3)?).map(|_| self.node(depth)).collect()
    }

    /// A patch against `source` (a stream of its own) and the length of what it makes: of the
    /// source evaluated so far, changed by `u`; of an empty patch where it does not evaluate.
    fn patch_of(&mut self, source: &Node) -> Result<(Node, u64)> {
        let from = node(&self.f, source).unwrap_or_default();
        let mut to = from.clone();
        for _ in 0..self.u.int_in_range(0..=4)? {
            let at = self.u.int_in_range(0..=to.len())?;
            match self.u.int_in_range(0..=2)? {
                0 if at < to.len() => to[at] = self.u.arbitrary()?,
                1 => to.insert(at, self.u.arbitrary()?),
                _ => to.truncate(at),
            }
        }
        let p = patch::make(&from, &to, 1).unwrap_or_default();
        Ok((self.f.push_as("d", Kind::Compressed, p), to.len() as u64))
    }

    fn node(&mut self, depth: u32) -> Result<Node> {
        // (a leaf at the depth's end: a slice, or an output)
        let placeable = self.placeable();
        let kinds = match (depth, placeable.is_empty()) {
            (0, true) => 1,
            (0, false) => 2,
            _ => 7,
        };
        Ok(match self.u.choose_index(kinds)? {
            0 => {
                let s = self.u.choose_index(self.f.streams.len())?;
                let n = self.f.streams[s].data.len() as u64;
                let start = self.u.int_in_range(0..=n)?;
                let len = self.u.int_in_range(0..=n - start)?;
                // (now and then out of the stream)
                match self.u.ratio(1, 32)? {
                    true => Node::slice(s, start, len + 1),
                    false => Node::slice(s, start, len),
                }
            }
            1 if !placeable.is_empty() => {
                let i = placeable[self.u.choose_index(placeable.len())?];
                let o = self.outputs.swap_remove(i);
                let (k, Node::Output { shared, .. }) = (self.group_of(&o), &o) else { unreachable!() };
                self.took.push(*shared);
                if self.leaf.is_some() {
                    self.bound[k] = self.leaf;
                }
                o
            }
            1 | 2 => Node::Concat(self.nodes(depth - 1)?),
            3 => Node::Pack(self.nodes(depth - 1)?),
            4 | 5 => {
                let m = if self.u.ratio(1, 2)? { REV } else { COPY };
                let inputs = self.nodes(depth - 1)?;
                let len = inputs.iter().map(Node::len).sum::<u64>();
                // (now and then of another size than it makes)
                let len = if self.u.ratio(1, 32)? { len + 1 } else { len };
                let (resources, params) = (self.resources()?, self.params()?);
                Node::apply_with(m, resources, params, inputs, len)
            }
            _ => {
                let source = self.node(depth - 1)?;
                let (patch, len) = self.patch_of(&source)?;
                Node::patch(source, patch, len)
            }
        })
    }
}

/// What `f` evaluates to by the reference evaluator: None where it does not (a slice out of its
/// stream, an application of another size than it makes, a patch that does not apply).
pub fn reference(f: &Formula) -> Option<Vec<u8>> {
    node(f, &f.root)
}

/// Node `n` of `f` evaluated.
fn node(f: &Formula, n: &Node) -> Option<Vec<u8>> {
    Reference { f, shared: Default::default() }.node(n)
}

/// What each of `nodes` (of `f`'s leaves) evaluates to by the reference evaluator.
pub fn references(f: &Formula, nodes: &[Node]) -> Vec<Option<Vec<u8>>> {
    let r = Reference { f, shared: Default::default() };
    nodes.iter().map(|n| r.node(n)).collect()
}

/// The reference evaluator of a formula: what each application of several outputs makes, as it
/// is evaluated (once: they feed one another).
struct Reference<'f> {
    f: &'f Formula,
    shared: std::cell::RefCell<std::collections::HashMap<usize, Option<Vec<u8>>>>,
}

impl Reference<'_> {
    /// What application of several outputs `i` makes, all of its outputs.
    fn shared(&self, i: usize) -> Option<Vec<u8>> {
        if let Some(d) = self.shared.borrow().get(&i) {
            return d.clone();
        }
        let all = |ns: &[Node]| ns.iter().map(|n| self.node(n)).collect::<Option<Vec<_>>>();
        let s = self.f.shared.get(i)?;
        let d = match &s.of {
            // (cut into the outputs' sizes, which its inputs add up to)
            Of::Apply(a) if a.module == CUT => all(&a.inputs).map(|d| d.concat()),
            Of::Node(n) => self.node(n),
            Of::Patch { inputs: [source, p], len } => {
                self.node(source).zip(self.node(p)).and_then(|(s, p)| patch::apply(&s, &p, *len).ok())
            }
            _ => None,
        };
        let d = d.filter(|d| d.len() as u64 == s.sizes.iter().sum::<u64>());
        self.shared.borrow_mut().insert(i, d.clone());
        d
    }

    /// Node `n` evaluated.
    fn node(&self, n: &Node) -> Option<Vec<u8>> {
        let f = self.f;
        let all = |ns: &[Node]| ns.iter().map(|n| self.node(n)).collect::<Option<Vec<_>>>();
        let out = match n {
            Node::Slice { stream, start, len } => {
                let d = &f.streams.get(*stream)?.data;
                d.get(usize::try_from(*start).ok()?..usize::try_from(start.checked_add(*len)?).ok()?)?.to_vec()
            }
            Node::Concat(ns) => all(ns)?.concat(),
            Node::Pack(ns) => packed::pack(&all(ns)?),
            Node::Apply(a) => {
                let d = all(&a.inputs)?.concat();
                let d = match a.module {
                    m if m == REV => d.into_iter().rev().collect(),
                    m if m == COPY => d,
                    _ => return None,
                };
                (d.len() as u64 == a.len).then_some(d)?
            }
            Node::Output { shared, index, len } => {
                let (s, d) = (&f.shared[*shared], self.shared(*shared)?);
                let at = s.sizes[..*index].iter().sum::<u64>() as usize;
                let d = d[at..at + *s.sizes.get(*index)? as usize].to_vec();
                (d.len() as u64 == *len).then_some(d)?
            }
            Node::Patch(p) => patch::apply(&self.node(&p.source)?, &self.node(&p.patch)?, p.len).ok()?,
            Node::ModuleBytes(..) | Node::ResourceBytes(_) | Node::Either(_) => return None,
        };
        (out.len() as u64 == n.len()).then_some(out)
    }
}

/// `roundtrip`'s body: of `d`, a formula archived and decoded again (whole, and streamed), or a
/// tree of 1 to 3 segments committed one after another (appended to by another writer now and
/// then, as `aqr -a` does, after the footer or over it), each of 1 or 2 batches of leaves
/// ([`batch`]), files of later segments replacing earlier ones of their paths now and then, and
/// tombstones of them, its files decoded again; where [`reference`] gives what each is, the encoder takes it and the archive gives it
/// back.
pub fn roundtrip(d: &[u8]) {
    let mut u = Unstructured::new(d);
    let (Ok(level), Ok(summed), Ok(tree)) = (
        u.choose(&[Level::STORED, Level::Zstd(1), Level::Zstd(3), Level::LOW]).copied(),
        u.arbitrary::<bool>(),
        u.arbitrary::<bool>(),
    ) else {
        return;
    };
    match tree {
        true => tree_roundtrip(&mut u, level, summed),
        false => file_roundtrip(&mut u, level, summed),
    }
}

fn file_roundtrip(u: &mut Unstructured, level: Level, summed: bool) {
    let Ok(f) = formula(u) else { return };
    let runner = crate::toys();
    let want = reference(&f);
    let sum = want.as_deref().filter(|_| summed).map(checksum);
    let enc = Encoder::new(level);
    let c = match (enc.encode(&f, sum), &want) {
        (Ok(c), _) => c,
        (Err(e), Some(_)) => panic!("a formula that evaluates not archived: {e}\n{f:?}"),
        (Err(_), None) => return,
    };
    let got = aqr_archive::decode_value(&c, &runner);
    let Some(want) = want else { return };
    match got {
        Ok(Value::File(got)) => assert!(got == want, "decoded otherwise\n{f:?}"),
        r => panic!("an archive of a formula that evaluates does not decode: {:?}\n{f:?}", r.map(|_| ())),
    }
    let a = aqr_archive::open(&c, &runner).unwrap();
    let mut streamed = Vec::new();
    a.write_file(1, &mut |d| {
        streamed.extend_from_slice(d);
        Ok(())
    })
    .unwrap();
    assert!(streamed == want, "streamed otherwise\n{f:?}");
}

fn tree_roundtrip(u: &mut Unstructured, level: Level, summed: bool) {
    let Ok(dictionaries) = u.arbitrary::<bool>() else { return };
    let enc = Encoder::new(level).with_dictionaries(dictionaries);
    let runner = crate::toys();
    let mut w = SegmentWriter::new();
    let mut out = w.head().to_vec();
    // by path, what the archive is to give, its segments merged
    let mut want: Paths = Default::default();
    let mut root = Extra::default();
    let mut batches = Vec::new();
    // (whether every output written evaluates, of files replaced or not named too: the archive
    // holds them still)
    let mut evaluates = true;
    for k in 0..u.int_in_range(1..=3).unwrap_or(1) {
        // (appended to by another writer now and then, of what the archive tells of itself, as
        // `aqr -a` appends)
        // (or written over its footer, from where it starts, as where the footer's first bytes
        // are across a sector: the archive without a footer until the commit)
        if k > 0 && u.arbitrary::<bool>().unwrap_or(false) {
            let Some((footer, Some(metadata))) = aqr_archive::footer_of(&out) else { panic!("no footer") };
            w = match u.arbitrary::<bool>().unwrap_or(false) {
                true => SegmentWriter::appending(footer, metadata),
                false => {
                    out.truncate(footer.start);
                    SegmentWriter::replacing(footer.start, metadata)
                }
            };
        }
        out.extend_from_slice(w.pad());
        let (mut contents, mut wants) = (Vec::new(), Vec::new());
        for _ in 0..u.int_in_range(1..=2).unwrap_or(1) {
            let Ok((f, outputs)) = batch(u) else { break };
            let all: Vec<Node> = outputs.iter().flatten().cloned().collect();
            let mut r = references(&f, &all).into_iter();
            let ws: Vec<Vec<Option<Vec<u8>>>> = outputs.iter().map(|o| r.by_ref().take(o.len()).collect()).collect();
            let leaves: Vec<Leaf> = outputs
                .into_iter()
                .zip(&ws)
                .map(|(outputs, w)| {
                    let sums = w.iter().map(|w| w.as_deref().filter(|_| summed).map(checksum)).collect();
                    Leaf { outputs, sums }
                })
                .collect();
            let evaluates = ws.iter().flatten().all(Option::is_some);
            let mut b = match (w.batch(&enc, &f, &leaves), evaluates) {
                (Ok(b), _) => b,
                (Err(e), true) => panic!("a batch that evaluates not archived: {e}\n{f:?}\n{leaves:?}"),
                (Err(_), false) => return,
            };
            w.commit(&b);
            let (d, r) = b.take_blocks();
            out.extend_from_slice(&d[r]);
            for (&leaf, (l, w)) in b.leaf_at.iter().zip(leaves.iter().zip(ws)) {
                for (output, (n, w)) in l.outputs.iter().zip(w).enumerate() {
                    contents.push(ContentAt { leaf, output, size: n.len() });
                    wants.push(w);
                }
            }
            batches.push((f, leaves));
        }
        if contents.is_empty() {
            return;
        }
        // its entries: the files, directories, links and tombstones at paths of a few names
        // (so that later segments meet earlier ones' paths), merged into what is to be
        evaluates &= wants.iter().all(Option::is_some);
        let mut seg = Segment::default();
        // (the root's extra merged as a directory's: kept where the segment's is null)
        seg.t.root = extra(u);
        if !seg.t.root.is_empty() {
            root = seg.t.root.clone();
        }
        // (each content a file, or now and then a fork of the file before; files of link groups
        // now and then, of the segment's own or archive-wide)
        let mut named: Vec<ContentAt> = Vec::new();
        let mut last: Option<(usize, Vec<Vec<u8>>)> = None;
        for (i, w) in wants.into_iter().enumerate() {
            if let Some((e, p)) = &last
                && u.ratio(1, 4).unwrap_or(false)
            {
                let forks = &mut seg.t.entries[*e].extra.forks;
                let name = format!("k{}", forks.len()).into_bytes();
                forks.push((name.clone(), named.len()));
                named.push(contents[i]);
                if let Some(Want::File(_, _, f, _)) = want.get_mut(p) {
                    f.push((name, w));
                }
                continue;
            }
            let Some(p) = path(u) else { break };
            let mut x = extra(u);
            let group = match u.int_in_range(0..=5).unwrap_or(0) {
                0 | 1 => Some(u.int_in_range(0..=1).unwrap_or(0)),
                2 => Some(-u.int_in_range(1..=2).unwrap_or(1)),
                _ => None,
            };
            x.link_group = group;
            if let Some(e) = seg.put(u, &mut want, &p, EntryNode::File(named.len()), x.clone()) {
                named.push(contents[i]);
                // (a group of the segment's own told apart from those of others; archive-wide ones not)
                let label = group.map(|g| if g < 0 { (-1, g) } else { (k as i64, g) });
                x.link_group = None;
                merge(&mut want, &p, Want::File(w, x, Vec::new(), label));
                last = Some((e, p));
            }
        }
        for _ in 0..u.int_in_range(0..=3).unwrap_or(0) {
            let Some(p) = path(u) else { break };
            let x = extra(u);
            let (node, w, x) = match u.int_in_range(0..=2).unwrap_or(0) {
                0 => (EntryNode::Dir, Some(Want::Dir(x.clone())), x),
                1 => {
                    let target = p.iter().rev().cloned().collect::<Vec<_>>().join(&b'/');
                    let mut x = x;
                    x.win_link_type = u.ratio(1, 3).unwrap_or(false).then(|| u.int_in_range(0..=2).unwrap_or(0));
                    (EntryNode::Link(target.clone()), Some(Want::Link(target, x.clone())), x)
                }
                // (a tombstone of no extra)
                _ => (EntryNode::TOMBSTONE, None, Extra::default()),
            };
            if seg.put(u, &mut want, &p, node, x).is_some() {
                match w {
                    Some(w) => merge(&mut want, &p, w),
                    // (a tombstone: its parents declared, merged as directories; it and what is under
                    // it removed)
                    None => {
                        for k in 1..p.len() {
                            merge(&mut want, &p[..k], Want::Dir(Extra::default()));
                        }
                        want.retain(|q, _| !q.starts_with(&p));
                    }
                }
            }
        }
        let t = seg.t;
        let contents = named;
        let status = if u.arbitrary::<bool>().unwrap_or(false) { Status::Complete } else { Status::None };
        match (w.close_as(&enc, &t, &contents, status), evaluates) {
            (Ok(m), _) => out.extend_from_slice(&m),
            (Err(e), true) => panic!("a segment that evaluates not closed: {e}\n{batches:?}"),
            (Err(_), false) => return,
        }
        let (end, old) = w.footer(0);
        out.extend_from_slice(&end);
        // (committed: the footer before made a spacer block)
        if let Some(old) = old {
            let k = kill(old.len());
            out[old.start..old.start + k.len()].copy_from_slice(&k);
            out[old.start + k.len()..old.end].fill(0);
        }
    }
    let got = aqr_archive::decode_value(&out, &runner);
    if !evaluates {
        return;
    }
    match got {
        Ok(Value::Tree(t, files)) => {
            let mut paths: Vec<Vec<Vec<u8>>> = Vec::with_capacity(t.entries.len());
            let mut got = std::collections::BTreeMap::new();
            for e in &t.entries {
                let mut p = e.parent.map_or_else(Vec::new, |q| paths[q].clone());
                p.push(e.name.clone());
                let mut x = e.extra.clone();
                let w = match &e.node {
                    &EntryNode::File(c) => {
                        let forks = x.forks.drain(..).map(|(n, k)| (n, Some(files[k].clone()))).collect();
                        let label = x.link_group.take().map(|g| (0, g));
                        Some(Want::File(Some(files[c].clone()), x, forks, label))
                    }
                    EntryNode::Dir => Some(Want::Dir(x)),
                    EntryNode::Link(t) => Some(Want::Link(t.clone(), x)),
                    _ => None,
                };
                if let Some(w) = w {
                    got.insert(p.clone(), w);
                }
                paths.push(p);
            }
            // (link groups by the files of each, the labels apart)
            assert!(groups(&got) == groups(&want), "link groups decoded otherwise\n{got:?}\n{want:?}");
            let unlabelled = |m: &Paths| -> Paths {
                m.iter()
                    .map(|(p, w)| match w {
                        Want::File(d, x, f, _) => (p.clone(), Want::File(d.clone(), x.clone(), f.clone(), None)),
                        w => (p.clone(), w.clone()),
                    })
                    .collect()
            };
            let (got, want) = (unlabelled(&got), unlabelled(&want));
            assert!(got == want, "entries decoded otherwise\n{got:?}\n{want:?}\n{batches:?}");
            assert!(t.root == root, "the root's extra decoded otherwise\n{:?}\n{root:?}", t.root);
        }
        r => panic!("an archive of a tree that evaluates does not decode: {:?}\n{batches:?}", r.map(|_| ())),
    }
}

/// What a tree is to have at a path: a file (its bytes, None where they do not evaluate by the
/// reference; its extra but its forks and link group; its forks' names and bytes; the label of
/// its link group), a directory or a link, and its extra.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Want {
    File(Option<Vec<u8>>, Extra, Vec<(Vec<u8>, Option<Vec<u8>>)>, Option<(i64, i128)>),
    Dir(Extra),
    Link(Vec<u8>, Extra),
}

/// The link groups of `m`: the paths of the files of each label.
fn groups(m: &Paths) -> std::collections::BTreeSet<Vec<Vec<Vec<u8>>>> {
    let mut of: std::collections::BTreeMap<(i64, i128), Vec<Vec<Vec<u8>>>> = Default::default();
    for (p, w) in m {
        if let Want::File(_, _, _, Some(l)) = w {
            of.entry(*l).or_default().push(p.clone());
        }
    }
    of.into_values().collect()
}

/// An entry extra of `u`: null (empty) half of the time, else some of its members of any entry
/// (times, the mode, the owners, a comment).
fn extra(u: &mut Unstructured) -> Extra {
    let mut x = Extra::default();
    if u.arbitrary::<bool>().unwrap_or(false) {
        return x;
    }
    let mut some = || u.arbitrary::<bool>().unwrap_or(false);
    let (t, m, o, c) = (some(), some(), some(), some());
    if t {
        let secs = u.int_in_range(-(1i64 << 34)..=1 << 34).unwrap_or(0);
        x.mtime = Some(Time::new(secs, u.int_in_range(0..=999_999_999).unwrap_or(0)));
    }
    if m {
        x.mode = Some(u.int_in_range(0..=0o7777).unwrap_or(0o644));
    }
    if o {
        x.uid = Some(u.arbitrary::<u32>().unwrap_or(0) as u64);
        x.gid = Some(u.arbitrary::<u16>().unwrap_or(0) as u64);
    }
    if c {
        let n = u.int_in_range(1..=8).unwrap_or(1);
        let comment: Vec<u8> = (0..n).map(|_| b'a' + u.int_in_range(0..=25).unwrap_or(0)).collect();
        x.comment = Some(comment.into());
    }
    x
}

type Paths = std::collections::BTreeMap<Vec<Vec<u8>>, Want>;

/// A path of 1 to 3 names of `a` to `d`.
fn path(u: &mut Unstructured) -> Option<Vec<Vec<u8>>> {
    let n = u.int_in_range(1..=3).ok()?;
    (0..n).map(|_| u.choose(&[b"a", b"b", b"c", b"d"]).ok().map(|n| n.to_vec())).collect()
}

/// Node `w` at path `p` merged into `paths` (spec, "Merging segments"): a directory merged with
/// one there (its extra replacing that one's where it is not null), its parents too (a segment
/// declares them, of null extras); any other node replacing what is there, with what is under
/// it.
fn merge(paths: &mut Paths, p: &[Vec<u8>], w: Want) {
    for k in 1..p.len() {
        merge(paths, &p[..k], Want::Dir(Extra::default()));
    }
    // (a directory merged into one: its extra where it has one, else the one there)
    if let (Want::Dir(x), Some(Want::Dir(y))) = (&w, paths.get_mut(p)) {
        if !x.is_empty() {
            *y = x.clone();
        }
        return;
    }
    paths.retain(|q, _| !q.starts_with(p));
    paths.insert(p.to_vec(), w);
}

/// A segment's tree as it is made: by path, its entry.
#[derive(Default)]
struct Segment {
    t: Tree,
    at: std::collections::HashMap<Vec<Vec<u8>>, usize>,
}

impl Segment {
    /// Node `node` of `extra` at path `p`, its parents directories (declared where they are not,
    /// of extras of `u` a quarter of the time, else null, merged into `want` as they are): its
    /// entry, or None where the path or a parent is taken by another node.
    fn put(
        &mut self,
        u: &mut Unstructured,
        want: &mut Paths,
        p: &[Vec<u8>],
        node: EntryNode,
        extra: Extra,
    ) -> Option<usize> {
        let not_dir = |k: usize| self.at.get(&p[..k]).is_some_and(|&e| self.t.entries[e].node != EntryNode::Dir);
        if self.at.contains_key(p) || (1..p.len()).any(not_dir) {
            return None;
        }
        let mut parent = None;
        for k in 1..p.len() {
            parent = Some(match self.at.get(&p[..k]) {
                Some(&e) => e,
                None => {
                    let x = if u.ratio(1, 4).unwrap_or(false) { self::extra(u) } else { Extra::default() };
                    merge(want, &p[..k], Want::Dir(x.clone()));
                    self.push(parent, &p[..k], EntryNode::Dir, x)
                }
            });
        }
        Some(self.push(parent, p, node, extra))
    }

    fn push(&mut self, parent: Option<usize>, p: &[Vec<u8>], node: EntryNode, extra: Extra) -> usize {
        let name = p.last().unwrap().clone();
        self.t.entries.push(Entry { parent, name, node, extra });
        self.at.insert(p.to_vec(), self.t.entries.len() - 1);
        self.t.entries.len() - 1
    }
}
