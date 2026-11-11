//! Runs the Wasm modules of aqr natively, by Wasmtime: [`wasm::load`] makes a module ready to
//! apply as the spec's "apply a module" does, with the memory limits enforced. Wasmtime is linked
//! with the feature `wasmtime` (always but on Windows x86-64); else the modules run by the
//! interpreter blob, this crate built with it, which the program carries and gives
//! ([`wasm::set_interpreter`]). With the feature
//! `sandbox`, [`sandbox::Sandbox`] runs a module in a worker process the system locks down.
//!
//! aqr-decompress uses this to run modules it does not know natively, and the self-extracting
//! interpreter blob uses the same code; neither pulls in the native codecs.

#[cfg(feature = "wasmtime")]
mod cow;
#[cfg(feature = "wasmtime")]
mod exec;
#[cfg(feature = "sandbox")]
pub mod sandbox;
#[cfg(feature = "wasm")]
pub mod wasm;
