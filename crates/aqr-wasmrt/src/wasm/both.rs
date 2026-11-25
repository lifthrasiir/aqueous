//! Both engines linked (features `wasmtime` and `wah`): the modules run by the one that
//! `-Z wasm:engine` picks, Wasmtime by default as the faster of the two (`aqr -d -Z wasm` of 12
//! samples, in the process: 2140 ms, WAH 2225 ms). A sandbox's workers are told the options of
//! `wasm:` set, so they run by the same engine.

use super::*;
use super::{engine as wasmtime, wah_engine as wah};

use aqr_archive::Loaded;
#[cfg(feature = "sandbox")]
use aqr_formula::Resource;

aqr_common::zopt! {
    /// The engine running the modules, of the two linked.
    static ENGINE: Opt<&'static str> =
        Opt::value("wasm:engine", "wasmtime", "the engine running modules, of those linked").choices(&["wasmtime", "wah"]);
}

fn by_wah() -> bool {
    ENGINE.get() == "wah"
}

/// [`wasmtime::warm_up`] or [`wah::warm_up`], of the engine picked.
#[cfg(feature = "sandbox")]
pub fn warm_up() {
    if by_wah() { wah::warm_up() } else { wasmtime::warm_up() }
}

/// `wasm`, of SHA-512/256 `hash`, checked against the profile and compiled by the engine picked
/// (once a process).
pub fn load(wasm: &[u8], hash: [u8; 32]) -> Decoded<Box<dyn Loaded>> {
    if by_wah() { wah::load(wasm, hash) } else { wasmtime::load(wasm, hash) }
}

/// [`load`] with the bound of the application's memories `bound` of its own, for tests.
#[doc(hidden)]
pub fn load_bounded(wasm: &[u8], hash: [u8; 32], bound: u64) -> Decoded<Box<dyn Loaded>> {
    if by_wah() { wah::load_bounded(wasm, hash, bound) } else { wasmtime::load_bounded(wasm, hash, bound) }
}

/// [`load`] of a module as if it were of a release (not kept), for tests of the earlier rules.
#[doc(hidden)]
pub fn load_released(wasm: &[u8]) -> Decoded<Box<dyn Loaded>> {
    if by_wah() { wah::load_released(wasm) } else { wasmtime::load_released(wasm) }
}

/// A module compiled by either engine, as [`compiled`] gives it.
#[cfg(feature = "sandbox")]
#[derive(Clone)]
pub(crate) enum Shared {
    Wasmtime(wasmtime::Shared),
    Wah(wah::Shared),
}

/// [`load`], of the module itself.
#[cfg(feature = "sandbox")]
pub(crate) fn compiled(wasm: &[u8], hash: [u8; 32]) -> Decoded<Shared> {
    Ok(if by_wah() {
        Shared::Wah(wah::compiled(wasm, hash)?)
    } else {
        Shared::Wasmtime(wasmtime::compiled(wasm, hash)?)
    })
}

#[cfg(feature = "sandbox")]
impl Shared {
    /// [`Loaded::apply`] at the `concurrency` chosen.
    pub(crate) fn apply_with(
        &self,
        params: &[u8],
        resources: &[Resource],
        inputs: &[&[u8]],
        sizes: &[u64],
        b: Bounds,
    ) -> Decoded<Vec<Vec<u8>>> {
        match self {
            Shared::Wasmtime(s) => s.apply_with(params, resources, inputs, sizes, b),
            Shared::Wah(s) => s.apply_with(params, resources, inputs, sizes, b),
        }
    }

    /// [`Loaded::apply`], the outputs written into `outs`.
    pub(crate) fn apply_into(
        &self,
        params: &[u8],
        resources: &[Resource],
        inputs: &[&[u8]],
        outs: &mut [&mut [u8]],
        b: Bounds,
    ) -> Decoded<()> {
        match self {
            Shared::Wasmtime(s) => s.apply_into(params, resources, inputs, outs, b),
            Shared::Wah(s) => s.apply_into(params, resources, inputs, outs, b),
        }
    }
}
