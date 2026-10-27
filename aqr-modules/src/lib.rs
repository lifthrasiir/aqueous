//! The modules of aqr-decompress as Wasm modules, built from the recompressors' code by
//! `build.sh` into `wasm/` (a binary of `src/bin` each), with [`KNOWN`] made from them. The
//! modules are kept with their hashes so that archives name them by their bytes, and a
//! decompressor runs one it knows natively.
//!
//! Each binary defines the entry point with [`whole!`], on all the bytes of the memories.

#![cfg_attr(target_family = "wasm", no_std)]

#[cfg(not(target_family = "wasm"))]
mod known;

#[cfg(not(target_family = "wasm"))]
pub use known::KNOWN;

/// A module built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Known {
    pub name: &'static str,
    pub version: u16,
    /// SHA-512/256 of [`Known::wasm`], as the spec hashes.
    pub sha512_256: [u8; 32],
    pub wasm: &'static [u8],
    /// The data of its module block, made by `aqr_formula::aqr::module_block_data`.
    pub block: &'static [u8],
}

/// The bytes of `s`, hexadecimal digits.
pub const fn hex<const N: usize>(s: &str) -> [u8; N] {
    const fn digit(c: u8) -> u8 {
        match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            _ => panic!("not a hexadecimal digit"),
        }
    }
    let s = s.as_bytes();
    assert!(s.len() == 2 * N);
    let mut out = [0; N];
    let mut i = 0;
    while i < N {
        out[i] = digit(s[2 * i]) << 4 | digit(s[2 * i + 1]);
        i += 1;
    }
    out
}

/// The entry point of a whole module running `$f`, code taking byte slices (see
/// `aqr_api::whole`), on the parameters, the resources and the inputs: each resource and input
/// named with its index, as `inputs [data: 0, layout: 1]`.
#[macro_export]
macro_rules! whole {
    ($(resources [$($r:ident: $ri:literal),* $(,)?])? $(inputs [$($i:ident: $ii:literal),* $(,)?])?, $f:expr) => {
        aqr_api::entry! {
            whole fn apply(params_len, $($($r,)*)? $($($i,)*)? size) {
                let res: &[aqr_api::Mem] = &[$($(aqr_api::Mem::new::<aqr_api::Resource<$ri>>($r)),*)?];
                let ins: &[aqr_api::Mem] = &[$($(aqr_api::Mem::new::<aqr_api::Input<$ii>>($i)),*)?];
                let params = aqr_api::Mem::new::<aqr_api::Params>(params_len);
                aqr_api::finish(aqr_api::whole(&params, res, ins, &mut aqr_api::Out::new(size), $f));
            }
        }
    };
}
