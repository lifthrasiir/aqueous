//! aqr-infer-recs as modules of entry formulas (see aqr-formula): a file is the bytes between
//! its regions, as they are, around the regions' applications. A table of fixed-size records
//! is an application of `recs-fields`, the records from the byte planes of their fields; a chain
//! of entries an application of `recs-entries` to the heads (an application of `recs-fields`)
//! and the tails. Floats lifted to whole numbers ([`crate::floats`]) are put back by
//! `recs-floats`, applied to the records `recs-fields` makes. `recs-fields` is a transpose of
//! any records, which other recompressors may apply to theirs as well. FORMAT.md describes the
//! modules in full.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use aqr_formula::{Formula, Module, Node};

use crate::module::{entries_params, fields_params, floats_params, lifted_width};
use crate::regions::Region;
use crate::{Choice, Form, Shape, fields, floats};

pub use crate::module::{Field, Float, How, decode_entries, decode_fields, decode_floats};

pub const FIELDS: Module = Module::new("recs-fields", 1).with_plan(fields_memory);
pub const ENTRIES: Module = Module::new("recs-entries", 1).with_plan(entries_memory);
pub const FLOATS: Module = Module::new("recs-floats", 1).with_plan(floats_memory);

/// The memories of the modules: the initial memory, and the plans
/// ([`crate::module::fields_plan`], `entries_plan`, `floats_plan`).
fn fields_memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let &[planes] = u.inputs else { return None };
    Some(u.initial() + crate::module::fields_plan(u.params, planes)?)
}

fn entries_memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    Some(u.initial() + crate::module::entries_plan(u.params.len() as u64, u.inputs, u.output()))
}

fn floats_memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let &[lifted] = u.inputs else { return None };
    Some(u.initial() + crate::module::floats_plan(u.params, lifted)?)
}

/// A file shorter than this is not looked into.
pub const MIN_LEN: usize = 16 << 10;

/// The least a quick estimate must say the regions may save ([`crate::infer_if`]): where it says
/// less, the inference would save little if anything, and it is not run.
pub const LEAST: f64 = 0.05;

/// The regions inferred in data, by its hash and length (None where the estimate turned it down
/// or none was taken): the same data is often met again (the same object files in archives of
/// several versions of a library, say), a quarter of the inferences on the 10 GB corpus.
type Known = HashMap<(u64, usize), Option<Arc<Vec<Choice>>>>;

/// The most data [`Known`] holds, before it is cleared.
const MAX_KNOWN: usize = 1 << 16;

/// The formula of `d` with the regions of records inferred in it, where the quick estimate says
/// they may save [`LEAST`] at least and some are taken; None otherwise. On one thread.
pub fn encode(d: &[u8], threads: usize) -> Option<Formula> {
    static KNOWN: Mutex<Option<Known>> = Mutex::new(None);
    if d.len() < MIN_LEN {
        return None;
    }
    let key = (xxhash_rust::xxh3::xxh3_64(d), d.len());
    let known = KNOWN.lock().unwrap().as_ref().and_then(|k| k.get(&key).cloned());
    let choices = match known {
        Some(c) => c,
        None => {
            let c = crate::infer_if(d, LEAST, threads);
            let c = c.filter(|c| !c.is_empty()).map(Arc::new);
            let mut k = KNOWN.lock().unwrap();
            let k = k.get_or_insert_with(HashMap::new);
            if k.len() >= MAX_KNOWN {
                k.clear();
            }
            k.insert(key, c.clone());
            c
        }
    }?;
    Some(formula(d, &choices))
}

/// `records`, `k` bytes each, as an application of `recs-fields` by `fields`, their planes a
/// stream of `class` pushed to `f`: for recompressors whose formats tell their records' fields.
pub fn apply_fields(
    f: &mut Formula,
    class: impl Into<std::borrow::Cow<'static, str>>,
    records: &[u8],
    k: usize,
    fields: &[Field],
) -> Node {
    let planes = f.push(class, fields::encode(records, k, fields));
    Node::apply(FIELDS, fields_params(records.len() / k, k, fields), vec![planes], records.len() as u64)
}

/// The bytes of a region that make a group of rows, an application of the modules (at most;
/// [`GROUP_ROWS`] too): their planes are field after field, so a group cannot be cut finer
/// than a whole table, and a table of many rows would be decoded whole in one instance.
pub const GROUP_BYTES: usize = 32 << 20;

/// The most rows of a group: `recs-fields` holds 8 bytes a row more where fields refer to
/// others.
pub const GROUP_ROWS: usize = 1 << 22;

/// What a group of rows takes of the streams, and what its modules give.
struct Sizes {
    /// Its planes' bytes.
    planes: usize,
    /// Its fields' records (the planes decoded).
    lifted: usize,
    /// Its records' heads (the lifted ones unlifted).
    heads: usize,
    /// Where its tails start in the stream of tails, and their bytes.
    tail_at: usize,
    tails: usize,
}

/// The formula of `d` with the regions `choices` (sorted, apart) in their forms.
pub fn formula(d: &[u8], choices: &[Choice]) -> Formula {
    formula_in(d, choices, GROUP_BYTES, GROUP_ROWS)
}

/// [`formula`] with groups of `group_bytes` and `group_rows` at most (the test of groups at
/// small sizes). A region of rows fewer than a group is one application of each module (of
/// the whole planes); in more, an application of the modules to each group of rows, over
/// slices of the one stream of planes (and of tails), groups' planes one after another, and a
/// Concat of the groups. The fields are the region's, chosen once; each group's deltas start
/// afresh, as each application does.
pub fn formula_in(d: &[u8], choices: &[Choice], group_bytes: usize, group_rows: usize) -> Formula {
    let mut f = Formula::default();
    let mut raw = Vec::new();
    let mut gaps = Vec::new();
    let mut at = 0;
    for c in choices {
        gaps.push((raw.len(), c.region.start - at));
        raw.extend_from_slice(&d[at..c.region.start]);
        at = c.region.end;
    }
    gaps.push((raw.len(), d.len() - at));
    raw.extend_from_slice(&d[at..]);
    let Node::Slice { stream, .. } = f.push("recs/raw", raw) else { unreachable!() };
    let mut parts = Vec::new();
    for (c, &(start, len)) in choices.iter().zip(&gaps) {
        if len > 0 {
            parts.push(Node::slice(stream, start as u64, len as u64));
        }
        let r = &c.region;
        let fl = r.fields_of(c.form);
        let k = r.width();
        let lifts = if c.form == Form::Fields { &r.floats[..] } else { &[] };
        let k2 = lifted_width(k, lifts);
        // the rows of each group
        let row_bytes = match &r.shape {
            Shape::Fixed { .. } => k,
            Shape::Entries { .. } => (r.end - r.start).div_ceil(r.count.max(1)),
        };
        let rows = (group_bytes / row_bytes.max(k2).max(1)).clamp(1, group_rows.max(1));
        let groups: Vec<std::ops::Range<usize>> =
            (0..r.count).step_by(rows).map(|a| a..(a + rows).min(r.count)).collect();
        // the groups' records (a group's heads), planes and tails
        let mut all = Vec::new();
        let mut tails = Vec::new();
        let mut sizes = Vec::with_capacity(groups.len());
        for g in &groups {
            let (m, t): (std::borrow::Cow<[u8]>, _) = match &r.shape {
                Shape::Fixed { .. } => (d[r.start + g.start * k..r.start + g.end * k].into(), None),
                Shape::Entries { head, starts, .. } => {
                    let head = *head;
                    let s = &starts[g.start..=g.end];
                    (Region::heads(d, s, head).into(), Some(s.windows(2).flat_map(move |w| &d[w[0] + head..w[1]])))
                }
            };
            let lifted = floats::lift(&m, k, lifts);
            let before = all.len();
            all.extend_from_slice(&fields::encode(&lifted, k2, &fl));
            let tail_at = tails.len();
            tails.extend(t.into_iter().flatten());
            sizes.push(Sizes {
                planes: all.len() - before,
                lifted: lifted.len(),
                heads: m.len(),
                tail_at,
                tails: tails.len() - tail_at,
            });
        }
        let Node::Slice { stream: planes, .. } = f.push("recs/planes", all) else { unreachable!() };
        let tails = matches!(r.shape, Shape::Entries { .. }).then(|| f.push("recs/tails", tails));
        let mut at = 0u64;
        for (g, z) in groups.iter().zip(&sizes) {
            let slice = Node::slice(planes, at, z.planes as u64);
            at += z.planes as u64;
            let mut node = Node::apply(FIELDS, fields_params(g.len(), k2, &fl), vec![slice], z.lifted as u64);
            if !lifts.is_empty() {
                node = Node::apply(FLOATS, floats_params(g.len(), k, lifts), vec![node], z.heads as u64);
            }
            parts.push(match (&r.shape, &tails) {
                (Shape::Entries { rule, head, starts }, Some(Node::Slice { stream, .. })) => {
                    let tails = Node::slice(*stream, z.tail_at as u64, z.tails as u64);
                    let len = (starts[g.end] - starts[g.start]) as u64;
                    Node::apply(ENTRIES, entries_params(g.len(), *head, rule), vec![node, tails], len)
                }
                _ => node,
            });
        }
    }
    let &(start, len) = gaps.last().unwrap();
    if len > 0 {
        parts.push(Node::slice(stream, start as u64, len as u64));
    }
    f.root = Node::concat(parts);
    f
}
