//! Formulas made of a fuzzer's bytes ([`formula`]), for `roundtrip`, and what they evaluate to
//! by a reference evaluator of their own ([`reference`]), apart from aqr-archive's (the
//! decoder's): a recursive walk of the nodes, the toy modules as `src/lib.rs` has them.

use aqr_archive::{Encoder, Level, Value, checksum};
use aqr_formula::{Formula, Kind, Node, Of, packed, patch};
use libfuzzer_sys::arbitrary::{Result, Unstructured};

use crate::{COPY, CUT, REV};

/// The longest stream made.
const STREAM_MAX: usize = 4096;
/// The depth of nodes at most.
const DEPTH: u32 = 4;

/// What makes a formula of `u`: the formula so far and the outputs of its applications of
/// several not placed yet.
struct Gen<'a, 'b> {
    u: &'a mut Unstructured<'b>,
    f: Formula,
    outputs: Vec<Node>,
}

/// A formula made of `u`: up to 3 streams, up to 3 applications of several outputs (of `cut`,
/// or a node or a patch split), and a root of slices, concatenations, packs, applications of
/// `rev` and `copy`, outputs and patches; each output placed once. Now and then a slice out of
/// its stream or an application of a size not its own: the formula then fails to evaluate.
pub fn formula(u: &mut Unstructured) -> Result<Formula> {
    let mut g = Gen { u, f: Formula::default(), outputs: Vec::new() };
    for _ in 0..g.u.int_in_range(1..=3)? {
        g.stream()?;
    }
    for _ in 0..g.u.int_in_range(0..=3)? {
        g.shared()?;
    }
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

impl Gen<'_, '_> {
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

    fn shared(&mut self) -> Result<()> {
        let outputs = match self.u.int_in_range(0..=2)? {
            0 => {
                let inputs = self.nodes(DEPTH - 1)?;
                let total = inputs.iter().map(Node::len).sum();
                let sizes = self.sizes(total)?;
                let a = match Node::apply(CUT, self.params()?, inputs, 0) {
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
        let kinds = match (depth, self.outputs.is_empty()) {
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
            1 if !self.outputs.is_empty() => {
                let i = self.u.choose_index(self.outputs.len())?;
                self.outputs.swap_remove(i)
            }
            1 | 2 => Node::Concat(self.nodes(depth - 1)?),
            3 => Node::Pack(self.nodes(depth - 1)?),
            4 | 5 => {
                let m = if self.u.ratio(1, 2)? { REV } else { COPY };
                let inputs = self.nodes(depth - 1)?;
                let len = inputs.iter().map(Node::len).sum::<u64>();
                // (now and then of another size than it makes)
                let len = if self.u.ratio(1, 32)? { len + 1 } else { len };
                Node::apply(m, self.params()?, inputs, len)
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

/// `roundtrip`'s body: a formula made of `d` archived and decoded again (whole, and streamed),
/// which must give what [`reference`] does where it gives anything.
pub fn roundtrip(d: &[u8]) {
    let mut u = Unstructured::new(d);
    let (Ok(level), Ok(summed), Ok(f)) = (
        u.choose(&[Level::STORED, Level::Zstd(1), Level::Zstd(3), Level::LOW]).copied(),
        u.arbitrary::<bool>(),
        formula(&mut u),
    ) else {
        return;
    };
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
