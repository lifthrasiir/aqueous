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
//! A module defines its entry point with [`entry!`], and takes a list of byte sequences as one
//! input with [`Packed`].
//!
//! The code of a module that also runs natively as a library takes the memories as values,
//! [`Src`] and [`Dst`]: byte slices natively ([`Bytes`], [`BytesMut`]), and [`Mem`] and [`Out`] that the entry point makes
//! within the module, reporting an error with [`finish`].

#![cfg_attr(target_family = "wasm", no_std)]

#[cfg(feature = "alloc")]
extern crate alloc;

#[cfg(feature = "alloc")]
pub mod chan;
#[cfg(not(target_family = "wasm"))]
pub mod emu;
mod io;
mod packed;
#[cfg(feature = "alloc")]
pub mod task;
#[cfg(target_family = "wasm")]
#[doc(hidden)]
pub mod wasm;
#[cfg(not(target_family = "wasm"))]
use emu::MemId;

pub use io::{Bytes, BytesMut, Dst, Mem, Out, Src, finish};
#[cfg(feature = "alloc")]
pub use io::{load, whole};
pub use packed::Packed;

mod sealed {
    pub trait Sealed {}
}

/// A memory other than the working memory.
pub trait Memory: sealed::Sealed {
    /// The size in bytes, a multiple of 65536. Fixed during an application.
    fn size() -> u64;
    /// Copies `dst.len()` bytes from offset `src` into `dst`.
    fn read(src: u64, dst: &mut [u8]);
}

/// A memory that the `J`-th output can be copied into from ([`Output::copy_from`]): any.
#[doc(hidden)]
pub trait CopyTo<const J: u32>: Memory {
    fn copy_to(dst: u64, src: u64, len: u64);
}

/// The `J`-th output, which can be written ([`Output::write`]).
#[doc(hidden)]
pub trait Writable: Memory {
    fn write(dst: u64, src: &[u8]);
}

/// The memory of the parameters (`aqr_params`).
pub struct Params;
/// The memory of the `J`-th output (`aqr_output`J), for `J` < 16.
pub struct Output<const J: u32>;
/// The memory of the `K`-th input (`aqr_input`K), for `K` < 64.
pub struct Input<const K: u32>;
/// The memory of the `I`-th resource (`aqr_resource`I), for `I` < 16.
pub struct Resource<const I: u32>;

impl<const J: u32> Output<J>
where
    Self: Writable,
{
    /// Copies `src` to offset `dst`.
    #[inline]
    pub fn write(dst: u64, src: &[u8]) {
        <Self as Writable>::write(dst, src)
    }

    /// Copies `len` bytes from offset `src` of `M` to offset `dst`, without the working memory.
    #[inline]
    pub fn copy_from<M: CopyTo<J>>(dst: u64, src: u64, len: u64) {
        M::copy_to(dst, src, len)
    }

    /// Appends the bytes up to the offset `hi` of the output (`aqr_commit`). Part modules only.
    #[inline]
    pub fn commit(hi: u64) {
        sys::commit(J, hi)
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

#[cfg(not(target_family = "wasm"))]
use emu::sys;
#[cfg(target_family = "wasm")]
use wasm as sys;

/// Every memory, and every copy into an output from one of them.
macro_rules! memories {
    ($($ty:ty => $name:literal, $id:expr;)*) => {
        $(
            impl sealed::Sealed for $ty {}
            sys::memory!($ty, $name, $id);
        )*
        copies!([$($ty => $name, $id;)*] 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15);
    };
}

macro_rules! copies {
    (@to $j:literal [$($ty:ty => $name:literal, $id:expr;)*]) => {$(
        sys::copy!($j, $ty, $name, $id);
    )*};
    ($mems:tt $($j:literal)*) => {$(
        sys::output!($j);
        copies!(@to $j $mems);
    )*};
}

memories! {
    Params => "params", MemId::Params;
    Output<0> => "output0", MemId::Output(0);
    Output<1> => "output1", MemId::Output(1);
    Output<2> => "output2", MemId::Output(2);
    Output<3> => "output3", MemId::Output(3);
    Output<4> => "output4", MemId::Output(4);
    Output<5> => "output5", MemId::Output(5);
    Output<6> => "output6", MemId::Output(6);
    Output<7> => "output7", MemId::Output(7);
    Output<8> => "output8", MemId::Output(8);
    Output<9> => "output9", MemId::Output(9);
    Output<10> => "output10", MemId::Output(10);
    Output<11> => "output11", MemId::Output(11);
    Output<12> => "output12", MemId::Output(12);
    Output<13> => "output13", MemId::Output(13);
    Output<14> => "output14", MemId::Output(14);
    Output<15> => "output15", MemId::Output(15);
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
    Input<16> => "input16", MemId::Input(16);
    Input<17> => "input17", MemId::Input(17);
    Input<18> => "input18", MemId::Input(18);
    Input<19> => "input19", MemId::Input(19);
    Input<20> => "input20", MemId::Input(20);
    Input<21> => "input21", MemId::Input(21);
    Input<22> => "input22", MemId::Input(22);
    Input<23> => "input23", MemId::Input(23);
    Input<24> => "input24", MemId::Input(24);
    Input<25> => "input25", MemId::Input(25);
    Input<26> => "input26", MemId::Input(26);
    Input<27> => "input27", MemId::Input(27);
    Input<28> => "input28", MemId::Input(28);
    Input<29> => "input29", MemId::Input(29);
    Input<30> => "input30", MemId::Input(30);
    Input<31> => "input31", MemId::Input(31);
    Input<32> => "input32", MemId::Input(32);
    Input<33> => "input33", MemId::Input(33);
    Input<34> => "input34", MemId::Input(34);
    Input<35> => "input35", MemId::Input(35);
    Input<36> => "input36", MemId::Input(36);
    Input<37> => "input37", MemId::Input(37);
    Input<38> => "input38", MemId::Input(38);
    Input<39> => "input39", MemId::Input(39);
    Input<40> => "input40", MemId::Input(40);
    Input<41> => "input41", MemId::Input(41);
    Input<42> => "input42", MemId::Input(42);
    Input<43> => "input43", MemId::Input(43);
    Input<44> => "input44", MemId::Input(44);
    Input<45> => "input45", MemId::Input(45);
    Input<46> => "input46", MemId::Input(46);
    Input<47> => "input47", MemId::Input(47);
    Input<48> => "input48", MemId::Input(48);
    Input<49> => "input49", MemId::Input(49);
    Input<50> => "input50", MemId::Input(50);
    Input<51> => "input51", MemId::Input(51);
    Input<52> => "input52", MemId::Input(52);
    Input<53> => "input53", MemId::Input(53);
    Input<54> => "input54", MemId::Input(54);
    Input<55> => "input55", MemId::Input(55);
    Input<56> => "input56", MemId::Input(56);
    Input<57> => "input57", MemId::Input(57);
    Input<58> => "input58", MemId::Input(58);
    Input<59> => "input59", MemId::Input(59);
    Input<60> => "input60", MemId::Input(60);
    Input<61> => "input61", MemId::Input(61);
    Input<62> => "input62", MemId::Input(62);
    Input<63> => "input63", MemId::Input(63);
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
/// and the declared size of each output, so their number fixes the numbers of the resources, the
/// inputs and the outputs together (the module tells them apart).
/// Natively, `$name::ENTRY` is the entry point for [`emu::apply`].
///
/// ```ignore
/// aqr_api::entry! {
///     whole fn copy(params_len, in0_len, size) {
///         Output::<0>::copy_from::<Input<0>>(0, 0, in0_len);
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

            // The body of the tasks' entry point `aqr_task`, which aqr-ld makes of it (and
            // drops where the module spawns none).
            #[cfg(target_family = "wasm")]
            #[unsafe(export_name = "aqr_task_body")]
            extern "C" fn task(rec: *const u8, i: u32) {
                unsafe { $crate::wasm::task_body(rec, i) }
            }

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
