//! Formula blocks evaluated as streams of their values. A program is read as a graph of
//! values: each is taken once, as the stack machine has no Dup, so that only applications of
//! several outputs and Split instructions fork it. Each output is pulled from the graph a piece at a time, and each value is made as it is
//! first pulled:
//!
//! - a concatenation or a packed byte sequence is not made whole, its items pulled in turn;
//! - a slice is of its block, shared (a Split of a slice or a literal is cut where it is);
//! - an application takes its inputs whole and gives its outputs (of a whole module: the spec's
//!   part modules are applied the same way for now, their windows all of their data);
//! - the outputs of a node of several are each pulled by itself, those pulled out of turn held
//!   until they are;
//! - a value a Checksum instruction covers is hashed as it is pulled, and checked at its end.
//!
//! Written out ([`run`] with a sink), the parts of each output, through its concatenations, are
//! written in turn, those made by applications made ahead on workers of the pool; the blocks
//! are decoded ahead of their first slice as they are.

use std::collections::VecDeque;

use super::*;

/// A value of a program: output `out` of node `node`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct Val {
    node: usize,
    out: usize,
}

/// What a node of the graph makes: of an instruction, or a piece of a Split of a slice or a
/// literal. Of several outputs, it has the index of its state among the evaluation's ([`Fan`]).
enum Op<'p, 'a> {
    Literal(&'a [u8]),
    /// Of a non-leaf block, by its ordinal among all.
    Slice {
        block: usize,
        start: u64,
        len: u64,
    },
    Apply(&'p Apply<'a>, Vec<Val>, Option<usize>),
    Concat(Vec<Val>),
    Pack(Vec<Val>),
    Split(Val, &'p [u64], usize),
    Patch(Val, Val, u64),
}

/// A program as a graph of its values.
struct Graph<'p, 'a> {
    ops: Vec<Op<'p, 'a>>,
    /// The length of each node's output (of nodes of several outputs: 0).
    lens: Vec<u64>,
    /// The nodes of several outputs.
    fans: usize,
    /// The checksums of the values Checksum instructions cover.
    sums: HashMap<Val, Vec<u64>>,
    /// The outputs, in order.
    roots: Vec<Val>,
}

impl<'p, 'a> Graph<'p, 'a> {
    /// The graph of program `p` (read, so of a stack that never runs short), each slice cut by a
    /// Split counted among those of its block in `blocks`.
    fn of(p: &'p Program<'a>, blocks: &dyn Blocks) -> Decoded<Self> {
        let mut g = Graph { ops: Vec::new(), lens: Vec::new(), fans: 0, sums: HashMap::new(), roots: Vec::new() };
        let mut stack: Vec<Val> = Vec::new();
        for insn in &p.insns {
            match *insn {
                Insn::Nop | Insn::Concat(1) => {}
                Insn::Literal(l) => stack.push(g.push(Op::Literal(l), l.len() as u64)),
                Insn::Swap(depth) => {
                    let top = stack.len() - 1;
                    stack.swap(top, top - depth);
                }
                Insn::Slice { block, start, len } => {
                    stack.push(g.push(Op::Slice { block: p.blocks[block], start, len }, len));
                }
                Insn::Apply(ref a) => {
                    let inputs = stack.split_off(stack.len() - a.num_inputs);
                    let node = g.ops.len();
                    match a.output_sizes[..] {
                        [len] => stack.push(g.push(Op::Apply(a, inputs, None), len)),
                        _ => {
                            let fan = g.fan();
                            g.push(Op::Apply(a, inputs, Some(fan)), 0);
                            stack.extend((0..a.output_sizes.len()).map(|out| Val { node, out }));
                        }
                    }
                }
                Insn::Concat(n) => {
                    let parts = stack.split_off(stack.len() - n);
                    let len = parts.iter().map(|&v| g.len(v)).sum();
                    stack.push(g.push(Op::Concat(parts), len));
                }
                Insn::Pack(n) => {
                    let parts = stack.split_off(stack.len() - n);
                    let len = packed::header_len(n) + parts.iter().map(|&v| g.len(v)).sum::<u64>();
                    stack.push(g.push(Op::Pack(parts), len));
                }
                Insn::Split(ref sizes) => {
                    let x = stack.pop().unwrap();
                    if sizes.iter().try_fold(0u64, |s, &n| s.checked_add(n)) != Some(g.len(x)) {
                        return invalid("a split into sizes not adding up");
                    }
                    // slices and literals cut where they are (a block's slices counted first, as
                    // any is let go of once pulled)
                    match g.ops[x.node] {
                        Op::Slice { block, start, .. } if !g.sums.contains_key(&x) => {
                            match sizes.len() {
                                0 => blocks.release(block),
                                n => blocks.add(block, n - 1),
                            }
                            let mut at = start;
                            for &len in sizes {
                                stack.push(g.push(Op::Slice { block, start: at, len }, len));
                                at += len;
                            }
                        }
                        Op::Literal(l) if !g.sums.contains_key(&x) => {
                            let mut at = 0;
                            for &len in sizes {
                                let len = len as usize;
                                stack.push(g.push(Op::Literal(&l[at..at + len]), len as u64));
                                at += len;
                            }
                        }
                        _ => {
                            let (node, fan) = (g.ops.len(), g.fan());
                            g.push(Op::Split(x, sizes, fan), 0);
                            stack.extend((0..sizes.len()).map(|out| Val { node, out }));
                        }
                    }
                }
                Insn::Patch(len) => {
                    let patch = stack.pop().unwrap();
                    let source = stack.pop().unwrap();
                    stack.push(g.push(Op::Patch(source, patch, len), len));
                }
                Insn::Checksum(sum) => g.sums.entry(*stack.last().unwrap()).or_default().push(sum),
            }
        }
        g.roots = stack;
        Ok(g)
    }

    /// A node of a single output of `len` bytes (or of several, then of none): its value.
    fn push(&mut self, op: Op<'p, 'a>, len: u64) -> Val {
        self.ops.push(op);
        self.lens.push(len);
        Val { node: self.ops.len() - 1, out: 0 }
    }

    /// A state for a node of several outputs.
    fn fan(&mut self) -> usize {
        self.fans += 1;
        self.fans - 1
    }

    fn len(&self, v: Val) -> u64 {
        match self.ops[v.node] {
            Op::Apply(a, _, Some(_)) => a.output_sizes[v.out],
            Op::Split(_, sizes, _) => sizes[v.out],
            _ => self.lens[v.node],
        }
    }
}

/// The non-leaf blocks a graph slices: [`Contents`] in decoding.
trait Blocks: Sync {
    /// The data of block `o`.
    fn block(&self, o: usize) -> Decoded<Arc<Vec<u8>>>;

    /// One slice of block `o` pulled: once none is left, it is let go of.
    fn release(&self, o: usize);

    /// `n` more slices of block `o` to be pulled.
    fn add(&self, o: usize, n: usize);
}

impl Blocks for Contents<'_, '_> {
    fn block(&self, o: usize) -> Decoded<Arc<Vec<u8>>> {
        Contents::block(self, o)
    }

    fn release(&self, o: usize) {
        Contents::release(self, o);
    }

    fn add(&self, o: usize, n: usize) {
        self.refs[o].fetch_add(n, Ordering::Relaxed);
    }
}

/// A piece of a value: of the formula block's data (a literal), or shared (of a block, or of
/// bytes made).
#[derive(Clone)]
enum Chunk<'a> {
    Borrowed(&'a [u8]),
    Shared(Arc<Vec<u8>>, Range<usize>),
}

impl Chunk<'_> {
    fn owned(d: Vec<u8>) -> Self {
        let n = d.len();
        Chunk::Shared(Arc::new(d), 0..n)
    }

    /// Its first `n` bytes, and the rest.
    fn split(self, n: usize) -> (Self, Self) {
        match self {
            Chunk::Borrowed(d) => (Chunk::Borrowed(&d[..n]), Chunk::Borrowed(&d[n..])),
            Chunk::Shared(d, r) => {
                let at = r.start + n;
                (Chunk::Shared(d.clone(), r.start..at), Chunk::Shared(d, at..r.end))
            }
        }
    }
}

impl std::ops::Deref for Chunk<'_> {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match self {
            Chunk::Borrowed(d) => d,
            Chunk::Shared(d, r) => &d[r.clone()],
        }
    }
}

/// A value pulled a piece at a time.
trait Source<'a>: Send {
    /// The next piece of the value (not empty), none once it is all given.
    fn next(&mut self, at: At<'_, '_, 'a>) -> Decoded<Option<Chunk<'a>>>;
}

type Src<'a> = Box<dyn Source<'a> + 'a>;

/// The evaluation of a graph: how it applies a module (an application of the program, its
/// evaluated inputs), its blocks, and the states of its nodes of several outputs.
struct Eval<'e, 'a> {
    g: &'e Graph<'e, 'a>,
    #[allow(clippy::type_complexity)]
    apply: &'e (dyn Fn(&Apply<'a>, &[&[u8]]) -> Decoded<Vec<Vec<u8>>> + Sync),
    blocks: &'e dyn Blocks,
    fans: Vec<Mutex<Fan<'a>>>,
}

/// Where values are pulled: of an evaluation, and (on the thread writing the outputs out) what
/// to tell of each block as it is first sliced, to decode those after it ahead.
#[derive(Clone, Copy)]
struct At<'r, 'e, 'a> {
    ev: &'r Eval<'e, 'a>,
    ahead: Option<&'r (dyn Fn(usize) + Sync)>,
}

/// The state of a node of several outputs.
enum Fan<'a> {
    /// Not pulled yet.
    Idle,
    /// An application's outputs, each until it is pulled.
    Made(Vec<Option<Vec<u8>>>),
    /// A Split's input, pulled so far: the bytes of it pulled, the output they reach into (and
    /// where it starts), and those of each output not yet pulled of it.
    Split {
        input: Src<'a>,
        read: u64,
        out: usize,
        start: u64,
        held: Vec<VecDeque<Chunk<'a>>>,
    },
    Failed(Failure),
}

impl<'a> Eval<'_, 'a> {
    /// The source of value `v`.
    fn source(&self, v: Val) -> Src<'a> {
        let s: Src<'a> = match self.g.ops[v.node] {
            Op::Literal(l) => Box::new(Once(Some(Chunk::Borrowed(l)).filter(|c| !c.is_empty()))),
            Op::Slice { block, start, len } => Box::new(SliceSource { block, start, len, done: false }),
            Op::Apply(_, _, None) | Op::Patch(..) => Box::new(Made { node: v.node, done: false }),
            Op::Apply(..) | Op::Split(..) => Box::new(Fanned { v, end: None }),
            Op::Concat(_) => Box::new(Items { node: v.node, next: 0, cur: None, header: false }),
            Op::Pack(_) => Box::new(Items { node: v.node, next: 0, cur: None, header: true }),
        };
        match self.g.sums.contains_key(&v) {
            true => Box::new(Summed { inner: s, v, hash: Xxh64::new(0), done: false }),
            false => s,
        }
    }
}

/// A value of one piece.
struct Once<'a>(Option<Chunk<'a>>);

impl<'a> Source<'a> for Once<'a> {
    fn next(&mut self, _: At<'_, '_, 'a>) -> Decoded<Option<Chunk<'a>>> {
        Ok(self.0.take())
    }
}

struct SliceSource {
    block: usize,
    start: u64,
    len: u64,
    done: bool,
}

impl<'a> Source<'a> for SliceSource {
    fn next(&mut self, at: At<'_, '_, 'a>) -> Decoded<Option<Chunk<'a>>> {
        if std::mem::replace(&mut self.done, true) {
            return Ok(None);
        }
        if let Some(tell) = at.ahead {
            tell(self.block);
        }
        let d = at.ev.blocks.block(self.block)?;
        at.ev.blocks.release(self.block);
        let r = usize::try_from(self.start)
            .ok()
            .zip(usize::try_from(self.len).ok())
            .and_then(|(s, l)| Some(s..s.checked_add(l)?))
            .filter(|r| r.end <= d.len())
            .ok_or_else(|| Failure::Invalid("slice out of its block".into()))?;
        Ok((!r.is_empty()).then_some(Chunk::Shared(d, r)))
    }
}

/// A value of a node of a single output made whole: an application's, a patch's.
struct Made {
    node: usize,
    done: bool,
}

impl<'a> Source<'a> for Made {
    fn next(&mut self, at: At<'_, '_, 'a>) -> Decoded<Option<Chunk<'a>>> {
        if std::mem::replace(&mut self.done, true) {
            return Ok(None);
        }
        let out = match at.ev.g.ops[self.node] {
            Op::Apply(a, ref inputs, _) => applied(at, a, inputs)?.pop().expect("an output"),
            Op::Patch(source, patch, len) => {
                let (source, patch) = (whole(at, source)?, whole(at, patch)?);
                crate::patch::apply(&source, &patch, len).map_err(|e| Failure::Invalid(e.0))?
            }
            _ => unreachable!("an application or a patch"),
        };
        Ok((!out.is_empty()).then(|| Chunk::owned(out)))
    }
}

/// The outputs of application `a` on `inputs`, pulled whole.
fn applied<'a>(at: At<'_, '_, 'a>, a: &Apply<'a>, inputs: &[Val]) -> Decoded<Vec<Vec<u8>>> {
    let inputs = inputs.iter().map(|&v| whole(at, v)).collect::<Decoded<Vec<_>>>()?;
    let bytes: Vec<&[u8]> = inputs.iter().map(|i| &**i).collect();
    (at.ev.apply)(a, &bytes)
}

/// A value pulled whole: in one piece where it is (a slice, say), else gathered.
enum Whole<'a> {
    Empty,
    One(Chunk<'a>),
    Gathered(Vec<u8>),
}

impl Whole<'_> {
    fn into_vec(self) -> Vec<u8> {
        match self {
            Whole::Empty => Vec::new(),
            Whole::One(c) => match c {
                Chunk::Shared(d, r) if r.start == 0 && r.end == d.len() => {
                    Arc::try_unwrap(d).unwrap_or_else(|d| d[..].to_vec())
                }
                c => c.to_vec(),
            },
            Whole::Gathered(d) => d,
        }
    }
}

impl std::ops::Deref for Whole<'_> {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match self {
            Whole::Empty => &[],
            Whole::One(c) => c,
            Whole::Gathered(d) => d,
        }
    }
}

/// Value `v` pulled whole.
fn whole<'a>(at: At<'_, '_, 'a>, v: Val) -> Decoded<Whole<'a>> {
    let mut s = at.ev.source(v);
    let Some(first) = s.next(at)? else { return Ok(Whole::Empty) };
    let Some(second) = s.next(at)? else { return Ok(Whole::One(first)) };
    let mut out = Vec::with_capacity(at.ev.g.len(v) as usize);
    out.extend_from_slice(&first);
    out.extend_from_slice(&second);
    drop((first, second));
    while let Some(c) = s.next(at)? {
        out.extend_from_slice(&c);
    }
    Ok(Whole::Gathered(out))
}

/// The items of a concatenation or a packed byte sequence (its header first), in turn.
struct Items<'a> {
    node: usize,
    next: usize,
    cur: Option<Src<'a>>,
    header: bool,
}

impl<'a> Source<'a> for Items<'a> {
    fn next(&mut self, at: At<'_, '_, 'a>) -> Decoded<Option<Chunk<'a>>> {
        let g = at.ev.g;
        let (Op::Concat(items) | Op::Pack(items)) = &g.ops[self.node] else { unreachable!("items") };
        if std::mem::take(&mut self.header) {
            let mut h = Vec::new();
            packed::put_header(&mut h, items.iter().map(|&v| g.len(v)));
            return Ok(Some(Chunk::owned(h)));
        }
        loop {
            if let Some(s) = &mut self.cur {
                if let Some(c) = s.next(at)? {
                    return Ok(Some(c));
                }
                self.cur = None;
            }
            let Some(&v) = items.get(self.next) else { return Ok(None) };
            self.next += 1;
            self.cur = Some(at.ev.source(v));
        }
    }
}

/// An output of a node of several: and where it ends, of a Split's input.
struct Fanned {
    v: Val,
    end: Option<u64>,
}

impl<'a> Source<'a> for Fanned {
    fn next(&mut self, at: At<'_, '_, 'a>) -> Decoded<Option<Chunk<'a>>> {
        let (ev, v) = (at.ev, self.v);
        let op = &ev.g.ops[v.node];
        let (Op::Apply(_, _, Some(fan)) | Op::Split(_, _, fan)) = *op else {
            unreachable!("a node of several outputs")
        };
        // (pulled with the state held: what it pulls is of nodes under it, of other states)
        let mut f = ev.fans[fan].lock().unwrap();
        if let Fan::Idle = *f {
            *f = match *op {
                Op::Apply(a, ref inputs, _) => match applied(at, a, inputs) {
                    Ok(outs) => Fan::Made(outs.into_iter().map(Some).collect()),
                    Err(e) => Fan::Failed(e),
                },
                Op::Split(x, sizes, _) => Fan::Split {
                    input: ev.source(x),
                    read: 0,
                    out: 0,
                    start: 0,
                    held: sizes.iter().map(|_| VecDeque::new()).collect(),
                },
                _ => unreachable!("a node of several outputs"),
            };
        }
        match &mut *f {
            Fan::Made(outs) => Ok(outs[v.out].take().filter(|o| !o.is_empty()).map(Chunk::owned)),
            Fan::Split { input, read, out, start, held } => {
                let Op::Split(_, sizes, _) = *op else { unreachable!("a split") };
                let end = *self.end.get_or_insert_with(|| sizes[..=v.out].iter().sum());
                loop {
                    if let Some(c) = held[v.out].pop_front() {
                        return Ok(Some(c));
                    }
                    if *read >= end {
                        return Ok(None);
                    }
                    let Some(mut c) = input.next(at)? else {
                        return invalid("a split of a value shorter than declared");
                    };
                    // (its bytes to the outputs they are of: those of others held until pulled)
                    while !c.is_empty() {
                        while *start + sizes[*out] <= *read {
                            *start += sizes[*out];
                            *out += 1;
                        }
                        let n = c.len().min((*start + sizes[*out] - *read) as usize);
                        let (piece, rest) = c.split(n);
                        held[*out].push_back(piece);
                        *read += n as u64;
                        c = rest;
                    }
                }
            }
            Fan::Failed(e) => Err(e.clone()),
            Fan::Idle => unreachable!("pulled"),
        }
    }
}

/// A value a Checksum instruction covers, hashed as it is pulled.
struct Summed<'a> {
    inner: Src<'a>,
    v: Val,
    hash: Xxh64,
    done: bool,
}

impl<'a> Source<'a> for Summed<'a> {
    fn next(&mut self, at: At<'_, '_, 'a>) -> Decoded<Option<Chunk<'a>>> {
        if self.done {
            return Ok(None);
        }
        match self.inner.next(at)? {
            Some(c) => {
                self.hash.update(&c);
                Ok(Some(c))
            }
            None => {
                self.done = true;
                let sum = self.hash.digest();
                match at.ev.g.sums[&self.v].iter().all(|&s| s == sum) {
                    true => Ok(None),
                    false => invalid("output checksum mismatch"),
                }
            }
        }
    }
}

/// Parts of an output evaluated at most at once ahead of where they are written ([`run`]), and
/// their bytes at most held ahead (but the next part's).
const AHEAD_PARTS: usize = 8;
const AHEAD_PART_BYTES: u64 = 256 << 20;

/// Blocks decoded at most at once ahead of their first slice ([`run`]).
const AHEAD_BLOCKS: usize = 3;

/// The outputs of formula block `p`, with the non-leaf blocks in `contents`. Where `sink` is
/// given, they are written to it in turn as they are pulled, rather than held whole, and none
/// is returned.
pub(super) fn run<'a>(
    p: Program<'a>,
    env: &Env,
    contents: &Contents,
    sink: Option<&mut Taker<'_>>,
    threads: usize,
) -> Decoded<Vec<Vec<u8>>> {
    let g = Graph::of(&p, contents)?;
    let apply = |a: &Apply<'a>, inputs: &[&[u8]]| {
        let res: Vec<Resource> = a.resources.iter().map(|&i| env.resources[p.resources[i]].clone()).collect();
        super::apply(env, p.modules[a.module], a, inputs, &res)
    };
    let ev =
        Eval { g: &g, apply: &apply, blocks: contents, fans: (0..g.fans).map(|_| Mutex::new(Fan::Idle)).collect() };
    let Some(sink) = sink else {
        return blocks_ahead(&g, contents, |ahead| {
            let at = At { ev: &ev, ahead };
            g.roots.iter().map(|&r| whole(at, r).map(Whole::into_vec)).collect()
        });
    };
    // the parts of each output, through its concatenations (but those a Checksum instruction
    // covers, which are pulled as they are: the output's own hashed as it is written), and the
    // applications among them, made ahead
    let roots: Vec<(Vec<Val>, bool)> = g.roots.iter().map(|&r| parts_of(&g, r)).collect();
    let heavy: Vec<(Val, u64)> = roots
        .iter()
        .flat_map(|(parts, _)| parts)
        .filter(|v| matches!(g.ops[v.node], Op::Apply(_, _, None)))
        .map(|&v| (v, g.len(v)))
        .collect();
    let slots: Vec<Waited<Option<Decoded<Vec<u8>>>>> = heavy.iter().map(|_| Waited::new(None)).collect();
    let mut emit = |parts: Option<&PartsAhead<'_, '_, '_, 'a>>| -> Decoded<Vec<Vec<u8>>> {
        blocks_ahead(&g, contents, |ahead| {
            let at = At { ev: &ev, ahead };
            let mut k = 0;
            for (&r, (of, own)) in g.roots.iter().zip(&roots) {
                write(at, r, of, *own, &mut *sink, parts, &mut k)?;
            }
            Ok(Vec::new())
        })
    };
    if threads < 2 || heavy.len() < 2 {
        return emit(None);
    }
    aqr_common::pool::scope(threads.min(AHEAD_PARTS), |s| {
        let parts = (s.workers() > 0).then(|| PartsAhead {
            scope: s,
            ev: &ev,
            heavy: &heavy,
            slots: &slots,
            spawned: Cell::new(0),
            running: Arc::default(),
        });
        emit(parts.as_ref())
    })
}

/// The parts of output `r`, through its concatenations but those a Checksum instruction covers,
/// in order: and whether its own checksum is to be checked as they are written (where it is a
/// concatenation gone through).
fn parts_of(g: &Graph, r: Val) -> (Vec<Val>, bool) {
    let through = |v: Val| matches!(g.ops[v.node], Op::Concat(_));
    if !through(r) {
        return (vec![r], false);
    }
    let (mut parts, mut todo) = (Vec::new(), vec![r]);
    while let Some(v) = todo.pop() {
        match &g.ops[v.node] {
            Op::Concat(items) if v == r || (through(v) && !g.sums.contains_key(&v)) => todo.extend(items.iter().rev()),
            _ => parts.push(v),
        }
    }
    (parts, g.sums.contains_key(&r))
}

/// Output `r` written to `sink` by its `parts` ([`parts_of`]), its checksum checked where `own`:
/// the applications among them (the `k`th onwards of those of all outputs) taken of `ahead`
/// where given.
fn write(
    at: At,
    r: Val,
    parts: &[Val],
    own: bool,
    sink: &mut Taker,
    ahead: Option<&PartsAhead>,
    k: &mut usize,
) -> Decoded<()> {
    let mut hash = own.then(|| Xxh64::new(0));
    let mut put = |d: &[u8]| {
        if let Some(h) = &mut hash {
            h.update(d);
        }
        sink(d)
    };
    for &v in parts {
        if let Op::Apply(_, _, None) = at.ev.g.ops[v.node] {
            *k += 1;
            if let Some(a) = ahead {
                put(&a.take(*k - 1)?)?;
                continue;
            }
        }
        let mut s = at.ev.source(v);
        while let Some(c) = s.next(at)? {
            put(&c)?;
        }
    }
    match hash {
        Some(h) if at.ev.g.sums[&r].iter().any(|&s| s != h.digest()) => invalid("output checksum mismatch"),
        _ => Ok(()),
    }
}

/// The applications among the parts of the outputs, evaluated ahead ([`run`]): each its value
/// and length, its bytes once made (on a worker of `scope`), and those spawned so far.
struct PartsAhead<'s, 'env, 'e, 'a> {
    scope: &'s aqr_common::pool::Scope<'s, 'env>,
    ev: &'s Eval<'e, 'a>,
    heavy: &'s [(Val, u64)],
    /// Each told of its part made ahead.
    slots: &'s [Waited<Option<Decoded<Vec<u8>>>>],
    spawned: Cell<usize>,
    running: Arc<AtomicUsize>,
}

impl PartsAhead<'_, '_, '_, '_> {
    /// The bytes of part `k` (taken in order): made ahead and waited for, else made now; the
    /// parts after it spawned on the scope's workers while they are free.
    fn take(&self, k: usize) -> Decoded<Vec<u8>> {
        let here = k >= self.spawned.get();
        let mut next = self.spawned.get().max(k + 1);
        // (the bytes of those made ahead and not yet taken)
        let mut held: u64 = self.heavy[k + 1..next].iter().map(|h| h.1).sum();
        while next < self.heavy.len()
            && self.running.load(Ordering::Relaxed) < self.scope.workers()
            && (next == k + 1 || held + self.heavy[next].1 <= AHEAD_PART_BYTES)
        {
            let (v, len) = self.heavy[next];
            held += len;
            let (ev, slot, running) = (self.ev, &self.slots[next], self.running.clone());
            running.fetch_add(1, Ordering::Relaxed);
            self.scope.spawn(move || {
                let d = whole(At { ev, ahead: None }, v).map(Whole::into_vec);
                *slot.lock().unwrap() = Some(d);
                running.fetch_sub(1, Ordering::Relaxed);
                slot.notify_all();
            });
            next += 1;
        }
        self.spawned.set(next);
        if here {
            return whole(At { ev: self.ev, ahead: None }, self.heavy[k].0).map(Whole::into_vec);
        }
        let mut d = self.slots[k].lock().unwrap();
        loop {
            if let Some(d) = d.take() {
                return d;
            }
            d = self.slots[k].wait(d).unwrap();
        }
    }
}

/// `f` given what to tell of each block as it is first sliced, by which the blocks after it (in
/// the order `g` first slices them) are decoded ahead on workers of the pool where they are
/// free, [`AHEAD_BLOCKS`] at most at once.
fn blocks_ahead<R>(g: &Graph, contents: &Contents, f: impl FnOnce(Option<&(dyn Fn(usize) + Sync)>) -> R) -> R {
    let mut firsts: Vec<usize> = Vec::new();
    let mut first_at: HashMap<usize, usize> = HashMap::new();
    for op in &g.ops {
        if let &Op::Slice { block, .. } = op {
            first_at.entry(block).or_insert_with(|| {
                firsts.push(block);
                firsts.len() - 1
            });
        }
    }
    if firsts.len() < 2 {
        return f(None);
    }
    let (next, running) = (AtomicUsize::new(1), Arc::new(AtomicUsize::new(0)));
    aqr_common::pool::scope(AHEAD_BLOCKS, |s| {
        if s.workers() == 0 {
            return f(None);
        }
        let tell = |o: usize| {
            let Some(&at) = first_at.get(&o) else { return };
            // (those after the block first sliced now, as workers are free)
            let mut n = next.load(Ordering::Relaxed).max(at + 1);
            while n < firsts.len() && n <= at + AHEAD_BLOCKS && running.load(Ordering::Relaxed) < s.workers() {
                let (b, running) = (firsts[n], running.clone());
                running.fetch_add(1, Ordering::Relaxed);
                s.spawn(move || {
                    contents.ahead(b);
                    running.fetch_sub(1, Ordering::Relaxed);
                });
                n += 1;
            }
            next.store(n, Ordering::Relaxed);
        };
        f(Some(&tell))
    })
}
