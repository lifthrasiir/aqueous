//! The contents of a formula placed ([`place_all`]): what is made apart, and the slices of the
//! contents.

use super::*;

aqr_common::zopt! {
    /// Contents a content made apart starts at most ahead of the first not sealed, where they are
    /// sealed ([`make_later`]), for each thread. On the Windows Server DVD (16 threads), the spill's
    /// peak and the time: 5.29 GB and 169.1 s with none; 4.45 GB, 171.4 s at 4; 4.24 GB, 175.2 s at
    /// 2 (where a long content holds the turn, fewer start meanwhile).
    pub(crate) static SEAL_AHEAD: Opt<usize> =
        Opt::value("seal-ahead", 4, "contents made apart that start ahead of the first not sealed, per thread");
}

/// Of each stream of `f` of data not made yet that an application among what its root
/// concatenates takes, that application and where its output starts in `f`'s.
pub(crate) fn applications_of_lazy(f: &Formula) -> HashMap<usize, (Node, usize)> {
    let mut out = HashMap::new();
    let mut at = 0usize;
    let mut walk = vec![&f.root];
    while let Some(n) = walk.pop() {
        match n {
            // (in order)
            Node::Concat(ns) => walk.extend(ns.iter().rev()),
            n => {
                if let Node::Apply(a) = n {
                    let mut lazy = None;
                    a.inputs.iter().for_each(|i| {
                        i.visit(&mut |m| {
                            if let &Node::Slice { stream, .. } = m
                                && f.streams.get(stream).is_some_and(|s| s.data.is_lazy())
                            {
                                lazy.get_or_insert(stream);
                            }
                        })
                    });
                    if let Some(s) = lazy {
                        out.insert(s, (n.clone(), at));
                    }
                }
                at += n.len() as usize;
            }
        }
    }
    out
}

/// About the thread time making data `d` takes, of its bytes and how long its format takes for
/// a byte: thread-milliseconds by MB of the files of each format of 10gb (`--stats`, an M4;
/// rounded), 50 for the others ([`formats::Spec::cost`]).
pub(crate) fn cost(d: &[u8]) -> u64 {
    let per_mb = detect::detect(d).map_or(formats::Spec::DEFAULT.cost, |f| f.cost());
    d.len() as u64 * per_mb
}

/// What a context of data made apart ([`make_later`], [`ahead`]) shares with the context it is
/// made from: the prevalidator, what the files archived together hold, their remaps and words,
/// the text noted and coded once, and whether module blocks are sized.
pub(crate) struct Apart {
    pub(crate) pre: Option<Arc<verify::Prevalidator>>,
    pub(crate) held: Option<Arc<Held>>,
    pub(crate) bodies: Option<Arc<inflated::Bodies>>,
    pub(crate) rewritten: Option<Arc<crate::lossy::Rewritten>>,
    pub(crate) remaps: Option<Arc<exe::Remaps>>,
    pub(crate) words: Option<Arc<aqr_wrt::formula::Shared>>,
    /// What was noted before it was made ([`Cx::coded_held`]), for each context made of it.
    pub(crate) coded_held: Arc<HashSet<(usize, u64)>>,
    pub(crate) once: Arc<Once>,
    pub(crate) shares_modules: bool,
    pub(crate) siblings: Option<Arc<siblings::Placed>>,
}

impl Cx {
    pub(crate) fn apart(&self) -> Apart {
        Apart {
            pre: self.pre.clone(),
            held: self.held.clone(),
            bodies: self.bodies.clone(),
            rewritten: self.rewritten.clone(),
            remaps: self.remaps.clone(),
            words: self.words.borrow().clone(),
            coded_held: Arc::new(self.coded_held.borrow().clone()),
            once: self.once.clone(),
            shares_modules: self.enc.shares_modules(),
            siblings: self.siblings.clone(),
        }
    }
}

impl Apart {
    /// A context of data of `n` bytes.
    pub(crate) fn cx(&self, n: usize, opt: &Options) -> Cx {
        let mut own = Cx::of_len(n, opt);
        if self.shares_modules {
            own.enc.share_modules();
        }
        own.pre = self.pre.clone();
        own.held = self.held.clone();
        own.bodies = self.bodies.clone();
        own.rewritten = self.rewritten.clone();
        own.remaps = self.remaps.clone();
        own.share_words(self.words.clone());
        own.coded_held = RefCell::new((*self.coded_held).clone());
        own.once = self.once.clone();
        own.siblings = self.siblings.clone();
        own
    }
}

/// Whether formula `f` of a content takes the place of its slice ([`nested`]): one of
/// applications, or aqr-ppmd's, which only cuts the header off the data, so that the data is
/// stored once where another file starts with it, or aqr-mkv's, which holds frames seen
/// before once, or a gzip or zlib stream's below level 4, which cuts its DEFLATE stream off for
/// files made of it ([`inflated::is`]), or a PE image's, which cuts its resources apart for the
/// same ones of other images to be stored once ([`exe::resources_apart`]).
pub(crate) fn takes_place(f: &Formula) -> bool {
    !f.applies().is_empty()
        || aqr_ppmd::formula::is(f)
        || aqr_mkv::formula::is(f)
        || aqueous::is(f)
        || inflated::is(f)
        || exe::resources_apart(f)
}

/// Contents, each its slice and its formula where it has one ([`nested`]).
pub(crate) type Contents = Vec<(Node, Option<Made>)>;

/// What takes the place of a content's slice: its formula, or an output of a set.
enum Placed {
    Made(Made),
    Node(Node),
}

/// How many slices there are of each stream of `f`.
pub(crate) fn slice_counts(f: &Formula) -> Vec<usize> {
    let mut slices = vec![0; f.streams.len()];
    f.visit(&mut |n| {
        if let &Node::Slice { stream, .. } = n
            && let Some(n) = slices.get_mut(stream)
        {
            *n += 1;
        }
    });
    slices
}

/// Puts the formulas of [`nested`] in place in `f`, and the outputs of the `sets` of them
/// ([`sets::sets`], whose formulas are `None` in `children`); the bytes left without a slice
/// are dropped.
pub(crate) fn place_all(f: &mut Formula, children: Vec<Option<Made>>, sets: Vec<sets::Set>) {
    // (the applications of several outputs [`nested`] looked into, not those of the sets)
    let own = f.shared.len();
    let mut outputs: Vec<Option<Node>> = (0..children.len()).map(|_| None).collect();
    for s in sets {
        let Node::Concat(outs) = f.embed(s.f) else { unreachable!("a set's root is its outputs") };
        s.members.into_iter().zip(outs).for_each(|(m, o)| outputs[m] = Some(o));
    }
    let children = children.into_iter().zip(outputs).map(|(c, o)| match o {
        Some(o) => Some(Placed::Node(o)),
        None => c.map(Placed::Made),
    });
    // (of the applications of several outputs too)
    let slices = slice_counts(f);
    let mut children = children.into_iter();
    // (the applications embedded in the inputs of one of them go before it, as its inputs take
    // their outputs)
    let mut order: Vec<usize> = (own..f.shared.len()).collect();
    for i in 0..own {
        let embedded = f.shared.len();
        // (a patch's source and patch, of contents kept as they are: nothing placed in them)
        if let Some(module) = f.shared[i].apply().map(|a| a.module) {
            let inputs = std::mem::take(&mut f.shared[i].apply_mut().expect("an application").inputs);
            let mut n = Node::apply(module, Vec::new(), inputs, 0);
            place(&mut n, f, &slices, &mut children);
            let Node::Apply(a) = n else { unreachable!("an application") };
            f.shared[i].apply_mut().expect("an application").inputs = a.inputs;
        } else if let aqr_formula::Of::Node(n) = &mut f.shared[i].of {
            // (a node split: what is placed in it is cut into the same outputs)
            let mut n = std::mem::replace(n, Node::Concat(Vec::new()));
            place(&mut n, f, &slices, &mut children);
            f.shared[i].of = aqr_formula::Of::Node(n);
        }
        order.extend(embedded..f.shared.len());
        order.push(i);
    }
    let mut root = std::mem::replace(&mut f.root, Node::Concat(Vec::new()));
    let embedded = f.shared.len();
    place(&mut root, f, &slices, &mut children);
    debug_assert!(children.next().is_none(), "a child left without a slice");
    f.root = root;
    order.extend(embedded..f.shared.len());
    f.reorder_shared(&order);
    exe::resolve(f);
    f.compact();
}

/// The applications of several outputs of `f`, as nodes: what is below them comes before the
/// root, wherever contents are looked for.
pub(crate) fn shared_nodes(f: &Formula) -> Vec<Node> {
    use aqr_formula::{Of, Patch};
    let node = |s: &aqr_formula::Shared| match &s.of {
        Of::Apply(a) => Node::Apply(Box::new(a.clone())),
        Of::Patch { inputs: [source, patch], len } => {
            Node::Patch(Box::new(Patch { source: source.clone(), patch: patch.clone(), len: *len }))
        }
        Of::Node(n) => n.clone(),
    };
    f.shared.iter().map(node).collect()
}

/// [`description_bound`] of all of `f`: its root, and its applications of several outputs with
/// their sizes.
pub(crate) fn bound_of(f: &Formula) -> usize {
    let shared = f.shared.iter().zip(shared_nodes(f)).map(|(s, n)| {
        let sizes: usize = s.sizes.iter().map(|&n| aqr_common::varint::len(n)).sum();
        description_bound(&n) + 1 + aqr_common::varint::len(s.sizes.len() as u64) + sizes
    });
    description_bound(&f.root) + shared.sum::<usize>()
}

/// The contents below node `n` of `f`, in order: each slice, and its formula where it has any
/// application: a slice of a stream that is [`Stream::content`] ([`Formula::mark_contents`]),
/// scanned. `apart` is [`bundle::contents_apart`] of `f`.
pub(crate) fn contents(
    n: &Node,
    f: &Formula,
    apart: bool,
    opt: &Options,
    depth: u32,
    cx: &Cx,
    out: &mut Vec<(Node, Option<Made>)>,
) {
    match n {
        &Node::Slice { stream, .. } if is_content(f, stream) => content_slices(n, f, apart, opt, depth, cx, out),
        // (patches are made of recompressed data, not looked into; nor alternatives, of contents
        // made already)
        Node::Slice { .. }
        | Node::Output { .. }
        | Node::Patch(_)
        | Node::ModuleBytes(..)
        | Node::ResourceBytes(_)
        | Node::Either(_) => {}
        Node::Concat(ns) | Node::Pack(ns) => ns.iter().for_each(|n| contents(n, f, apart, opt, depth, cx, out)),
        Node::Apply(a) => a.inputs.iter().for_each(|n| contents(n, f, apart, opt, depth, cx, out)),
    }
}

/// [`contents`] of node `n`, a content scanned: each of its slices is a content of the kind of
/// its stream, and the applications in it have contents of their own.
fn content_slices(
    n: &Node,
    f: &Formula,
    apart: bool,
    opt: &Options,
    depth: u32,
    cx: &Cx,
    out: &mut Vec<(Node, Option<Made>)>,
) {
    match n {
        &Node::Slice { stream, start, len } => {
            let s = f.streams.get(stream);
            // (data not made yet is made as it is come to, [`make_later`])
            if let Some(st) = s.filter(|s| s.data.is_lazy())
                && start + len <= st.data.len() as u64
                && apart
                && let Some(nest) = Nest::Scan.of(st.kind)
            {
                cx.later.borrow_mut().push((out.len(), n.clone(), nest));
                out.push((n.clone(), None));
                return;
            }
            let d = s.filter(|s| !s.data.is_lazy()).and_then(|s| s.data.get(start as usize..)?.get(..len as usize));
            // (the first of a set's contents with a parent: made in the set; any other alone)
            if d.is_some_and(|d| cx.skip.borrow_mut().remove(&(d.as_ptr() as usize, d.len()))) {
                out.push((n.clone(), None));
                return;
            }
            // (the files of an archive made later, at once, and so are those of an image or a
            // WIM file, and a ZIP archive's entries)
            if (depth == 0 && cx.top_sets.is_some() || apart)
                && let Some(nest) = s.and_then(|s| Nest::Scan.of(s.kind)).filter(|_| d.is_some())
            {
                cx.later.borrow_mut().push((out.len(), n.clone(), nest));
                out.push((n.clone(), None));
                return;
            }
            let child = s.zip(d).and_then(|(s, d)| made_deferring(d, opt, depth + 1, Nest::Scan.of(s.kind)?, cx));
            out.push((n.clone(), child.filter(|c| takes_place(&c.f))));
        }
        Node::Concat(ns) | Node::Pack(ns) => ns.iter().for_each(|n| content_slices(n, f, apart, opt, depth, cx, out)),
        Node::Apply(_) => contents(n, f, apart, opt, depth, cx, out),
        Node::Output { .. } | Node::Patch(_) | Node::ModuleBytes(..) | Node::ResourceBytes(_) | Node::Either(_) => {}
    }
}

/// Puts the formulas of the contents below node `n` of `f`, in the order of [`contents`], in
/// place of their slices: cut out of the stream of their data itself where they are all of it,
/// the only slice of it (of the `slices` of each stream).
fn place(n: &mut Node, f: &mut Formula, slices: &[usize], children: &mut impl Iterator<Item = Option<Placed>>) {
    match n {
        &mut Node::Slice { stream, .. } if is_content(f, stream) => place_slices(n, f, slices, children),
        Node::Slice { .. } | Node::Output { .. } | Node::Patch(_) | Node::ModuleBytes(..) | Node::ResourceBytes(_) => {}
        // (a decoding kept beside its coded bytes, [`Undo::defer`]: its contents are in the
        // formula made)
        Node::Either(e) => place(&mut e.made, f, slices, children),
        Node::Concat(ns) | Node::Pack(ns) => ns.iter_mut().for_each(|n| place(n, f, slices, children)),
        Node::Apply(a) => a.inputs.iter_mut().for_each(|n| place(n, f, slices, children)),
    }
}

/// [`place`] in node `n`, a content: in the order of [`content_slices`].
fn place_slices(n: &mut Node, f: &mut Formula, slices: &[usize], children: &mut impl Iterator<Item = Option<Placed>>) {
    match n {
        &mut Node::Slice { stream, start, len } => match children.next().flatten() {
            Some(Placed::Node(o)) => *n = o,
            // (a formula that lost by little beside the data as it is, a copy of what it keeps of it)
            Some(Placed::Made(child)) if let Some(sizes) = child.close => {
                let made = child.cut(&f.streams[stream].data[start as usize..][..len as usize]);
                let made = f.embed(made);
                *n = Node::Either(Box::new(aqr_formula::Either { kept: n.clone(), made, len, sizes }));
            }
            Some(Placed::Made(child)) => {
                // (a child is of a slice within its stream)
                let data = &mut f.streams[stream].data;
                // (one cut already, out of the bytes it took, takes none: [`make_later`])
                let child = if child.gaps.is_none() {
                    child.f
                } else if slices[stream] == 1 && start == 0 && len == data.len() as u64 {
                    child.cut_from(std::mem::take(data))
                } else {
                    child.cut(&data[start as usize..][..len as usize])
                };
                *n = f.embed(child);
            }
            None => {}
        },
        Node::Concat(ns) | Node::Pack(ns) => ns.iter_mut().for_each(|n| place_slices(n, f, slices, children)),
        Node::Apply(_) => place(n, f, slices, children),
        Node::Output { .. } | Node::Patch(_) | Node::ModuleBytes(..) | Node::ResourceBytes(_) | Node::Either(_) => {}
    }
}

/// Whether `stream` of `f` is [`Stream::content`].
pub(crate) fn is_content(f: &Formula, stream: usize) -> bool {
    f.streams.get(stream).is_some_and(|s| s.content)
}
