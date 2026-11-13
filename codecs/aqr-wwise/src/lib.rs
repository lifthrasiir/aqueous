//! Audiokinetic Wwise's Vorbis media (`.wem`): a RIFF WAVE of format `FFFF` whose stream keeps
//! a stripped setup header (its codebooks as ids into a library Wwise's runtime has, the rest
//! of it with some fields left out) and audio packets with their first bits left out.
//!
//! `wem` reads a medium's layout, `stripped` the setup header, as far as rebuilding a Vorbis
//! setup header from codebooks found for its ids.

pub mod stripped;
pub mod wem;
