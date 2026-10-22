//! The interface of an aqr module (spec, "Module execution") for modules written in Rust.
//!
//! On WebAssembly, everything here lowers to imports from the toolchain-private module `aqr_api`,
//! which `aqr-ld` rewrites into the `aqr` imports and the multi-memory instructions of the spec;
//! the working memory is the ordinary memory of the module.
//! Elsewhere, [`emu`] emulates the host, so the same module runs natively under tests.
//!
//! The memories other than the working memory are only sized and copied in bulk:
//! [`Memory::size`], [`Memory::read`] into the working memory, [`Output::write`] from it
//! and [`Output::copy_from`] between the memories. Reading or writing out of bounds traps
//! (panics natively), which the host takes as an invalid archive.
//! On WebAssembly, a panic traps without formatting its message, [`log`]ging its location first
//! with the feature `panic-location`.
//!
//! A module defines its entry point with [`entry!`].

#![cfg_attr(target_family = "wasm", no_std)]

#[cfg(not(target_family = "wasm"))]
pub mod emu;
#[cfg(target_family = "wasm")]
mod wasm;
#[cfg(not(target_family = "wasm"))]
use emu::MemId;

mod sealed {
    pub trait Sealed {}
}

/// A memory other than the working memory.
pub trait Memory: sealed::Sealed {
    /// The size in bytes, a multiple of 65536. Fixed during an application.
    fn size() -> u64;
    /// Copies `dst.len()` bytes from offset `src` into `dst`.
    fn read(src: u64, dst: &mut [u8]);
    #[doc(hidden)]
    fn copy_to_output(dst: u64, src: u64, len: u64);
}

/// The memory of the parameters (`aqr_params`).
pub struct Params;
/// The memory of the output (`aqr_output`).
pub struct Output;
/// The memory of the `K`-th input (`aqr_input`K), for `K` < 16.
pub struct Input<const K: u32>;
/// The memory of the `I`-th resource (`aqr_resource`I), for `I` < 16.
pub struct Resource<const I: u32>;

impl Output {
    /// Copies `src` to offset `dst`.
    #[inline]
    pub fn write(dst: u64, src: &[u8]) {
        sys::write_output(dst, src)
    }

    /// Copies `len` bytes from offset `src` of `M` to offset `dst`, without the working memory.
    #[inline]
    pub fn copy_from<M: Memory>(dst: u64, src: u64, len: u64) {
        M::copy_to_output(dst, src, len)
    }

    /// Appends the bytes up to the offset `hi` of the output (`aqr_commit`). Part modules only.
    #[inline]
    pub fn commit(hi: u64) {
        sys::commit(hi)
    }
}

impl<const K: u32> Input<K> {
    /// Makes the input visible up to the offset `hi` (`aqr_request`). Part modules only.
    #[inline]
    pub fn request(hi: u64) {
        sys::request(K, hi)
    }

    /// Gives up the input below the offset `lo` (`aqr_release`). Part modules only.
    #[inline]
    pub fn release(lo: u64) {
        sys::release(K, lo)
    }
}

/// `aqr_heartbeat`.
#[inline]
pub fn heartbeat() {
    sys::heartbeat()
}

/// `aqr_log`, a diagnostic message with no effect on the application. Precede [`trap`] to report a failure.
#[inline]
pub fn log(msg: &[u8]) {
    sys::log(msg)
}

/// Traps (`unreachable`), failing the application as an invalid archive.
#[inline]
pub fn trap() -> ! {
    sys::trap()
}

#[cfg(target_family = "wasm")]
use wasm as sys;
#[cfg(not(target_family = "wasm"))]
use emu::sys;

macro_rules! memories {
    ($($ty:ty => $name:literal, $id:expr;)*) => {$(
        impl sealed::Sealed for $ty {}
        sys::memory!($ty, $name, $id);
    )*};
}

memories! {
    Params => "params", MemId::Params;
    Output => "output", MemId::Output;
    Input<0> => "input0", MemId::Input(0);
    Input<1> => "input1", MemId::Input(1);
    Input<2> => "input2", MemId::Input(2);
    Input<3> => "input3", MemId::Input(3);
    Input<4> => "input4", MemId::Input(4);
    Input<5> => "input5", MemId::Input(5);
    Input<6> => "input6", MemId::Input(6);
    Input<7> => "input7", MemId::Input(7);
    Input<8> => "input8", MemId::Input(8);
    Input<9> => "input9", MemId::Input(9);
    Input<10> => "input10", MemId::Input(10);
    Input<11> => "input11", MemId::Input(11);
    Input<12> => "input12", MemId::Input(12);
    Input<13> => "input13", MemId::Input(13);
    Input<14> => "input14", MemId::Input(14);
    Input<15> => "input15", MemId::Input(15);
    Resource<0> => "resource0", MemId::Resource(0);
    Resource<1> => "resource1", MemId::Resource(1);
    Resource<2> => "resource2", MemId::Resource(2);
    Resource<3> => "resource3", MemId::Resource(3);
    Resource<4> => "resource4", MemId::Resource(4);
    Resource<5> => "resource5", MemId::Resource(5);
    Resource<6> => "resource6", MemId::Resource(6);
    Resource<7> => "resource7", MemId::Resource(7);
    Resource<8> => "resource8", MemId::Resource(8);
    Resource<9> => "resource9", MemId::Resource(9);
    Resource<10> => "resource10", MemId::Resource(10);
    Resource<11> => "resource11", MemId::Resource(11);
    Resource<12> => "resource12", MemId::Resource(12);
    Resource<13> => "resource13", MemId::Resource(13);
    Resource<14> => "resource14", MemId::Resource(14);
    Resource<15> => "resource15", MemId::Resource(15);
}

/// Defines the entry point `aqr_apply_whole` or `aqr_apply_part` as `$name::apply`.
///
/// The arguments are the lengths, in order, of the parameters, of each resource and of each input,
/// and the declared output size, so their number fixes the numbers of the resources and the inputs.
/// Natively, `$name::ENTRY` is the entry point for [`emu::apply`].
///
/// ```ignore
/// aqr_api::entry! {
///     whole fn copy(params_len, in0_len, size) {
///         Output::copy_from::<Input<0>>(0, 0, in0_len);
///     }
/// }
/// ```
#[macro_export]
macro_rules! entry {
    (whole fn $name:ident($($arg:ident),* $(,)?) $body:block) => {
        $crate::entry!(@ "aqr_apply_whole", Whole, $name, ($($arg),*), $body);
    };
    (part fn $name:ident($($arg:ident),* $(,)?) $body:block) => {
        $crate::entry!(@ "aqr_apply_part", Part, $name, ($($arg),*), $body);
    };
    (@ $export:literal, $kind:ident, $name:ident, ($($arg:ident),*), $body:block) => {
        pub mod $name {
            #[allow(unused_imports)]
            use super::*;

            #[cfg(target_family = "wasm")]
            #[unsafe(export_name = $export)]
            pub extern "C" fn apply($($arg: u64),*) $body

            // Natively a Rust function, so that a trap (panic) unwinds into `emu::apply`.
            #[cfg(not(target_family = "wasm"))]
            pub fn apply($($arg: u64),*) $body

            #[cfg(not(target_family = "wasm"))]
            pub const ENTRY: $crate::emu::Entry = $crate::emu::Entry {
                kind: $crate::emu::Kind::$kind,
                arity: [$(stringify!($arg)),*].len(),
                call: |args| {
                    let &[$($arg),*] = args else { unreachable!() };
                    apply($($arg),*)
                },
            };
        }
    };
}
