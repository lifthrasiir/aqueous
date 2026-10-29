//! aqr-infer-recs as modules of entry formulas (see aqr-formula): a file is the bytes between
//! its regions, as they are, around the regions' applications. A table of fixed-size records
//! is an application of `recs-fields`, the records from the byte planes of their fields; a chain
//! of entries an application of `recs-entries` to the heads (an application of `recs-fields`)
//! and the tails. `recs-fields` is a transpose of any records, which other recompressors may
//! apply to theirs as well. FORMAT.md describes the modules in full.

use aqr_formula::{Formula, Module, Node};

use crate::module::{entries_params, fields_params};
use crate::{Choice, Shape, fields};

pub use crate::module::{decode_entries, decode_fields};

pub const FIELDS: Module = Module { name: "recs-fields", version: 1 };
pub const ENTRIES: Module = Module { name: "recs-entries", version: 1 };

/// A file shorter than this is not looked into.
pub const MIN_LEN: usize = 16 << 10;

/// The least a quick estimate must say the regions may save ([`crate::infer_if`]): where it says
/// less, the inference would save little if anything, and it is not run.
pub const LEAST: f64 = 0.05;

/// The formula of `d` with the regions of records inferred in it, where the quick estimate says
/// they may save [`LEAST`] at least and some are taken; None otherwise. On one thread.
pub fn encode(d: &[u8], _threads: usize) -> Option<Formula> {
    if d.len() < MIN_LEN {
        return None;
    }
    let choices = std::panic::catch_unwind(|| crate::infer_if(d, LEAST)).ok()??;
    (!choices.is_empty()).then(|| formula(d, &choices))
}

/// The formula of `d` with the regions `choices` (sorted, apart) in their forms.
pub fn formula(d: &[u8], choices: &[Choice]) -> Formula {
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
        let (records, tails) = r.records(d);
        let k = r.width();
        let planes = f.push("recs/planes", fields::encode(&records, k, &fl));
        let recs = Node::apply(FIELDS, fields_params(r.count, k, &fl), vec![planes], records.len() as u64);
        parts.push(match (&r.shape, tails) {
            (Shape::Entries { rule, head, .. }, Some(tails)) => {
                let tails = f.push("recs/tails", tails);
                Node::apply(ENTRIES, entries_params(r.count, *head, rule), vec![recs, tails], (r.end - r.start) as u64)
            }
            _ => recs,
        });
    }
    let &(start, len) = gaps.last().unwrap();
    if len > 0 {
        parts.push(Node::slice(stream, start as u64, len as u64));
    }
    f.root = Node::concat(parts);
    f
}
