//! What of aqr-ld other tools use: [`pemerge`], merging native module libraries into a stub
//! executable (the self-extracting archives that `aqr --format sfx` makes).

pub mod pemerge;
mod unwind;
