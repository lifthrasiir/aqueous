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
//!   until they are (beyond a bound, in a file);
//! - a value a Checksum instruction covers is hashed as it is pulled, and checked at its end.
//!
//! Written out ([`run`] with a sink), the parts of each output, through its concatenations, are
//! written in turn, those made by applications made ahead (a whole module's on workers of the
//! pool, a part module's on a thread of its own, its output queued until written, within the
//! bound of the bytes held ahead); the blocks are decoded ahead of their first slice as they are.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Condvar;

use super::spool::Spool;
use super::*;
use crate::chash::{Ch, Hashed, Records, record_key};

/// A value of a program: output `out` of node `node`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct Val {
    node: usize,
    out: usize,
}

/// An application of a graph: an Apply instruction of a formula block, or an application of a
/// [`crate::Formula`] (of a single output, or of several of its `shared`, as a Split of an
/// application of one where its outputs are split).
#[derive(Clone, Copy)]
enum Ap<'p, 'a> {
    Block(&'p Apply<'a>),
    One(&'p crate::Apply),
    Shared(&'p crate::Shared),
}

/// The Apply instruction of application `a` of a formula block.
fn block<'p, 'a>(a: Ap<'p, 'a>) -> &'p Apply<'a> {
    match a {
        Ap::Block(a) => a,
        _ => unreachable!("of a formula block"),
    }
}

impl<'p> Ap<'p, '_> {
    /// The sizes of its outputs.
    fn sizes(&self) -> &'p [u64] {
        match *self {
            Ap::Block(a) => &a.output_sizes,
            Ap::One(a) => std::slice::from_ref(&a.len),
            Ap::Shared(s) if s.split => std::slice::from_ref(s.apply().map_or(&0, |a| &a.len)),
            Ap::Shared(s) => &s.sizes,
        }
    }
}

/// What a node of the graph makes: of an instruction, or a piece of a Split of a slice or a
/// literal. Of several outputs, it has the index of its state among the evaluation's ([`Fan`]).
enum Op<'p, 'a> {
    Literal(&'a [u8]),
    /// Of a formula's stream not at hand (sunk, or not made): failing where it is pulled.
    Absent,
    /// Of a non-leaf block, by its ordinal among all.
    Slice {
        block: usize,
        start: u64,
        len: u64,
    },
    Apply(Ap<'p, 'a>, Vec<Val>, Option<usize>),
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
                        [len] => stack.push(g.push(Op::Apply(Ap::Block(a), inputs, None), len)),
                        _ => {
                            let fan = g.fan();
                            g.push(Op::Apply(Ap::Block(a), inputs, Some(fan)), 0);
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
            Op::Apply(a, _, Some(_)) => a.sizes()[v.out],
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
/// evaluated inputs), whether one is a part module and how it applies that, its blocks, the
/// states of its nodes of several outputs, and the threads applying part modules.
struct Eval<'e, 'a> {
    g: &'e Graph<'e, 'a>,
    #[allow(clippy::type_complexity)]
    apply: &'e (dyn Fn(Ap<'e, 'a>, &[&[u8]]) -> Decoded<Vec<Vec<u8>>> + Sync),
    part: &'e (dyn Fn(Ap<'e, 'a>) -> bool + Sync),
    #[allow(clippy::type_complexity)]
    apply_part: &'e (dyn Fn(Ap<'e, 'a>, &mut [&mut dyn PartIn], &mut [&mut dyn PartOut]) -> Decoded<()> + Sync),
    blocks: &'e dyn Blocks,
    fans: Vec<Mutex<Fan<'a>>>,
    threads: Threads,
    /// Of an evaluation of a value of its own ([`apart`]): the slices of each block left to be
    /// pulled, and the block while any is (of `blocks`, which may let go of it before).
    kept: Option<Mutex<Counted>>,
}

/// Of each block, the slices left to be pulled and its data while any is ([`Eval::kept`]).
type Counted = HashMap<usize, (usize, Option<Arc<Vec<u8>>>)>;

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
    /// A part module's application running: the queues of its outputs, each until it is pulled.
    Part(Vec<Option<Arc<Queue>>>),
    /// A Split's input, pulled so far: the bytes of it pulled, the output they reach into (and
    /// where it starts), and those of each output not yet pulled of it.
    Split {
        input: Src<'a>,
        read: u64,
        out: usize,
        start: u64,
        held: Vec<Held<'a>>,
    },
    Failed(Failure),
}

impl<'a> Eval<'_, 'a> {
    /// The data of block `o`: of [`Eval::kept`] where it is there, else of the blocks (kept then
    /// while slices of it are left to be pulled).
    fn block(&self, o: usize) -> Decoded<Arc<Vec<u8>>> {
        let Some(kept) = &self.kept else { return self.blocks.block(o) };
        if let Some((_, Some(d))) = kept.lock().unwrap().get(&o) {
            return Ok(d.clone());
        }
        let d = self.blocks.block(o)?;
        Ok(match kept.lock().unwrap().get_mut(&o) {
            Some((left, held)) if *left > 0 => held.get_or_insert(d).clone(),
            _ => d,
        })
    }

    /// One slice of block `o` pulled ([`Blocks::release`]).
    fn release(&self, o: usize) {
        self.blocks.release(o);
        if let Some(kept) = &self.kept
            && let Some(e) = kept.lock().unwrap().get_mut(&o)
        {
            e.0 = e.0.saturating_sub(1);
            if e.0 == 0 {
                e.1 = None;
            }
        }
    }

    /// Whether value `v` is made whole by an application, ahead where it is a part of an
    /// output written out ([`run`]): not a part module's ([`Eval::made_streamed`]).
    fn made_whole(&self, v: Val) -> bool {
        matches!(self.g.ops[v.node], Op::Apply(a, _, None) if !(self.part)(a))
    }

    /// Whether value `v` is an application of a part module of a single output, which is
    /// started ahead as well ([`run`]), its output queued whole until it is written.
    fn made_streamed(&self, v: Val) -> bool {
        matches!(self.g.ops[v.node], Op::Apply(a, _, None) if (self.part)(a))
    }

    /// The source of value `v`.
    fn source(&self, v: Val) -> Src<'a> {
        let s: Src<'a> = match self.g.ops[v.node] {
            Op::Literal(l) => Box::new(Once(Some(Chunk::Borrowed(l)).filter(|c| !c.is_empty()))),
            Op::Absent => Box::new(Absent),
            Op::Slice { block, start, len } => Box::new(SliceSource { block, start, len, done: false }),
            Op::Apply(a, _, None) if (self.part)(a) => Box::new(Queued { node: v.node, q: None }),
            Op::Apply(_, _, None) | Op::Patch(..) => Box::new(Made { node: v.node, done: false }),
            Op::Apply(..) | Op::Split(..) => Box::new(Fanned { v, end: None, q: None }),
            Op::Concat(_) => Box::new(Items { node: v.node, next: 0, cur: None, header: false }),
            Op::Pack(_) => Box::new(Items { node: v.node, next: 0, cur: None, header: true }),
        };
        match self.g.sums.contains_key(&v) {
            true => Box::new(Summed { inner: s, v, hash: Xxh64::new(0), done: false }),
            false => s,
        }
    }
}

/// A value not at hand ([`Op::Absent`]).
struct Absent;

impl<'a> Source<'a> for Absent {
    fn next(&mut self, _: At<'_, '_, 'a>) -> Decoded<Option<Chunk<'a>>> {
        invalid("a stream not at hand")
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
        let d = at.ev.block(self.block)?;
        at.ev.release(self.block);
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
fn applied<'e, 'a>(at: At<'_, 'e, 'a>, a: Ap<'e, 'a>, inputs: &[Val]) -> Decoded<Vec<Vec<u8>>> {
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
    /// Of a part module's application, the queue of its output.
    q: Option<Arc<Queue>>,
}

impl Drop for Fanned {
    fn drop(&mut self) {
        if let Some(q) = &self.q {
            q.cancel();
        }
    }
}

impl<'a> Source<'a> for Fanned {
    fn next(&mut self, at: At<'_, '_, 'a>) -> Decoded<Option<Chunk<'a>>> {
        if let Some(q) = &self.q {
            return q.pop();
        }
        let (ev, v) = (at.ev, self.v);
        let op = &ev.g.ops[v.node];
        let (Op::Apply(_, _, Some(fan)) | Op::Split(_, _, fan)) = *op else {
            unreachable!("a node of several outputs")
        };
        // (pulled with the state held: what it pulls is of nodes under it, of other states)
        let mut f = ev.fans[fan].lock().unwrap();
        if let Fan::Idle = *f {
            *f = match *op {
                Op::Apply(a, _, _) if (ev.part)(a) => {
                    Fan::Part(start(at, v.node, usize::MAX, HELD_BYTES).into_iter().map(Some).collect())
                }
                Op::Apply(a, ref inputs, _) => match applied(at, a, inputs) {
                    Ok(outs) => Fan::Made(outs.into_iter().map(Some).collect()),
                    Err(e) => Fan::Failed(e),
                },
                Op::Split(x, sizes, _) => Fan::Split {
                    input: ev.source(x),
                    read: 0,
                    out: 0,
                    start: 0,
                    held: sizes.iter().map(|_| Held::default()).collect(),
                },
                _ => unreachable!("a node of several outputs"),
            };
        }
        match &mut *f {
            Fan::Made(outs) => Ok(outs[v.out].take().filter(|o| !o.is_empty()).map(Chunk::owned)),
            Fan::Part(qs) => {
                let q = qs[v.out].take().expect("each output pulled by one");
                drop(f);
                self.q.insert(q).pop()
            }
            Fan::Split { input, read, out, start, held } => {
                let Op::Split(_, sizes, _) = *op else { unreachable!("a split") };
                let end = *self.end.get_or_insert_with(|| sizes[..=v.out].iter().sum());
                loop {
                    if let Some(c) = held[v.out].pop()? {
                        return Ok(Some(c));
                    }
                    if *read >= end {
                        return Ok(None);
                    }
                    let Some(mut c) = input.next(at)? else {
                        return invalid("a split of a value shorter than declared");
                    };
                    // (its bytes to the outputs they are of: those of others held until pulled)
                    let mut mine = None;
                    while !c.is_empty() {
                        while *start + sizes[*out] <= *read {
                            *start += sizes[*out];
                            *out += 1;
                        }
                        let n = c.len().min((*start + sizes[*out] - *read) as usize);
                        let (piece, rest) = c.split(n);
                        match *out == v.out {
                            true => mine = Some(piece),
                            false => held[*out].push(piece)?,
                        }
                        *read += n as u64;
                        c = rest;
                    }
                    if mine.is_some() {
                        return Ok(mine);
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
    let resources = |a: &Apply<'a>| -> Vec<Resource> {
        a.resources.iter().map(|&i| env.resources[p.resources[i]].clone()).collect()
    };
    let loaded = |a: &Apply<'a>| {
        let m = p.modules[a.module];
        env.loaded[m].get_or_init(|| env.runner.load(&env.modules[m])).as_ref().map_err(Failure::clone)
    };
    let apply = |a: Ap<'_, 'a>, inputs: &[&[u8]]| {
        let a = block(a);
        super::apply(env, p.modules[a.module], a, inputs, &resources(a))
    };
    let part = |a: Ap<'_, 'a>| loaded(block(a)).is_ok_and(|l| l.is_part());
    let apply_part = |a: Ap<'_, 'a>, ins: &mut [&mut dyn PartIn], outs: &mut [&mut dyn PartOut]| {
        let a = block(a);
        super::within_limit(a, p.modules[a.module])?;
        loaded(a)?.apply_part(a.params, &resources(a), ins, outs, &a.output_sizes, a.memory_limit, &a.task_limits)
    };
    let ev = Eval {
        g: &g,
        apply: &apply,
        part: &part,
        apply_part: &apply_part,
        blocks: contents,
        fans: (0..g.fans).map(|_| Mutex::new(Fan::Idle)).collect(),
        threads: Threads::default(),
        kept: None,
    };
    // (the threads of part modules let go of and joined before what they borrow goes)
    let _joined = Joined(&ev);
    emit(&g, &ev, Some(contents), sink, threads)
}

/// The outputs of graph `g` evaluated by `ev` (of the non-leaf blocks in `contents` where it
/// slices any), written to `sink` where given as [`run`] writes them.
fn emit<'e, 'a>(
    g: &'e Graph<'e, 'a>,
    ev: &Eval<'e, 'a>,
    contents: Option<&Contents>,
    sink: Option<&mut Taker<'_>>,
    threads: usize,
) -> Decoded<Vec<Vec<u8>>> {
    let Some(sink) = sink else {
        return blocks_ahead(g, contents, |ahead| {
            let at = At { ev, ahead };
            g.roots.iter().map(|&r| whole(at, r).map(Whole::into_vec)).collect()
        });
    };
    // the parts of each output, through its concatenations (but those a Checksum instruction
    // covers, which are pulled as they are: the output's own hashed as it is written), and the
    // applications among them, made ahead
    let roots: Vec<(Vec<Val>, bool)> = g.roots.iter().map(|&r| parts_of(g, r)).collect();
    let heavy: Vec<(Val, u64)> = roots
        .iter()
        .flat_map(|(parts, _)| parts)
        .filter(|&&v| ev.made_whole(v) || ev.made_streamed(v))
        .map(|&v| (v, g.len(v)))
        .collect();
    let slots: Vec<Waited<Option<Decoded<Ahead>>>> = heavy.iter().map(|_| Waited::new(None)).collect();
    let mut emit = |parts: Option<&PartsAhead<'_, '_, '_, 'a>>| -> Decoded<Vec<Vec<u8>>> {
        blocks_ahead(g, contents, |ahead| {
            let at = At { ev, ahead };
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
            ev,
            heavy: &heavy,
            slots: &slots,
            spawned: Cell::new(0),
            running: Arc::default(),
            queues: RefCell::default(),
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
        if at.ev.made_whole(v) || at.ev.made_streamed(v) {
            *k += 1;
            if let Some(a) = ahead {
                match a.take(*k - 1)? {
                    Ahead::Bytes(d) => put(&d)?,
                    Ahead::Queue(q) => {
                        while let Some(c) = q.pop()? {
                            put(&c)?;
                        }
                    }
                    Ahead::Here => {
                        let mut s = at.ev.source(v);
                        while let Some(c) = s.next(at)? {
                            put(&c)?;
                        }
                    }
                }
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
/// and length, its bytes once made (a whole module's on a worker of `scope`; a part module's
/// output queued whole as it is made, on a thread of its own), and those spawned so far.
struct PartsAhead<'s, 'env, 'e, 'a> {
    scope: &'s aqr_common::pool::Scope<'s, 'env>,
    ev: &'s Eval<'e, 'a>,
    heavy: &'s [(Val, u64)],
    /// Each told of its part made ahead.
    slots: &'s [Waited<Option<Decoded<Ahead>>>],
    spawned: Cell<usize>,
    running: Arc<AtomicUsize>,
    /// The queues of the part modules started, to count those still at work.
    queues: RefCell<Vec<Arc<Queue>>>,
}

/// A part made ahead of its turn to be written.
enum Ahead {
    /// A whole module's output.
    Bytes(Vec<u8>),
    /// A part module's output, as it is made.
    Queue(Arc<Queue>),
    /// Not made ahead: to be pulled now.
    Here,
}

impl PartsAhead<'_, '_, '_, '_> {
    /// Part `k` (taken in order): made ahead and waited for, else to be made now; the parts
    /// after it started while the pool's workers are free and what they hold is within
    /// [`AHEAD_PART_BYTES`].
    fn take(&self, k: usize) -> Decoded<Ahead> {
        let here = k >= self.spawned.get();
        let mut next = self.spawned.get().max(k + 1);
        // (the bytes of those made ahead and not yet taken)
        let mut held: u64 = self.heavy[k + 1..next].iter().map(|h| h.1).sum();
        while next < self.heavy.len()
            && self.running() < self.scope.workers()
            && (next == k + 1 || held + self.heavy[next].1 <= AHEAD_PART_BYTES)
        {
            let (v, len) = self.heavy[next];
            held += len;
            if self.ev.made_streamed(v) {
                // (its queue holds all of it, in memory as the bound above goes: it never waits)
                let q = start(At { ev: self.ev, ahead: None }, v.node, (len as usize).max(1), len as usize).pop();
                let q = q.expect("an output");
                self.queues.borrow_mut().push(q.clone());
                *self.slots[next].lock().unwrap() = Some(Ok(Ahead::Queue(q)));
            } else {
                let (ev, slot, running) = (self.ev, &self.slots[next], self.running.clone());
                running.fetch_add(1, Ordering::Relaxed);
                self.scope.spawn(move || {
                    let d = whole(At { ev, ahead: None }, v).map(|w| Ahead::Bytes(w.into_vec()));
                    *slot.lock().unwrap() = Some(d);
                    running.fetch_sub(1, Ordering::Relaxed);
                    slot.notify_all();
                });
            }
            next += 1;
        }
        self.spawned.set(next);
        if here {
            return match self.ev.made_streamed(self.heavy[k].0) {
                true => Ok(Ahead::Here),
                false => whole(At { ev: self.ev, ahead: None }, self.heavy[k].0).map(|w| Ahead::Bytes(w.into_vec())),
            };
        }
        let mut d = self.slots[k].lock().unwrap();
        loop {
            if let Some(d) = d.take() {
                return d;
            }
            d = self.slots[k].wait(d).unwrap();
        }
    }

    /// The applications ahead at work: whole modules' and part modules' not yet done.
    fn running(&self) -> usize {
        let mut queues = self.queues.borrow_mut();
        queues.retain(|q| !q.over());
        self.running.load(Ordering::Relaxed) + queues.len()
    }
}

/// `f` given what to tell of each block as it is first sliced, by which the blocks after it (in
/// the order `g` first slices them) are decoded ahead on workers of the pool where they are
/// free, [`AHEAD_BLOCKS`] at most at once.
fn blocks_ahead<R>(g: &Graph, contents: Option<&Contents>, f: impl FnOnce(Option<&(dyn Fn(usize) + Sync)>) -> R) -> R {
    let Some(contents) = contents else { return f(None) };
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

/// Bytes the queue of a part module's application's output holds at most before the module
/// waits for them to be pulled, where it is its only output (else all: see [`Fan`]).
const QUEUE_BYTES: usize = 16 << 20;

/// Bytes held at most in memory of what an output of several of a node is pulled out of its
/// turn (a queue's, or a split's), the rest in a file ([`Spool`]).
#[cfg(not(test))]
const HELD_BYTES: usize = 64 << 20;
#[cfg(test)]
const HELD_BYTES: usize = 1 << 20;

/// The pieces of an output of a Split pulled out of its turn: shared, until they are more than
/// [`HELD_BYTES`]; the rest copied to a spool.
#[derive(Default)]
struct Held<'a> {
    pieces: VecDeque<Chunk<'a>>,
    bytes: usize,
    spool: Option<Spool>,
}

impl<'a> Held<'a> {
    fn push(&mut self, c: Chunk<'a>) -> Decoded<()> {
        match &mut self.spool {
            None if self.bytes + c.len() <= HELD_BYTES => {
                self.bytes += c.len();
                self.pieces.push_back(c);
                Ok(())
            }
            s => s.get_or_insert_with(|| Spool::new(0)).push(&c).map_err(spooled),
        }
    }

    fn pop(&mut self) -> Decoded<Option<Chunk<'a>>> {
        if let Some(c) = self.pieces.pop_front() {
            self.bytes -= c.len();
            return Ok(Some(c));
        }
        match &mut self.spool {
            Some(s) => s.pop().map(|p| p.map(Chunk::owned)).map_err(spooled),
            None => Ok(None),
        }
    }
}

/// A failure of a spool's file.
fn spooled(e: std::io::Error) -> Failure {
    Failure::Resource(format!("a file of bytes held: {e}"))
}

/// The bytes of an output of a part module's application, as the module commits them, until
/// they are pulled: at most `cap` of them, the module waiting for room.
struct Queue {
    state: Mutex<QueueState>,
    changed: std::sync::Condvar,
    cap: usize,
}

struct QueueState {
    pieces: Spool,
    /// Whether all is committed, or the application failed.
    done: bool,
    failed: Option<Failure>,
    /// Whether it is no longer pulled: the evaluation ends, or what pulls it went.
    gone: bool,
}

impl Queue {
    fn new(cap: usize, held: usize) -> Arc<Queue> {
        let state = QueueState { pieces: Spool::new(held), done: false, failed: None, gone: false };
        Arc::new(Queue { state: Mutex::new(state), changed: std::sync::Condvar::new(), cap })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, QueueState> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn push(&self, d: &[u8]) -> Decoded<()> {
        let mut q = self.lock();
        while !q.gone && !q.pieces.is_empty() && q.pieces.len() + d.len() as u64 > self.cap as u64 {
            q = self.changed.wait(q).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        if q.gone {
            return resource("an output no longer pulled");
        }
        q.pieces.push(d).map_err(spooled)?;
        drop(q);
        self.changed.notify_all();
        Ok(())
    }

    /// Whether the application is over: all committed, or failed, or no longer pulled.
    fn over(&self) -> bool {
        let q = self.lock();
        q.done || q.failed.is_some() || q.gone
    }

    /// All committed (`r` of the application succeeding), or the application failed.
    fn end(&self, r: &Decoded<()>) {
        let mut q = self.lock();
        match r {
            Ok(()) => q.done = true,
            Err(e) => q.failed = Some(e.clone()),
        }
        drop(q);
        self.changed.notify_all();
    }

    fn pop<'a>(&self) -> Decoded<Option<Chunk<'a>>> {
        let mut q = self.lock();
        loop {
            if let Some(p) = q.pieces.pop().map_err(spooled)? {
                drop(q);
                self.changed.notify_all();
                return Ok(Some(Chunk::owned(p)));
            }
            if let Some(e) = &q.failed {
                return Err(e.clone());
            }
            if q.done {
                return Ok(None);
            }
            if q.gone {
                return resource("an evaluation given up");
            }
            q = self.changed.wait(q).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    /// No longer pulled: the module fails where it commits more.
    fn cancel(&self) {
        self.lock().gone = true;
        self.changed.notify_all();
    }
}

/// The output of a part module's application of a single output, pulled of its queue (the
/// application started as it is first pulled).
struct Queued {
    node: usize,
    q: Option<Arc<Queue>>,
}

impl Drop for Queued {
    fn drop(&mut self) {
        if let Some(q) = &self.q {
            q.cancel();
        }
    }
}

impl<'a> Source<'a> for Queued {
    fn next(&mut self, at: At<'_, '_, 'a>) -> Decoded<Option<Chunk<'a>>> {
        let node = self.node;
        let q = self.q.get_or_insert_with(|| start(at, node, QUEUE_BYTES, HELD_BYTES).pop().expect("an output"));
        q.pop()
    }
}

/// Starts the application of a part module of node `node` on a thread of its own, each of its
/// outputs to a queue of `cap` bytes: their queues.
fn start(at: At, node: usize, cap: usize, held: usize) -> Vec<Arc<Queue>> {
    let ev = at.ev;
    let Op::Apply(a, ref inputs, _) = ev.g.ops[node] else { unreachable!("an application") };
    let qs: Vec<Arc<Queue>> = a.sizes().iter().map(|_| Queue::new(cap, held)).collect();
    ev.threads.queues.lock().unwrap().extend(qs.iter().map(Arc::downgrade));
    let outs = qs.clone();
    let run = move || {
        let at = At { ev, ahead: None };
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut ins: Vec<Pulled> = inputs
                .iter()
                .map(|&v| Pulled { at, src: ev.source(v), len: ev.g.len(v), done: 0, rest: None })
                .collect();
            let mut pushed: Vec<Pushed> = outs.iter().map(|q| Pushed { q: q.clone(), n: 0 }).collect();
            let mut ins_dyn: Vec<&mut dyn PartIn> = ins.iter_mut().map(|i| i as &mut dyn PartIn).collect();
            let mut outs_dyn: Vec<&mut dyn PartOut> = pushed.iter_mut().map(|o| o as &mut dyn PartOut).collect();
            (ev.apply_part)(a, &mut ins_dyn, &mut outs_dyn)?;
            match pushed.iter().zip(a.sizes()).all(|(o, &n)| o.n == n) {
                true => Ok(()),
                false => invalid("a part module's outputs of other sizes than declared"),
            }
        }));
        let r = r.unwrap_or_else(|_| invalid("a part module panicked"));
        outs.iter().for_each(|q| q.end(&r));
    };
    // SAFETY: what it borrows (the evaluation and what it does) goes only once it is joined
    // ([`Joined`])
    unsafe { ev.threads.spawn(Box::new(run)) };
    qs
}

/// An input of a part module's application: its value pulled as the module requests it, how far,
/// and what is left of the piece pulled last.
struct Pulled<'r, 'e, 'a> {
    at: At<'r, 'e, 'a>,
    src: Src<'a>,
    len: u64,
    done: u64,
    rest: Option<Chunk<'a>>,
}

impl PartIn for Pulled<'_, '_, '_> {
    fn len(&self) -> u64 {
        self.len
    }

    fn pull(&mut self, hi: u64, put: &mut dyn FnMut(&[u8])) -> Decoded<()> {
        while self.done < hi {
            let c = match self.rest.take() {
                Some(c) => c,
                None => match self.src.next(self.at)? {
                    Some(c) => c,
                    None => return invalid("an input shorter than declared"),
                },
            };
            let n = c.len().min((hi - self.done) as usize);
            let (head, tail) = c.split(n);
            put(&head);
            self.done += n as u64;
            self.rest = (!tail.is_empty()).then_some(tail);
        }
        Ok(())
    }
}

/// An output of a part module's application, to its queue: and the bytes pushed.
struct Pushed {
    q: Arc<Queue>,
    n: u64,
}

impl PartOut for Pushed {
    fn push(&mut self, d: &[u8]) -> Decoded<()> {
        self.n += d.len() as u64;
        self.q.push(d)
    }
}

/// The threads applying part modules, and the queues of their outputs.
#[derive(Default)]
struct Threads {
    running: Mutex<Vec<std::thread::JoinHandle<()>>>,
    queues: Mutex<Vec<std::sync::Weak<Queue>>>,
}

impl Threads {
    /// Runs `f` on a thread of its own.
    ///
    /// # Safety
    ///
    /// What `f` borrows outlives [`Threads::join`].
    unsafe fn spawn<'f>(&self, f: Box<dyn FnOnce() + Send + 'f>) {
        // SAFETY: joined before what it borrows goes, as the caller ensures
        let f: Box<dyn FnOnce() + Send + 'static> = unsafe { std::mem::transmute(f) };
        let t = std::thread::Builder::new().name("aqr-part".into()).spawn(f).expect("a thread");
        self.running.lock().unwrap().push(t);
    }

    /// Every queue let go of (the modules committing to them failing), and every thread
    /// joined, those they spawn too.
    fn join(&self) {
        loop {
            self.queues.lock().unwrap().drain(..).filter_map(|q| q.upgrade()).for_each(|q| q.cancel());
            let Some(t) = self.running.lock().unwrap().pop() else { break };
            let _ = t.join();
        }
    }
}

/// Joins the threads of an evaluation as it goes, its states of nodes of several outputs let go
/// first (the queues they pull of among them).
struct Joined<'r, 'e, 'a>(&'r Eval<'e, 'a>);

impl Drop for Joined<'_, '_, '_> {
    fn drop(&mut self) {
        for f in &self.0.fans {
            *f.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Fan::Idle;
        }
        self.0.threads.join();
    }
}

/// The content hashes of the outputs of program `p` (each with the checksums Checksum
/// instructions take of it), running a module only where they are not told otherwise: of
/// literals and slices by their bytes (the blocks decoded a few at a time, each once), of
/// concatenations, packed byte sequences and splits by those of their items, and of applications
/// by `records` (of their modules as `identify` tells them, of the hashes of their inputs). A
/// value whose hash is not told so, but is needed (an input of an application, an output), is
/// evaluated (the blocks it slices decoded again, of `raw`), its hash made as it is pulled, and
/// an application evaluated so told to `records` ([`Records::learn`]). None where a value but an
/// output has a checksum.
#[allow(clippy::too_many_arguments)]
pub(super) fn hashed(
    p: &Program,
    env: &Env,
    contents: &Contents,
    raw: &[(&[u8], bool, Option<usize>)],
    dictionaries: &[dict::Dictionary],
    records: &dyn Records,
    identify: &dyn Fn(&[u8]) -> Option<String>,
    threads: usize,
) -> Decoded<Option<Vec<Hashed>>> {
    let g = Graph::of(p, contents)?;
    if g.sums.keys().any(|v| !g.roots.contains(v)) {
        return Ok(None);
    }
    // the slices hashed by block, each block decoded once and let go of as its slices are
    // hashed, [`AHEAD_BLOCKS`] at once
    let mut by_block: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (n, op) in g.ops.iter().enumerate() {
        if let Op::Slice { block, .. } = *op {
            by_block.entry(block).or_default().push(n);
        }
    }
    let by_block: Vec<(usize, Vec<usize>)> = by_block.into_iter().collect();
    let hashes = aqr_common::pool::par_map(by_block.len(), threads.clamp(1, AHEAD_BLOCKS + 1), |k| {
        let (block, ref nodes) = by_block[k];
        let d = contents.block(block)?;
        let mut out = Vec::with_capacity(nodes.len());
        for &n in nodes {
            let Op::Slice { start, len, .. } = g.ops[n] else { unreachable!("a slice") };
            contents.release(block);
            let r = usize::try_from(start)
                .ok()
                .zip(usize::try_from(len).ok())
                .and_then(|(s, l)| Some(s..s.checked_add(l)?))
                .filter(|r| r.end <= d.len())
                .ok_or_else(|| Failure::Invalid("slice out of its block".into()))?;
            out.push((n, Ch::of(&d[r])));
        }
        Ok(out)
    });
    let mut sliced: HashMap<usize, Ch> = HashMap::new();
    for h in hashes {
        sliced.extend(h?);
    }
    // what the hashes do not tell, evaluated as decoding does (the blocks decoded again)
    let resources =
        |a: &Apply| -> Vec<Resource> { a.resources.iter().map(|&i| env.resources[p.resources[i]].clone()).collect() };
    let loaded = |a: &Apply| {
        let m = p.modules[a.module];
        env.loaded[m].get_or_init(|| env.runner.load(&env.modules[m])).as_ref().map_err(Failure::clone)
    };
    let apply = |a: Ap<'_, '_>, inputs: &[&[u8]]| {
        let a = block(a);
        super::apply(env, p.modules[a.module], a, inputs, &resources(a))
    };
    let part = |a: Ap<'_, '_>| loaded(block(a)).is_ok_and(|l| l.is_part());
    let apply_part = |a: Ap<'_, '_>, ins: &mut [&mut dyn PartIn], outs: &mut [&mut dyn PartOut]| {
        let a = block(a);
        super::within_limit(a, p.modules[a.module])?;
        loaded(a)?.apply_part(a.params, &resources(a), ins, outs, &a.output_sizes, a.memory_limit, &a.task_limits)
    };
    let fresh = Fresh { raw, dictionaries, cache: Mutex::default(), decoded: Condvar::new() };
    let ev = Eval {
        g: &g,
        apply: &apply,
        part: &part,
        apply_part: &apply_part,
        blocks: &fresh,
        // (none: each value evaluated is of an evaluation of its own, [`apart`])
        fans: Vec::new(),
        threads: Threads::default(),
        kept: None,
    };
    let names: Vec<Option<String>> = p.modules.iter().map(|&m| identify(&env.modules[m])).collect();
    let key = |a: &Apply, ins: &[Ch]| -> Option<u128> {
        let name = names[a.module].as_deref()?;
        let res: Vec<Ch> = a.resources.iter().map(|&i| Ch::of(&env.resources[p.resources[i]])).collect();
        Some(record_key(name, a.params, &res, ins, &a.output_sizes))
    };
    let mut h = Hashing {
        at: At { ev: &ev, ahead: None },
        vals: Vec::with_capacity(g.ops.len()),
        records,
        key: &key,
        made: HashMap::new(),
        pieces: HashMap::new(),
    };
    for (n, op) in g.ops.iter().enumerate() {
        let outs = match *op {
            Op::Apply(_, _, Some(_)) | Op::Split(..) => g.out_count(n),
            _ => 1,
        };
        let get = |vals: &[Vec<Option<Vec<Ch>>>], v: Val| vals[v.node][v.out].clone();
        let made: Vec<Option<Vec<Ch>>> = match *op {
            Op::Literal(l) => vec![Some(vec![Ch::of(l)])],
            Op::Slice { .. } => vec![Some(vec![sliced[&n]])],
            Op::Absent | Op::Patch(..) => vec![None],
            Op::Concat(ref items) => {
                vec![items.iter().map(|&v| get(&h.vals, v)).collect::<Option<Vec<_>>>().map(|p| p.concat())]
            }
            Op::Pack(ref items) => {
                let mut head = Vec::new();
                packed::put_header(&mut head, items.iter().map(|&v| g.len(v)));
                let items = items.iter().map(|&v| get(&h.vals, v)).collect::<Option<Vec<_>>>();
                vec![items.map(|p| [vec![Ch::of(&head)], p.concat()].concat())]
            }
            Op::Split(x, sizes, _) => match get(&h.vals, x).and_then(|p| cut(&p, sizes)) {
                Some(pieces) => pieces.into_iter().map(Some).collect(),
                None => vec![None; outs],
            },
            Op::Apply(Ap::One(_) | Ap::Shared(_), ..) => vec![None; outs],
            Op::Apply(Ap::Block(a), ref inputs, fan) => {
                h.vals.push(vec![None; outs]);
                // (the inputs' hashes, made where they are not told)
                let mut ins = Vec::with_capacity(inputs.len());
                for &v in inputs {
                    ins.push(h.need(v)?);
                }
                h.vals.pop();
                let told = key(a, &ins).and_then(|k| records.outputs(k));
                match (told, fan) {
                    (Some(pieces), Some(_))
                        if pieces.len() == outs && pieces.iter().zip(&a.output_sizes).all(|(c, &n)| c.len() == n) =>
                    {
                        pieces.into_iter().map(|c| Some(vec![c])).collect()
                    }
                    (Some(pieces), None) if joined(&pieces).len() == a.output_sizes[0] => vec![Some(pieces)],
                    _ => vec![None; outs],
                }
            }
        };
        h.vals.push(made);
    }
    h.ahead(threads);
    let roots = g.roots.iter().map(|&r| Ok((h.need(r)?, g.sums.get(&r).cloned().unwrap_or_default())));
    roots.collect::<Decoded<Vec<_>>>().map(Some)
}

/// The hash of pieces of hashes put together.
fn joined(pieces: &[Ch]) -> Ch {
    pieces.iter().fold(Ch::EMPTY, |h, &c| h.cat(c))
}

impl Graph<'_, '_> {
    /// The outputs of node `n`.
    fn out_count(&self, n: usize) -> usize {
        match self.ops[n] {
            Op::Apply(a, _, Some(_)) => a.sizes().len(),
            Op::Split(_, sizes, _) => sizes.len(),
            _ => 1,
        }
    }

    /// The slices of each block that values `from` pull, evaluated (at most: a Split's input is
    /// pulled only as far as its outputs pulled reach), each node of several outputs pulling its
    /// inputs once.
    fn slices_of(&self, from: &[Val]) -> HashMap<usize, usize> {
        let mut out = HashMap::new();
        let mut fans = HashSet::new();
        let mut todo: Vec<usize> = from.iter().map(|v| v.node).collect();
        while let Some(n) = todo.pop() {
            match self.ops[n] {
                Op::Slice { block, .. } => *out.entry(block).or_insert(0) += 1,
                Op::Literal(_) | Op::Absent => {}
                Op::Concat(ref items) | Op::Pack(ref items) => todo.extend(items.iter().map(|v| v.node)),
                Op::Apply(_, ref inputs, None) => todo.extend(inputs.iter().map(|v| v.node)),
                Op::Apply(_, ref inputs, Some(_)) if fans.insert(n) => todo.extend(inputs.iter().map(|v| v.node)),
                Op::Split(x, ..) if fans.insert(n) => todo.push(x.node),
                Op::Apply(..) | Op::Split(..) => {}
                Op::Patch(x, y, _) => todo.extend([x.node, y.node]),
            }
        }
        out
    }
}

/// The hashes of the values of a graph as they are known ([`hashed`]): each node's outputs' (as
/// pieces), those not told none, and how they are evaluated where they are needed.
struct Hashing<'r, 'e, 'a> {
    at: At<'r, 'e, 'a>,
    vals: Vec<Vec<Option<Vec<Ch>>>>,
    records: &'r dyn Records,
    key: &'r dyn Fn(&Apply, &[Ch]) -> Option<u128>,
    /// The hashes of values made beforehand, on threads ([`Hashing::ahead`]): of a value, and of
    /// a Split's outputs (by its node).
    made: HashMap<Val, Decoded<Ch>>,
    pieces: HashMap<usize, Decoded<Vec<Ch>>>,
}

impl Hashing<'_, '_, '_> {
    /// The hash of value `v` (of a node told already), evaluated where it is not told: a
    /// concatenation or a packed byte sequence by its items, an application of a single output
    /// by its inputs' hashes and its output (told to the records), anything else by its bytes.
    fn need(&mut self, v: Val) -> Decoded<Ch> {
        if let Some(p) = &self.vals[v.node][v.out] {
            return Ok(joined(p));
        }
        let g = self.at.ev.g;
        let h = match g.ops[v.node] {
            Op::Concat(ref items) => {
                items.iter().try_fold(Ch::EMPTY, |h, &c| Ok::<_, Failure>(h.cat(self.need(c)?)))?
            }
            Op::Pack(ref items) => {
                let mut head = Vec::new();
                packed::put_header(&mut head, items.iter().map(|&v| g.len(v)));
                items.iter().try_fold(Ch::of(&head), |h, &c| Ok::<_, Failure>(h.cat(self.need(c)?)))?
            }
            Op::Apply(Ap::Block(a), ref inputs, None) => {
                let ins = inputs.iter().map(|&i| self.need(i)).collect::<Decoded<Vec<Ch>>>()?;
                let out = match self.made.remove(&v) {
                    Some(made) => made?,
                    None => self.stream(v)?,
                };
                if let Some(k) = (self.key)(a, &ins) {
                    self.records.learn(k, &[out]);
                }
                out
            }
            Op::Split(x, sizes, _) => {
                // (all its outputs at once, by one pass over its input: pulled one by one, each
                // would hold the pieces before it, of a file in pieces all over an image)
                let pieces = match self.pieces.remove(&v.node) {
                    Some(made) => made?,
                    None => self.split(x, sizes)?,
                };
                let h = pieces[v.out];
                self.vals[v.node] = pieces.into_iter().map(|c| Some(vec![c])).collect();
                return Ok(h);
            }
            Op::Apply(_, _, Some(_)) => {
                // (all its outputs at once, by one run: pulled one by one, each would run it)
                let outs = self.fanned(v.node)?;
                let h = outs[v.out];
                self.vals[v.node] = outs.into_iter().map(|c| Some(vec![c])).collect();
                return Ok(h);
            }
            _ => match self.made.remove(&v) {
                Some(made) => made?,
                None => self.stream(v)?,
            },
        };
        self.vals[v.node][v.out] = Some(vec![h]);
        Ok(h)
    }

    /// The values the outputs need evaluated ([`Hashing::need`]: those whose hashes are not
    /// told), evaluated at once beforehand on up to `threads` threads, their hashes kept for
    /// `need` to take: an image's applications, thousands in a leaf of its own, took the verify's
    /// time one after another. (Not the outputs of an application of several: those are pulled
    /// of its state, in turn.)
    fn ahead(&mut self, threads: usize) {
        if threads < 2 {
            return;
        }
        let g = self.at.ev.g;
        let mut heavy = Vec::new();
        let mut seen = HashSet::new();
        let mut todo: Vec<Val> = g.roots.iter().rev().copied().collect();
        while let Some(v) = todo.pop() {
            if self.vals[v.node][v.out].is_some() || !seen.insert(v) {
                continue;
            }
            match g.ops[v.node] {
                Op::Concat(ref items) | Op::Pack(ref items) => todo.extend(items.iter().rev()),
                Op::Apply(Ap::Block(_), ref inputs, None) => {
                    todo.extend(inputs.iter().rev());
                    heavy.push(v);
                }
                Op::Split(..) if seen.insert(Val { node: v.node, out: usize::MAX }) => heavy.push(v),
                Op::Split(..) | Op::Apply(_, _, Some(_)) => {}
                _ => heavy.push(v),
            }
        }
        if heavy.len() < 2 {
            return;
        }
        // (by the first block each slices, so that those made at once share the blocks they decode)
        let first = |v: Val| -> usize {
            let mut todo = vec![v];
            let mut seen = HashSet::new();
            while let Some(v) = todo.pop() {
                if !seen.insert(v.node) {
                    continue;
                }
                match g.ops[v.node] {
                    Op::Slice { block, .. } => return block,
                    Op::Concat(ref items) | Op::Pack(ref items) => todo.extend(items.iter().rev()),
                    Op::Apply(_, ref inputs, _) => todo.extend(inputs.iter().rev()),
                    Op::Split(x, ..) => todo.push(x),
                    Op::Patch(x, y, _) => todo.extend([y, x]),
                    _ => {}
                }
            }
            usize::MAX
        };
        heavy.sort_by_cached_key(|&v| first(v));
        // (in batches of [`AHEAD_HASHED_BYTES`] of output at most, as each holds its output and
        // its inputs while it is made)
        let at = self.at;
        let mut rest = &heavy[..];
        while !rest.is_empty() {
            let (mut n, mut bytes) = (0, 0);
            while n < rest.len() && (n == 0 || bytes + g.len(rest[n]) <= AHEAD_HASHED_BYTES) {
                bytes += g.len(rest[n]);
                n += 1;
            }
            let (batch, after) = rest.split_at(n);
            rest = after;
            let made = aqr_common::pool::par_map(batch.len(), threads, |k| {
                let v = batch[k];
                match g.ops[v.node] {
                    Op::Split(x, sizes, _) => Err(split_at(at, x, sizes)),
                    _ => Ok(stream_at(at, v)),
                }
            });
            for (&v, m) in batch.iter().zip(made) {
                match m {
                    Ok(h) => _ = self.made.insert(v, h),
                    Err(p) => _ = self.pieces.insert(v.node, p),
                }
            }
        }
    }

    /// The hashes of value `x` cut into pieces of `sizes`, evaluated, as it is pulled.
    fn split(&self, x: Val, sizes: &[u64]) -> Decoded<Vec<Ch>> {
        split_at(self.at, x, sizes)
    }

    /// The hash of value `v`, evaluated, as it is pulled.
    fn stream(&self, v: Val) -> Decoded<Ch> {
        stream_at(self.at, v)
    }

    /// The hashes of the outputs of application `node` of several, evaluated, as they are pulled
    /// in turn (the module run once).
    fn fanned(&self, node: usize) -> Decoded<Vec<Ch>> {
        fanned_at(self.at, node)
    }
}

/// `f` run on an evaluation of its own, as `at`'s but of states of nodes of several outputs not
/// pulled yet, and of threads of its own (joined as it ends): the hashes of values evaluated in
/// turn, or at once on threads, each pulls what it is made of anew, where those of `at` would give
/// what another value pulled of them already (a Split of a value of another Split's outputs, of a
/// disk of fragmented files cut into a DMG's chunks, pulled the second again).
///
/// The blocks are kept for it while slices of them are left to be pulled of `from` (the values it
/// pulls): of a block cache of its own, the evaluation of a disk's Split decoded the blocks of
/// its files' classes again and again, in the order of the disk (50 GB of a DMG of 1.3 GB), and
/// the pieces held of them (a Split's outputs pulled out of turn) kept the copies.
fn apart<T>(at: At, from: &[Val], f: impl FnOnce(At) -> T) -> T {
    let ev = Eval {
        g: at.ev.g,
        apply: at.ev.apply,
        part: at.ev.part,
        apply_part: at.ev.apply_part,
        blocks: at.ev.blocks,
        fans: (0..at.ev.g.fans).map(|_| Mutex::new(Fan::Idle)).collect(),
        threads: Threads::default(),
        kept: Some(Mutex::new(at.ev.g.slices_of(from).into_iter().map(|(b, n)| (b, (n, None))).collect())),
    };
    let _joined = Joined(&ev);
    f(At { ev: &ev, ahead: at.ahead })
}

/// [`Hashing::split`] of `at`.
fn split_at(at: At, x: Val, sizes: &[u64]) -> Decoded<Vec<Ch>> {
    apart(at, &[x], |at| {
        let mut s = at.ev.source(x);
        let mut out = Vec::with_capacity(sizes.len());
        let (mut h, mut left) = (Ch::EMPTY, sizes.first().copied().unwrap_or(0));
        let next = |h: &mut Ch, left: &mut u64, out: &mut Vec<Ch>| {
            while *left == 0 && out.len() < sizes.len() {
                out.push(std::mem::replace(h, Ch::EMPTY));
                *left = sizes.get(out.len()).copied().unwrap_or(0);
            }
        };
        next(&mut h, &mut left, &mut out);
        while let Some(c) = s.next(at)? {
            let mut c: &[u8] = &c;
            while !c.is_empty() {
                if out.len() == sizes.len() {
                    return invalid("a split of a value longer than declared");
                }
                let n = c.len().min(left as usize);
                h = h.then(&c[..n]);
                (left, c) = (left - n as u64, &c[n..]);
                next(&mut h, &mut left, &mut out);
            }
        }
        match out.len() == sizes.len() {
            true => Ok(out),
            false => invalid("a split of a value shorter than declared"),
        }
    })
}

/// [`Hashing::stream`] of `at`.
fn stream_at(at: At, v: Val) -> Decoded<Ch> {
    apart(at, &[v], |at| hash_of(at, v))
}

/// [`Hashing::fanned`] of `at`.
fn fanned_at(at: At, node: usize) -> Decoded<Vec<Ch>> {
    let outs: Vec<Val> = (0..at.ev.g.out_count(node)).map(|out| Val { node, out }).collect();
    apart(at, &outs, |at| outs.iter().map(|&v| hash_of(at, v)).collect())
}

/// The hash of value `v`, pulled of the states of `at`.
fn hash_of(at: At, v: Val) -> Decoded<Ch> {
    let mut s = at.ev.source(v);
    let mut h = Ch::EMPTY;
    while let Some(c) = s.next(at)? {
        h = h.then(&c);
    }
    Ok(h)
}

/// Bytes of the blocks [`Fresh`] keeps decoded (but [`AHEAD_BLOCKS`] and one, whatever their
/// size): an image's applications on threads take their inputs of several blocks each, of many
/// small ones (thousands, of the classes of its files), which a few were decoded again for each.
const FRESH_BYTES: usize = 512 << 20;

/// Bytes of output at most of the values [`Hashing::ahead`] makes at once.
const AHEAD_HASHED_BYTES: u64 = 128 << 20;

/// The non-leaf blocks of an archive decoded as they are asked for, again where they were let go
/// of: `raw` (data as stored, whether of Zstandard, and of a dictionary), the last few kept.
struct Fresh<'c> {
    raw: &'c [(&'c [u8], bool, Option<usize>)],
    dictionaries: &'c [dict::Dictionary],
    /// The blocks kept, the last used last, each None while it is decoded (those asking for it
    /// then waiting for it, rather than decoding it as well: of values made on threads,
    /// [`Hashing::ahead`]).
    cache: Mutex<Kept>,
    decoded: Condvar,
}

/// The blocks [`Fresh`] keeps: each its index and its data (None while it is decoded).
type Kept = VecDeque<(usize, Option<Arc<Vec<u8>>>)>;

impl Blocks for Fresh<'_> {
    fn block(&self, o: usize) -> Decoded<Arc<Vec<u8>>> {
        let mut cache = self.cache.lock().unwrap();
        while let Some(k) = cache.iter().position(|(b, _)| *b == o) {
            match cache[k].1.clone() {
                Some(d) => {
                    let hit = cache.remove(k).expect("there");
                    cache.push_back(hit);
                    return Ok(d);
                }
                None => cache = self.decoded.wait(cache).unwrap(),
            }
        }
        cache.push_back((o, None));
        drop(cache);
        let (d, zstd, dict) = self.raw[o];
        let d = decoded_with(d, zstd, true, dict.map(|d| &self.dictionaries[d])).map(Arc::new);
        let mut cache = self.cache.lock().unwrap();
        let k = cache.iter().position(|(b, _)| *b == o).expect("being decoded");
        match &d {
            Ok(d) => cache[k].1 = Some(d.clone()),
            // (asked for again: decoded again, to fail there too)
            Err(_) => _ = cache.remove(k),
        }
        // (the oldest decoded let go of, those being decoded kept: past [`FRESH_BYTES`] and
        // [`AHEAD_BLOCKS`] and one)
        let bytes = |c: &Kept| c.iter().filter_map(|e| e.1.as_ref()).map(|d| d.len()).sum::<usize>();
        while cache.iter().filter(|e| e.1.is_some()).count() > AHEAD_BLOCKS + 1 && bytes(&cache) > FRESH_BYTES {
            let k = cache.iter().position(|e| e.1.is_some()).expect("one decoded");
            cache.remove(k);
        }
        drop(cache);
        self.decoded.notify_all();
        d
    }

    fn release(&self, _: usize) {}

    fn add(&self, _: usize, _: usize) {}
}

/// The pieces of hashes `pieces` cut into `sizes`, where they are cut there.
fn cut(pieces: &[Ch], sizes: &[u64]) -> Option<Vec<Vec<Ch>>> {
    let mut out = Vec::with_capacity(sizes.len());
    let mut it = pieces.iter().copied().filter(|c| !c.is_empty()).peekable();
    for &n in sizes {
        let (mut got, mut these) = (0, Vec::new());
        while got < n {
            let c = it.next()?;
            got += c.len();
            these.push(c);
        }
        if got != n {
            return None;
        }
        out.push(these);
    }
    it.next().is_none().then_some(out)
}

impl<'f> Graph<'f, 'f> {
    /// The graph of formula `f` (its outputs checked to be taken once): its slices literals of the
    /// bytes of its streams; each application of several outputs a node before what takes them,
    /// one of a single output split as a formula block has it (a Split instruction).
    fn of_formula(f: &'f crate::Formula) -> crate::Result<Self> {
        f.check_outputs()?;
        let mut g = Graph { ops: Vec::new(), lens: Vec::new(), fans: 0, sums: HashMap::new(), roots: Vec::new() };
        let mut outputs: Vec<Vec<Val>> = Vec::with_capacity(f.shared.len());
        for s in &f.shared {
            let inputs = s.inputs().iter().map(|n| g.node(f, n, &outputs)).collect::<crate::Result<Vec<_>>>()?;
            let n = s.sizes.len();
            let vals = match (s.split, &s.of) {
                (_, crate::Of::Patch { len, .. }) => {
                    let p = g.push(Op::Patch(inputs[0], inputs[1], *len), *len);
                    let (node, fan) = (g.ops.len(), g.fan());
                    g.push(Op::Split(p, &s.sizes, fan), 0);
                    (0..n).map(|out| Val { node, out }).collect()
                }
                (_, crate::Of::Node(_)) => {
                    let (node, fan) = (g.ops.len(), g.fan());
                    g.push(Op::Split(inputs[0], &s.sizes, fan), 0);
                    (0..n).map(|out| Val { node, out }).collect()
                }
                (true, crate::Of::Apply(a)) => {
                    let a = g.push(Op::Apply(Ap::Shared(s), inputs, None), a.len);
                    let (node, fan) = (g.ops.len(), g.fan());
                    g.push(Op::Split(a, &s.sizes, fan), 0);
                    (0..n).map(|out| Val { node, out }).collect()
                }
                (false, crate::Of::Apply(_)) => {
                    let (node, fan) = (g.ops.len(), g.fan());
                    g.push(Op::Apply(Ap::Shared(s), inputs, Some(fan)), 0);
                    (0..n).map(|out| Val { node, out }).collect()
                }
            };
            outputs.push(vals);
        }
        let root = g.node(f, &f.root, &outputs)?;
        g.roots = vec![root];
        Ok(g)
    }

    /// The value of node `n` of formula `f`, of the `outputs` of its applications of several.
    fn node(&mut self, f: &'f crate::Formula, n: &'f crate::Node, outputs: &[Vec<Val>]) -> crate::Result<Val> {
        use crate::{Error, Node};
        Ok(match n {
            &Node::Slice { stream, start, len } => {
                let s = f.streams.get(stream).ok_or_else(|| Error::new(format!("no stream #{stream}")))?;
                if s.data.sunk_in().is_some() || s.data.is_lazy() {
                    return Ok(self.push(Op::Absent, len));
                }
                let d = usize::try_from(start)
                    .ok()
                    .zip(usize::try_from(len).ok())
                    .and_then(|(a, l)| s.data.get(a..a.checked_add(l)?))
                    .ok_or_else(|| Error::new(format!("slice [{start}, +{len}) out of stream #{stream}")))?;
                self.push(Op::Literal(d), len)
            }
            Node::Concat(ns) => {
                let items = ns.iter().map(|n| self.node(f, n, outputs)).collect::<crate::Result<Vec<_>>>()?;
                let len = items.iter().map(|&v| self.len(v)).sum();
                self.push(Op::Concat(items), len)
            }
            Node::Pack(ns) => {
                let items = ns.iter().map(|n| self.node(f, n, outputs)).collect::<crate::Result<Vec<_>>>()?;
                let len = packed::header_len(items.len()) + items.iter().map(|&v| self.len(v)).sum::<u64>();
                self.push(Op::Pack(items), len)
            }
            Node::Apply(a) => {
                let inputs = a.inputs.iter().map(|n| self.node(f, n, outputs)).collect::<crate::Result<Vec<_>>>()?;
                self.push(Op::Apply(Ap::One(a), inputs, None), a.len)
            }
            &Node::Output { shared, index, .. } => outputs[shared][index],
            Node::ModuleBytes(_, d) => self.push(Op::Literal(d), d.len() as u64),
            Node::ResourceBytes(r) => {
                let d = r.stored_data().ok_or_else(|| Error::new("the bytes of a resource not stored"))?;
                self.push(Op::Literal(d), d.len() as u64)
            }
            Node::Patch(p) => {
                let (source, patch) = (self.node(f, &p.source, outputs)?, self.node(f, &p.patch, outputs)?);
                self.push(Op::Patch(source, patch, p.len), p.len)
            }
        })
    }
}

/// What takes the output of a formula as it is evaluated ([`eval_formula`]).
pub(crate) type FormulaSink<'s> = dyn FnMut(&[u8]) -> crate::Result<()> + 's;

/// No blocks: a formula's graph slices none.
struct NoBlocks;

impl Blocks for NoBlocks {
    fn block(&self, _: usize) -> Decoded<Arc<Vec<u8>>> {
        invalid("a block of a formula")
    }

    fn release(&self, _: usize) {}

    fn add(&self, _: usize, _: usize) {}
}

/// `go` given the graph of formula `f` and its evaluation, its modules run by `m` (whole: of an
/// application of several outputs split, by [`crate::Modules::apply_shared`], its pieces put
/// together, so that what records them has their pieces), its threads joined after.
fn with_formula<M: crate::Modules + Sync, R>(
    f: &crate::Formula,
    m: &M,
    go: impl FnOnce(&Eval) -> crate::Result<R>,
) -> crate::Result<R> {
    use crate::Error;
    let g = Graph::of_formula(f)?;
    let failed = |e: Error| Failure::Invalid(e.0);
    let apply = |a: Ap<'_, '_>, inputs: &[&[u8]]| -> Decoded<Vec<Vec<u8>>> {
        match a {
            Ap::One(a) => m.apply(a, inputs).map(|o| vec![o]).map_err(failed),
            Ap::Shared(s) if s.split => m.apply_shared(s, inputs).map(|p| vec![p.concat()]).map_err(failed),
            Ap::Shared(s) => m.apply_shared(s, inputs).map_err(failed),
            Ap::Block(_) => unreachable!("of a formula"),
        }
        .and_then(|outs| {
            match outs.len() == a.sizes().len() && outs.iter().zip(a.sizes()).all(|(o, &n)| o.len() as u64 == n) {
                true => Ok(outs),
                false => invalid("outputs of other sizes than declared"),
            }
        })
    };
    let part_module = |_: Ap<'_, '_>| false;
    let apply_part = |_: Ap<'_, '_>, _: &mut [&mut dyn PartIn], _: &mut [&mut dyn PartOut]| -> Decoded<()> {
        unreachable!("no part modules in a formula's evaluation")
    };
    let ev = Eval {
        g: &g,
        apply: &apply,
        part: &part_module,
        apply_part: &apply_part,
        blocks: &NoBlocks,
        fans: (0..g.fans).map(|_| Mutex::new(Fan::Idle)).collect(),
        threads: Threads::default(),
        kept: None,
    };
    let _joined = Joined(&ev);
    go(&ev)
}

/// A failure of the evaluation as a formula's.
fn formula_error(f: Failure) -> crate::Error {
    match f {
        Failure::Invalid(m) | Failure::Resource(m) | Failure::Decryption(m) => crate::Error::new(m),
        f => crate::Error::new(format!("{f:?}")),
    }
}

/// The output of formula `f`, its modules run by `m` ([`with_formula`]), written to `part` in
/// pieces as it is pulled where given (and none returned), its applications among what its
/// output concatenates made ahead on up to `threads` threads.
pub(crate) fn eval_formula<M: crate::Modules + Sync>(
    f: &crate::Formula,
    m: &M,
    part: Option<&mut FormulaSink<'_>>,
    threads: usize,
) -> crate::Result<Vec<u8>> {
    with_formula(f, m, |ev| match part {
        None => emit(ev.g, ev, None, None, threads).map_err(formula_error).map(|mut o| o.pop().unwrap_or_default()),
        Some(part) => {
            // (the callback's failure told apart from the evaluation's)
            let mut given: Option<crate::Error> = None;
            let mut take = |d: &[u8]| part(d).map_err(|e| Failure::Invalid(given.insert(e).0.clone()));
            match emit(ev.g, ev, None, Some(&mut take), threads) {
                Ok(_) => Ok(Vec::new()),
                Err(f) => Err(given.take().unwrap_or_else(|| formula_error(f))),
            }
        }
    })
}

/// What `limit` gives of each application of formula `f` in `order`, given its inputs made (by
/// `m`) only as it asks for them: None where it gives none, or an input asked for cannot be made.
pub(crate) fn limits_of<M: crate::Modules + Sync>(
    f: &crate::Formula,
    m: &M,
    order: &[&crate::Apply],
    limit: &dyn Fn(&crate::Apply, &dyn crate::Contents) -> Option<crate::Plan>,
) -> crate::Result<Vec<Option<crate::Plan>>> {
    with_formula(f, m, |ev| {
        let g = ev.g;
        // (the node of each application, by its address)
        let mut node_of: HashMap<usize, usize> = HashMap::new();
        for (n, op) in g.ops.iter().enumerate() {
            match *op {
                Op::Apply(Ap::One(a), ..) => _ = node_of.insert(a as *const crate::Apply as usize, n),
                Op::Apply(Ap::Shared(s), ..) => {
                    _ = node_of.insert(s.apply().map_or(0, |a| a as *const crate::Apply as usize), n)
                }
                _ => {}
            }
        }
        let at = At { ev, ahead: None };
        Ok(order
            .iter()
            .map(|&a| {
                let n = *node_of.get(&(a as *const crate::Apply as usize))?;
                let Op::Apply(_, ref inputs, _) = g.ops[n] else { unreachable!("an application") };
                let made: Vec<std::cell::OnceCell<Option<Vec<u8>>>> =
                    inputs.iter().map(|_| Default::default()).collect();
                limit(a, &Asked { at, inputs, made })
            })
            .collect())
    })
}

/// The inputs of an application made as they are asked for ([`limits_of`]).
struct Asked<'r, 'e, 'a> {
    at: At<'r, 'e, 'a>,
    inputs: &'r [Val],
    made: Vec<std::cell::OnceCell<Option<Vec<u8>>>>,
}

impl crate::Contents for Asked<'_, '_, '_> {
    fn input(&self, k: usize) -> Option<&[u8]> {
        let v = *self.inputs.get(k)?;
        self.made[k].get_or_init(|| whole(self.at, v).ok().map(Whole::into_vec)).as_deref()
    }
}
