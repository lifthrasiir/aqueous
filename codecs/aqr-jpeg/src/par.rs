//! rayon natively; in WebAssembly, where rayon has no threads, the same calls run in order on
//! the calling thread, without rayon's code in the module (its threads there are tasks, by
//! `aqr_common::pool`).

#[cfg(not(target_family = "wasm"))]
pub use rayon::{current_num_threads, join, prelude, scope};

#[cfg(target_family = "wasm")]
pub mod prelude {
    pub trait IntoParallelIterator: IntoIterator + Sized {
        fn into_par_iter(self) -> Self::IntoIter {
            self.into_iter()
        }
    }

    impl<T: IntoIterator> IntoParallelIterator for T {}

    pub trait IntoParallelRefIterator<'a> {
        type Iter;
        fn par_iter(&'a self) -> Self::Iter;
    }

    impl<'a, T: ?Sized + 'a> IntoParallelRefIterator<'a> for T
    where
        &'a T: IntoIterator,
    {
        type Iter = <&'a T as IntoIterator>::IntoIter;
        fn par_iter(&'a self) -> Self::Iter {
            self.into_iter()
        }
    }

    pub trait IntoParallelRefMutIterator<'a> {
        type Iter;
        fn par_iter_mut(&'a mut self) -> Self::Iter;
    }

    impl<'a, T: ?Sized + 'a> IntoParallelRefMutIterator<'a> for T
    where
        &'a mut T: IntoIterator,
    {
        type Iter = <&'a mut T as IntoIterator>::IntoIter;
        fn par_iter_mut(&'a mut self) -> Self::Iter {
            self.into_iter()
        }
    }

    pub trait ParallelSlice<T> {
        fn par_windows(&self, size: usize) -> std::slice::Windows<'_, T>;
    }

    impl<T> ParallelSlice<T> for [T] {
        fn par_windows(&self, size: usize) -> std::slice::Windows<'_, T> {
            self.windows(size)
        }
    }
}

#[cfg(target_family = "wasm")]
pub fn current_num_threads() -> usize {
    1
}

#[cfg(target_family = "wasm")]
pub fn join<RA, RB>(a: impl FnOnce() -> RA, b: impl FnOnce() -> RB) -> (RA, RB) {
    (a(), b())
}

#[cfg(target_family = "wasm")]
pub struct Scope<'s>(std::marker::PhantomData<&'s ()>);

#[cfg(target_family = "wasm")]
impl<'s> Scope<'s> {
    pub fn spawn(&self, f: impl FnOnce(&Scope<'s>) + 's) {
        f(self)
    }
}

#[cfg(target_family = "wasm")]
pub fn scope<'s, R>(f: impl FnOnce(&Scope<'s>) -> R) -> R {
    f(&Scope(std::marker::PhantomData))
}
