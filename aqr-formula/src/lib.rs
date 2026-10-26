//! Entry formulas (xform-design.md §4.2): what a recompressor makes of a file, as an
//! expression over the data it extracted rather than one opaque byte string.
//!
//! A [`Formula`] is a tree of [`Node`]s over [`Stream`]s:
//! - `slice`: a byte range of a stream;
//! - `concat`: nodes one after another;
//! - `pack`: nodes as a [`packed`] byte sequence, for a module taking a list of them as one
//!   input;
//! - `apply`: a [`Module`] (a pure function of its inputs, resources and parameters) applied
//!   to nodes, declaring the size of its output;
//! - `output`: an output of an application of several outputs, [`Shared`], which the formula
//!   lists apart and whose every output is taken by exactly one `output` node;
//! - `patch`: a [`patch`] of a node against another one, its source.
//!
//! Streams are not placed yet: the archive encoder lays them out into non-leaf blocks, and
//! [`Stream::class`] tells it which streams are alike (to share frames and a dictionary, §3),
//! and [`Stream::kind`] what the recompressor knows of the bytes: whether they may hold files
//! for another recompressor to look for, are one such file, or are not worth looking into.
//! What belongs to the archive is left out: `ref` to other entries, and the resource
//! declarations of `apply` (memory limit, capacities), which the archive encoder
//! measures while verifying (§6). [`Resource`]s (§5.6), the models that modules take, are
//! held by the applications, and the archive encoder stores each once.
//!
//! [`Formula::eval`] is the reference, strict evaluation: every node is materialised, with
//! the modules run by a [`Modules`] implementation.
//!
//! [`aqr`] stores a formula as an Aqueous archive of the file alone.

pub mod aqr;
pub mod packed;
pub mod patch;
pub mod probe;

use std::borrow::Cow;
use std::fmt;
use std::sync::Arc;

/// A module, by name and version of its input format. Eventually a Wasm module identified by
/// its hash; a native implementation stands in for it (§5.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Module {
    pub name: &'static str,
    pub version: u16,
}

impl fmt::Display for Module {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.name, self.version)
    }
}

/// Data extracted by a recompressor, which `slice` nodes point into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stream {
    /// What the bytes are, e.g. `x86/OP` or `flate/record`; streams of a class are alike.
    pub class: Cow<'static, str>,
    /// What the bytes are to whoever recompresses them in turn, alike for streams of a class.
    pub kind: Kind,
    /// Whether the bytes are data as they are, a file or what a file holds, rather than a
    /// representation of the recompressor's own: whoever takes the formula may recompress its
    /// slices in turn wherever they are (aqr-compress, as it does an application's content).
    pub content: bool,
    pub data: Vec<u8>,
}

/// What a recompressor knows of a stream's bytes, for whoever takes them as data to recompress
/// in turn (aqr-compress, where they are an application's content) and for the archive
/// encoder. It says what the bytes are, not what is to be done with them: that is for whoever
/// takes them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Anything: files may be anywhere in it.
    #[default]
    Unknown,
    /// Samples of an image, uncompressed (rows perhaps filtered by a predictor).
    Pixels,
    /// Text, or a syntax in text such as PDF objects.
    Text,
    /// A font program (TrueType, OpenType, CFF, Type 1).
    Font,
    /// Binary records of fixed layout, such as a cross-reference table.
    Table,
    /// Of high entropy, such as compressed or encrypted data that no recompressor takes: holds
    /// no files and does not compress, and is stored as it is.
    Compressed,
    /// A file of the format named (as aqr-compress names them: `zlib`, `png`, ...) from the
    /// first byte: recompressed as such without looking for it, and kept as it is where its
    /// recompressor turns it down (but for the parts of it the recompressor tells, [`Region`]).
    /// A name not known is taken as [`Kind::Unknown`].
    Format(&'static str),
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Kind::Unknown => f.write_str("unknown"),
            Kind::Pixels => f.write_str("pixels"),
            Kind::Text => f.write_str("text"),
            Kind::Font => f.write_str("font"),
            Kind::Table => f.write_str("table"),
            Kind::Compressed => f.write_str("compressed"),
            Kind::Format(name) => write!(f, "format={name}"),
        }
    }
}

/// What a part of a file is, as the recompressor of its format knows it: for whoever looks
/// into a file that recompressor turned down (aqr-compress, scanning it for files).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    /// The bytes of the part, in the file.
    pub span: std::ops::Range<usize>,
    pub kind: Kind,
    /// For a [`Kind::Format`] that holds data (as zlib does), the kind of that data.
    pub contents: Kind,
}

/// Bytes that applications of a module share, typically a model (§5.6): a resource block of
/// an archive. Clones share the bytes.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Resource(Arc<[u8]>);

impl Resource {
    pub fn new(data: impl Into<Arc<[u8]>>) -> Resource {
        Resource(data.into())
    }
}

impl std::ops::Deref for Resource {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for Resource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Resource({} bytes, {:016x})", self.len(), xxhash_rust::xxh64::xxh64(self, 0))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    /// `len` bytes of stream `stream` (an index into [`Formula::streams`]) from `start`.
    Slice {
        stream: usize,
        start: u64,
        len: u64,
    },
    Concat(Vec<Node>),
    /// The [`packed`] byte sequence of the nodes.
    Pack(Vec<Node>),
    Apply(Box<Apply>),
    /// Output `index` (of `len` bytes) of the application of several outputs
    /// [`Formula::shared`]`[shared]`. Each output is taken exactly once.
    Output {
        shared: usize,
        index: usize,
        len: u64,
    },
    Patch(Box<Patch>),
}

/// The output of `patch` ([`patch`]), of `len` bytes, against `source`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Patch {
    pub source: Node,
    pub patch: Node,
    pub len: u64,
}

/// An application of several outputs, which [`Node::Output`]s take one each (anywhere in the
/// formula: in its root, or in the inputs of a later one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shared {
    /// The application, its `len` the outputs' sizes added up.
    pub apply: Apply,
    /// Bytes of each output.
    pub sizes: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Apply {
    pub module: Module,
    /// Module-defined, in order: models and the like that applications share.
    pub resources: Vec<Resource>,
    /// Module-defined; small values such as a code's origin.
    pub params: Vec<u8>,
    pub inputs: Vec<Node>,
    /// Bytes of the output.
    pub len: u64,
}

impl Node {
    pub fn slice(stream: usize, start: u64, len: u64) -> Node {
        Node::Slice { stream, start, len }
    }

    /// The concatenation of `nodes`, flattened: nested concatenations are spliced in, empty
    /// slices dropped and adjacent slices of a stream merged; a single node is itself.
    pub fn concat(nodes: impl IntoIterator<Item = Node>) -> Node {
        fn push(out: &mut Vec<Node>, n: Node) {
            match n {
                Node::Concat(ns) => ns.into_iter().for_each(|n| push(out, n)),
                Node::Slice { len: 0, .. } => {}
                Node::Slice { stream, start, len } => match out.last_mut() {
                    Some(Node::Slice { stream: s, start: a, len: l }) if *s == stream && *a + *l == start => *l += len,
                    _ => out.push(n),
                },
                n => out.push(n),
            }
        }
        let mut out = Vec::new();
        nodes.into_iter().for_each(|n| push(&mut out, n));
        if out.len() == 1 { out.pop().unwrap() } else { Node::Concat(out) }
    }

    /// The [`packed`] byte sequence of `nodes`, as they are.
    pub fn pack(nodes: impl IntoIterator<Item = Node>) -> Node {
        Node::Pack(nodes.into_iter().collect())
    }

    /// An application without resources.
    pub fn apply(module: Module, params: Vec<u8>, inputs: Vec<Node>, len: u64) -> Node {
        Node::apply_with(module, Vec::new(), params, inputs, len)
    }

    pub fn apply_with(module: Module, resources: Vec<Resource>, params: Vec<u8>, inputs: Vec<Node>, len: u64) -> Node {
        Node::Apply(Box::new(Apply { module, resources, params, inputs, len }))
    }

    /// The output of `patch`, of `len` bytes, against `source` ([`patch::make`]).
    pub fn patch(source: Node, patch: Node, len: u64) -> Node {
        Node::Patch(Box::new(Patch { source, patch, len }))
    }

    /// Bytes of the output, as declared.
    pub fn len(&self) -> u64 {
        match self {
            Node::Slice { len, .. } => *len,
            Node::Concat(ns) => ns.iter().map(Node::len).sum(),
            Node::Pack(ns) => packed::header_len(ns.len()) + ns.iter().map(Node::len).sum::<u64>(),
            Node::Apply(a) => a.len,
            Node::Output { len, .. } => *len,
            Node::Patch(p) => p.len,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Calls `f` on this node and every node below it, parents first.
    pub fn visit<'a>(&'a self, f: &mut impl FnMut(&'a Node)) {
        f(self);
        match self {
            Node::Slice { .. } | Node::Output { .. } => {}
            Node::Concat(ns) | Node::Pack(ns) => ns.iter().for_each(|n| n.visit(f)),
            Node::Apply(a) => a.inputs.iter().for_each(|n| n.visit(f)),
            Node::Patch(p) => [&p.source, &p.patch].into_iter().for_each(|n| n.visit(f)),
        }
    }

    /// Calls `f` on every slice below this node, as `(stream, start, len)`.
    fn slices_mut(&mut self, f: &mut impl FnMut(&mut usize, &mut u64, &mut u64)) {
        self.visit_mut(&mut |n| {
            if let Node::Slice { stream, start, len } = n {
                f(stream, start, len)
            }
        });
    }

    /// Calls `f` on this node and every node below it, parents first.
    fn visit_mut(&mut self, f: &mut impl FnMut(&mut Node)) {
        f(self);
        match self {
            Node::Slice { .. } | Node::Output { .. } => {}
            Node::Concat(ns) | Node::Pack(ns) => ns.iter_mut().for_each(|n| n.visit_mut(f)),
            Node::Apply(a) => a.inputs.iter_mut().for_each(|n| n.visit_mut(f)),
            Node::Patch(p) => [&mut p.source, &mut p.patch].into_iter().for_each(|n| n.visit_mut(f)),
        }
    }

    /// Moves the streams the slices below take by `streams`, and the applications of several
    /// outputs the outputs below take by `shared`.
    fn shift(&mut self, streams: usize, shared: usize) {
        self.visit_mut(&mut |n| match n {
            Node::Slice { stream, .. } => *stream += streams,
            Node::Output { shared: s, .. } => *s += shared,
            _ => {}
        });
    }
}

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
    pub fn push(&mut self, class: impl Into<Cow<'static, str>>, data: Vec<u8>) -> Node {
        self.push_as(class, Kind::Unknown, data)
    }

    /// Adds a stream of `kind`, returning a slice of all of it.
    pub fn push_as(&mut self, class: impl Into<Cow<'static, str>>, kind: Kind, data: Vec<u8>) -> Node {
        self.push_stream(Stream { class: class.into(), kind, content: false, data })
    }

    /// Adds a stream of `kind` that is [`Stream::content`], returning a slice of all of it.
    pub fn push_content(&mut self, class: impl Into<Cow<'static, str>>, kind: Kind, data: Vec<u8>) -> Node {
        self.push_stream(Stream { class: class.into(), kind, content: true, data })
    }

    fn push_stream(&mut self, s: Stream) -> Node {
        let len = s.data.len() as u64;
        self.streams.push(s);
        Node::slice(self.streams.len() - 1, 0, len)
    }

    /// Adds the application `a` of outputs of `sizes` bytes (its `len` set to their sum),
    /// returning the outputs to be placed in the formula, each once.
    pub fn share(&mut self, mut a: Apply, sizes: Vec<u64>) -> Vec<Node> {
        a.len = sizes.iter().sum();
        let shared = self.shared.len();
        let outputs = sizes.iter().enumerate().map(|(index, &len)| Node::Output { shared, index, len }).collect();
        self.shared.push(Shared { apply: a, sizes });
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
            s.apply.inputs.iter_mut().for_each(|n| n.shift(streams, shared));
            self.shared.push(s);
        }
        root
    }

    /// Calls `f` on every node of the inputs of the applications of several outputs, in order,
    /// then of the root, parents first.
    pub fn visit<'a>(&'a self, f: &mut impl FnMut(&'a Node)) {
        self.shared.iter().flat_map(|s| &s.apply.inputs).for_each(|n| n.visit(f));
        self.root.visit(f);
    }

    /// [`Node::slices_mut`] of the inputs of the applications of several outputs, and of the
    /// root.
    fn slices_mut(&mut self, f: &mut impl FnMut(&mut usize, &mut u64, &mut u64)) {
        self.shared.iter_mut().flat_map(|s| &mut s.apply.inputs).for_each(|n| n.slices_mut(f));
        self.root.slices_mut(f);
    }

    /// Drops what no slice points to: the bytes of a stream outside every slice of it, and
    /// streams without slices. The output is the same: a stream with a slice out of it is
    /// kept whole, and slices of streams that do not exist still point to none.
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
            if r.iter().any(|&(_, b, _)| b > s.data.len() as u64) {
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
            // in place: the bytes kept only move towards the start
            let (mut data, mut at) = (s.data, 0);
            for m in &mut merged {
                let (a, b) = (m.0 as usize, m.1 as usize);
                if a != at {
                    data.copy_within(a..b, at);
                }
                m.2 = at as u64;
                at += b - a;
            }
            data.truncate(at);
            data.shrink_to_fit();
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
    /// slices kept before that it starts with, one after another (the longest each time), and
    /// what follows of its own; one cut nowhere is kept. Slices past the end of their stream
    /// are left as they are. The output is the same.
    pub fn share_slices(&mut self, min: usize) {
        type Sl = (usize, u64, u64);
        let mut slices: Vec<Sl> = Vec::new();
        self.visit(&mut |n| {
            if let &Node::Slice { stream, start, len } = n
                && len >= min as u64
                && self
                    .streams
                    .get(stream)
                    .is_some_and(|s| start.checked_add(len).is_some_and(|e| e <= s.data.len() as u64))
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
                let found = kept
                    .get(&head(rest))
                    .and_then(|ks| ks.iter().filter(|k| rest.starts_with(bytes(k))).max_by_key(|k| k.2).copied());
                let Some(k) = found else { break };
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
                    Some(ps) => pieces.extend_from_slice(&ps.clone()),
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
        self.shared.iter_mut().flat_map(|s| &mut s.apply.inputs).for_each(|n| n.visit_mut(&mut cut));
        self.root.visit_mut(&mut cut);
        self.compact();
    }

    /// Puts the streams of each class and kind one after another, in one stream (in the place
    /// of the first): they are alike, and archive smaller together (the archive encoder
    /// compresses each stream on its own). The streams of [`Kind::Compressed`] (stored as they
    /// are), the contents, and those that a slice goes past the end of are left apart. The
    /// output is the same.
    pub fn merge_classes(&mut self) {
        self.merge_classes_within(u64::MAX);
    }

    /// [`Formula::merge_classes`], into streams of `limit` bytes at most: a stream that would
    /// take one past it starts another of its class and kind (one longer is left as it is).
    pub fn merge_classes_within(&mut self, limit: u64) {
        let n = self.streams.len();
        let lens: Vec<u64> = self.streams.iter().map(|s| s.data.len() as u64).collect();
        let mut past = vec![false; n];
        self.slices_mut(&mut |&mut s, &mut a, &mut l| {
            if let Some(p) = past.get_mut(s) {
                *p |= a.checked_add(l).is_none_or(|e| e > lens[s]);
            }
        });
        let mut to = vec![(0usize, 0u64); n];
        let mut firsts: Vec<(Cow<'static, str>, Kind, usize)> = Vec::new();
        let mut streams: Vec<Stream> = Vec::with_capacity(n);
        for (i, s) in std::mem::take(&mut self.streams).into_iter().enumerate() {
            let alike = s.kind != Kind::Compressed && !s.content && !past[i];
            match firsts.iter_mut().find(|f| alike && f.0 == s.class && f.1 == s.kind) {
                Some(&mut (_, _, j)) if (streams[j].data.len() + s.data.len()) as u64 <= limit => {
                    to[i] = (j, streams[j].data.len() as u64);
                    streams[j].data.extend_from_slice(&s.data);
                }
                Some(first) => {
                    first.2 = streams.len();
                    to[i] = (streams.len(), 0);
                    streams.push(s);
                }
                None => {
                    if alike {
                        firsts.push((s.class.clone(), s.kind, streams.len()));
                    }
                    to[i] = (streams.len(), 0);
                    streams.push(s);
                }
            }
        }
        let new = streams.len();
        self.slices_mut(&mut |s, a, _| match to.get(*s) {
            Some(&(j, o)) => (*s, *a) = (j, *a + o),
            None => *s = *s - n + new,
        });
        self.streams = streams;
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
        mut data: Vec<u8>,
        ends: &[(Kind, usize)],
        prefix: &str,
        own: usize,
    ) -> Node {
        if ends.iter().all(|k| k.0 == Kind::Unknown)
            && ends.iter().scan(0, |s, &(_, e)| Some(e - std::mem::replace(s, e))).all(|n| n < own)
        {
            let len = data.len() as u64;
            self.streams[raw].data = data;
            return Node::slice(raw, 0, len);
        }
        // the stream of each kind, by its first entry
        let mut order: Vec<Kind> = vec![Kind::Unknown];
        ends.iter().for_each(|k| {
            if !order.contains(&k.0) {
                order.push(k.0)
            }
        });
        let mut of_kind: Vec<Vec<u8>> = vec![Vec::new(); order.len()];
        // (each the kind's index, start and length, and whether the next of its kind joins it)
        let (mut parts, mut start, mut at) = (Vec::<(usize, usize, usize, bool)>::with_capacity(ends.len()), 0, 0);
        for &(kind, end) in ends {
            let k = order.iter().position(|&o| o == kind).unwrap();
            let len = end - start;
            let from = if k == 0 {
                data.copy_within(start..end, at);
                at += len;
                at - len
            } else {
                of_kind[k].extend_from_slice(&data[start..end]);
                of_kind[k].len() - len
            };
            let open = !matches!(kind, Kind::Format(_)) && len < own;
            match parts.last_mut() {
                _ if len == 0 => {}
                Some(p) if p.0 == k && p.3 && open => p.2 += len,
                _ => parts.push((k, from, len, open)),
            }
            start = end;
        }
        data.truncate(at);
        data.shrink_to_fit();
        self.streams[raw].data = data;
        let base = self.streams.len() - 1;
        for (kind, d) in order.into_iter().zip(of_kind).skip(1) {
            let class: Cow<'static, str> = match kind {
                Kind::Format(name) => format!("{prefix}/{name}").into(),
                kind => format!("{prefix}/{kind}").into(),
            };
            self.push_as(class, kind, d);
        }
        let stream = |k| if k == 0 { raw } else { base + k };
        let mut parts: Vec<Node> =
            parts.into_iter().map(|(k, start, len, _)| Node::slice(stream(k), start as u64, len as u64)).collect();
        if parts.len() == 1 { parts.pop().unwrap() } else { Node::Concat(parts) }
    }

    /// The applications, those of the root parents first, then those of several outputs.
    pub fn applies(&self) -> Vec<&Apply> {
        let mut out = Vec::new();
        self.root.visit(&mut |n| {
            if let Node::Apply(a) = n {
                out.push(&**a);
            }
        });
        for i in self.shared.iter().flat_map(|s| &s.apply.inputs) {
            i.visit(&mut |n| {
                if let Node::Apply(a) = n {
                    out.push(&**a);
                }
            });
        }
        out.extend(self.shared.iter().map(|s| &s.apply));
        out
    }

    /// Checks the outputs of the applications of several outputs: each taken exactly once, of
    /// the size declared, and in the inputs of an application only from those before it.
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
            if s.sizes.is_empty() || s.apply.len != s.sizes.iter().sum::<u64>() {
                return Err(Error::new(format!("application #{i} of several outputs of bad sizes")));
            }
            s.apply.inputs.iter().try_for_each(|n| check(n, i))?;
        }
        check(&self.root, self.shared.len())?;
        match taken.iter().position(|t| t.contains(&false)) {
            Some(i) => Err(Error::new(format!("an output of #{i} not taken"))),
            None => Ok(()),
        }
    }

    /// The output, with every module run by `m`. Fails on slices out of their stream, on
    /// modules whose output is not of the declared size, and on outputs not taken once
    /// ([`Formula::check_outputs`]).
    pub fn eval(&self, m: &impl Modules) -> Result<Vec<u8>> {
        self.check_outputs()?;
        // the outputs of the applications of several outputs, in order
        let mut outputs: Vec<Vec<Vec<u8>>> = Vec::with_capacity(self.shared.len());
        for s in &self.shared {
            let inputs = s.apply.inputs.iter().map(|n| self.bytes(n, m, &outputs)).collect::<Result<Vec<_>>>()?;
            let inputs: Vec<&[u8]> = inputs.iter().map(|i| &**i).collect();
            let out = m.apply_shared(s, &inputs)?;
            if out.len() != s.sizes.len() || out.iter().zip(&s.sizes).any(|(o, &n)| o.len() as u64 != n) {
                return Err(Error::new(format!("{} made outputs of other sizes than {:?}", s.apply.module, s.sizes)));
            }
            outputs.push(out);
        }
        // the output of a single application, or a stream, as it is
        Ok(self.bytes(&self.root, m, &outputs)?.into_owned())
    }

    fn eval_into(&self, n: &Node, m: &impl Modules, outputs: &[Vec<Vec<u8>>], out: &mut Vec<u8>) -> Result<()> {
        match n {
            Node::Concat(ns) => ns.iter().try_for_each(|n| self.eval_into(n, m, outputs, out)),
            _ => {
                out.extend_from_slice(&self.bytes(n, m, outputs)?);
                Ok(())
            }
        }
    }

    /// The bytes of `n`, given the `outputs` of the applications of several outputs (those
    /// before it).
    fn bytes<'a>(&'a self, n: &Node, m: &impl Modules, outputs: &'a [Vec<Vec<u8>>]) -> Result<Cow<'a, [u8]>> {
        match n {
            &Node::Slice { stream, start, len } => {
                let data = &self.streams.get(stream).ok_or_else(|| Error::new(format!("no stream #{stream}")))?.data;
                usize::try_from(start)
                    .ok()
                    .zip(usize::try_from(len).ok())
                    .and_then(|(a, l)| data.get(a..a.checked_add(l)?))
                    .map(Cow::Borrowed)
                    .ok_or_else(|| Error::new(format!("slice [{start}, +{len}) out of stream #{stream}")))
            }
            Node::Concat(_) => {
                let mut out = Vec::with_capacity(n.len() as usize);
                self.eval_into(n, m, outputs, &mut out)?;
                Ok(Cow::Owned(out))
            }
            Node::Pack(ns) => {
                let mut out = Vec::with_capacity(n.len() as usize);
                packed::put_header(&mut out, ns.iter().map(Node::len));
                ns.iter().try_for_each(|n| self.eval_into(n, m, outputs, &mut out))?;
                Ok(Cow::Owned(out))
            }
            // (checked, and taken only once)
            &Node::Output { shared, index, .. } => Ok(Cow::Borrowed(&outputs[shared][index])),
            Node::Patch(p) => {
                let (source, patch) = (self.bytes(&p.source, m, outputs)?, self.bytes(&p.patch, m, outputs)?);
                Ok(Cow::Owned(patch::apply(&source, &patch, p.len)?))
            }
            Node::Apply(a) => {
                let inputs = a.inputs.iter().map(|n| self.bytes(n, m, outputs)).collect::<Result<Vec<_>>>()?;
                let inputs: Vec<&[u8]> = inputs.iter().map(|i| &**i).collect();
                let out = m.apply(a, &inputs)?;
                if out.len() as u64 != a.len {
                    return Err(Error::new(format!("{} made {} bytes, not {}", a.module, out.len(), a.len)));
                }
                Ok(Cow::Owned(out))
            }
        }
    }
}

/// Indented, a node a line: `slice #stream [start, +len) CLASS` (and the kind of the stream
/// unless [`Kind::Unknown`] and not [`Stream::content`], with `content` if it is), `concat LEN`, `pack LEN`, `apply MODULE LEN` with the lengths of its
/// resources and its parameters in hex, `output #SHARED.INDEX LEN`, `patch LEN` (its source then
/// its patch): first each application of
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
                Node::Patch(p) => {
                    writeln!(f, "patch {}", p.len)?;
                    [&p.source, &p.patch].into_iter().try_for_each(|n| node(fm, n, depth + 1, f))
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
            write!(f, "shared #{i} {}", s.apply.module)?;
            s.sizes.iter().try_for_each(|n| write!(f, " {n}"))?;
            attrs(&s.apply, f)?;
            s.apply.inputs.iter().try_for_each(|n| node(self, n, 1, f))?;
        }
        node(self, &self.root, 0, f)
    }
}

/// Runs modules for [`Formula::eval`].
pub trait Modules {
    /// The output of `a` on `inputs`, the evaluated `a.inputs` (with `a.resources`).
    fn apply(&self, a: &Apply, inputs: &[&[u8]]) -> Result<Vec<u8>>;

    /// The outputs of `s` on `inputs`, the evaluated `s.apply.inputs`: by default those of a
    /// module of one output, by [`Modules::apply`].
    fn apply_shared(&self, s: &Shared, inputs: &[&[u8]]) -> Result<Vec<Vec<u8>>> {
        match s.sizes[..] {
            [_] => Ok(vec![self.apply(&s.apply, inputs)?]),
            _ => Err(Error::new(format!("{}: {} outputs", s.apply.module, s.sizes.len()))),
        }
    }
}

/// A function as the modules, e.g. a `match` on `a.module` in tests.
impl<F: Fn(&Apply, &[&[u8]]) -> Result<Vec<u8>>> Modules for F {
    fn apply(&self, a: &Apply, inputs: &[&[u8]]) -> Result<Vec<u8>> {
        self(a, inputs)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl Error {
    pub fn new(msg: impl Into<String>) -> Error {
        Error(msg.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    const REV: Module = Module { name: "rev", version: 1 };

    /// `rev` reverses the concatenation of its inputs.
    struct Rev;

    impl Modules for Rev {
        fn apply(&self, a: &Apply, inputs: &[&[u8]]) -> Result<Vec<u8>> {
            assert_eq!(a.module, REV);
            Ok(inputs.concat().into_iter().rev().collect())
        }
    }

    #[test]
    fn concat_flattens() {
        let n = Node::concat([
            Node::slice(0, 0, 2),
            Node::concat([Node::slice(0, 2, 3), Node::slice(1, 0, 0)]),
            Node::slice(0, 6, 1),
        ]);
        assert_eq!(n, Node::Concat(vec![Node::slice(0, 0, 5), Node::slice(0, 6, 1)]));
        assert_eq!(Node::concat([Node::slice(0, 0, 1), Node::slice(0, 1, 1)]), Node::slice(0, 0, 2));
    }

    #[test]
    fn eval_embedded() {
        let child = Formula::raw("b", b"xyz".to_vec());
        let mut f = Formula::default();
        let a = f.push("a", b"hello".to_vec());
        let b = f.embed(child);
        assert_eq!(b, Node::slice(1, 0, 3));
        f.root = Node::concat([Node::slice(0, 0, 2), Node::apply(REV, vec![], vec![a, b], 8)]);
        assert_eq!(f.len(), 10);
        assert_eq!(f.eval(&Rev).unwrap(), b"hezyxolleh");
        assert_eq!(f.applies().len(), 1);
        let shown = f.to_string();
        assert_eq!(
            shown,
            "concat 10\n  slice #0 [0, +2) a\n  apply rev@1 8\n    slice #0 [0, +5) a\n    slice #1 [0, +3) b\n"
        );
        f.streams[1].kind = Kind::Format("zlib");
        assert!(f.to_string().ends_with("slice #1 [0, +3) b (format=zlib)\n"), "{f}");
        f.streams[1].content = true;
        assert!(f.to_string().ends_with("slice #1 [0, +3) b (format=zlib, content)\n"), "{f}");
    }

    #[test]
    fn eval_pack() {
        let mut f = Formula::raw("a", b"hello".to_vec());
        f.root = Node::pack([
            Node::slice(0, 1, 2),
            Node::apply(REV, vec![], vec![Node::slice(0, 0, 3)], 3),
            Node::slice(0, 0, 0),
        ]);
        assert_eq!(f.len(), 32 + 5);
        let out = f.eval(&Rev).unwrap();
        assert_eq!(packed::unpack(&out).unwrap(), [&b"el"[..], b"leh", b""]);
        assert_eq!(
            f.to_string(),
            "pack 37\n  slice #0 [1, +2) a\n  apply rev@1 3\n    slice #0 [0, +3) a\n  slice #0 [0, +0) a\n"
        );
    }

    #[test]
    fn compact() {
        let mut f = Formula::default();
        f.push("a", b"0123456789".to_vec());
        f.push("unused", b"xyz".to_vec());
        f.push("b", b"abcdef".to_vec());
        f.root = Node::concat([
            Node::slice(0, 6, 2),
            Node::apply(REV, vec![], vec![Node::slice(0, 1, 3), Node::slice(0, 2, 3), Node::slice(0, 9, 0)], 4),
            Node::slice(2, 4, 3),
            Node::slice(5, 0, 0),
        ]);
        let mut c = f.clone();
        c.compact();
        assert_eq!(c.eval(&Rev), f.eval(&Rev));
        // The slice past the end of "b" keeps it whole; the slice of no stream stays so.
        assert_eq!(
            c.streams.iter().map(|s| (&*s.class, &s.data[..])).collect::<Vec<_>>(),
            [("a", &b"123467"[..]), ("b", &b"abcdef"[..])]
        );
        assert_eq!(
            c.root,
            Node::concat([
                Node::slice(0, 4, 2),
                Node::apply(REV, vec![], vec![Node::slice(0, 0, 3), Node::slice(0, 1, 3), Node::slice(0, 6, 0)], 4),
                Node::slice(1, 4, 3),
                Node::slice(4, 0, 0),
            ])
        );
        f.root = Node::slice(2, 1, 2);
        f.compact();
        assert_eq!(f.eval(&Rev).unwrap(), b"bc");
        assert_eq!(f.streams.len(), 1);
        assert_eq!(f.streams[0].data, b"bc");
        // a stream sliced whole is not copied
        let mut f = Formula::raw("a", b"abc".to_vec());
        let whole = f.streams[0].data.as_ptr();
        f.compact();
        assert_eq!(f.streams[0].data.as_ptr(), whole);
    }

    const CUT: Module = Module { name: "cut", version: 1 };

    /// `rev`, and `cut`, cutting the concatenation of its inputs into its outputs.
    struct RevCut;

    impl Modules for RevCut {
        fn apply(&self, a: &Apply, inputs: &[&[u8]]) -> Result<Vec<u8>> {
            Rev.apply(a, inputs)
        }

        fn apply_shared(&self, s: &Shared, inputs: &[&[u8]]) -> Result<Vec<Vec<u8>>> {
            assert_eq!(s.apply.module, CUT);
            let (all, mut at) = (inputs.concat(), 0);
            let mut next = |n: u64| {
                at += n as usize;
                all[at - n as usize..at].to_vec()
            };
            Ok(s.sizes.iter().map(|&n| next(n)).collect())
        }
    }

    fn cut(inputs: Vec<Node>) -> Apply {
        Apply { module: CUT, resources: vec![], params: vec![], inputs, len: 0 }
    }

    /// Outputs of applications of several outputs, taken by the root and by later ones; shown,
    /// embedded and compacted.
    #[test]
    fn eval_shared() {
        let mut f = Formula::raw("a", b"abcdefgh".to_vec());
        let a = f.root.clone();
        let [o0, o1] = <[Node; 2]>::try_from(f.share(cut(vec![a]), vec![3, 5])).unwrap();
        let [p0, p1] = <[Node; 2]>::try_from(f.share(cut(vec![o1]), vec![1, 4])).unwrap();
        f.root = Node::concat([p1, Node::slice(0, 7, 1), o0, Node::apply(REV, vec![], vec![p0], 1)]);
        assert_eq!(f.len(), 9);
        assert_eq!(f.eval(&RevCut).unwrap(), b"efghhabcd");
        assert_eq!(f.applies().len(), 3);
        assert_eq!(
            f.to_string(),
            "shared #0 cut@1 3 5\n  slice #0 [0, +8) a\nshared #1 cut@1 1 4\n  output #0.1 5\n\
             concat 9\n  output #1.1 4\n  slice #0 [7, +1) a\n  output #0.0 3\n  apply rev@1 1\n    output #1.0 1\n"
        );
        // embedded after a formula of its own application of several outputs
        let mut g = Formula::raw("b", b"xy".to_vec());
        let b = g.root.clone();
        let [x, y] = <[Node; 2]>::try_from(g.share(cut(vec![b]), vec![1, 1])).unwrap();
        let child = g.embed(f.clone());
        g.root = Node::concat([y, child, x]);
        assert_eq!(g.eval(&RevCut).unwrap(), b"yefghhabcdx");
        // compacted: the slices of the applications are kept
        let mut c = f.clone();
        c.streams[0].data.extend_from_slice(b"unused");
        c.compact();
        assert_eq!(c.eval(&RevCut).unwrap(), b"efghhabcd");
        assert_eq!(c.streams[0].data, b"abcdefgh");
    }

    /// A patch of a slice against another one, shown.
    #[test]
    fn eval_patch() {
        let (source, target) = (b"the quick brown fox jumps".to_vec(), b"the quick red fox jumps!".to_vec());
        let mut f = Formula::raw("a", source.clone());
        let d = f.push_as("d", Kind::Compressed, patch::make(&source, &target, 3).unwrap());
        let n = target.len() as u64;
        f.root = Node::concat([Node::slice(0, 0, 4), Node::patch(Node::slice(0, 0, 25), d, n)]);
        assert_eq!(f.eval(&Rev).unwrap(), [&b"the "[..], &target].concat());
        assert!(
            f.to_string().starts_with("concat 28\n  slice #0 [0, +4) a\n  patch 24\n    slice #0 [0, +25) a\n"),
            "{f}"
        );
        let Node::Concat(ns) = &mut f.root else { unreachable!() };
        let Node::Patch(p) = &mut ns[1] else { unreachable!() };
        p.len += 1;
        assert!(f.eval(&Rev).is_err());
    }

    /// Streams of a class and kind merged, but those compressed, the contents and those a slice
    /// goes past; the output the same.
    #[test]
    fn merge_classes() {
        let mut f = Formula::default();
        let a0 = f.push("a", b"012".to_vec());
        let b = f.push("b", b"xyz".to_vec());
        let a1 = f.push("a", b"345".to_vec());
        let c = f.push_as("a", Kind::Compressed, b"ccc".to_vec());
        let t = f.push_content("a", Kind::Unknown, b"ttt".to_vec());
        f.push("a", b"678".to_vec());
        let a3 = f.push("a", b"9".to_vec());
        f.root = Node::concat([
            a1,
            Node::apply(REV, vec![], vec![a0, b, Node::slice(5, 1, 2)], 8),
            c,
            t,
            a3,
            Node::slice(5, 0, 1),
        ]);
        let want = f.eval(&Rev).unwrap();
        let mut g = f.clone();
        g.merge_classes();
        assert_eq!(g.eval(&Rev), Ok(want));
        let classes: Vec<_> = g.streams.iter().map(|s| (&*s.class, &s.data[..])).collect();
        assert_eq!(classes, [("a", &b"0123456789"[..]), ("b", &b"xyz"[..]), ("a", &b"ccc"[..]), ("a", &b"ttt"[..])]);
        // within 6 bytes a stream
        let mut g = f.clone();
        g.merge_classes_within(6);
        assert_eq!(g.eval(&Rev), f.eval(&Rev));
        let classes: Vec<_> = g.streams.iter().map(|s| (&*s.class, &s.data[..])).collect();
        assert_eq!(
            classes,
            [("a", &b"012345"[..]), ("b", &b"xyz"[..]), ("a", &b"ccc"[..]), ("a", &b"ttt"[..]), ("a", &b"6789"[..])]
        );
        // a slice past the end of the last keeps it apart
        let mut past = f.clone();
        let Node::Concat(ns) = &mut past.root else { unreachable!() };
        ns.push(Node::slice(6, 0, 3));
        past.merge_classes();
        assert_eq!(past.streams.len(), 5);
        assert_eq!((&past.streams[0].data[..], &past.streams[4].data[..]), (&b"012345678"[..], &b"9"[..]));
    }

    /// Slices cut into the shorter ones they start with, wherever those lie, but those shorter
    /// than the least, and those past the end of their stream; the output the same.
    #[test]
    fn share_slices() {
        let long: Vec<u8> = (0..40u32).flat_map(|i| i.to_le_bytes()).collect();
        let mut f = Formula::default();
        let a = f.push("a", long[..100].to_vec());
        let b = f.push_content("b", Kind::Unknown, long.clone());
        let c = f.push("c", long[..100].to_vec());
        let d = f.push("d", long[..8].to_vec());
        f.push("e", long[..120].to_vec());
        let mut other = long[..100].to_vec();
        other[99] ^= 1;
        let o = f.push("o", other);
        // long twice within, after other bytes
        f.push("z", [&b"xyz"[..], &long[..], &long[..]].concat());
        f.root = Node::concat([
            a,
            Node::apply(REV, vec![], vec![Node::slice(2, 0, 20), b], 180),
            c,
            d,
            o,
            Node::slice(4, 100, 20),
            Node::slice(6, 3, 120),
            Node::slice(6, 163, 160),
            Node::slice(6, 0, 4),
        ]);
        let want = f.eval(&Rev).unwrap();
        let mut g = f.clone();
        g.share_slices(16);
        assert_eq!(g.eval(&Rev), Ok(want));
        let classes: Vec<_> = g.streams.iter().map(|s| (&*s.class, s.data.len())).collect();
        // long[..20] of c, long[20..100] of a, long[100..120] of e, long[120..] of b (each
        // of those alone first); o its own but for the first 20
        assert_eq!(classes, [("a", 80), ("b", 40), ("c", 20), ("d", 8), ("e", 20), ("o", 80), ("z", 4)]);
        // a slice past the end of one is left as it is, and keeps it whole
        let Node::Concat(ns) = &mut f.root else { unreachable!() };
        ns.push(Node::slice(2, 90, 20));
        f.share_slices(16);
        let classes: Vec<_> = f.streams.iter().map(|s| (&*s.class, s.data.len())).collect();
        assert_eq!(classes, [("a", 80), ("b", 40), ("c", 100), ("d", 8), ("e", 20), ("o", 80), ("z", 4)]);
        assert!(matches!(&f.root, Node::Concat(ns) if ns.last() == Some(&Node::slice(2, 90, 20))));
    }

    /// Outputs not taken once each, of other sizes, or taken by an application before theirs.
    #[test]
    fn shared_errors() {
        let mut f = Formula::raw("a", b"abcdefgh".to_vec());
        let a = f.root.clone();
        let [o0, o1] = <[Node; 2]>::try_from(f.share(cut(vec![a]), vec![3, 5])).unwrap();
        for root in [
            Node::concat([o0.clone()]),
            Node::concat([o0.clone(), o1.clone(), o0.clone()]),
            Node::concat([o0.clone(), Node::Output { shared: 0, index: 1, len: 4 }]),
            Node::concat([o0.clone(), o1.clone(), Node::Output { shared: 0, index: 2, len: 0 }]),
        ] {
            f.root = root;
            assert!(f.check_outputs().is_err() && f.eval(&RevCut).is_err(), "{f}");
        }
        f.root = Node::concat([o0, o1]);
        assert_eq!(f.eval(&RevCut).unwrap(), b"abcdefgh");
        f.shared[0].apply.inputs = vec![Node::Output { shared: 0, index: 0, len: 3 }];
        assert!(f.check_outputs().is_err());
        // a module of one output only, by default
        let mut g = Formula::raw("a", b"ab".to_vec());
        let a = g.root.clone();
        g.root = Node::concat(g.share(Apply { module: REV, ..cut(vec![a]) }, vec![1, 1]));
        assert!(g.eval(&Rev).is_err());
    }

    #[test]
    fn eval_errors() {
        let mut f = Formula::raw("a", b"abc".to_vec());
        f.root = Node::slice(0, 1, 3);
        assert!(f.eval(&Rev).is_err());
        f.root = Node::apply(REV, vec![], vec![Node::slice(0, 0, 3)], 2);
        assert!(f.eval(&Rev).is_err());
    }
}
