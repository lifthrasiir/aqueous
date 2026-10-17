//! Entry formulas (xform-design.md §4.2): what a recompressor makes of a file, as an
//! expression over the data it extracted rather than one opaque byte string.
//!
//! A [`Formula`] is a tree of [`Node`]s over [`Stream`]s:
//! - `slice`: a byte range of a stream;
//! - `concat`: nodes one after another;
//! - `apply`: a [`Module`] (a pure function of its inputs and parameters) applied to nodes,
//!   declaring the size of its output.
//!
//! Streams are not placed yet: the archive encoder lays them out into non-leaf blocks, and
//! [`Stream::class`] tells it which streams are alike (to share frames and a dictionary, §3).
//! What belongs to the archive is left out: `ref` to other entries, and the resource
//! declarations of `apply` (memory limit, windows, capacities), which the archive encoder
//! measures while verifying (§6). So are resources (§5.6), until a module takes one; the
//! recompressors' models are compiled into them for now.
//!
//! [`Formula::eval`] is the reference, strict evaluation: every node is materialised, with
//! the modules run by a [`Modules`] implementation.

use std::borrow::Cow;
use std::fmt;

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

    pub fn apply(module: Module, params: Vec<u8>, inputs: Vec<Node>, len: u64) -> Node {
        Node::Apply(Box::new(Apply { module, params, inputs, len }))
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

    fn shift(&mut self, by: usize) {
        match self {
            Node::Slice { stream, .. } => *stream += by,
            Node::Concat(ns) => ns.iter_mut().for_each(|n| n.shift(by)),
            Node::Apply(a) => a.inputs.iter_mut().for_each(|n| n.shift(by)),
        }
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
/// with its parameters in hex.
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
    /// The output of `a` on `inputs`, the evaluated `a.inputs`.
    fn apply(&self, a: &Apply, inputs: &[&[u8]]) -> Result<Vec<u8>>;
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
    fn eval_errors() {
        let mut f = Formula::raw("a", b"abc".to_vec());
        f.root = Node::slice(0, 1, 3);
        assert!(f.eval(&Rev).is_err());
        f.root = Node::apply(REV, vec![], vec![Node::slice(0, 0, 3)], 2);
        assert!(f.eval(&Rev).is_err());
    }
}
