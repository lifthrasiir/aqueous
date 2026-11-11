//! InstallShield cabinets (`ISc(`, InstallShield 5 on: `data1.cab`, `data2.cab`, ... beside a
//! `data1.hdr`) taken apart into the contents of their files, the DEFLATE chunks of which are
//! rebuilt by aqr-flate's module; `FORMAT.md` tells what is taken. The formula is the cabinet's
//! bytes in order, the chunks the outputs of an application of `flate-set` split, so this crate
//! has no module of its own.

pub mod formula;
pub mod isc;
