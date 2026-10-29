//! The memory limits declared for the applications of modules (spec, "Module execution"): the
//! memories of the parameters, resources, inputs and outputs, and the module's estimate of its
//! working memories ([`aqr_formula::Module::memory_usage_in_pages`]) with room for an estimate's
//! error. The inputs are those that the recompressors made: the prevalidation, which evaluates
//! the formulas as they are made ([`crate::verify`]), has the limits worked out where it runs an
//! application, and [`declare`] works out the others, making their inputs as estimates ask.

use std::collections::HashMap;

use aqr_formula::{Apply, Contents, Formula, Modules, NO_LIMIT, Node, PAGE, Usage};
use xxhash_rust::xxh3::Xxh3;

/// The limit declared of an estimate `e`: `e` and a quarter, and 1 MiB.
fn room(e: u64) -> u64 {
    e.saturating_add(e / 4).saturating_add(1 << 20)
}

/// What an application is but for its inputs' bytes: its module, parameters, resources'
/// lengths, inputs' lengths and output's length, hashed. Applications alike in all of these
/// get the largest of their limits ([`Limits`]).
pub(crate) type Shape = u128;

pub(crate) fn shape(a: &Apply, inputs: &[u64]) -> Shape {
    let mut h = Xxh3::new();
    let mut part = |b: &[u8]| {
        h.update(&(b.len() as u64).to_le_bytes());
        h.update(b);
    };
    part(a.module.name.as_bytes());
    part(&a.module.version.to_le_bytes());
    part(&a.params);
    a.resources.iter().for_each(|r| part(&(r.len() as u64).to_le_bytes()));
    inputs.iter().for_each(|i| part(&i.to_le_bytes()));
    part(&a.len.to_le_bytes());
    h.digest128()
}

/// Limits worked out, by [`shape`].
#[derive(Default)]
pub(crate) struct Limits(pub HashMap<Shape, u64>);

impl Limits {
    pub fn put(&mut self, s: Shape, limit: u64) {
        let l = self.0.entry(s).or_insert(0);
        *l = (*l).max(limit);
    }

    pub fn extend(&mut self, other: Limits) {
        other.0.into_iter().for_each(|(s, l)| self.put(s, l));
    }
}

/// The limit of application `a` of inputs of `lens` bytes (the bytes themselves in
/// `contents`) and outputs of `outputs` bytes: [`NO_LIMIT`] for a module of no estimate or
/// none built (tests'), and where its estimate fails, which is a bug of the estimate (declaring
/// none rather than losing the archive).
pub(crate) fn limit(a: &Apply, lens: &[u64], outputs: &[u64], contents: &dyn Contents) -> u64 {
    let Some(initial_pages) = a.module.memory.and(aqr_decompress::initial_pages_of(a.module)) else {
        return NO_LIMIT;
    };
    let resources: Vec<u64> = a.resources.iter().map(|r| r.len() as u64).collect();
    let usage = Usage { params: &a.params, resources: &resources, inputs: lens, outputs, contents, initial_pages };
    let Some(pages) = a.module.memory_usage_in_pages(&usage) else {
        eprintln!("aqr-compress: no memory estimate of {} (a bug), none declared", a.module);
        return NO_LIMIT;
    };
    let fit = |n: &u64| n.div_ceil(PAGE).saturating_mul(PAGE);
    let imported: u64 = [&[a.params.len() as u64][..], &resources, lens, outputs].concat().iter().map(fit).sum();
    imported.saturating_add(room(pages.saturating_mul(PAGE)))
}

/// Declares the memory limits of `f`'s applications: those of `known` (worked out as the
/// prevalidation ran them), else worked out, the inputs made by `m` as estimates ask for them.
pub(crate) fn declare(f: &mut Formula, m: &impl Modules, known: &Limits) {
    let declared = f.declare_memory(m, &|a, contents| {
        let lens: Vec<u64> = a.inputs.iter().map(Node::len).collect();
        Some(match known.0.get(&shape(a, &lens)) {
            Some(&l) => l,
            None => limit(a, &lens, &[a.len], contents),
        })
    });
    declared.expect("a limit for each application");
}
