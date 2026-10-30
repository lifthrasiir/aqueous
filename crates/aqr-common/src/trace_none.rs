//! [`trace`](self) on WebAssembly: nothing of it, so that none is in the modules (a constant
//! `on()` alone leaves their drop glue and thread-locals there).

#[derive(Clone)]
pub struct Context;
pub struct Span;
pub struct Item;
pub struct Adopted;

#[inline]
pub fn on() -> bool {
    false
}

#[inline]
pub fn enable() {}

#[inline]
pub fn register() {}

#[inline]
pub fn span(_: &'static str) -> Span {
    Span
}

#[inline]
pub fn wait(_: &'static str) -> Span {
    Span
}

#[inline]
pub fn item(_: Option<usize>) -> Item {
    Item
}

#[inline]
pub fn context() -> Option<Context> {
    None
}

#[inline]
pub fn adopt(_: Option<Context>) -> Adopted {
    Adopted
}

#[inline]
pub(crate) fn allocated(_: *mut u8, _: usize) {}

#[inline]
pub(crate) fn freed(_: *mut u8, _: usize) {}

#[inline]
pub(crate) fn reallocated(_: *mut u8, _: usize, _: *mut u8, _: usize) {}
