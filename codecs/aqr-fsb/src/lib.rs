//! FMOD's FSB5 sound banks: their layout ([`fsb5`]), split into their samples ([`formula`]),
//! and the Vorbis setup headers they leave out ([`setups`]). FORMAT.md specifies them.

#[cfg(feature = "encode")]
pub mod formula;
pub mod fsb5;
pub mod setups;
