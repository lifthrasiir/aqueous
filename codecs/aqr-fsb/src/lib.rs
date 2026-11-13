//! FMOD's FSB5 sound banks: their layout ([`fsb5`]) and split into their samples ([`formula`]),
//! the Vorbis setup headers they leave out found by aqr-vorbis's `setups`. FORMAT.md specifies them.

#[cfg(feature = "encode")]
pub mod formula;
pub mod fsb5;
