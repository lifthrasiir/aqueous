//! Modules ([`Module`]) and the plans of their working memory ([`Plan`], from the [`Usage`] of
//! an application).

use super::*;

/// A module, by name and version of its input format. Eventually a Wasm module identified by
/// its hash; a native implementation stands in for it (§5.7). It is its name and version (as
/// equal and hashed); `plan` gives the memory of its applications, `contents` what of their
/// inputs an encoder may recompress in turn.
#[derive(Debug, Clone, Copy)]
pub struct Module {
    pub name: &'static str,
    pub version: u16,
    /// The module's memory plan: what its working memory takes at most, by construction
    /// (`aqr_api::stack`), declared without room. None only for a stand-in of a test.
    pub plan: Option<Planner>,
    /// The inputs of its applications that are data as it is, a file or what a file holds,
    /// rather than a representation of the module's own (the data of a compressed stream, an
    /// archive's members): what an encoder may look into for files in turn.
    pub contents: &'static [usize],
}

/// A module's memory plan: the limit of its application's instance, and of a task of each of
/// its classes (the spec's task limits), in bytes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Plan {
    pub instance: u64,
    pub tasks: Vec<u64>,
}

impl Plan {
    /// The minimal bound of an application of this plan (spec, "Module execution"): the
    /// instance's limit plus one task of each class (the concurrency 1); [`NO_LIMIT`] (not
    /// known) counts for nothing.
    pub fn minimal(&self) -> u64 {
        minimal_bound(self.instance, &self.tasks)
    }

    /// The plan of a module of one class of tasks.
    pub fn one(instance: u64, task: u64) -> Plan {
        Plan { instance, tasks: vec![task] }
    }
}

impl From<(u64, Vec<u64>)> for Plan {
    fn from((instance, tasks): (u64, Vec<u64>)) -> Plan {
        Plan { instance, tasks }
    }
}

/// How a module gives its [`Plan`]: from the lengths and contents an application gives
/// ([`Usage`]); None where it cannot tell.
#[derive(Debug, Clone, Copy)]
pub enum Planner {
    /// A module that spawns no tasks: the limit of its instance.
    Whole(fn(&Usage<'_>) -> Option<u64>),
    /// A module that spawns tasks.
    Tasks(fn(&Usage<'_>) -> Option<Plan>),
}

/// What an application gives its module, for its memory plan ([`Module::plan_for`]).
#[derive(Clone, Copy)]
pub struct Usage<'a> {
    pub params: &'a [u8],
    /// The lengths of the resources' contents, the inputs and the outputs.
    pub resources: &'a [u64],
    pub inputs: &'a [u64],
    pub outputs: &'a [u64],
    /// The inputs themselves (which whoever declares the memory limit has), for what their
    /// headers and indexes tell of the buffers the module makes: made as they are asked for.
    pub contents: &'a dyn Contents,
    /// The pages of the working memory as the module is instantiated (its data and stack).
    pub initial_pages: u64,
}

impl<'a> Usage<'a> {
    /// Bytes of the parameters, the resources and the inputs: what `aqr_api::whole` copies into
    /// the working memory.
    pub fn copied(&self) -> u64 {
        self.params.len() as u64 + self.resources.iter().sum::<u64>() + self.inputs.iter().sum::<u64>()
    }

    /// Bytes of the outputs.
    pub fn output(&self) -> u64 {
        self.outputs.iter().sum()
    }

    /// The `k`-th input ([`Usage::contents`]).
    pub fn content(&self, k: usize) -> Option<&'a [u8]> {
        self.contents.input(k)
    }

    /// The first `N` inputs, of `N` in all.
    pub fn all_contents<const N: usize>(&self) -> Option<[&'a [u8]; N]> {
        if self.inputs.len() != N {
            return None;
        }
        let mut out = [&[][..]; N];
        for (k, o) in out.iter_mut().enumerate() {
            *o = self.content(k)?;
        }
        Some(out)
    }

    /// Bytes of the working memory as it is instantiated.
    pub fn initial(&self) -> u64 {
        self.initial_pages * PAGE
    }

    /// The plan of a module that spawns no tasks, copies its parameters, resources and
    /// inputs (`aqr_api::whole`) and gives back its outputs from vectors of their sizes, taking
    /// `extra` bytes besides in `buffers` large allocations: all but the small allocations, for
    /// which there is [`SMALL`], and a page for each large one (the allocator's, as it grows the
    /// memory for it), of the inputs and outputs those of a page at least.
    pub fn whole(&self, extra: u64, buffers: u64) -> u64 {
        let large = self.outputs.iter().filter(|&&n| n >= PAGE).count() as u64;
        self.base() + self.output() + extra + (large + buffers) * PAGE
    }

    /// The working memory of a module that copies its parameters, resources and inputs
    /// (`aqr_api::whole`) as it starts (before its outputs and its own buffers), with the pages of
    /// slack of the large ones and [`SMALL`]: what its tasks copy where it spawns them first.
    pub fn base(&self) -> u64 {
        let large = self.resources.iter().chain(self.inputs).filter(|&&n| n >= PAGE).count() as u64;
        self.initial() + self.copied() + large * PAGE + SMALL
    }
}

/// Bytes of a plan for the small allocations of a module (of no more than a few KiB in
/// all, whatever the input).
pub const SMALL: u64 = 64 << 10;

/// The inputs of an application, as a plan reads them ([`Usage::contents`]).
pub trait Contents {
    /// The `k`-th input; None where there is none, or it could not be made.
    fn input(&self, k: usize) -> Option<&[u8]>;
}

impl Contents for Vec<&[u8]> {
    fn input(&self, k: usize) -> Option<&[u8]> {
        self.get(k).copied()
    }
}

/// Bytes of a page of WebAssembly memory.
pub const PAGE: u64 = 65536;

impl Module {
    /// The module `name@version`, of no memory plan: for a stand-in in a test, as every module
    /// built into a Wasm module has a plan ([`Module::with_plan`]), and none is declared a limit
    /// without it.
    pub const fn new(name: &'static str, version: u16) -> Module {
        Module { name, version, plan: None, contents: &[] }
    }

    /// The module whose inputs `inputs` are data as it is ([`Module::contents`]).
    pub const fn with_contents(self, inputs: &'static [usize]) -> Module {
        Module { contents: inputs, ..self }
    }

    /// The module of the memory plan `plan`, a module spawning no tasks: whose working memory is
    /// laid out in advance (`aqr_api::stack`), so that it takes what the plan gives at most.
    pub const fn with_plan(self, plan: fn(&Usage<'_>) -> Option<u64>) -> Module {
        Module { plan: Some(Planner::Whole(plan)), ..self }
    }

    /// [`Module::with_plan`] of a module spawning tasks: with the limit of a task of each class.
    pub const fn with_task_plan(self, plan: fn(&Usage<'_>) -> Option<Plan>) -> Module {
        Module { plan: Some(Planner::Tasks(plan)), ..self }
    }

    /// What the working memories of an application of `usage` take at most, rounded up to pages
    /// (see [`Planner`]); None where it has no plan.
    pub fn plan_for(&self, usage: &Usage<'_>) -> Option<Plan> {
        let page = |n: u64| n.div_ceil(PAGE) * PAGE;
        let p = match self.plan? {
            Planner::Whole(f) => Plan { instance: f(usage)?, tasks: Vec::new() },
            Planner::Tasks(f) => f(usage)?,
        };
        Some(Plan { instance: page(p.instance), tasks: p.tasks.into_iter().map(page).collect() })
    }
}

impl PartialEq for Module {
    fn eq(&self, other: &Module) -> bool {
        (self.name, self.version) == (other.name, other.version)
    }
}

impl Eq for Module {}

impl std::hash::Hash for Module {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        (self.name, self.version).hash(h);
    }
}

impl fmt::Display for Module {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.name, self.version)
    }
}
