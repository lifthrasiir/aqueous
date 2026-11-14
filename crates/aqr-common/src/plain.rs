//! The plain data that [`crate::allowance`] and [`crate::meter`] and their stand-ins on
//! WebAssembly (`*_none.rs`) alike hand out.

/// An attempt that gave up: the bytes its allowance held as it did, and its limit. Unwound
/// with by `allowance::check`, and returned by `allowance::attempt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exceeded {
    pub used: u64,
    pub limit: u64,
}

/// What a meter has seen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Measured {
    /// The most bytes held at once by the meter's own work (its tasks' not, but for what they
    /// hand back).
    pub peak: u64,
    /// Of the tasks under it (`meter::task`): the most a task held at once of its own, and the
    /// most one held with the meter's peak as it started (the memory a task copies in
    /// WebAssembly, the instance's as it is then: of a spawn of all of the memory).
    pub task: u64,
    pub task_with_copy: u64,
    /// Tasks under it.
    pub tasks: u64,
}
