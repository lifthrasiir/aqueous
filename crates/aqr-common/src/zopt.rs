//! Developer options (`aqr -Z KEY[=VALUE]`): values that are otherwise constants — bounds,
//! thresholds, sample sizes, switches of a stage — set at run time, so that another setting is
//! tried without a rebuild. They are for development, not stable across versions.
//!
//! Each crate declares its own ([`zopt!`](crate::zopt!)): a static [`Opt`] that the code reads
//! ([`Opt::get`]) where it had the constant, registered by its name for `aqr` to set and list
//! (`-Z help`, `-Z NS:help`). Names are `[a-z0-9-]` words, `NS:NAME` for those of one format or
//! stage. A switch is given as `-Z NAME` and `-Z no-NAME` (`NS:no-NAME`); a value as
//! `-Z NAME=VALUE`, and as `-Z no-NAME` too where a value turns the thing off ([`Opt::off`]).
//!
//! In WebAssembly (the modules) an option is its default: nothing is registered, and the code
//! of a module does not change by reading one.

#[cfg(not(target_family = "wasm"))]
use std::sync::OnceLock;

/// An option's default.
pub enum Default<T: 'static> {
    Value(T),
    /// Made of other settings when read (the system's memory, the level, …), and what it is.
    Derived(fn() -> T, &'static str),
}

/// What an option's values are, as told and parsed.
pub trait Value: Copy + Send + Sync + 'static {
    /// The placeholder of a value in `-Z help`: `N`, `SIZE`, `X`, …
    const META: &'static str;
    fn parse(s: &str) -> Result<Self, String>;
    fn show(&self) -> String;
    /// What the registry does with an option of this type (natively). The types here have theirs
    /// as statics of this crate, so that a crate declaring options of them does not compile its
    /// own copy of that code (generics are not shared across crates in optimised builds).
    #[cfg(not(target_family = "wasm"))]
    #[doc(hidden)]
    const OPS: &'static Ops = &Ops::of::<Self>();
    /// [`Value::OPS`] of `Option<Self>` and of `&[Self]`, of the same crate as `OPS`.
    #[cfg(not(target_family = "wasm"))]
    #[doc(hidden)]
    const OPTION_OPS: &'static Ops = &Ops::of::<Option<Self>>();
    #[cfg(not(target_family = "wasm"))]
    #[doc(hidden)]
    const LIST_OPS: &'static Ops = &Ops::of::<&'static [Self]>();
}

/// [`Value::OPS`], [`Value::OPTION_OPS`] and [`Value::LIST_OPS`] of a type of this crate,
/// statics of it.
macro_rules! own_ops_of {
    ($t:ty) => {
        #[cfg(not(target_family = "wasm"))]
        const OPS: &'static Ops = {
            static OPS: Ops = Ops::of::<$t>();
            &OPS
        };
        #[cfg(not(target_family = "wasm"))]
        const OPTION_OPS: &'static Ops = {
            static OPS: Ops = Ops::of::<Option<$t>>();
            &OPS
        };
        #[cfg(not(target_family = "wasm"))]
        const LIST_OPS: &'static Ops = {
            static OPS: Ops = Ops::of::<&'static [$t]>();
            &OPS
        };
    };
}

/// A developer option of type `T`. Declared by [`zopt!`](crate::zopt!).
pub struct Opt<T: Value> {
    pub name: &'static str,
    pub help: &'static str,
    default: Default<T>,
    /// The value when given without one (`-Z NAME`).
    bare: Option<T>,
    /// The value of `-Z no-NAME`.
    off: Option<T>,
    /// Shown and taken as a size (`64K`, `1M`).
    size: bool,
    /// The values taken where they are words (empty: any).
    choices: &'static [&'static str],
    #[cfg(not(target_family = "wasm"))]
    set: OnceLock<T>,
}

impl<T: Value> Opt<T> {
    /// An option of a value, with no form without one.
    pub const fn value(name: &'static str, default: T, help: &'static str) -> Self {
        Opt {
            name,
            help,
            default: Default::Value(default),
            bare: None,
            off: None,
            size: false,
            choices: &[],
            #[cfg(not(target_family = "wasm"))]
            set: OnceLock::new(),
        }
    }

    /// Its default made of other settings when read, `what` telling it.
    pub const fn derived(mut self, f: fn() -> T, what: &'static str) -> Self {
        self.default = Default::Derived(f, what);
        self
    }

    /// `-Z no-NAME` giving `v`.
    pub const fn off(mut self, v: T) -> Self {
        self.off = Some(v);
        self
    }

    /// `-Z NAME` (no value) giving `v`.
    pub const fn bare(mut self, v: T) -> Self {
        self.bare = Some(v);
        self
    }

    /// The words it takes.
    pub const fn choices(mut self, words: &'static [&'static str]) -> Self {
        self.choices = words;
        self
    }

    /// The value given on this thread ([`Opt::set_here`]), else the value set, else the default.
    #[inline]
    pub fn get(&self) -> T {
        #[cfg(not(target_family = "wasm"))]
        if let Some(v) = here::get(self as *const Self as usize, "") {
            return v;
        }
        #[cfg(not(target_family = "wasm"))]
        if let Some(v) = self.set.get() {
            return *v;
        }
        self.default()
    }

    /// Gives `v` as the option's value on this thread until the guard returned is dropped (then
    /// the one before again), whatever `-Z` set: for tests, which share a process and its
    /// options. Other threads (the pool's workers) see the option as it is.
    #[cfg(not(target_family = "wasm"))]
    pub fn set_here(&'static self, v: T) -> Here {
        Here::new(self as *const Self as usize, "", Box::new(v))
    }

    /// The default.
    pub fn default(&self) -> T {
        match self.default {
            Default::Value(v) => v,
            Default::Derived(f, _) => f(),
        }
    }

    /// Whether it was set (`-Z`).
    pub fn is_set(&self) -> bool {
        #[cfg(not(target_family = "wasm"))]
        return self.set.get().is_some();
        #[cfg(target_family = "wasm")]
        false
    }
}

impl Opt<usize> {
    /// A size: shown with `K`, `M`, `G` where whole.
    pub const fn size(name: &'static str, default: usize, help: &'static str) -> Self {
        let mut o = Self::value(name, default, help);
        o.size = true;
        o
    }
}

impl Opt<Option<usize>> {
    /// A size or none, none by default where `default` is `None`.
    pub const fn size_or_off(name: &'static str, default: Option<usize>, help: &'static str) -> Self {
        let mut o = Self::value(name, default, help).off(None);
        o.size = true;
        o
    }
}

impl Opt<bool> {
    /// A switch: `-Z NAME`, `-Z no-NAME`, `=on`, `=off`.
    pub const fn flag(name: &'static str, default: bool, help: &'static str) -> Self {
        Self::value(name, default, help).bare(true).off(false)
    }
}

/// A size of `K`, `M`, `G` (of 1024) or bytes.
pub fn parse_size(s: &str) -> Result<usize, String> {
    let (n, shift) = match s.as_bytes().last() {
        Some(b'K' | b'k') => (&s[..s.len() - 1], 10),
        Some(b'M' | b'm') => (&s[..s.len() - 1], 20),
        Some(b'G' | b'g') => (&s[..s.len() - 1], 30),
        _ => (s, 0),
    };
    n.parse::<usize>().ok().and_then(|n| n.checked_mul(1 << shift)).ok_or_else(|| format!("not a size: {s}"))
}

/// A size shown with `K`, `M`, `G` where whole.
pub fn show_size(n: usize) -> String {
    match n {
        0 => "0".into(),
        n if n % (1 << 30) == 0 => format!("{}G", n >> 30),
        n if n % (1 << 20) == 0 => format!("{}M", n >> 20),
        n if n % (1 << 10) == 0 => format!("{}K", n >> 10),
        n => n.to_string(),
    }
}

impl Value for bool {
    const META: &'static str = "on|off";
    fn parse(s: &str) -> Result<Self, String> {
        match s {
            "on" | "yes" | "true" | "1" => Ok(true),
            "off" | "no" | "false" | "0" => Ok(false),
            _ => Err(format!("not on or off: {s}")),
        }
    }
    fn show(&self) -> String {
        if *self { "on" } else { "off" }.into()
    }
    own_ops_of!(bool);
}

macro_rules! numbers {
    ($($t:ty),*) => {$(
        impl Value for $t {
            own_ops_of!($t);
            const META: &'static str = "N";
            fn parse(s: &str) -> Result<Self, String> {
                // (sizes too, as counts are often of powers of two)
                match s.parse() {
                    Ok(n) => Ok(n),
                    Err(_) => parse_size(s).ok().and_then(|n| n.try_into().ok()).ok_or_else(|| format!("not a number: {s}")),
                }
            }
            fn show(&self) -> String {
                self.to_string()
            }
        }
    )*};
}
numbers!(usize, u64, u32, u16, u8, i64, i32);

impl Value for f64 {
    const META: &'static str = "X";
    fn parse(s: &str) -> Result<Self, String> {
        s.parse().ok().filter(|x: &f64| x.is_finite()).ok_or_else(|| format!("not a number: {s}"))
    }
    fn show(&self) -> String {
        self.to_string()
    }
    own_ops_of!(f64);
}

impl Value for &'static str {
    const META: &'static str = "TEXT";
    fn parse(s: &str) -> Result<Self, String> {
        Ok(Box::leak(s.to_owned().into_boxed_str()))
    }
    fn show(&self) -> String {
        self.to_string()
    }
    own_ops_of!(&'static str);
}

/// A list, of values separated by `,`.
impl<T: Value> Value for &'static [T] {
    const META: &'static str = "LIST";
    fn parse(s: &str) -> Result<Self, String> {
        let v = match s {
            "" => Vec::new(),
            s => s.split(',').map(T::parse).collect::<Result<Vec<_>, _>>()?,
        };
        Ok(Box::leak(v.into_boxed_slice()))
    }
    fn show(&self) -> String {
        self.iter().map(T::show).collect::<Vec<_>>().join(",")
    }
    #[cfg(not(target_family = "wasm"))]
    const OPS: &'static Ops = T::LIST_OPS;
}

/// A value, or none (off).
impl<T: Value> Value for Option<T> {
    const META: &'static str = T::META;
    fn parse(s: &str) -> Result<Self, String> {
        T::parse(s).map(Some)
    }
    fn show(&self) -> String {
        self.as_ref().map_or("off".into(), T::show)
    }
    #[cfg(not(target_family = "wasm"))]
    const OPS: &'static Ops = T::OPTION_OPS;
}

/// A word naming a namespace (`-Z NS:help`), and what it covers. Declared by
/// [`zopt_namespace!`](crate::zopt_namespace!).
pub struct Namespace {
    pub name: &'static str,
    pub help: &'static str,
}

/// A family of developer options of type `T`, one for each of a set of names (formats,
/// modules): `-Z PREFIX:MEMBER=VALUE` (`-Z heap:gzip=4`). Unset, a member is the default of the
/// code reading it ([`Family::get`] is `None`). Declared by [`zopt!`](crate::zopt!).
pub struct Family<T: Value> {
    /// `PREFIX:WHAT`, what a member is in capitals: `heap:FORMAT`, `native:MODULE`.
    pub name: &'static str,
    pub help: &'static str,
    /// The members (natively; their names as given).
    #[allow(dead_code)]
    members: fn() -> Vec<&'static str>,
    /// The default, as told (`as formats.rs has it`).
    #[allow(dead_code)]
    default: &'static str,
    #[allow(dead_code)]
    bare: Option<T>,
    #[allow(dead_code)]
    off: Option<T>,
    #[cfg(not(target_family = "wasm"))]
    set: std::sync::RwLock<Vec<(&'static str, T)>>,
}

impl<T: Value> Family<T> {
    pub const fn new(
        name: &'static str,
        members: fn() -> Vec<&'static str>,
        default: &'static str,
        help: &'static str,
    ) -> Self {
        Family {
            name,
            help,
            members,
            default,
            bare: None,
            off: None,
            #[cfg(not(target_family = "wasm"))]
            set: std::sync::RwLock::new(Vec::new()),
        }
    }

    /// `-Z PREFIX:no-MEMBER` giving `v`.
    pub const fn off(mut self, v: T) -> Self {
        self.off = Some(v);
        self
    }

    /// `-Z PREFIX:MEMBER` (no value) giving `v`.
    pub const fn bare(mut self, v: T) -> Self {
        self.bare = Some(v);
        self
    }

    /// The value of `member` given on this thread ([`Family::set_here`]), else set, if any.
    pub fn get(&self, member: &str) -> Option<T> {
        #[cfg(not(target_family = "wasm"))]
        if let Some(v) = here::get(self as *const Self as usize, member) {
            return Some(v);
        }
        #[cfg(not(target_family = "wasm"))]
        return self.set.read().unwrap().iter().find(|m| m.0 == member).map(|m| m.1);
        #[cfg(target_family = "wasm")]
        {
            let _ = member;
            None
        }
    }
}

#[cfg(not(target_family = "wasm"))]
impl<T: Value> Family<T> {
    /// [`Opt::set_here`] of `member`.
    pub fn set_here(&'static self, member: &str, v: T) -> Here {
        Here::new(self as *const Self as usize, member, Box::new(v))
    }
}

/// Values given on a thread ([`Opt::set_here`]), by the option (where it is) and the member of a
/// family (empty for an option).
#[cfg(not(target_family = "wasm"))]
mod here {
    use std::any::Any;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Whether any value was ever given on a thread: else [`get`] looks no further, so that
    /// reading an option costs as before outside tests.
    pub(super) static ANY: AtomicBool = AtomicBool::new(false);

    type Key = (usize, String);

    thread_local! {
        pub(super) static VALUES: RefCell<HashMap<Key, Box<dyn Any>>> = RefCell::new(HashMap::new());
    }

    #[inline]
    pub(super) fn get<T: Copy + 'static>(at: usize, member: &str) -> Option<T> {
        if !ANY.load(Ordering::Relaxed) {
            return None;
        }
        let mut v = std::mem::MaybeUninit::<T>::uninit();
        // (copied by `copy`: `T` is `Copy`, and of the type checked)
        let copy = &mut |b: &dyn Any| unsafe {
            std::ptr::copy_nonoverlapping(b as *const dyn Any as *const T, v.as_mut_ptr(), 1)
        };
        find(at, member, std::any::TypeId::of::<T>(), copy).then(|| unsafe { v.assume_init() })
    }

    /// Gives `copy` the value at (`at`, `member`) if it is of type `id`: of no type, so that the
    /// crates reading options do not each compile the lookup.
    fn find(at: usize, member: &str, id: std::any::TypeId, copy: &mut dyn FnMut(&dyn Any)) -> bool {
        VALUES.with(|v| match v.borrow().get(&(at, member.to_owned())) {
            Some(b) if (**b).type_id() == id => {
                copy(&**b);
                true
            }
            _ => false,
        })
    }

    pub(super) fn put(key: Key, v: Option<Box<dyn Any>>) -> Option<Box<dyn Any>> {
        ANY.store(true, Ordering::Relaxed);
        VALUES.with(|m| {
            let mut m = m.borrow_mut();
            match v {
                Some(v) => m.insert(key, v),
                None => m.remove(&key),
            }
        })
    }
}

/// A value given to an option on this thread ([`Opt::set_here`], [`Family::set_here`]): the one
/// before again once dropped. Of the thread it was made on (not `Send`).
#[cfg(not(target_family = "wasm"))]
#[must_use = "the value is given only until the guard is dropped"]
pub struct Here {
    key: (usize, String),
    before: Option<Box<dyn std::any::Any>>,
}

#[cfg(not(target_family = "wasm"))]
impl Here {
    fn new(at: usize, member: &str, v: Box<dyn std::any::Any>) -> Here {
        let key = (at, member.to_owned());
        let before = here::put(key.clone(), Some(v));
        Here { key, before }
    }
}

#[cfg(not(target_family = "wasm"))]
impl Drop for Here {
    fn drop(&mut self) {
        here::put(std::mem::take(&mut self.key), self.before.take());
    }
}

impl Family<bool> {
    /// A switch of each member: `-Z PREFIX:MEMBER`, `-Z PREFIX:no-MEMBER`.
    pub const fn flags(
        name: &'static str,
        members: fn() -> Vec<&'static str>,
        default: &'static str,
        help: &'static str,
    ) -> Self {
        Self::new(name, members, default, help).bare(true).off(false)
    }
}

/// Declares developer options: statics of [`Opt`] (or [`Family`]), registered by their names
/// (natively).
///
/// ```ignore
/// aqr_common::zopt! {
///     /// Streams shorter than this are not taken as held by the files archived along.
///     pub(crate) static SHARE_MIN: Opt<usize> = Opt::size("share-min", 256 << 10, "streams shorter are not taken as held by files archived along");
/// }
/// ```
#[macro_export]
macro_rules! zopt {
    ($($(#[$m:meta])* $vis:vis static $name:ident: $kind:ident<$t:ty> = $init:expr;)*) => {$(
        $(#[$m])*
        $vis static $name: $crate::zopt::$kind<$t> = {
            #[allow(unused_imports)]
            use $crate::zopt::{Family, Opt};
            $init
        };
        $crate::__zopt_submit! { $crate::zopt::Entry::$kind(&$name) }
    )*};
}

/// Declares a namespace of developer options and what it covers.
#[macro_export]
macro_rules! zopt_namespace {
    ($name:literal, $help:literal) => {
        $crate::__zopt_submit! { $crate::zopt::Namespace { name: $name, help: $help } }
    };
}

/// Registers an option or a namespace where the registry is built (feature `registry`: the
/// programs taking `-Z`). Elsewhere (the modules' native libraries and the blobs, which take no
/// `-Z`) nothing is registered: a registration is kept by the linker with all it reaches, of
/// every crate linked, read or not.
#[cfg(all(not(target_family = "wasm"), any(feature = "registry", test)))]
#[doc(hidden)]
#[macro_export]
macro_rules! __zopt_submit {
    ($($t:tt)*) => {
        $crate::zopt::inventory::submit! { $($t)* }
    };
}

#[cfg(not(all(not(target_family = "wasm"), any(feature = "registry", test))))]
#[doc(hidden)]
#[macro_export]
macro_rules! __zopt_submit {
    ($($t:tt)*) => {};
}

crate::zopt! {
    /// The bytes that an archive of other formats (ZIP, git packs, EGG, RAR, …) may decode its
    /// members to at most, of `n` input bytes: [`LIMIT_BASE`] and [`LIMIT_RATIO`] times `n`; the
    /// members past it are kept as they are. Each format's own (`zip:limit-base`) is by default
    /// these.
    pub static LIMIT_BASE: Opt<usize> = Opt::size(
        "limit-base",
        64 << 20,
        "bytes an archive of other formats may decode its members to, besides limit-ratio per byte",
    );
    pub static LIMIT_RATIO: Opt<usize> =
        Opt::value("limit-ratio", 32, "bytes per byte of input an archive of other formats may decode to");
}

/// [`LIMIT_BASE`] and [`LIMIT_RATIO`] of `n` bytes: `base + ratio·n`.
pub fn limit_of(n: usize, base: &Opt<usize>, ratio: &Opt<usize>) -> usize {
    base.get().saturating_add(ratio.get().saturating_mul(n))
}

#[cfg(not(target_family = "wasm"))]
#[doc(hidden)]
pub use inventory;
#[cfg(not(target_family = "wasm"))]
pub use native::*;

#[cfg(not(target_family = "wasm"))]
mod native {
    use super::{Default, Family, Namespace, Opt, Value, show_size};

    /// An option or a family as registered, of any type: where it is and its type's [`Ops`].
    pub struct Entry {
        at: *const (),
        ops: &'static Ops,
        family: bool,
    }

    // (`at` is an `Opt` or a `Family`, which are `Sync`)
    unsafe impl Sync for Entry {}
    unsafe impl Send for Entry {}

    #[allow(non_snake_case)]
    impl Entry {
        /// The entry of an option ([`zopt!`](crate::zopt!) names the kind).
        pub const fn Opt<T: Value>(o: &'static Opt<T>) -> Entry {
            Entry { at: o as *const Opt<T> as *const (), ops: T::OPS, family: false }
        }

        /// The entry of a family.
        pub const fn Family<T: Value>(f: &'static Family<T>) -> Entry {
            Entry { at: f as *const Family<T> as *const (), ops: T::OPS, family: true }
        }
    }

    /// What the registry does with an [`Opt`] or a [`Family`] of a type, by where it is: the
    /// functions of [`Dyn`] made for the type once, not in every crate declaring options of it.
    #[doc(hidden)]
    pub struct Ops {
        opt: Fns,
        family: Fns,
    }

    struct Fns {
        name: fn(*const ()) -> &'static str,
        help: fn(*const ()) -> &'static str,
        usage: fn(*const ()) -> String,
        default_text: fn(*const ()) -> String,
        set_text: fn(*const ()) -> Option<String>,
        member: fn(*const (), &str) -> Option<&'static str>,
        members: fn(*const ()) -> Option<Vec<&'static str>>,
        value_of: ValueOf,
    }

    type ValueOf = fn(*const (), &'static str, Option<&str>, bool) -> Result<Box<dyn FnOnce() -> bool>, String>;

    impl Ops {
        pub const fn of<T: Value>() -> Ops {
            Ops { opt: fns::<Opt<T>>(), family: fns::<Family<T>>() }
        }
    }

    /// [`Fns`] of an `Opt` or a `Family`, at a pointer that is of `D` (an [`Entry`]'s).
    const fn fns<D: Dyn + 'static>() -> Fns {
        // (`at` points to a static of `D`, as `Entry`'s constructors make it)
        fn d<D: 'static>(at: *const ()) -> &'static D {
            unsafe { &*(at as *const D) }
        }
        Fns {
            name: |at| d::<D>(at).name(),
            help: |at| d::<D>(at).help(),
            usage: |at| d::<D>(at).usage(),
            default_text: |at| d::<D>(at).default_text(),
            set_text: |at| d::<D>(at).set_text(),
            member: |at, name| d::<D>(at).member(name),
            members: |at| d::<D>(at).members(),
            value_of: |at, member, v, negated| d::<D>(at).value_of(member, v, negated),
        }
    }

    impl Entry {
        fn fns(&self) -> &'static Fns {
            if self.family { &self.ops.family } else { &self.ops.opt }
        }
    }

    impl Dyn for Entry {
        fn name(&self) -> &'static str {
            (self.fns().name)(self.at)
        }

        fn help(&self) -> &'static str {
            (self.fns().help)(self.at)
        }

        fn usage(&self) -> String {
            (self.fns().usage)(self.at)
        }

        fn default_text(&self) -> String {
            (self.fns().default_text)(self.at)
        }

        fn set_text(&self) -> Option<String> {
            (self.fns().set_text)(self.at)
        }

        fn member(&self, name: &str) -> Option<&'static str> {
            (self.fns().member)(self.at, name)
        }

        fn members(&self) -> Option<Vec<&'static str>> {
            (self.fns().members)(self.at)
        }

        fn value_of(
            &'static self,
            member: &'static str,
            value: Option<&str>,
            negated: bool,
        ) -> Result<Box<dyn FnOnce() -> bool>, String> {
            (self.fns().value_of)(self.at, member, value, negated)
        }
    }

    inventory::collect!(Entry);
    inventory::collect!(Namespace);

    /// What the registry takes of an [`Opt`].
    pub trait Dyn: Sync {
        fn name(&self) -> &'static str;
        fn help(&self) -> &'static str;
        /// How it is given: `[no-]NAME`, `NAME=SIZE`, `NAME[=N] | no-NAME`.
        fn usage(&self) -> String;
        /// The default, as told.
        fn default_text(&self) -> String;
        /// The value set, if any.
        fn set_text(&self) -> Option<String>;
        /// The member that `name` is of a family (the name itself of an option): `None` where it
        /// is not of this one.
        fn member(&self, name: &str) -> Option<&'static str>;
        /// A family's members, natively; `None` of an option.
        fn members(&self) -> Option<Vec<&'static str>> {
            None
        }
        /// Its value of `-Z NAME=VALUE` (`Some`), `-Z NAME` (`None`), or `-Z no-NAME`, as a
        /// setting of it: false where it was set already.
        fn value_of(
            &'static self,
            member: &'static str,
            value: Option<&str>,
            negated: bool,
        ) -> Result<Box<dyn FnOnce() -> bool>, String>;
    }

    /// The value of `-Z NAME=VALUE` (`Some`), `-Z NAME` (`None`), or `-Z no-NAME`.
    fn value<T: Value>(
        usage: &dyn Fn() -> String,
        (bare, off, size, choices): (Option<T>, Option<T>, bool, &[&str]),
        value: Option<&str>,
        negated: bool,
    ) -> Result<T, String> {
        Ok(match (value, negated) {
            (Some(_), true) => return Err("no- takes no value".into()),
            (None, true) => off.ok_or("takes no no- form")?,
            (None, false) => bare.ok_or_else(|| format!("takes a value ({})", usage()))?,
            (Some(s), false) => {
                if !choices.is_empty() && !choices.contains(&s) {
                    return Err(format!("takes one of {}", choices.join(", ")));
                }
                match size {
                    true => T::parse(&super::parse_size(s)?.to_string())?,
                    false => T::parse(s)?,
                }
            }
        })
    }

    /// The usage of a name `ns` `word` (`ns` with its `:`) of values `meta`.
    fn usage_of(ns: &str, word: &str, meta: &str, switch: bool, bare: bool, off: bool) -> String {
        match (bare, off) {
            _ if switch => format!("{ns}[no-]{word}"),
            (false, false) => format!("{ns}{word}={meta}"),
            (false, true) => format!("{ns}{word}={meta} | {ns}no-{word}"),
            (true, false) => format!("{ns}{word}[={meta}]"),
            (true, true) => format!("{ns}[no-]{word}[={meta}]"),
        }
    }

    impl<T: Value> Dyn for Opt<T> {
        fn name(&self) -> &'static str {
            self.name
        }

        fn help(&self) -> &'static str {
            self.help
        }

        fn usage(&self) -> String {
            let (ns, word) = split(self.name);
            let meta = match () {
                _ if !self.choices.is_empty() => self.choices.join("|"),
                _ if self.size => "SIZE".into(),
                _ => T::META.into(),
            };
            usage_of(ns, word, &meta, T::META == "on|off", self.bare.is_some(), self.off.is_some())
        }

        fn default_text(&self) -> String {
            match &self.default {
                Default::Value(v) => self.show(v),
                Default::Derived(f, what) => format!("{what} ({})", self.show(&f())),
            }
        }

        fn set_text(&self) -> Option<String> {
            self.set.get().map(|v| self.show(v))
        }

        fn member(&self, name: &str) -> Option<&'static str> {
            (name == self.name).then_some(self.name)
        }

        fn value_of(
            &'static self,
            _: &'static str,
            v: Option<&str>,
            negated: bool,
        ) -> Result<Box<dyn FnOnce() -> bool>, String> {
            let v = value(&|| self.usage(), (self.bare, self.off, self.size, self.choices), v, negated)?;
            let cell = &self.set;
            Ok(Box::new(move || cell.set(v).is_ok()))
        }
    }

    impl<T: Value> Dyn for Family<T> {
        fn name(&self) -> &'static str {
            self.name
        }

        fn help(&self) -> &'static str {
            self.help
        }

        fn usage(&self) -> String {
            let (ns, what) = split(self.name);
            usage_of(ns, what, T::META, T::META == "on|off", self.bare.is_some(), self.off.is_some())
        }

        fn default_text(&self) -> String {
            self.default.into()
        }

        fn set_text(&self) -> Option<String> {
            let set = self.set.read().unwrap();
            (!set.is_empty())
                .then(|| set.iter().map(|(m, v)| format!("{m}={}", v.show())).collect::<Vec<_>>().join(","))
        }

        fn member(&self, name: &str) -> Option<&'static str> {
            let rest = name.strip_prefix(split(self.name).0)?;
            (self.members)().into_iter().find(|m| *m == rest)
        }

        fn members(&self) -> Option<Vec<&'static str>> {
            Some((self.members)())
        }

        fn value_of(
            &'static self,
            member: &'static str,
            v: Option<&str>,
            negated: bool,
        ) -> Result<Box<dyn FnOnce() -> bool>, String> {
            let v = value(&|| self.usage(), (self.bare, self.off, false, &[]), v, negated)?;
            Ok(Box::new(move || {
                let mut set = self.set.write().unwrap();
                if set.iter().any(|m| m.0 == member) {
                    return false;
                }
                set.push((member, v));
                true
            }))
        }
    }

    impl<T: Value> Opt<T> {
        fn show(&self, v: &T) -> String {
            let s = v.show();
            match (self.size, s.parse::<usize>()) {
                (true, Ok(n)) => show_size(n),
                _ => s,
            }
        }
    }

    /// `NS:` and the rest of a name (`""` and the name where it has no namespace).
    fn split(name: &str) -> (&str, &str) {
        match name.find(':') {
            Some(i) => name.split_at(i + 1),
            None => ("", name),
        }
    }

    /// The options registered, by name.
    pub fn all() -> Vec<&'static dyn Dyn> {
        let mut v: Vec<&'static dyn Dyn> = inventory::iter::<Entry>.into_iter().map(|e| e as &dyn Dyn).collect();
        v.sort_by_key(|o| o.name());
        v
    }

    /// The namespaces: those declared and those of options, by name, with what they cover.
    pub fn namespaces() -> Vec<(&'static str, &'static str, usize)> {
        let mut v: Vec<(&str, &str, usize)> = Vec::new();
        for o in all() {
            if let Some(i) = o.name().find(':') {
                let ns = &o.name()[..i];
                match v.iter_mut().find(|n| n.0 == ns) {
                    Some(n) => n.2 += 1,
                    None => v.push((ns, "", 1)),
                }
            }
        }
        for n in inventory::iter::<Namespace> {
            match v.iter_mut().find(|v| v.0 == n.name) {
                Some(v) if v.1.is_empty() => v.1 = n.help,
                Some(_) => {}
                None => v.push((n.name, n.help, 0)),
            }
        }
        v.sort();
        v
    }

    /// Whether a name is well formed: words of `[a-z0-9-]`, at most one `NS:`, no word
    /// `help` or beginning with `no-`.
    pub fn well_formed(name: &str) -> bool {
        let word = |w: &str| {
            !w.is_empty()
                && w != "help"
                && !w.starts_with("no-")
                && !w.starts_with('-')
                && !w.ends_with('-')
                && w.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        };
        match name.split_once(':') {
            Some((ns, rest)) => word(ns) && word(rest),
            None => word(name),
        }
    }

    /// What [`apply`] did.
    #[derive(Debug, PartialEq)]
    pub enum Applied {
        /// Options set.
        Set,
        /// Help asked for (`help`, `NS:help`): the text.
        Help(String),
    }

    /// Sets the options of `-Z` arguments (`NAME`, `NAME=VALUE`, `no-NAME`, `NS:no-NAME`), the
    /// last of a name taken; or the help asked for (`help`, `NS:help`), told of the options as
    /// set by the others. Once in a process: an option set already is not set again.
    pub fn apply<S: AsRef<str>>(args: &[S]) -> Result<Applied, String> {
        let opts = all();
        if let Some(d) = opts.windows(2).find(|w| w[0].name() == w[1].name()) {
            return Err(format!("-Z {}: declared twice", d[0].name()));
        }
        let mut sets: Vec<(String, Box<dyn FnOnce() -> bool>)> = Vec::new();
        let mut help = None;
        for a in args {
            let a = a.as_ref();
            let (key, value) = match a.split_once('=') {
                Some((k, v)) => (k, Some(v)),
                None => (a, None),
            };
            let (ns, word) = split(key);
            if word == "help" && value.is_none() {
                help = Some(ns.trim_end_matches(':').to_string());
                continue;
            }
            let find = |name: &str| {
                // (an option of the name before a family it would be a member of)
                let exact = opts.iter().find(|o| o.members().is_none() && o.member(name).is_some());
                exact.or_else(|| opts.iter().find(|o| o.member(name).is_some())).map(|o| (*o, o.member(name).unwrap()))
            };
            let (found, negated) = match (find(key), word.strip_prefix("no-")) {
                (Some(f), _) => (Some(f), false),
                (None, Some(w)) => (find(&format!("{ns}{w}")), true),
                (None, None) => (None, false),
            };
            let Some((o, member)) = found else {
                return Err(unknown(key, &opts));
            };
            let set = o.value_of(member, value, negated).map_err(|e| format!("-Z {a}: {e}"))?;
            let id = format!("{}:{member}", o.name());
            sets.retain(|s| s.0 != id);
            sets.push((id, set));
        }
        for (id, set) in sets {
            if !set() {
                return Err(format!("-Z {id}: set already"));
            }
        }
        Ok(match help {
            None => Applied::Set,
            Some(ns) => Applied::Help(self::help(&ns)?),
        })
    }

    fn unknown(name: &str, opts: &[&'static dyn Dyn]) -> String {
        let (ns, _) = split(name);
        let ns = ns.trim_end_matches(':');
        let known_ns = ns.is_empty() || namespaces().iter().any(|n| n.0 == ns);
        let near: Vec<_> = opts.iter().map(|o| o.name()).filter(|n| close(n, name)).collect();
        let mut e = format!("-Z {name}: no such option");
        if !near.is_empty() {
            e += &format!(" (did you mean {}?)", near.join(", "));
        }
        e += &match known_ns {
            true if ns.is_empty() => " (-Z help lists them)".to_string(),
            true => format!(" (-Z {ns}:help lists those of {ns}:)"),
            false => format!(" (no namespace {ns}: -Z help lists them)"),
        };
        e
    }

    /// Whether two names differ by an edit or two (or one holds the other).
    fn close(a: &str, b: &str) -> bool {
        let (a, b) = (a.as_bytes(), b.as_bytes());
        let mut row: Vec<usize> = (0..=b.len()).collect();
        for (i, &x) in a.iter().enumerate() {
            let mut prev = row[0];
            row[0] = i + 1;
            for (j, &y) in b.iter().enumerate() {
                let cur = row[j + 1];
                row[j + 1] = (prev + (x != y) as usize).min(row[j] + 1).min(cur + 1);
                prev = cur;
            }
        }
        row[b.len()] <= 2
    }

    /// The help of the options of namespace `ns` (`""`: those of none, and the namespaces).
    pub fn help(ns: &str) -> Result<String, String> {
        let spaces = namespaces();
        if !ns.is_empty() && !spaces.iter().any(|n| n.0 == ns) {
            return Err(format!("-Z {ns}:help: no namespace {ns}: (-Z help lists them)"));
        }
        let opts: Vec<_> = all()
            .into_iter()
            .filter(|o| match o.name().find(':') {
                Some(i) => &o.name()[..i] == ns,
                None => ns.is_empty(),
            })
            .collect();
        let mut s = String::new();
        match ns {
            "" => {
                s += "Developer options, not stable across versions: -Z NAME=VALUE (-ZNAME=VALUE),\n\
                        -Z NAME and -Z no-NAME of a switch (NS:no-NAME in a namespace).\n"
            }
            _ => {
                let what = spaces.iter().find(|n| n.0 == ns).map_or("", |n| n.1);
                s += &format!("Developer options of {ns}:{}{what}\n", if what.is_empty() { "" } else { " " });
            }
        }
        table(&mut s, &opts);
        if ns.is_empty() {
            s += "\nNamespaces (-Z NS:help):\n";
            let w = spaces.iter().map(|n| n.0.len()).max().unwrap_or(0);
            for (name, what, n) in &spaces {
                s += &format!("  {name:w$}  {n:>3}  {what}\n");
            }
        }
        Ok(s)
    }

    fn table(s: &mut String, opts: &[&'static dyn Dyn]) {
        if opts.is_empty() {
            *s += "  (none)\n";
            return;
        }
        let rows: Vec<_> = opts
            .iter()
            .map(|o| {
                let set = o.set_text().map_or(String::new(), |v| format!(" [set: {v}]"));
                (o.usage(), format!("{}{set}", o.default_text()), o.help(), o.members())
            })
            .collect();
        // (a usage or default too long goes on a line of its own)
        let uw = rows.iter().map(|r| r.0.len()).filter(|&n| n <= 34).max().unwrap_or(0);
        let dw = rows.iter().map(|r| r.1.chars().count()).filter(|&n| n <= 16).max().unwrap_or(0);
        for (usage, default, help, members) in rows {
            if usage.len() > uw {
                *s += &format!("  {usage}\n  {:uw$}  {default:dw$}  {help}\n", "");
            } else if default.chars().count() > dw {
                *s += &format!("  {usage:uw$}  {default}\n  {:uw$}  {:dw$}  {help}\n", "", "");
            } else {
                *s += &format!("  {usage:uw$}  {default:dw$}  {help}\n");
            }
            // (a family's members, in lines of about 100 columns)
            if let Some(mut m) = members {
                m.sort();
                let indent = 4 + uw;
                let mut line = String::new();
                for w in m {
                    if !line.is_empty() && indent + line.len() + w.len() + 2 > 100 {
                        *s += &format!("{:indent$}{line},\n", "");
                        line.clear();
                    }
                    if !line.is_empty() {
                        line += ", ";
                    }
                    line += w;
                }
                *s += &format!("{:indent$}{line}\n", "");
            }
        }
    }
}

#[cfg(all(test, not(target_family = "wasm")))]
mod tests {
    use super::*;

    crate::zopt! {
        static FLAG: Opt<bool> = Opt::flag("zopt-test:flag", false, "a switch");
        static SIZE: Opt<usize> = Opt::size("zopt-test:size", 64 << 10, "a size");
        static OFF: Opt<Option<usize>> = Opt::size_or_off("zopt-test:ldm", Some(1 << 20), "a size or off");
        static LIST: Opt<&'static [u32]> = Opt::<&[u32]>::value("zopt-test:list", &[4, 6, 9], "levels");
        static DERIVED: Opt<usize> = Opt::value("zopt-test:derived", 0, "derived").derived(|| 7, "seven");
        static WORD: Opt<&'static str> = Opt::value("zopt-test:word", "auto", "a choice").choices(&["auto", "log", "xsv"]);
        static HEAP: Family<f64> = Family::new("zopt-test:FORMAT", || vec!["gzip", "zip"], "per format", "heap");
        static ON: Family<bool> = Family::flags("zopt-test-on:FORMAT", || vec!["tar", "zst"], "on", "formats taken");
    }
    crate::zopt_namespace!("zopt-test", "options of the tests of zopt");

    /// A value given on a thread is its value there until dropped, over what was set, the one
    /// before again after it; other threads see the option as it is.
    #[test]
    fn set_here() {
        crate::zopt! {
            static HERE: Opt<usize> = Opt::value("zopt-test:here", 3, "given on a thread");
            static HERE_ON: Family<bool> = Family::flags("zopt-test-here:FORMAT", || vec!["tar"], "on", "formats taken");
        }
        assert_eq!(HERE.get(), 3);
        {
            let _a = HERE.set_here(5);
            assert_eq!(HERE.get(), 5);
            {
                let _b = HERE.set_here(7);
                assert_eq!(HERE.get(), 7);
                assert_eq!(std::thread::spawn(|| HERE.get()).join().unwrap(), 3);
            }
            assert_eq!(HERE.get(), 5);
            let _t = HERE_ON.set_here("tar", false);
            assert_eq!((HERE_ON.get("tar"), HERE_ON.get("zst")), (Some(false), None));
        }
        assert_eq!((HERE.get(), HERE_ON.get("tar")), (3, None));
    }

    #[test]
    fn options() {
        assert_eq!(SIZE.get(), 64 << 10);
        assert_eq!(DERIVED.get(), 7);
        let r = apply(&[
            "zopt-test:flag",
            "zopt-test:size=1m",
            "zopt-test:size=2M",
            "zopt-test:no-ldm",
            "zopt-test:list=1,2",
            "zopt-test:word=xsv",
            "zopt-test:zip=7.5",
            "zopt-test-on:no-tar",
        ]);
        assert_eq!(r, Ok(Applied::Set));
        assert!(FLAG.get());
        assert_eq!(SIZE.get(), 2 << 20);
        assert_eq!(OFF.get(), None);
        assert_eq!(LIST.get(), &[1, 2]);
        assert_eq!(WORD.get(), "xsv");
        assert_eq!((HEAP.get("zip"), HEAP.get("gzip")), (Some(7.5), None));
        assert_eq!((ON.get("tar"), ON.get("zst")), (Some(false), None));
        assert!(apply(&["zopt-test:rar=1"]).unwrap_err().contains("no such option"));
        assert!(apply(&["zopt-test:zip=1"]).unwrap_err().contains("set already"));
        assert!(apply(&["zopt-test:size=3m"]).unwrap_err().contains("set already"));
        assert!(apply(&["zopt-test:word=csv"]).unwrap_err().contains("one of"));
        assert!(apply(&["zopt-test:no-size"]).unwrap_err().contains("no no- form"));
        assert!(apply(&["zopt-test:siz=1"]).unwrap_err().contains("did you mean zopt-test:size"));
        assert!(apply(&["zopt-test:size"]).unwrap_err().contains("takes a value"));
        assert!(apply(&["nowhere:x"]).unwrap_err().contains("no namespace nowhere"));
        let Ok(Applied::Help(h)) = apply(&["zopt-test:help"]) else { panic!() };
        assert!(h.contains("zopt-test:[no-]flag"), "{h}");
        assert!(h.contains("zopt-test:size=SIZE"), "{h}");
        assert!(h.contains("[set: 2M]"), "{h}");
        assert!(h.contains("zopt-test:ldm=SIZE | zopt-test:no-ldm"), "{h}");
        assert!(h.contains("seven (7)"), "{h}");
        assert!(h.contains("auto|log|xsv"), "{h}");
        assert!(h.contains("zopt-test:FORMAT=X") && h.contains("gzip, zip") && h.contains("zip=7.5"), "{h}");
        let Ok(Applied::Help(h)) = apply(&["help"]) else { panic!() };
        assert!(h.contains("zopt-test") && h.contains("options of the tests of zopt"), "{h}");
    }

    #[test]
    fn names() {
        for o in all().into_iter().filter(|o| o.members().is_none()) {
            assert!(well_formed(o.name()), "{}", o.name());
        }
        assert!(!well_formed("no-x") && !well_formed("a:b:c") && !well_formed("A") && !well_formed("x:help"));
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size("64K"), Ok(64 << 10));
        assert_eq!(show_size(3 << 20), "3M");
        assert_eq!(show_size(1536), "1536");
    }
}
