//! Entry formulas (xform-design.md §4.2): what a recompressor makes of a file, as an
//! expression over the data it extracted rather than one opaque byte string.
//!
//! A [`Formula`] is a tree of [`Node`]s over [`Stream`]s:
//! - `slice`: a byte range of a stream;
//! - `concat`: nodes one after another;
//! - `apply`: a [`Module`] (a pure function of its inputs, resources and parameters) applied
//!   to nodes, declaring the size of its output.
//!
//! Streams are not placed yet: the archive encoder lays them out into non-leaf blocks, and
//! [`Stream::class`] tells it which streams are alike (to share frames and a dictionary, §3).
//! What belongs to the archive is left out: `ref` to other entries, and the resource
//! declarations of `apply` (memory limit, windows, capacities), which the archive encoder
//! measures while verifying (§6). [`Resource`]s (§5.6), the models that modules take, are
//! held by the applications, and the archive encoder stores each once.
//!
//! [`Formula::eval`] is the reference, strict evaluation: every node is materialised, with
//! the modules run by a [`Modules`] implementation.
//!
//! [`aqr`] stores a formula as an Aqueous archive of the file alone.

pub mod aqr;

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
    pub data: Vec<u8>,
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
    Apply(Box<Apply>),
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

    /// An application without resources.
    pub fn apply(module: Module, params: Vec<u8>, inputs: Vec<Node>, len: u64) -> Node {
        Node::apply_with(module, Vec::new(), params, inputs, len)
    }

    pub fn apply_with(module: Module, resources: Vec<Resource>, params: Vec<u8>, inputs: Vec<Node>, len: u64) -> Node {
        Node::Apply(Box::new(Apply { module, resources, params, inputs, len }))
    }

    /// Bytes of the output, as declared.
    pub fn len(&self) -> u64 {
        match self {
            Node::Slice { len, .. } => *len,
            Node::Concat(ns) => ns.iter().map(Node::len).sum(),
            Node::Apply(a) => a.len,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Calls `f` on this node and every node below it, parents first.
    pub fn visit<'a>(&'a self, f: &mut impl FnMut(&'a Node)) {
        f(self);
        match self {
            Node::Slice { .. } => {}
            Node::Concat(ns) => ns.iter().for_each(|n| n.visit(f)),
            Node::Apply(a) => a.inputs.iter().for_each(|n| n.visit(f)),
        }
    }

    /// Calls `f` on every slice below this node, as `(stream, start, len)`.
    fn slices_mut(&mut self, f: &mut impl FnMut(&mut usize, &mut u64, &mut u64)) {
        match self {
            Node::Slice { stream, start, len } => f(stream, start, len),
            Node::Concat(ns) => ns.iter_mut().for_each(|n| n.slices_mut(f)),
            Node::Apply(a) => a.inputs.iter_mut().for_each(|n| n.slices_mut(f)),
        }
    }

    fn shift(&mut self, by: usize) {
        self.slices_mut(&mut |stream, _, _| *stream += by);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Formula {
    pub streams: Vec<Stream>,
    pub root: Node,
}

impl Default for Formula {
    fn default() -> Formula {
        Formula { streams: Vec::new(), root: Node::Concat(Vec::new()) }
    }
}

impl Formula {
    /// `data` as it is: a slice of a single stream.
    pub fn raw(class: impl Into<Cow<'static, str>>, data: Vec<u8>) -> Formula {
        let mut f = Formula::default();
        f.root = f.push(class, data);
        f
    }

    /// Adds a stream, returning a slice of all of it.
    pub fn push(&mut self, class: impl Into<Cow<'static, str>>, data: Vec<u8>) -> Node {
        let len = data.len() as u64;
        self.streams.push(Stream { class: class.into(), data });
        Node::slice(self.streams.len() - 1, 0, len)
    }

    /// Takes over the streams of `child`, returning its root to be placed in this formula.
    pub fn embed(&mut self, child: Formula) -> Node {
        let mut root = child.root;
        root.shift(self.streams.len());
        self.streams.extend(child.streams);
        root
    }

    /// Drops what no slice points to: the bytes of a stream outside every slice of it, and
    /// streams without slices. The output is the same: a stream with a slice out of it is
    /// kept whole, and slices of streams that do not exist still point to none.
    pub fn compact(&mut self) {
        // Per stream, the ranges of its slices, merged into (start, end, start in the new data).
        let mut ranges = vec![Vec::new(); self.streams.len()];
        self.root.slices_mut(&mut |&mut s, &mut a, &mut l| {
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
            let mut data = Vec::new();
            for m in &mut merged {
                m.2 = data.len() as u64;
                data.extend_from_slice(&s.data[m.0 as usize..m.1 as usize]);
            }
            *r = merged;
            streams.push(Stream { class: s.class, data });
        }
        let new = streams.len();
        self.root.slices_mut(&mut |s, a, _| match ranges.get(*s) {
            Some(r) => {
                let m = r[r.partition_point(|m| m.0 <= *a) - 1];
                *a = m.2 + (*a - m.0);
                *s = index[*s];
            }
            None => *s = *s - old + new,
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

    /// The applications, parents first.
    pub fn applies(&self) -> Vec<&Apply> {
        let mut out = Vec::new();
        self.root.visit(&mut |n| {
            if let Node::Apply(a) = n {
                out.push(&**a);
            }
        });
        out
    }

    /// The output, with every module run by `m`. Fails on slices out of their stream and on
    /// modules whose output is not of the declared size.
    pub fn eval(&self, m: &impl Modules) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(self.len() as usize);
        self.eval_into(&self.root, m, &mut out)?;
        Ok(out)
    }

    fn eval_into(&self, n: &Node, m: &impl Modules, out: &mut Vec<u8>) -> Result<()> {
        match n {
            Node::Concat(ns) => ns.iter().try_for_each(|n| self.eval_into(n, m, out)),
            _ => {
                out.extend_from_slice(&self.bytes(n, m)?);
                Ok(())
            }
        }
    }

    fn bytes(&self, n: &Node, m: &impl Modules) -> Result<Cow<'_, [u8]>> {
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
                self.eval_into(n, m, &mut out)?;
                Ok(Cow::Owned(out))
            }
            Node::Apply(a) => {
                let inputs = a.inputs.iter().map(|n| self.bytes(n, m)).collect::<Result<Vec<_>>>()?;
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

/// Indented, a node a line: `slice #stream [start, +len)`, `concat LEN`, `apply MODULE LEN`
/// with the lengths of its resources and its parameters in hex.
impl fmt::Display for Formula {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fn node(fm: &Formula, n: &Node, depth: usize, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "{:1$}", "", depth * 2)?;
            match n {
                &Node::Slice { stream, start, len } => {
                    let class = fm.streams.get(stream).map_or("?", |s| &s.class);
                    writeln!(f, "slice #{stream} [{start}, +{len}) {class}")
                }
                Node::Concat(ns) => {
                    writeln!(f, "concat {}", n.len())?;
                    ns.iter().try_for_each(|n| node(fm, n, depth + 1, f))
                }
                Node::Apply(a) => {
                    write!(f, "apply {} {}", a.module, a.len)?;
                    if !a.resources.is_empty() {
                        write!(f, " resources")?;
                        a.resources.iter().try_for_each(|r| write!(f, " {}", r.len()))?;
                    }
                    if !a.params.is_empty() {
                        write!(f, " params ")?;
                        a.params.iter().try_for_each(|b| write!(f, "{b:02x}"))?;
                    }
                    writeln!(f)?;
                    a.inputs.iter().try_for_each(|n| node(fm, n, depth + 1, f))
                }
            }
        }
        node(self, &self.root, 0, f)
    }
}

/// Runs modules for [`Formula::eval`].
pub trait Modules {
    /// The output of `a` on `inputs`, the evaluated `a.inputs` (with `a.resources`).
    fn apply(&self, a: &Apply, inputs: &[&[u8]]) -> Result<Vec<u8>>;
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
