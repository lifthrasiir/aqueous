//! How modules run in decoding ([`Runner`], [`Loaded`], part modules' [`PartIn`] and
//! [`PartOut`]) and why an archive does not decode ([`Failure`]).

use super::*;

/// Why an archive does not decode, as the spec classifies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The archive violates the spec (or a module fails on its inputs).
    Invalid(String),
    /// The archive is encrypted, and no passphrase given unwraps its file key.
    Decryption(String),
    /// The archive exceeds what this decoder supports: a limit, or a feature not implemented
    /// yet (a module without a native implementation among them).
    Resource(String),
    /// The archive has a block reserved for critical extensions, which a later version of the
    /// spec may define.
    Unsupported(String),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Invalid(m) => write!(f, "invalid archive: {m}"),
            Failure::Resource(m) => write!(f, "resource failure: {m}"),
            Failure::Unsupported(m) => write!(f, "unsupported failure: {m}"),
            Failure::Decryption(m) => write!(f, "decryption failure: {m}"),
        }
    }
}

impl std::error::Error for Failure {}

/// A result of decoding.
pub type Decoded<T> = std::result::Result<T, Failure>;

pub(crate) fn invalid<T>(msg: impl Into<String>) -> Decoded<T> {
    Err(Failure::Invalid(msg.into()))
}

pub(crate) fn resource<T>(msg: impl Into<String>) -> Decoded<T> {
    Err(Failure::Resource(msg.into()))
}

/// Runs the modules of archives: applies the Wasm module of a module block as the spec's
/// "apply a module" does, natively where it knows the module, say.
pub trait Runner: Sync {
    /// Wasm module `wasm` made ready to apply, once for each module block applied; a failure if
    /// it is not a module, or cannot be run here.
    fn load(&self, wasm: &[u8]) -> Decoded<Box<dyn Loaded + '_>>;

    /// The Wasm module a shorthand module block's registered `hash` stands for (spec, "Registered
    /// dictionaries and modules"), from the runner's own registry; a failure if it is not known.
    /// A runner with a registry overrides this; the default knows none.
    fn registered(&self, hash: &[u8; 32]) -> Decoded<Vec<u8>> {
        let _ = hash;
        resource("this runner knows no registered modules")
    }
}

/// A module made ready to apply ([`Runner::load`]).
pub trait Loaded: Send + Sync {
    /// The outputs of an application, of `sizes` bytes each: from `inputs`, the resource
    /// blocks given in order and `params`, with the memories of at most `memory_limit` bytes and
    /// those of each task of class `c` of at most `task_limits[c]` (the spec's task limits; none:
    /// all of the application's under `memory_limit`). The decoder checks the outputs against
    /// `sizes`.
    fn apply(
        &self,
        params: &[u8],
        resources: &[Resource],
        inputs: &[&[u8]],
        sizes: &[u64],
        memory_limit: u64,
        task_limits: &[u64],
    ) -> Decoded<Vec<Vec<u8>>>;

    /// Whether it is applied as a part module ([`Loaded::apply_part`]) rather than whole.
    fn is_part(&self) -> bool {
        false
    }

    /// Whether an application of it of outputs of `sizes` bytes is applied as a part module:
    /// by default as [`Loaded::is_part`] tells, whatever the sizes.
    fn is_part_of(&self, sizes: &[u64]) -> bool {
        let _ = sizes;
        self.is_part()
    }

    /// An application as a part module: its inputs pulled as it requests them, its outputs (of
    /// `sizes` bytes) pushed as it commits them, all of them by the time it returns.
    // ([`Loaded::apply`]'s arguments, and the outputs)
    #[allow(clippy::too_many_arguments)]
    fn apply_part(
        &self,
        params: &[u8],
        resources: &[Resource],
        inputs: &mut [&mut dyn PartIn],
        outputs: &mut [&mut dyn PartOut],
        sizes: &[u64],
        memory_limit: u64,
        task_limits: &[u64],
    ) -> Decoded<()> {
        let _ = (params, resources, inputs, outputs, sizes, memory_limit, task_limits);
        resource("not a part module")
    }
}

/// An input of a part module's application ([`Loaded::apply_part`]), pulled as it is requested.
pub trait PartIn: Send {
    /// Its length in bytes.
    fn len(&self) -> u64;

    /// Its bytes from where they were pulled to before up to `hi` (at most [`PartIn::len`]),
    /// given to `put` in order, in pieces.
    fn pull(&mut self, hi: u64, put: &mut dyn FnMut(&[u8])) -> Decoded<()>;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// An output of a part module's application ([`Loaded::apply_part`]), taking its bytes in
/// order as they are committed.
pub trait PartOut: Send {
    fn push(&mut self, d: &[u8]) -> Decoded<()>;
}

/// Runs no module: for archives of none.
pub struct NoModules;

impl Runner for NoModules {
    fn load(&self, _: &[u8]) -> Decoded<Box<dyn Loaded + '_>> {
        resource("no module runs here")
    }
}

/// `modules` run by `m` ([`Natives`]), known by their [`stand_in`]s.
pub fn natives<'a, M: crate::Modules + Sync>(modules: &[Module], m: &'a M) -> Natives<'a, M> {
    Natives { modules: modules.to_vec(), m, identify: |w| stand_in_name(w).map(str::to_owned), registered: None }
}

/// Modules run by [`crate::Modules`], as archives apply them: [`crate::Modules::apply`] for one
/// output, else [`crate::Modules::apply_shared`]. The [`aqr_formula::Apply`] that `m` is given has no
/// inputs of its own (they are the evaluated ones). A module block is that of one of them where
/// its Wasm module is identified as it, by its [`stand_in`] unless [`Natives::identified_by`].
/// A shorthand module block's module is reconstructed by [`Natives::registered_by`], if given.
pub struct Natives<'a, M> {
    modules: Vec<Module>,
    m: &'a M,
    identify: fn(&[u8]) -> Option<String>,
    registered: Option<Registered>,
}

/// The Wasm module of a shorthand module block's registered hash ([`Natives::registered_by`]).
type Registered = fn(&[u8; 32]) -> Decoded<Vec<u8>>;

impl<M> Natives<'_, M> {
    /// The modules known by `identify`, which gives the name of the module of a Wasm module
    /// (`name@version`).
    pub fn identified_by(mut self, identify: fn(&[u8]) -> Option<String>) -> Self {
        self.identify = identify;
        self
    }

    /// The registered modules ([`Runner::registered`]) that `registered` reconstructs from their
    /// hashes.
    pub fn registered_by(mut self, registered: fn(&[u8; 32]) -> Decoded<Vec<u8>>) -> Self {
        self.registered = Some(registered);
        self
    }
}

impl<M: crate::Modules + Sync> Runner for Natives<'_, M> {
    fn load(&self, wasm: &[u8]) -> Decoded<Box<dyn Loaded + '_>> {
        struct Native<'a, M> {
            module: Module,
            m: &'a M,
        }

        impl<M: crate::Modules + Sync> Loaded for Native<'_, M> {
            fn apply(
                &self,
                params: &[u8],
                resources: &[Resource],
                inputs: &[&[u8]],
                sizes: &[u64],
                memory_limit: u64,
                task_limits: &[u64],
            ) -> Decoded<Vec<Vec<u8>>> {
                let a = aqr_formula::Apply {
                    module: self.module,
                    resources: resources.to_vec(),
                    params: params.to_vec(),
                    inputs: Vec::new(),
                    len: sizes.iter().sum(),
                    memory_limit,
                    task_limits: task_limits.to_vec(),
                };
                match sizes {
                    [_] => self.m.apply(&a, inputs).map(|out| vec![out]),
                    _ => self.m.apply_shared(
                        &aqr_formula::Shared { of: aqr_formula::Of::Apply(a), sizes: sizes.to_vec(), split: false },
                        inputs,
                    ),
                }
                .map_err(|e| Failure::Invalid(e.0))
            }
        }

        let name = (self.identify)(wasm).ok_or_else(|| Failure::Resource("an unknown module".into()))?;
        let module = self.modules.iter().copied().find(|m| m.to_string() == name);
        let module = module.ok_or_else(|| Failure::Resource(format!("no native implementation of {name}")))?;
        Ok(Box::new(Native { module, m: self.m }))
    }

    fn registered(&self, hash: &[u8; 32]) -> Decoded<Vec<u8>> {
        match self.registered {
            Some(r) => r(hash),
            None => resource("this runner knows no registered modules"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A module that is not a part module says so; a runner of no registry knows no registered
    /// modules; a part input is empty where its length is.
    #[test]
    fn defaults() {
        struct Whole;
        impl Loaded for Whole {
            fn apply(
                &self,
                _: &[u8],
                _: &[Resource],
                _: &[&[u8]],
                _: &[u64],
                _: u64,
                _: &[u64],
            ) -> Decoded<Vec<Vec<u8>>> {
                Ok(vec![])
            }
        }
        let w = Whole;
        assert!(!w.is_part() && !w.is_part_of(&[1, 2]));
        let r = w.apply_part(&[], &[], &mut [], &mut [], &[], 0, &[]);
        assert_eq!(r, resource("not a part module"));
        assert!(matches!(NoModules.registered(&[0; 32]), Err(Failure::Resource(_))));
        assert!(matches!(NoModules.load(b"\0asm"), Err(Failure::Resource(_))));

        struct Sized(u64);
        impl PartIn for Sized {
            fn len(&self) -> u64 {
                self.0
            }
            fn pull(&mut self, _: u64, _: &mut dyn FnMut(&[u8])) -> Decoded<()> {
                Ok(())
            }
        }
        assert!(Sized(0).is_empty() && !Sized(3).is_empty());
        // (the failures told)
        assert_eq!(Failure::Invalid("x".into()).to_string(), "invalid archive: x");
        assert_eq!(Failure::Resource("x".into()).to_string(), "resource failure: x");
    }
}
