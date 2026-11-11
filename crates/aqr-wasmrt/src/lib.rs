//! Runs the Wasm modules of aqr natively, by Wasmtime: [`wasm::load`] makes a module ready to
//! apply as the spec's "apply a module" does, with the memory limits enforced. With the feature
//! `sandbox`, [`sandbox::Sandbox`] runs a module in a worker process the system locks down.
//!
//! aqr-decompress uses this to run modules it does not know natively, and the self-extracting
//! interpreter blob uses the same code; neither pulls in the native codecs.

#[cfg(feature = "wasm")]
mod cow;
#[cfg(feature = "wasm")]
mod exec;
#[cfg(feature = "sandbox")]
pub mod sandbox;
#[cfg(feature = "wasm")]
pub mod wasm;
