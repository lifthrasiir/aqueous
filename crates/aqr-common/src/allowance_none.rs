//! [`crate::allowance`] on WebAssembly, where nothing is counted: its functions doing nothing,
//! so that what calls them is the same there.

pub use crate::plain::Exceeded;

#[derive(Clone, Default)]
pub struct Context;

impl Context {
    pub const NONE: Context = Context;
}

#[inline(always)]
pub fn context() -> Context {
    Context
}

pub struct Entered;

#[inline(always)]
pub fn enter(_: Context) -> Entered {
    Entered
}

#[inline]
pub fn attempt<R>(_: u64, f: impl FnOnce() -> R) -> Result<R, Exceeded> {
    Ok(f())
}

#[inline(always)]
pub fn check() {}

#[inline(always)]
pub(crate) fn allocated(_: *mut u8, _: usize) {}

#[inline(always)]
pub(crate) fn freed(_: *mut u8, _: usize) {}

#[inline(always)]
pub(crate) fn reallocated(_: *mut u8, _: usize, _: *mut u8, _: usize) {}
