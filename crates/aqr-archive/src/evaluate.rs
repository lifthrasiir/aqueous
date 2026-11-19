//! The evaluation of a [`Formula`]: by the evaluator of formula blocks (`eval`), read as a graph
//! of values as a formula block is, with its modules run by [`Modules`].

use aqr_formula::{Apply, Contents, Error, Formula, Node, Of, Plan, Result, Shared};

/// What gives an application's plan ([`Evaluate::plans`]): of the application, its outputs'
/// sizes and its inputs, None where it gives none.
pub type Limit<'a> = dyn Fn(&Apply, &[u64], &dyn Contents) -> Option<Plan> + 'a;

/// A [`Formula`] evaluated, and the memory limits of its applications measured and declared.
pub trait Evaluate {
    /// The output, with every module run by `m`. Fails on slices out of their stream, on
    /// modules whose output is not of the declared size, and on outputs not taken once
    /// ([`Formula::check_outputs`]).
    fn eval(&self, m: &(impl Modules + Sync)) -> Result<Vec<u8>>;

    /// [`Evaluate::eval`], the output given to `part` a piece at a time as it is pulled, rather than
    /// held whole (by the evaluator of formula blocks), the applications among what it
    /// concatenates made ahead on up to `threads` threads.
    fn eval_parts(
        &self,
        m: &(impl Modules + Sync),
        threads: usize,
        part: &mut dyn FnMut(&[u8]) -> Result<()>,
    ) -> Result<()>;

    /// What `limit` gives of each application ([`Formula::applies`]), its outputs' sizes as the
    /// decoder has them (one output of an application split) and its inputs, which are made (by
    /// `m`) only as `limit` reads them, without setting anything: None where it gives none or an
    /// input asked for cannot be made.
    fn plans<M: Modules + Sync>(&self, m: &M, limit: &Limit<'_>) -> Result<Vec<(&Apply, Option<Plan>)>>;

    /// Sets every application's memory limit ([`Apply::memory_limit`]) and task limits to what
    /// `limit` gives of it, its outputs' sizes (as [`Evaluate::plans`]) and its inputs, which are
    /// made (by `m`) only as `limit` reads them. Fails where it gives none, or an input asked for
    /// cannot be made.
    fn declare_memory<M: Modules + Sync>(&mut self, m: &M, limit: &Limit<'_>) -> Result<()>;
}

impl Evaluate for Formula {
    fn eval(&self, m: &(impl Modules + Sync)) -> Result<Vec<u8>> {
        crate::eval::eval_formula(self, m, None, 1)
    }

    fn eval_parts(
        &self,
        m: &(impl Modules + Sync),
        threads: usize,
        part: &mut dyn FnMut(&[u8]) -> Result<()>,
    ) -> Result<()> {
        crate::eval::eval_formula(self, m, Some(part), threads).map(drop)
    }

    fn plans<M: Modules + Sync>(&self, m: &M, limit: &Limit<'_>) -> Result<Vec<(&Apply, Option<Plan>)>> {
        let applies = self.applies();
        let limits = crate::eval::limits_of(self, m, &applies, limit)?;
        Ok(applies.into_iter().zip(limits).collect())
    }

    fn declare_memory<M: Modules + Sync>(&mut self, m: &M, limit: &Limit<'_>) -> Result<()> {
        fn visit<'f>(n: &'f Node, out: &mut Vec<&'f Apply>) {
            match n {
                Node::Concat(ns) | Node::Pack(ns) => ns.iter().for_each(|n| visit(n, out)),
                Node::Patch(p) => [&p.source, &p.patch].into_iter().for_each(|n| visit(n, out)),
                Node::Either(e) => [&e.kept, &e.made].into_iter().for_each(|n| visit(n, out)),
                Node::Apply(a) => {
                    out.push(a);
                    a.inputs.iter().for_each(|n| visit(n, out));
                }
                Node::Slice { .. } | Node::Output { .. } | Node::ModuleBytes(..) | Node::ResourceBytes(_) => {}
            }
        }
        fn visit_mut(n: &mut Node, limits: &mut impl Iterator<Item = Plan>) {
            match n {
                Node::Concat(ns) | Node::Pack(ns) => ns.iter_mut().for_each(|n| visit_mut(n, limits)),
                Node::Patch(p) => [&mut p.source, &mut p.patch].into_iter().for_each(|n| visit_mut(n, limits)),
                Node::Either(e) => [&mut e.kept, &mut e.made].into_iter().for_each(|n| visit_mut(n, limits)),
                Node::Apply(a) => {
                    let l = limits.next().expect("a limit for each application");
                    (a.memory_limit, a.task_limits) = (l.instance, l.tasks);
                    a.inputs.iter_mut().for_each(|n| visit_mut(n, limits));
                }
                Node::Slice { .. } | Node::Output { .. } | Node::ModuleBytes(..) | Node::ResourceBytes(_) => {}
            }
        }
        let limits = {
            let mut applies = Vec::new();
            for s in &self.shared {
                applies.extend(s.apply());
                s.inputs().iter().for_each(|n| visit(n, &mut applies));
            }
            visit(&self.root, &mut applies);
            let limits = crate::eval::limits_of(self, m, &applies, limit)?;
            (applies.iter().zip(limits))
                .map(|(a, l)| l.ok_or_else(|| Error::new(format!("no memory limit for {}", a.module))))
                .collect::<Result<Vec<Plan>>>()?
        };
        let mut limits = limits.into_iter();
        for s in &mut self.shared {
            if let Some(a) = s.apply_mut() {
                let l = limits.next().expect("a limit for each application");
                (a.memory_limit, a.task_limits) = (l.instance, l.tasks);
            }
            s.inputs_mut().iter_mut().for_each(|n| visit_mut(n, &mut limits));
        }
        visit_mut(&mut self.root, &mut limits);
        Ok(())
    }
}

/// Runs modules for [`Evaluate::eval`].
pub trait Modules {
    /// The output of `a` on `inputs`, the evaluated `a.inputs` (with `a.resources`).
    fn apply(&self, a: &Apply, inputs: &[&[u8]]) -> Result<Vec<u8>>;

    /// Whether `a`, an application of a single output, is applied by [`Modules::apply_part`]:
    /// its inputs pulled as it asks for them and its output pushed as it makes it, none of them
    /// held whole (the evaluation of a formula gathers the inputs of [`Modules::apply`] and holds
    /// its output). By default none is.
    fn is_part(&self, a: &Apply) -> bool {
        let _ = a;
        false
    }

    /// The output of `a` ([`Modules::is_part`]) pushed to `output`, its inputs (the evaluated
    /// `a.inputs`, with `a.resources`) pulled from `inputs`.
    fn apply_part(
        &self,
        a: &Apply,
        inputs: &mut [&mut dyn crate::PartIn],
        output: &mut dyn crate::PartOut,
    ) -> Result<()> {
        let _ = (inputs, output);
        Err(Error::new(format!("{} is no part module here", a.module)))
    }

    /// The outputs of `s` on `inputs`, the evaluated [`Shared::inputs`]: by default those split
    /// from the output of [`Modules::apply`] ([`Shared::split`]), or that output if `s` has one.
    fn apply_shared(&self, s: &Shared, inputs: &[&[u8]]) -> Result<Vec<Vec<u8>>> {
        let all = match &s.of {
            Of::Patch { .. } | Of::Node(_) => return s.patched(inputs).expect("a patch or a split"),
            Of::Apply(a) if s.split => self.apply(a, inputs)?,
            Of::Apply(a) if s.sizes.len() == 1 => return Ok(vec![self.apply(a, inputs)?]),
            Of::Apply(a) => return Err(Error::new(format!("{}: {} outputs", a.module, s.sizes.len()))),
        };
        if all.len() as u64 != s.bytes() {
            return Err(Error::new(format!("{} made {} bytes, not {}", s.name(), all.len(), s.bytes())));
        }
        let mut rest = &all[..];
        Ok(s.sizes.iter().map(|&n| rest.split_off(..n as usize).unwrap().to_vec()).collect())
    }
}

/// A function as the modules, e.g. a `match` on `a.module` in tests.
impl<F: Fn(&Apply, &[&[u8]]) -> Result<Vec<u8>>> Modules for F {
    fn apply(&self, a: &Apply, inputs: &[&[u8]]) -> Result<Vec<u8>> {
        self(a, inputs)
    }
}
