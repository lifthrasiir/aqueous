//! [`crate::meter`] on WebAssembly, where nothing is measured: its functions doing nothing, so
//! that what calls them is the same there.

use std::sync::Arc;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Measured {
    pub peak: u64,
    pub task: u64,
    pub task_with_copy: u64,
    pub tasks: u64,
}

pub struct Meter;

#[inline]
pub fn current() -> Option<Arc<Meter>> {
    None
}

pub struct Entered;

#[inline]
pub fn enter(_: Option<Arc<Meter>>) -> Entered {
    Entered
}

#[inline]
pub fn measure<R>(f: impl FnOnce() -> R) -> (R, Measured) {
    (f(), Measured::default())
}

#[inline]
pub fn task<R>(f: impl FnOnce() -> R) -> R {
    f()
}

#[inline]
pub(crate) fn allocated(_: *mut u8, _: usize) {}

#[inline]
pub(crate) fn freed(_: *mut u8, _: usize) {}

#[inline]
pub(crate) fn reallocated(_: *mut u8, _: usize, _: *mut u8, _: usize) {}
