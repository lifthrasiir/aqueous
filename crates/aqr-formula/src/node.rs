//! The nodes of a formula ([`Node`]), the applications of modules ([`Apply`], [`Shared`]) and
//! their resources ([`Resource`]), the bound of an application's memory.

use super::*;

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
pub struct Resource {
    data: Arc<[u8]>,
    /// The data of its block as stored, where it is given ([`Resource::with_stored`]).
    stored: Option<Arc<[u8]>>,
}

impl Resource {
    pub fn new(data: impl Into<Arc<[u8]>>) -> Resource {
        Resource { data: data.into(), stored: None }
    }

    /// The resource of `data` whose block has data `stored` as it is (checksummed Zstandard data
    /// of `data`, as of a resource or module block of another archive): for
    /// [`Node::ResourceBytes`]. A resource of the same bytes not stored so is another resource,
    /// of a block of its own. aqr-archive's `stored_resource` checks and decodes `stored`.
    pub fn with_stored(data: impl Into<Arc<[u8]>>, stored: impl Into<Arc<[u8]>>) -> Resource {
        Resource { data: data.into(), stored: Some(stored.into()) }
    }

    /// The data of its block as stored, if given ([`Resource::with_stored`]).
    pub fn stored_data(&self) -> Option<&[u8]> {
        self.stored.as_deref()
    }
}

impl std::ops::Deref for Resource {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.data
    }
}

impl fmt::Debug for Resource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Resource({} bytes, {:016x}", self.len(), xxhash_rust::xxh64::xxh64(self, 0))?;
        match &self.stored {
            Some(s) => write!(f, ", stored in {} bytes)", s.len()),
            None => write!(f, ")"),
        }
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
    /// The data of the module block of `module` as stored, which is to be these bytes (a
    /// ModuleBytes instruction): those of a module block of another archive, as when it is
    /// archived. The encoder fails where its block of the module is other bytes.
    ModuleBytes(Module, Arc<[u8]>),
    /// The data of the resource block of a resource [`Resource::stored`] as stored (a
    /// ResourceBytes instruction).
    ResourceBytes(Resource),
    /// Either of two nodes of the same output, to be chosen before the formula is archived
    /// ([`Formula::resolve`]): in memory only, never in an archive (the encoder and evaluation
    /// fail on one).
    Either(Box<Either>),
}

/// Two nodes of the same output of `len` bytes ([`Node::Either`]): what aqr-compress keeps where
/// a formula lost to its data as it is by little, to choose once the batch is made, with the
/// others alike ([`Formula::resolve`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Either {
    /// The data as it is (the slice that the formula made of it would have taken the place of).
    pub kept: Node,
    /// The formula made of the data, embedded (its streams those of the formula holding it).
    pub made: Node,
    pub len: u64,
    /// What `made` and `kept` archived in on their own, as they were compared.
    pub sizes: (usize, usize),
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
    /// What makes the outputs: an application, or a Patch instruction (always split).
    pub of: Of,
    /// Bytes of each output.
    pub sizes: Vec<u64>,
    /// Whether the module outputs the concatenation of the outputs, which a Split instruction
    /// separates ([`Formula::split`]), for a number of outputs that varies between applications;
    /// otherwise each is an output of the module ([`Formula::share`]).
    pub split: bool,
}

/// What makes the outputs of a [`Shared`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Of {
    /// An application, its `len` the outputs' sizes added up.
    Apply(Apply),
    /// A Patch instruction ([`Patch`]) of its `inputs`, the source then the patch, of `len`
    /// bytes, the outputs' sizes added up: e.g. a Zstandard frame's blocks decompressed (an
    /// empty source), what they hold taken by several files.
    Patch { inputs: [Node; 2], len: u64 },
    /// A node's bytes, cut into the outputs (a Split instruction of it): e.g. a file's contents,
    /// one content, of which an image holds pieces here and there.
    Node(Node),
}

impl Shared {
    /// The nodes it takes: the inputs of the application, or the source and the patch.
    pub fn inputs(&self) -> &[Node] {
        match &self.of {
            Of::Apply(a) => &a.inputs,
            Of::Patch { inputs, .. } => inputs,
            Of::Node(n) => std::slice::from_ref(n),
        }
    }

    pub fn inputs_mut(&mut self) -> &mut [Node] {
        match &mut self.of {
            Of::Apply(a) => &mut a.inputs,
            Of::Patch { inputs, .. } => inputs,
            Of::Node(n) => std::slice::from_mut(n),
        }
    }

    /// The application, where it is one.
    pub fn apply(&self) -> Option<&Apply> {
        match &self.of {
            Of::Apply(a) => Some(a),
            Of::Patch { .. } | Of::Node(_) => None,
        }
    }

    pub fn apply_mut(&mut self) -> Option<&mut Apply> {
        match &mut self.of {
            Of::Apply(a) => Some(a),
            Of::Patch { .. } | Of::Node(_) => None,
        }
    }

    /// Bytes of the outputs together.
    pub fn bytes(&self) -> u64 {
        match &self.of {
            Of::Apply(a) => a.len,
            Of::Patch { len, .. } => *len,
            Of::Node(n) => n.len(),
        }
    }

    /// The outputs of a Patch instruction ([`Of::Patch`]) on `inputs`, the source and the patch
    /// evaluated, or of a node split ([`Of::Node`]) on its bytes; None of an application.
    pub fn patched(&self, inputs: &[&[u8]]) -> Option<Result<Vec<Vec<u8>>>> {
        if let Of::Node(_) = self.of {
            let &[all] = inputs else { return Some(Err(Error::new("split: one input wanted"))) };
            if all.len() as u64 != self.sizes.iter().sum::<u64>() {
                return Some(Err(Error::new("split: outputs of other sizes than declared")));
            }
            let mut rest = all;
            return Some(Ok(self.sizes.iter().map(|&n| rest.split_off(..n as usize).unwrap().to_vec()).collect()));
        }
        let Of::Patch { len, .. } = self.of else { return None };
        let &[source, patch] = inputs else { return Some(Err(Error::new("patch: two inputs wanted"))) };
        Some(patch::apply(source, patch, len).and_then(|all| {
            if all.len() as u64 != len || self.sizes.iter().sum::<u64>() != len {
                return Err(Error::new("patch: outputs of other sizes than declared"));
            }
            let mut rest = &all[..];
            Ok(self.sizes.iter().map(|&n| rest.split_off(..n as usize).unwrap().to_vec()).collect())
        }))
    }

    /// What it is, for messages: the module, or `patch`.
    pub fn name(&self) -> String {
        match &self.of {
            Of::Apply(a) => a.module.to_string(),
            Of::Patch { .. } => "patch".into(),
            Of::Node(_) => "split".into(),
        }
    }
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
    /// The memory limit its instruction declares ([`NO_LIMIT`] until [`Formula::declare_memory`]
    /// sets it): of its instance.
    pub memory_limit: u64,
    /// The memory limit of a task of each class of the module (the spec's task limits): as many
    /// as the module has classes.
    pub task_limits: Vec<u64>,
}

/// The memory limit of an application declaring none.
pub const NO_LIMIT: u64 = u64::MAX;

/// The most that a decoder has to support of the minimal bound of an application
/// ([`minimal_bound`]), 512 MiB; encoders keep to less by default (spec, "Module execution").
pub const SUPPORTED_BOUND: u64 = 1 << 29;

/// What the decoder fails an application over, as bytes of its minimal bound
/// ([`set_application_limit`]); [`SUPPORTED_BOUND`] by default.
static APPLICATION_LIMIT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(SUPPORTED_BOUND);

/// Sets the most that an application may have as its minimal bound ([`minimal_bound`]) for this
/// process; a larger one fails as a resource failure before its module is applied
/// (`u64::MAX`: no limit, as an encoder that is allowed larger ones sets it).
pub fn set_application_limit(bytes: u64) {
    APPLICATION_LIMIT.store(bytes, std::sync::atomic::Ordering::Relaxed);
}

/// The limit set ([`set_application_limit`]).
pub fn application_limit() -> u64 {
    APPLICATION_LIMIT.load(std::sync::atomic::Ordering::Relaxed)
}

/// The minimal bound of an application of memory limit `memory_limit` and task limits
/// `task_limits`: the memory limit plus the sum of the task limits (one task of each class at
/// once). A limit of [`NO_LIMIT`] counts for nothing.
pub fn minimal_bound(memory_limit: u64, task_limits: &[u64]) -> u64 {
    let known = |l: u64| if l == NO_LIMIT { 0 } else { l };
    task_limits.iter().fold(known(memory_limit), |t, &l| t.saturating_add(known(l)))
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

    /// This node with every concatenation below it, itself among them, flattened as
    /// [`Node::concat`] does: one put in place of a slice within another (in
    /// [`Formula::share_slices`], or a formula embedded) is spliced into it. The parts of a pack
    /// stay its parts. The output is the same.
    pub fn flatten(&mut self) {
        match self {
            Node::Concat(ns) => {
                ns.iter_mut().for_each(Node::flatten);
                *self = Node::concat(std::mem::take(ns));
            }
            Node::Pack(ns) => ns.iter_mut().for_each(Node::flatten),
            Node::Apply(a) => a.inputs.iter_mut().for_each(Node::flatten),
            Node::Patch(p) => [&mut p.source, &mut p.patch].into_iter().for_each(Node::flatten),
            Node::Either(e) => [&mut e.kept, &mut e.made].into_iter().for_each(Node::flatten),
            Node::Slice { .. } | Node::Output { .. } | Node::ModuleBytes(..) | Node::ResourceBytes(_) => {}
        }
    }

    /// This node, a slice or a concatenation of slices (as [`Formula::push_by_kind`] gives the
    /// contents of an archive's members), cut into nodes of `sizes`, which add up to its bytes.
    /// Panics on another node, or where the sizes add up to more.
    pub fn cut(self, sizes: &[u64]) -> Vec<Node> {
        let slices = match self {
            Node::Concat(ns) => ns,
            n => vec![n],
        };
        let mut slices = slices.into_iter().map(|s| match s {
            Node::Slice { stream, start, len } => (stream, start, len),
            _ => panic!("slices of contents"),
        });
        let mut cur = slices.next();
        sizes
            .iter()
            .map(|&size| {
                let (mut parts, mut want) = (Vec::new(), size);
                while want > 0 {
                    let (s, a, l) = cur.expect("as many bytes as the sizes");
                    let k = l.min(want);
                    parts.push(Node::slice(s, a, k));
                    want -= k;
                    cur = if k == l { slices.next() } else { Some((s, a + k, l - k)) };
                }
                Node::concat(parts)
            })
            .collect()
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
        Node::Apply(Box::new(Apply {
            module,
            resources,
            params,
            inputs,
            len,
            memory_limit: NO_LIMIT,
            task_limits: Vec::new(),
        }))
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
            Node::ModuleBytes(_, d) => d.len() as u64,
            Node::ResourceBytes(r) => r.stored_data().map_or(0, |d| d.len() as u64),
            Node::Either(e) => e.len,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Calls `f` on this node and every node below it, parents first.
    pub fn visit<'a>(&'a self, f: &mut impl FnMut(&'a Node)) {
        f(self);
        match self {
            Node::Slice { .. } | Node::Output { .. } | Node::ModuleBytes(..) | Node::ResourceBytes(_) => {}
            Node::Concat(ns) | Node::Pack(ns) => ns.iter().for_each(|n| n.visit(f)),
            Node::Apply(a) => a.inputs.iter().for_each(|n| n.visit(f)),
            Node::Patch(p) => [&p.source, &p.patch].into_iter().for_each(|n| n.visit(f)),
            Node::Either(e) => [&e.kept, &e.made].into_iter().for_each(|n| n.visit(f)),
        }
    }

    /// Calls `f` on every slice below this node, as `(stream, start, len)`.
    pub fn slices_mut(&mut self, f: &mut impl FnMut(&mut usize, &mut u64, &mut u64)) {
        self.visit_mut(&mut |n| {
            if let Node::Slice { stream, start, len } = n {
                f(stream, start, len)
            }
        });
    }

    /// Replaces every slice below this node with what `f` makes of it, of `(stream, start,
    /// len)`; what it makes is not visited.
    pub(crate) fn map_slices(&mut self, f: &mut impl FnMut(usize, u64, u64) -> Node) {
        match self {
            &mut Node::Slice { stream, start, len } => *self = f(stream, start, len),
            Node::Output { .. } | Node::ModuleBytes(..) | Node::ResourceBytes(_) => {}
            Node::Concat(ns) | Node::Pack(ns) => ns.iter_mut().for_each(|n| n.map_slices(f)),
            Node::Apply(a) => a.inputs.iter_mut().for_each(|n| n.map_slices(f)),
            Node::Patch(p) => [&mut p.source, &mut p.patch].into_iter().for_each(|n| n.map_slices(f)),
            Node::Either(e) => [&mut e.kept, &mut e.made].into_iter().for_each(|n| n.map_slices(f)),
        }
    }

    /// Calls `f` on this node and every node below it, parents first.
    pub fn visit_mut(&mut self, f: &mut impl FnMut(&mut Node)) {
        f(self);
        match self {
            Node::Slice { .. } | Node::Output { .. } | Node::ModuleBytes(..) | Node::ResourceBytes(_) => {}
            Node::Concat(ns) | Node::Pack(ns) => ns.iter_mut().for_each(|n| n.visit_mut(f)),
            Node::Apply(a) => a.inputs.iter_mut().for_each(|n| n.visit_mut(f)),
            Node::Patch(p) => [&mut p.source, &mut p.patch].into_iter().for_each(|n| n.visit_mut(f)),
            Node::Either(e) => [&mut e.kept, &mut e.made].into_iter().for_each(|n| n.visit_mut(f)),
        }
    }

    /// Moves the streams the slices below take by `streams`, and the applications of several
    /// outputs the outputs below take by `shared`.
    pub(crate) fn shift(&mut self, streams: usize, shared: usize) {
        self.visit_mut(&mut |n| match n {
            Node::Slice { stream, .. } => *stream += streams,
            Node::Output { shared: s, .. } => *s += shared,
            _ => {}
        });
    }
}
