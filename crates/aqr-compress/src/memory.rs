//! The memory limits declared for the applications of modules (spec, "Module execution"): the
//! memories of the parameters, resources, inputs and outputs (a part module's inputs and
//! outputs within their windows), and the module's plan of its working memories
//! ([`aqr_formula::Module::plan_in_pages`]), which is exact and has no room. The inputs are those
//! that the recompressors made: the prevalidation, which evaluates the formulas as they are made
//! ([`crate::verify`]), has the limits worked out where it runs an application, and [`declare`]
//! works out the others, making their inputs as plans ask.

use std::collections::HashMap;
use std::sync::Mutex;

use aqr_formula::{Apply, Contents, Formula, Module, Modules, NO_LIMIT, Node, PAGE, Plan, Usage};
use xxhash_rust::xxh3::Xxh3;

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
pub(crate) struct Limits(pub HashMap<Shape, Plan>);

impl Limits {
    /// `limit` for `s`, the larger of it and any before it of each limit.
    pub fn put(&mut self, s: Shape, limit: Plan) {
        let l = self.0.entry(s).or_default();
        l.instance = l.instance.max(limit.instance);
        if l.tasks.len() < limit.tasks.len() {
            l.tasks.resize(limit.tasks.len(), 0);
        }
        l.tasks.iter_mut().zip(limit.tasks).for_each(|(l, n)| *l = (*l).max(n));
    }

    pub fn extend(&mut self, other: Limits) {
        other.0.into_iter().for_each(|(s, l)| self.put(s, l));
    }
}

/// The limits of application `a` of inputs of `lens` bytes (the bytes themselves in
/// `contents`) and outputs of `outputs` bytes: [`NO_LIMIT`] for a module of no plan or
/// none built (tests'), and where its plan fails, which is a bug of the plan (declaring
/// none rather than losing the archive): the instance's memory limit with the memories imported,
/// and a task limit for each class.
pub(crate) fn limit(a: &Apply, lens: &[u64], outputs: &[u64], contents: &dyn Contents) -> Plan {
    let none = Plan { instance: NO_LIMIT, tasks: Vec::new() };
    let Some(initial_pages) = a.module.plan.and(aqr_decompress::initial_pages_of(a.module)) else {
        return none;
    };
    let resources: Vec<u64> = a.resources.iter().map(|r| r.len() as u64).collect();
    let usage = Usage { params: &a.params, resources: &resources, inputs: lens, outputs, contents, initial_pages };
    let Some(plan) = a.module.plan_for(&usage) else {
        eprintln!("aqr-compress: no memory plan of {} (a bug), none declared", a.module);
        return none;
    };
    // (an input's or output's within its window, of a part module)
    let windows = aqr_decompress::windows_of(a.module);
    let fit = |n: u64, window: Option<u64>| n.div_ceil(PAGE).min(window.unwrap_or(u64::MAX)).saturating_mul(PAGE);
    let window = |output: bool, i: usize| windows.iter().find(|w| (w.0, w.1 as usize) == (output, i)).map(|w| w.2);
    let imported: u64 = [&[a.params.len() as u64][..], &resources].concat().iter().map(|&n| fit(n, None)).sum::<u64>()
        + lens.iter().enumerate().map(|(k, &n)| fit(n, window(false, k))).sum::<u64>()
        + outputs.iter().enumerate().map(|(j, &n)| fit(n, window(true, j))).sum::<u64>();
    // (a plan is what the module takes at most: no room)
    Plan { instance: imported.saturating_add(plan.instance), tasks: plan.tasks }
}

/// The bound on the minimal bound of an application ([`Plan::minimal`]) that the encoder keeps
/// to by default: 512 MiB for now, the most that the decoders support
/// ([`aqr_formula::SUPPORTED_BOUND`]). The target stays 256 MiB: it goes back to it once the
/// others over it stream (wrt and fasta have: their segments need about 105 and 170 MiB; 14
/// applications over 512 MiB in the survey, 61 over 256 before them).
pub const DEFAULT_BOUND: u64 = 512 << 20;

/// Modules allowed a larger bound than [`DEFAULT_BOUND`] by default, with their bounds (at most
/// [`aqr_formula::SUPPORTED_BOUND`]): where 256 MiB loses much and splitting is no help. None yet.
const EXCEPTIONS: &[(&str, u64)] = &[];

/// The most that the minimal bound of an application of `module` may be, under the `bound` of
/// [`crate::Options::application_bound`]: that given, or by default [`DEFAULT_BOUND`] or the
/// module's exception to it.
pub(crate) fn bound_of(bound: Option<u64>, module: Module) -> u64 {
    match bound {
        Some(b) => b,
        None => EXCEPTIONS.iter().find(|e| e.0 == module.name).map_or(DEFAULT_BOUND, |e| e.1.max(DEFAULT_BOUND)),
    }
}

/// A recompression not used, or an application kept, for its minimal bound over the bound: what
/// [`fallbacks`] tells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fallback {
    pub module: &'static str,
    /// The minimal bound of the application (the largest of the formula's), in bytes.
    pub needed: u64,
    /// The bytes of the data the recompressor was given.
    pub bytes: u64,
    /// The application was kept (made by a step after the recompressors), not given up.
    pub kept: bool,
}

static FALLBACKS: Mutex<Vec<Fallback>> = Mutex::new(Vec::new());

/// The recompressions given up so far, and the applications kept over the bound, for their
/// bound ([`crate::Options::application_bound`]); emptied.
pub fn fallbacks() -> Vec<Fallback> {
    std::mem::take(&mut *FALLBACKS.lock().unwrap())
}

/// The application of `f` of the largest minimal bound over the bound of its module under
/// `bound` ([`bound_of`]): its module and the bound, if any. Its inputs are made by `m` as
/// plans ask.
pub(crate) fn exceeding(f: &Formula, m: &(impl Modules + Sync), bound: Option<u64>) -> Option<(Module, u64)> {
    let worst: Mutex<Option<(Module, u64)>> = Mutex::new(None);
    let plans = f.plans(m, &|a, contents| {
        let p = match a.memory_limit {
            NO_LIMIT => {
                let lens: Vec<u64> = a.inputs.iter().map(Node::len).collect();
                limit(a, &lens, &[a.len], contents)
            }
            _ => Plan { instance: a.memory_limit, tasks: a.task_limits.clone() },
        };
        let needed = p.minimal();
        let mut w = worst.lock().unwrap();
        if needed > bound_of(bound, a.module) && w.is_none_or(|(_, n)| needed > n) {
            *w = Some((a.module, needed));
        }
        Some(p)
    });
    drop(plans);
    worst.into_inner().unwrap()
}

/// Records a recompression given up for the data of `bytes` bytes.
pub(crate) fn gave_up(module: Module, needed: u64, bytes: u64) {
    FALLBACKS.lock().unwrap().push(Fallback { module: module.name, needed, bytes, kept: false });
}

/// Records the applications of `f`, whose limits are declared, that are over the bound under
/// `bound` ([`bound_of`]) though the recompressors kept to it: the sets and the like make them
/// after. `bytes` is the length of the data `f` stands for.
pub(crate) fn note_kept(f: &Formula, bound: Option<u64>, bytes: u64) {
    for a in f.applies() {
        let needed = aqr_formula::minimal_bound(a.memory_limit, &a.task_limits);
        if needed > bound_of(bound, a.module) {
            FALLBACKS.lock().unwrap().push(Fallback {
                module: a.module.name,
                needed,
                bytes: a.len.min(bytes),
                kept: true,
            });
        }
    }
}

/// Declares the memory limits of `f`'s applications: those declared already kept (of a formula
/// sealed, whose inputs are no longer at hand), else those of `known` (worked out as the
/// prevalidation ran them), else worked out, the inputs made by `m` as plans ask for them.
pub(crate) fn declare(f: &mut Formula, m: &(impl Modules + Sync), known: &Limits) {
    let declared = f.declare_memory(m, &|a, contents| {
        if a.memory_limit != aqr_formula::NO_LIMIT {
            return Some(Plan { instance: a.memory_limit, tasks: a.task_limits.clone() });
        }
        let lens: Vec<u64> = a.inputs.iter().map(Node::len).collect();
        Some(match known.0.get(&shape(a, &lens)) {
            Some(l) => l.clone(),
            None => limit(a, &lens, &[a.len], contents),
        })
    });
    declared.expect("a limit for each application");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A part module's inputs and outputs count within their windows: flate-inflate's limit is
    /// the same small one whatever the lengths.
    #[test]
    fn part_windows() {
        let a = Apply {
            module: aqr_flate::formula::INFLATE,
            resources: vec![],
            params: vec![],
            inputs: vec![],
            len: 0,
            memory_limit: NO_LIMIT,
            task_limits: Vec::new(),
        };
        let none: Vec<&[u8]> = Vec::new();
        let small = limit(&a, &[1 << 20], &[4 << 20], &none);
        let large = limit(&a, &[1 << 32], &[1 << 34], &none);
        assert!(small == large && large.instance < 4 << 20, "{small:?} {large:?}");
    }
}
