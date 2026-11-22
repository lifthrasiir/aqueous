//! A formula ([`Formula`]): its streams and root, made by recompressors and changed by the archive
//! encoder (compacted, classes merged, slices shared, `either`s resolved).

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Formula {
    pub streams: Vec<Stream>,
    pub root: Node,
    /// The applications of several outputs, in order: the inputs of one take only outputs of
    /// those before it.
    pub shared: Vec<Shared>,
}

impl Default for Formula {
    fn default() -> Formula {
        Formula { streams: Vec::new(), root: Node::Concat(Vec::new()), shared: Vec::new() }
    }
}

impl Formula {
    /// `data` as it is: a slice of a single stream.
    pub fn raw(class: impl Into<Cow<'static, str>>, data: Vec<u8>) -> Formula {
        let mut f = Formula::default();
        f.root = f.push(class, data);
        f
    }

    /// Adds a stream of [`Kind::Unknown`], returning a slice of all of it.
    pub fn push(&mut self, class: impl Into<Cow<'static, str>>, data: impl Into<Bytes>) -> Node {
        self.push_as(class, Kind::Unknown, data)
    }

    /// Adds a stream of `kind`, returning a slice of all of it.
    pub fn push_as(&mut self, class: impl Into<Cow<'static, str>>, kind: Kind, data: impl Into<Bytes>) -> Node {
        self.push_stream(Stream { class: class.into(), kind, content: false, apart: false, data: data.into() })
    }

    /// Adds a stream of [`Kind::Unknown`] that is [`Stream::apart`], returning a slice of all of it.
    pub fn push_apart(&mut self, class: impl Into<Cow<'static, str>>, data: impl Into<Bytes>) -> Node {
        self.push_stream(Stream {
            class: class.into(),
            kind: Kind::Unknown,
            content: false,
            apart: true,
            data: data.into(),
        })
    }

    /// Adds a stream of `kind` that is [`Stream::content`], returning a slice of all of it.
    pub fn push_content(&mut self, class: impl Into<Cow<'static, str>>, kind: Kind, data: impl Into<Bytes>) -> Node {
        self.push_stream(Stream { class: class.into(), kind, content: true, apart: false, data: data.into() })
    }

    pub(crate) fn push_stream(&mut self, s: Stream) -> Node {
        let len = s.data.len() as u64;
        self.streams.push(s);
        Node::slice(self.streams.len() - 1, 0, len)
    }

    /// Adds the application `a` of outputs of `sizes` bytes (its `len` set to their sum),
    /// returning the outputs to be placed in the formula, each once.
    pub fn share(&mut self, a: Apply, sizes: Vec<u64>) -> Vec<Node> {
        self.push_shared(a, sizes, false)
    }

    /// [`Formula::share`] for a module of one output, the concatenation of the outputs of
    /// `sizes` bytes, which are split from it.
    pub fn split(&mut self, a: Apply, sizes: Vec<u64>) -> Vec<Node> {
        self.push_shared(a, sizes, true)
    }

    pub(crate) fn push_shared(&mut self, mut a: Apply, sizes: Vec<u64>, split: bool) -> Vec<Node> {
        a.len = sizes.iter().sum();
        let shared = self.shared.len();
        let outputs = sizes.iter().enumerate().map(|(index, &len)| Node::Output { shared, index, len }).collect();
        self.shared.push(Shared { of: Of::Apply(a), sizes, split });
        outputs
    }

    /// Adds the Patch instruction of `patch` against `source` ([`Of::Patch`]), its output split
    /// into outputs of `sizes` bytes, returning them to be placed in the formula, each once.
    pub fn split_patch(&mut self, source: Node, patch: Node, sizes: Vec<u64>) -> Vec<Node> {
        let shared = self.shared.len();
        let outputs = sizes.iter().enumerate().map(|(index, &len)| Node::Output { shared, index, len }).collect();
        let len = sizes.iter().sum();
        self.shared.push(Shared { of: Of::Patch { inputs: [source, patch], len }, sizes, split: true });
        outputs
    }

    /// Adds `node` cut into outputs of `sizes` bytes ([`Of::Node`]), their sum its length,
    /// returning them to be placed in the formula, each once.
    pub fn split_node(&mut self, node: Node, sizes: Vec<u64>) -> Vec<Node> {
        debug_assert_eq!(sizes.iter().sum::<u64>(), node.len());
        let shared = self.shared.len();
        let outputs = sizes.iter().enumerate().map(|(index, &len)| Node::Output { shared, index, len }).collect();
        self.shared.push(Shared { of: Of::Node(node), sizes, split: true });
        outputs
    }

    /// Takes over the streams and the applications of several outputs of `child`, returning
    /// its root to be placed in this formula.
    pub fn embed(&mut self, child: Formula) -> Node {
        let (streams, shared) = (self.streams.len(), self.shared.len());
        let mut root = child.root;
        root.shift(streams, shared);
        self.streams.extend(child.streams);
        for mut s in child.shared {
            s.inputs_mut().iter_mut().for_each(|n| n.shift(streams, shared));
            self.shared.push(s);
        }
        root
    }

    /// Puts the applications of several outputs in `order` (the old index of each, all once),
    /// their outputs taken where they were.
    pub fn reorder_shared(&mut self, order: &[usize]) {
        let mut to = vec![usize::MAX; self.shared.len()];
        for (new, &old) in order.iter().enumerate() {
            debug_assert_eq!(to[old], usize::MAX, "each once");
            to[old] = new;
        }
        debug_assert!(order.len() == to.len());
        let mut old: Vec<Option<Shared>> = std::mem::take(&mut self.shared).into_iter().map(Some).collect();
        self.shared = order.iter().map(|&i| old[i].take().expect("each once")).collect();
        let mut remap = |n: &mut Node| {
            if let Node::Output { shared, .. } = n {
                *shared = to[*shared];
            }
        };
        self.shared.iter_mut().flat_map(|s| s.inputs_mut()).for_each(|n| n.visit_mut(&mut remap));
        self.root.visit_mut(&mut remap);
    }

    /// Makes [`Stream::content`] every stream sliced in an input of an application that its
    /// module tells is data as it is ([`Module::contents`]), not in an application within it.
    pub fn mark_contents(&mut self) {
        fn walk(n: &Node, content: bool, out: &mut [bool]) {
            match n {
                &Node::Slice { stream, .. } => {
                    if let Some(o) = out.get_mut(stream) {
                        *o |= content;
                    }
                }
                Node::Concat(ns) | Node::Pack(ns) => ns.iter().for_each(|n| walk(n, content, out)),
                Node::Apply(a) => apply(a, out),
                Node::Patch(p) => [&p.source, &p.patch].into_iter().for_each(|n| walk(n, false, out)),
                Node::Either(e) => [&e.kept, &e.made].into_iter().for_each(|n| walk(n, content, out)),
                Node::Output { .. } | Node::ModuleBytes(..) | Node::ResourceBytes(_) => {}
            }
        }
        fn apply(a: &Apply, out: &mut [bool]) {
            for (i, n) in a.inputs.iter().enumerate() {
                walk(n, a.module.contents.contains(&i), out);
            }
        }
        let mut marks = vec![false; self.streams.len()];
        for s in &self.shared {
            match &s.of {
                Of::Apply(a) => apply(a, &mut marks),
                Of::Patch { inputs, .. } => inputs.iter().for_each(|n| walk(n, false, &mut marks)),
                Of::Node(n) => walk(n, false, &mut marks),
            }
        }
        walk(&self.root, false, &mut marks);
        for (s, &inside) in self.streams.iter_mut().zip(&marks) {
            s.content |= inside;
        }
    }

    /// Calls `f` on every node of the inputs of the applications of several outputs, in order,
    /// then of the root, parents first.
    pub fn visit<'a>(&'a self, f: &mut impl FnMut(&'a Node)) {
        self.shared.iter().flat_map(|s| s.inputs()).for_each(|n| n.visit(f));
        self.root.visit(f);
    }

    /// Replaces every slice of the inputs of the applications of several outputs and of the
    /// root with what `f` makes of it, of `(stream, start, len)`; what it makes is not visited.
    pub fn map_slices(&mut self, f: &mut impl FnMut(usize, u64, u64) -> Node) {
        self.shared.iter_mut().flat_map(|s| s.inputs_mut()).for_each(|n| n.map_slices(f));
        self.root.map_slices(f);
    }

    /// Calls `f` on every slice of the inputs of the applications of several outputs and of the
    /// root, as `(stream, start, len)`, which it may change.
    pub fn slices_mut(&mut self, f: &mut impl FnMut(&mut usize, &mut u64, &mut u64)) {
        self.shared.iter_mut().flat_map(|s| s.inputs_mut()).for_each(|n| n.slices_mut(f));
        self.root.slices_mut(f);
    }

    /// The alternatives of the formula ([`Node::Either`]), each with how many are around it, in
    /// the order [`Formula::resolve`] takes them: those in the applications of several outputs,
    /// then the root's, each after those within it.
    pub fn eithers(&self) -> Vec<(usize, &Either)> {
        fn walk<'a>(n: &'a Node, depth: usize, out: &mut Vec<(usize, &'a Either)>) {
            match n {
                Node::Either(e) => {
                    [&e.kept, &e.made].into_iter().for_each(|n| walk(n, depth + 1, out));
                    out.push((depth, e));
                }
                Node::Concat(ns) | Node::Pack(ns) => ns.iter().for_each(|n| walk(n, depth, out)),
                Node::Apply(a) => a.inputs.iter().for_each(|n| walk(n, depth, out)),
                Node::Patch(p) => [&p.source, &p.patch].into_iter().for_each(|n| walk(n, depth, out)),
                Node::Slice { .. } | Node::Output { .. } | Node::ModuleBytes(..) | Node::ResourceBytes(_) => {}
            }
        }
        let mut out = Vec::new();
        self.shared.iter().flat_map(|s| s.inputs()).chain([&self.root]).for_each(|n| walk(n, 0, &mut out));
        out
    }

    /// Puts in place of each alternative ([`Node::Either`]) what `made` tells of it, in the
    /// order of [`Formula::eithers`] (true: the formula made, false: the data kept), and drops
    /// what the others took ([`Formula::compact`]). A concatenation holding one is made again
    /// ([`Node::concat`]): what is chosen, a concatenation of slices of the data say, spliced
    /// into it, as it would have been made without alternatives. The output is the same. Panics
    /// where `made` is not of as many.
    pub fn resolve(&mut self, made: &[bool]) {
        self.choose(made, false);
        self.drop_shared_untaken();
        self.compact();
    }

    /// [`Formula::resolve`] of a formula whose root's parts are each its own (the files of a
    /// batch): the root not made again, its parts staying its parts.
    pub fn resolve_parts(&mut self, made: &[bool]) {
        self.choose(made, true);
        self.drop_shared_untaken();
        self.compact();
    }

    /// The formula with every alternative of it the formula made (`made`) or the data kept, as
    /// it would have been made so: the streams that only the others took dropped (the rest in
    /// their order, their bytes as they are; a stream no slice took before stays), and the
    /// applications of several outputs nothing takes. To size it as either.
    pub fn chosen(&self, made: bool) -> Formula {
        let used = |f: &Formula| {
            let mut used = vec![false; f.streams.len()];
            let mut mark = |n: &Node| {
                n.visit(&mut |n| {
                    if let &Node::Slice { stream, .. } = n
                        && let Some(u) = used.get_mut(stream)
                    {
                        *u = true;
                    }
                })
            };
            f.shared.iter().flat_map(|s| s.inputs()).for_each(&mut mark);
            mark(&f.root);
            used
        };
        let before = used(self);
        let mut f = self.clone();
        f.choose(&vec![made; f.eithers().len()], false);
        f.drop_shared_untaken();
        let after = used(&f);
        let mut to = vec![usize::MAX; f.streams.len()];
        let mut streams = Vec::new();
        for (i, s) in std::mem::take(&mut f.streams).into_iter().enumerate() {
            if after[i] || !before[i] {
                to[i] = streams.len();
                streams.push(s);
            }
        }
        f.streams = streams;
        f.slices_mut(&mut |stream, _, _| *stream = to[*stream]);
        f
    }

    /// [`Formula::resolve`] but for dropping what the others took; the root's parts kept as
    /// they are where `parts`.
    pub(crate) fn choose(&mut self, made: &[bool], parts: bool) {
        fn walk(n: &mut Node, made: &mut std::slice::Iter<bool>) {
            match n {
                Node::Either(e) => {
                    [&mut e.kept, &mut e.made].into_iter().for_each(|n| walk(n, made));
                    let take = *made.next().expect("a choice of each alternative");
                    let e = std::mem::replace(n, Node::Concat(Vec::new()));
                    let Node::Either(e) = e else { unreachable!("an alternative") };
                    *n = if take { e.made } else { e.kept };
                }
                // (made again where it held one: what is chosen spliced in)
                Node::Concat(ns) => {
                    let held = ns.iter().any(|n| matches!(n, Node::Either(_)));
                    ns.iter_mut().for_each(|n| walk(n, made));
                    if held {
                        *n = Node::concat(std::mem::take(ns));
                    }
                }
                Node::Pack(ns) => ns.iter_mut().for_each(|n| walk(n, made)),
                Node::Apply(a) => a.inputs.iter_mut().for_each(|n| walk(n, made)),
                Node::Patch(p) => [&mut p.source, &mut p.patch].into_iter().for_each(|n| walk(n, made)),
                Node::Slice { .. } | Node::Output { .. } | Node::ModuleBytes(..) | Node::ResourceBytes(_) => {}
            }
        }
        let mut it = made.iter();
        for s in &mut self.shared {
            s.inputs_mut().iter_mut().for_each(|n| walk(n, &mut it));
        }
        match &mut self.root {
            Node::Concat(ns) if parts => ns.iter_mut().for_each(|n| walk(n, &mut it)),
            root => walk(root, &mut it),
        }
        assert!(it.next().is_none(), "as many choices as alternatives");
    }

    /// Drops the applications of several outputs whose outputs nothing takes (those of the
    /// alternatives not chosen, [`Formula::resolve`]), the others in their order.
    pub(crate) fn drop_shared_untaken(&mut self) {
        let outputs_of = |n: &Node, out: &mut Vec<usize>| {
            n.visit(&mut |n| {
                if let &Node::Output { shared, .. } = n {
                    out.push(shared);
                }
            })
        };
        let mut taken = vec![false; self.shared.len()];
        let mut todo = Vec::new();
        outputs_of(&self.root, &mut todo);
        while let Some(i) = todo.pop() {
            if taken.get(i).is_none_or(|&t| t) {
                continue;
            }
            taken[i] = true;
            self.shared[i].inputs().iter().for_each(|n| outputs_of(n, &mut todo));
        }
        if taken.iter().all(|&t| t) {
            return;
        }
        let kept: Vec<usize> = (0..self.shared.len()).filter(|&i| taken[i]).collect();
        let mut to = vec![usize::MAX; self.shared.len()];
        kept.iter().enumerate().for_each(|(new, &old)| to[old] = new);
        let mut old: Vec<Option<Shared>> = std::mem::take(&mut self.shared).into_iter().map(Some).collect();
        self.shared = kept.iter().map(|&i| old[i].take().expect("each once")).collect();
        let mut remap = |n: &mut Node| {
            if let Node::Output { shared, .. } = n {
                *shared = to[*shared];
            }
        };
        self.shared.iter_mut().flat_map(|s| s.inputs_mut()).for_each(|n| n.visit_mut(&mut remap));
        self.root.visit_mut(&mut remap);
    }

    /// Drops what no slice points to: the bytes of a stream outside every slice of it, and
    /// streams without slices. The output is the same: a stream with a slice out of it is
    /// kept whole, and so is one sunk ([`Bytes::sunk`]), and slices of streams that do not
    /// exist still point to none.
    pub fn compact(&mut self) {
        // Per stream, the ranges of its slices, merged into (start, end, start in the new data).
        let mut ranges = vec![Vec::new(); self.streams.len()];
        self.slices_mut(&mut |&mut s, &mut a, &mut l| {
            if let Some(r) = ranges.get_mut(s) {
                r.push((a, a.saturating_add(l), 0));
            }
        });
        let old = self.streams.len();
        let mut index = vec![usize::MAX; old];
        let mut streams = Vec::new();
        for (i, (s, r)) in std::mem::take(&mut self.streams).into_iter().zip(&mut ranges).enumerate() {
            if r.is_empty() {
                continue;
            }
            index[i] = streams.len();
            // (a stream sunk kept whole: its slices are where its bytes are in the sink)
            if s.data.sunk_in().is_some() || r.iter().any(|&(_, b, _)| b > s.data.len() as u64) {
                *r = vec![(0, u64::MAX, 0)];
                streams.push(s);
                continue;
            }
            r.sort_unstable();
            let mut merged: Vec<(u64, u64, u64)> = Vec::new();
            for &(a, b, _) in r.iter() {
                match merged.last_mut() {
                    Some(m) if a <= m.1 => m.1 = m.1.max(b),
                    _ => merged.push((a, b, 0)),
                }
            }
            let mut at = 0;
            for m in &mut merged {
                m.2 = at;
                at += m.1 - m.0;
            }
            let data = if s.data.is_shared() && at < s.data.len() as u64 {
                // (one range kept of bytes known, a file mapped, say: shared as it is, not copied;
                // else bytes shared copied only as far as they are kept, to the store where one is
                // set: they were most likely of it)
                let one = match &merged[..] {
                    [m] => store::find(&s.data[m.0 as usize..m.1 as usize]),
                    _ => None,
                };
                one.unwrap_or_else(|| {
                    let mut d = store::Writer::new(Some(at as usize));
                    for m in &merged {
                        d.write(&s.data[m.0 as usize..m.1 as usize]);
                    }
                    d.into_bytes()
                })
            } else {
                // in place: the bytes kept only move towards the start
                let mut data = s.data;
                for m in &merged {
                    if m.0 != m.2 {
                        data.copy_within(m.0 as usize..m.1 as usize, m.2 as usize);
                    }
                }
                data.truncate(at as usize);
                data.shrink_to_fit();
                data
            };
            *r = merged;
            streams.push(Stream { data, ..s });
        }
        let new = streams.len();
        self.slices_mut(&mut |s, a, _| match ranges.get(*s) {
            Some(r) => {
                let m = r[r.partition_point(|m| m.0 <= *a) - 1];
                *a = m.2 + (*a - m.0);
                *s = index[*s];
            }
            None => *s = *s - old + new,
        });
        self.streams = streams;
    }

    /// Takes the slices of at least `min` bytes from other slices that hold the same bytes,
    /// and drops what is left without a slice ([`Formula::compact`]): a file and the same file
    /// in an archive, say, are then stored once. From the shortest slice on, each is cut into
    /// slices kept before that it starts with, one after another (the longest each time; else
    /// the longest start of one, of at least half of it), and what follows of its own; one cut
    /// nowhere is kept. A piece found again right after itself is a run of the same bytes
    /// (padding, zeros), which compresses to little: the rest of the run is kept of the slice's
    /// own bytes instead of cut into a slice of each copy. Slices past the end of their stream,
    /// and those of streams sunk ([`Bytes::sunk`], which a sink shares as it takes them), are
    /// left as they are. The concatenations below the root's parts are then flattened
    /// ([`Node::flatten`]), the root's parts (a batch's files) staying its parts. The output is
    /// the same.
    pub fn share_slices(&mut self, min: usize) {
        type Sl = (usize, u64, u64);
        let mut slices: Vec<Sl> = Vec::new();
        self.visit(&mut |n| {
            if let &Node::Slice { stream, start, len } = n
                && len >= min as u64
                && self.streams.get(stream).is_some_and(|s| {
                    s.data.sunk_in().is_none() && start.checked_add(len).is_some_and(|e| e <= s.data.len() as u64)
                })
            {
                slices.push((stream, start, len));
            }
        });
        slices.sort_unstable_by_key(|&(s, a, l)| (l, s, a));
        slices.dedup();
        let bytes = |&(s, a, l): &Sl| &self.streams[s].data[a as usize..][..l as usize];
        let head = |b: &[u8]| xxhash_rust::xxh3::xxh3_64(&b[..min]);
        // the slices kept, by their first `min` bytes
        let mut kept: std::collections::HashMap<u64, Vec<Sl>> = Default::default();
        let mut to: std::collections::HashMap<Sl, Vec<Sl>> = Default::default();
        for x in slices {
            let b = bytes(&x);
            let (mut at, mut pieces) = (0usize, Vec::new());
            while b.len() - at >= min {
                let rest = &b[at..];
                // (a kept slice the rest starts with, the longest; else the start of one, at least
                // half of it: of a text coded alike, its end cut by a token)
                let ks = kept.get(&head(rest)).map_or(&[][..], |ks| &ks[..]);
                let whole = ks.iter().filter(|k| rest.starts_with(bytes(k))).max_by_key(|k| k.2).copied();
                let found = whole.or_else(|| {
                    let common = |k: &Sl| bytes(k).iter().zip(rest).position(|(a, b)| a != b).unwrap_or(0);
                    ks.iter()
                        .map(|k| (k, common(k)))
                        .filter(|&(k, n)| n >= min && 2 * n as u64 >= k.2)
                        .max_by_key(|&(_, n)| n)
                        .map(|(k, n)| (k.0, k.1, n as u64))
                });
                let Some(k) = found else { break };
                if pieces.last() == Some(&k) {
                    // (the run of its copies, of its length as the period, of the slice's own)
                    let p = k.2 as usize;
                    let run = b[at..].iter().zip(&b[at - p..]).take_while(|(a, b)| a == b).count();
                    pieces.push((x.0, x.1 + at as u64, run as u64));
                    at += run;
                    continue;
                }
                pieces.push(k);
                at += k.2 as usize;
            }
            if at == 0 {
                kept.entry(head(b)).or_default().push(x);
                continue;
            }
            if at < b.len() {
                let rest = (x.0, x.1 + at as u64, x.2 - at as u64);
                match to.get(&rest) {
                    Some(ps) => pieces.extend_from_slice(ps),
                    None => {
                        if rest.2 >= min as u64 {
                            kept.entry(head(bytes(&rest))).or_default().push(rest);
                        }
                        pieces.push(rest);
                    }
                }
            }
            to.insert(x, pieces);
        }
        if to.is_empty() {
            self.flatten_parts();
            return;
        }
        // (the pieces are slices kept, not cut again)
        let mut cut = |n: &mut Node| {
            if let &mut Node::Slice { stream, start, len } = n
                && let Some(ps) = to.get(&(stream, start, len))
            {
                *n = Node::concat(ps.iter().map(|&(s, a, l)| Node::slice(s, a, l)));
            }
        };
        self.shared.iter_mut().flat_map(|s| s.inputs_mut()).for_each(|n| n.visit_mut(&mut cut));
        self.root.visit_mut(&mut cut);
        self.flatten_parts();
        self.compact();
    }

    /// [`Node::flatten`] of the inputs of the applications of several outputs and of the root's
    /// parts, or of the root where it is not a concatenation: the root's parts stay its parts.
    pub(crate) fn flatten_parts(&mut self) {
        self.shared.iter_mut().flat_map(|s| s.inputs_mut()).for_each(Node::flatten);
        match &mut self.root {
            Node::Concat(ns) => ns.iter_mut().for_each(Node::flatten),
            n => n.flatten(),
        }
    }

    /// Puts the streams of each class and kind one after another, in one stream (in the place
    /// of the first): they are alike, and archive smaller together (the archive encoder
    /// compresses each stream on its own). The streams of [`Kind::Compressed`] (mostly stored as
    /// they are), the contents, those [`Stream::apart`], and those that a slice goes past the end
    /// of are left apart. The output is the same.
    pub fn merge_classes(&mut self) {
        self.merge_classes_within(u64::MAX);
    }

    /// [`Formula::merge_classes`], into streams of `limit` bytes at most: a stream that would
    /// take one past it starts another of its class and kind (one longer is left as it is).
    /// Where a class and kind has several streams longer than half of it, `k` of them (at most
    /// `merge:group-most`), they are cut into pieces of a `k`th of it (of `merge:piece-min` at least),
    /// and each piece put with those of the others most like it, if any, up to `k` pieces of
    /// different streams (versions of a program's code, say): the pieces of a stream not so
    /// put are one stream, and a stream none of whose pieces is put so is left whole.
    pub fn merge_classes_within(&mut self, limit: u64) {
        let (to, groups) = self.merge_plan(limit, false);
        let mut old: Vec<Option<Stream>> = std::mem::take(&mut self.streams).into_iter().map(Some).collect();
        // (the pieces of each stream not copied yet: it is let go after its last)
        let mut left = vec![0usize; old.len()];
        groups.iter().flatten().for_each(|p| left[p.0] += 1);
        let mut streams = Vec::with_capacity(groups.len());
        for g in &groups {
            let (s, a, b) = g[0];
            let o = old[s].as_ref().expect("a stream in one group");
            // (a whole stream first taken, of its length at once, the others let go as they are copied)
            let (mut first, rest) = if a == 0 && b == o.data.len() as u64 {
                left[s] -= 1;
                (old[s].take().unwrap(), &g[1..])
            } else {
                (o.clone_empty(), &g[..])
            };
            if !rest.is_empty() {
                let total: u64 = g.iter().map(|p| p.2 - p.1).sum();
                let data = first.data.to_mut();
                data.reserve_exact(total as usize - data.len());
                for &(s, a, b) in rest {
                    data.extend_from_slice(
                        &old[s].as_ref().expect("a stream in one group").data[a as usize..b as usize],
                    );
                    left[s] -= 1;
                    if left[s] == 0 {
                        old[s] = None;
                    }
                }
            }
            streams.push(first);
        }
        self.remap(&to, groups.len());
        self.streams = streams;
    }

    /// [`Formula::merge_classes`] as the formula is archived, without moving the bytes: the
    /// contents merged too, as nothing recompresses them any more (a stream of each file of an
    /// image, say), as a tree's are. The formula of the streams merged (their bytes empty), and
    /// of each of its streams, the pieces of those of `self` it is the concatenation of, in
    /// order. Streams sunk ([`Bytes::sunk`]) are of no group: their slices are of the sink's
    /// streams, stream `k` of it after the groups (the formula's streams `groups.len() + k`,
    /// which whoever writes it adds).
    pub fn merged(&self) -> (Formula, Vec<Vec<Piece>>) {
        let (to, groups) = self.merge_plan(u64::MAX, true);
        let streams = groups
            .iter()
            .map(|g| {
                let s = &self.streams[g[0].0];
                let content = g.iter().all(|p| self.streams[p.0].content);
                Stream { content, ..s.clone_empty() }
            })
            .collect();
        let mut f = Formula { streams: Vec::new(), root: self.root.clone(), shared: self.shared.clone() };
        f.remap(&to, groups.len());
        f.streams = streams;
        (f, groups)
    }

    /// Of each stream, its pieces in order, each as where it starts, the merged stream it goes
    /// to and where in it; and of each merged stream, the pieces it is made of, in order
    /// ([`Formula::merge_classes_within`]); the contents merged as well where `contents`.
    pub(crate) fn merge_plan(&self, limit: u64, contents: bool) -> (Vec<Vec<At>>, Vec<Vec<Piece>>) {
        let n = self.streams.len();
        let lens: Vec<u64> = self.streams.iter().map(|s| s.data.len() as u64).collect();
        let mut past = vec![false; n];
        let mut visit = |n: &Node| {
            n.visit(&mut |n| {
                if let &Node::Slice { stream, start, len } = n
                    && let Some(p) = past.get_mut(stream)
                {
                    *p |= start.checked_add(len).is_none_or(|e| e > lens[stream]);
                }
            })
        };
        self.shared.iter().flat_map(|s| s.inputs()).for_each(&mut visit);
        visit(&self.root);
        let alike = |i: usize| {
            let s = &self.streams[i];
            s.kind != Kind::Compressed && (contents || !s.content) && !s.apart && !past[i] && s.data.sunk_in().is_none()
        };

        // the streams longer than half the limit, of each class and kind, cut into pieces of a
        // share of it (of each stream, their indices) and grouped, each group of the first
        // piece of it (with the others of it, if any)
        let (mut cut, mut pieces, mut groups_of) = (vec![Vec::new(); n], Vec::<Piece>::new(), Vec::new());
        let mut members: Vec<Vec<usize>> = Vec::new();
        let mut long: Vec<(&str, Kind, Vec<usize>)> = Vec::new();
        for (i, s) in self.streams.iter().enumerate().filter(|&(i, _)| alike(i) && lens[i] > limit / 2) {
            match long.iter_mut().find(|l| l.0 == s.class && l.1 == s.kind) {
                Some(l) => l.2.push(i),
                None => long.push((&s.class, s.kind, vec![i])),
            }
        }
        for (_, _, ss) in long.iter().filter(|l| l.2.len() > 1) {
            let most = ss.len().min(GROUP_MOST.get());
            let size = limit / most as u64;
            if size < PIECE_MIN.get() as u64 {
                continue;
            }
            let from = pieces.len();
            for &i in ss {
                for a in (0..lens[i]).step_by(size as usize) {
                    cut[i].push(pieces.len());
                    pieces.push((i, a, (a + size).min(lens[i])));
                }
            }
            let of: Vec<(usize, &[u8])> =
                pieces[from..].iter().map(|&(s, a, b)| (s, &self.streams[s].data[a as usize..b as usize])).collect();
            groups_of.extend(pieces::group(&of, most).into_iter().map(|g| from + g));
            members.resize(pieces.len(), Vec::new());
            (from..pieces.len()).for_each(|p| members[groups_of[p]].push(p));
        }

        let mut firsts: Vec<(Cow<'static, str>, Kind, usize)> = Vec::new();
        let (mut groups, mut glens): (Vec<Vec<Piece>>, Vec<u64>) = (Vec::with_capacity(n), Vec::with_capacity(n));
        let mut placed = vec![false; pieces.len()];
        for (i, s) in self.streams.iter().enumerate() {
            // (a stream sunk is of the sink's stream, after the groups)
            if s.data.sunk_in().is_some() {
                continue;
            }
            if cut[i].iter().any(|&p| members[groups_of[p]].len() > 1) {
                // each piece with those of its group (of streams after: one before took them
                // already), and the rest one after another
                let mut rest: Option<usize> = None;
                for &p in &cut[i] {
                    if placed[p] {
                        continue;
                    }
                    let (_, a, b) = pieces[p];
                    match (&members[groups_of[p]][..], rest) {
                        (ps @ [_, _, ..], _) => {
                            ps.iter().for_each(|&q| placed[q] = true);
                            groups.push(ps.iter().map(|&q| pieces[q]).collect());
                            glens.push(ps.iter().map(|&q| pieces[q].2 - pieces[q].1).sum());
                        }
                        (_, Some(r)) => {
                            match groups[r].last_mut() {
                                Some(l) if l.2 == a => l.2 = b,
                                _ => groups[r].push(pieces[p]),
                            }
                            glens[r] += b - a;
                        }
                        (_, None) => {
                            rest = Some(groups.len());
                            groups.push(vec![pieces[p]]);
                            glens.push(b - a);
                        }
                    }
                }
                continue;
            }
            let whole = (i, 0, lens[i]);
            match firsts.iter_mut().find(|f| alike(i) && f.0 == s.class && f.1 == s.kind) {
                Some(&mut (_, _, j)) if glens[j] + lens[i] <= limit => {
                    groups[j].push(whole);
                    glens[j] += lens[i];
                }
                Some(first) => {
                    first.2 = groups.len();
                    groups.push(vec![whole]);
                    glens.push(lens[i]);
                }
                None => {
                    if alike(i) {
                        firsts.push((s.class.clone(), s.kind, groups.len()));
                    }
                    groups.push(vec![whole]);
                    glens.push(lens[i]);
                }
            }
        }
        let mut to = vec![Vec::new(); n];
        for (g, ps) in groups.iter().enumerate() {
            ps.iter()
                .scan(0, |at, &(s, a, b)| Some((s, a, std::mem::replace(at, *at + b - a))))
                .for_each(|(s, a, at)| to[s].push((a, g, at)));
        }
        to.iter_mut().for_each(|t| t.sort_unstable());
        for (t, s) in to.iter_mut().zip(&self.streams) {
            if let Some(k) = s.data.sunk_in() {
                *t = vec![(0, groups.len() + k, 0)];
            }
        }
        (to, groups)
    }

    /// Slices of each stream made slices of the merged streams its pieces went to, at `to`
    /// ([`Formula::merge_plan`]), a slice over several pieces their concatenation; of a stream
    /// past them, of the same past `new` merged streams.
    pub(crate) fn remap(&mut self, to: &[Vec<At>], new: usize) {
        let n = to.len();
        let mut map = |s: usize, mut a: u64, mut l: u64| {
            let Some(ps) = to.get(s) else { return Node::slice(s - n + new, a, l) };
            let mut k = ps.partition_point(|p| p.0 <= a) - 1;
            let mut out = Vec::new();
            loop {
                let (start, g, at) = ps[k];
                let m = l.min(ps.get(k + 1).map_or(u64::MAX, |p| p.0) - a);
                out.push(Node::slice(g, at + a - start, m));
                (a, l, k) = (a + m, l - m, k + 1);
                if l == 0 {
                    break;
                }
            }
            if out.len() == 1 { out.pop().unwrap() } else { Node::concat(out) }
        };
        self.shared.iter_mut().flat_map(|s| s.inputs_mut()).for_each(|n| n.map_slices(&mut map));
        self.root.map_slices(&mut map);
    }

    /// Bytes of the output, as declared.
    pub fn len(&self) -> u64 {
        self.root.len()
    }

    pub fn is_empty(&self) -> bool {
        self.root.is_empty()
    }

    /// `data`, the contents of the entries of an archive, as streams by kind: `ends` gives the
    /// kind of each entry's contents and their end in `data`. Returns the node of their slices,
    /// in order: a slice for each entry of a [`Kind::Format`] or of at least `own` bytes, those
    /// of other kinds one after another being one. The contents of [`Kind::Unknown`] are stream
    /// `raw` (of this formula, empty), cut out of `data` in place; those of other kinds are
    /// streams added after the others, of class `{prefix}/{kind}` (`{prefix}/{name}` for a
    /// format).
    pub fn push_by_kind(
        &mut self,
        raw: usize,
        data: Vec<u8>,
        ends: &[(Kind, usize)],
        prefix: &str,
        own: usize,
    ) -> Node {
        let mut by = ByKind::new(own);
        let mut start = 0;
        for &(kind, end) in ends {
            by.add(kind, &data[start..end]);
            start = end;
        }
        drop(data);
        by.push(self, raw, prefix)
    }

    /// The modules and resources of the formula's blocks, each once: those of its applications
    /// ([`Formula::applies`]) in order, then those whose bytes it takes ([`Node::ModuleBytes`],
    /// [`Node::ResourceBytes`]).
    pub fn blocks(&self) -> (Vec<Module>, Vec<&Resource>) {
        let (mut modules, mut resources): (Vec<Module>, Vec<&Resource>) = (Vec::new(), Vec::new());
        let mut module = |m: Module| {
            if !modules.contains(&m) {
                modules.push(m);
            }
        };
        let mut resource = |r| {
            if !resources.contains(&r) {
                resources.push(r);
            }
        };
        for a in self.applies() {
            module(a.module);
            a.resources.iter().for_each(&mut resource);
        }
        for n in self.shared.iter().flat_map(|s| s.inputs()).chain([&self.root]) {
            n.visit(&mut |n| match n {
                &Node::ModuleBytes(m, _) => module(m),
                Node::ResourceBytes(r) => resource(r),
                _ => {}
            });
        }
        (modules, resources)
    }

    /// The applications, those of the root parents first, then those of several outputs.
    pub fn applies(&self) -> Vec<&Apply> {
        let mut out = Vec::new();
        self.root.visit(&mut |n| {
            if let Node::Apply(a) = n {
                out.push(&**a);
            }
        });
        for i in self.shared.iter().flat_map(|s| s.inputs()) {
            i.visit(&mut |n| {
                if let Node::Apply(a) = n {
                    out.push(&**a);
                }
            });
        }
        out.extend(self.shared.iter().filter_map(Shared::apply));
        out
    }

    /// Checks the outputs of the applications of several outputs: each taken exactly once, of
    /// the size declared, and in the inputs of an application only from those before it.
    /// Also checks that each resource whose bytes it takes is [`Resource::stored`].
    pub fn check_outputs(&self) -> Result<()> {
        let mut taken: Vec<Vec<bool>> = self.shared.iter().map(|s| vec![false; s.sizes.len()]).collect();
        let mut check = |n: &Node, before: usize| -> Result<()> {
            let mut r = Ok(());
            n.visit(&mut |n| {
                if let &Node::Output { shared, index, len } = n
                    && r.is_ok()
                {
                    r = match self.shared.get(shared).and_then(|s| s.sizes.get(index)) {
                        _ if shared >= before => {
                            Err(Error::new(format!("output #{shared}.{index} taken before it is")))
                        }
                        Some(&size) if size == len && !taken[shared][index] => {
                            taken[shared][index] = true;
                            Ok(())
                        }
                        Some(&size) if size != len => {
                            Err(Error::new(format!("output #{shared}.{index} of {len} bytes, not {size}")))
                        }
                        Some(_) => Err(Error::new(format!("output #{shared}.{index} taken twice"))),
                        None => Err(Error::new(format!("no output #{shared}.{index}"))),
                    };
                }
            });
            r
        };
        for (i, s) in self.shared.iter().enumerate() {
            if s.sizes.is_empty() || s.bytes() != s.sizes.iter().sum::<u64>() || (s.apply().is_none() && !s.split) {
                return Err(Error::new(format!("application #{i} of several outputs of bad sizes")));
            }
            s.inputs().iter().try_for_each(|n| check(n, i))?;
        }
        check(&self.root, self.shared.len())?;
        let mut stored = Ok(());
        for n in self.shared.iter().flat_map(|s| s.inputs()).chain([&self.root]) {
            n.visit(&mut |n| {
                if let Node::ResourceBytes(r) = n
                    && r.stored_data().is_none()
                {
                    stored = Err(Error::new("the bytes of a resource not stored"));
                }
            });
        }
        stored?;
        match taken.iter().position(|t| t.contains(&false)) {
            Some(i) => Err(Error::new(format!("an output of #{i} not taken"))),
            None => Ok(()),
        }
    }

    /// A formula of node `n` of this one (of no outputs of applications of several outputs) and
    /// the streams it slices, each the bytes `data` gives of it (by its index here).
    pub fn part(&self, n: &Node, data: &dyn Fn(usize) -> Bytes) -> Formula {
        let mut n = n.clone();
        let mut of: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
        let mut own: Vec<Stream> = Vec::new();
        n.slices_mut(&mut |s, _, _| {
            if let Some(st) = self.streams.get(*s) {
                *s = *of.entry(*s).or_insert_with(|| {
                    own.push(Stream { data: data(*s), ..st.clone_empty() });
                    own.len() - 1
                });
            }
        });
        Formula { streams: own, root: n, shared: Vec::new() }
    }
}

/// Indented, a node a line: `slice #stream [start, +len) CLASS` (and the kind of the stream
/// unless [`Kind::Unknown`] and not [`Stream::content`], with `content` if it is), `concat LEN`, `pack LEN`, `apply MODULE LEN` with the lengths of its
/// resources and its parameters in hex, `output #SHARED.INDEX LEN`, `patch LEN` (its source then
/// its patch), `module bytes MODULE LEN`, `resource bytes LEN`, `either LEN sizes MADE KEPT` (what
/// is kept, then what is made): first each application of
/// several outputs, `shared #I MODULE SIZES...` and its inputs, then the root.
impl fmt::Display for Formula {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fn node(fm: &Formula, n: &Node, depth: usize, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "{:1$}", "", depth * 2)?;
            match n {
                &Node::Slice { stream, start, len } => match fm.streams.get(stream) {
                    Some(s) if s.kind != Kind::Unknown || s.content => {
                        let content = if s.content { ", content" } else { "" };
                        writeln!(f, "slice #{stream} [{start}, +{len}) {} ({}{content})", s.class, s.kind)
                    }
                    s => writeln!(f, "slice #{stream} [{start}, +{len}) {}", s.map_or("?", |s| &s.class)),
                },
                Node::Concat(ns) | Node::Pack(ns) => {
                    writeln!(f, "{} {}", if let Node::Pack(_) = n { "pack" } else { "concat" }, n.len())?;
                    ns.iter().try_for_each(|n| node(fm, n, depth + 1, f))
                }
                Node::Apply(a) => {
                    write!(f, "apply {} {}", a.module, a.len)?;
                    attrs(a, f)?;
                    a.inputs.iter().try_for_each(|n| node(fm, n, depth + 1, f))
                }
                &Node::Output { shared, index, len } => writeln!(f, "output #{shared}.{index} {len}"),
                Node::ModuleBytes(m, d) => writeln!(f, "module bytes {m} {}", d.len()),
                Node::ResourceBytes(_) => writeln!(f, "resource bytes {}", n.len()),
                Node::Patch(p) => {
                    writeln!(f, "patch {}", p.len)?;
                    [&p.source, &p.patch].into_iter().try_for_each(|n| node(fm, n, depth + 1, f))
                }
                Node::Either(e) => {
                    writeln!(f, "either {} sizes {} {}", e.len, e.sizes.0, e.sizes.1)?;
                    [&e.kept, &e.made].into_iter().try_for_each(|n| node(fm, n, depth + 1, f))
                }
            }
        }
        // the resources' lengths and the parameters, then the end of the line
        fn attrs(a: &Apply, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            if !a.resources.is_empty() {
                write!(f, " resources")?;
                a.resources.iter().try_for_each(|r| write!(f, " {}", r.len()))?;
            }
            if !a.params.is_empty() {
                write!(f, " params ")?;
                a.params.iter().try_for_each(|b| write!(f, "{b:02x}"))?;
            }
            writeln!(f)
        }
        for (i, s) in self.shared.iter().enumerate() {
            write!(f, "shared #{i} {}{}", s.name(), if s.split { " split" } else { "" })?;
            s.sizes.iter().try_for_each(|n| write!(f, " {n}"))?;
            match s.apply() {
                Some(a) => attrs(a, f)?,
                None => writeln!(f)?,
            }
            s.inputs().iter().try_for_each(|n| node(self, n, 1, f))?;
        }
        node(self, &self.root, 0, f)
    }
}
