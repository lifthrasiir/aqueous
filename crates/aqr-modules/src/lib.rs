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
    /// SHA-512/256 of the module ([`Known::read_wasm`]), as the spec hashes.
    pub sha512_256: [u8; 32],
    /// The data of its module block, made by `aqr_formula::aqr::module_block_data`: all that
    /// binaries hold of the module (the module itself would add 3 MB to them).
    pub block: &'static [u8],
}

#[cfg(not(target_family = "wasm"))]
impl Known {
    /// The module, from `wasm/` (for tests and tools run in the source tree).
    pub fn read_wasm(&self) -> std::io::Result<Vec<u8>> {
        std::fs::read(format!("{}/wasm/{}.wasm", env!("CARGO_MANIFEST_DIR"), self.name.replace('/', "-")))
    }
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

/// The content of the custom section `aqr_task_slots` of `n` task slots (the spec, "Tasks"), which
/// [`whole!`]'s `slots` defines: from 1 to 64.
pub const fn task_slots(n: u32) -> [u8; 1] {
    assert!(n >= 1 && n <= 64, "task slots from 1 to 64");
    [n as u8]
}

/// The entry point of a whole module running `$f`, code taking byte slices (see
/// `aqr_api::whole`), on the parameters, the resources and the inputs: each resource and input
/// named with its index, as `inputs [data: 0, layout: 1]`. A module spawning tasks gives its task
/// slots first, as `slots 8,` (the custom section `aqr_task_slots`, which `aqr-ld` keeps only in
/// a module spawning tasks), and `$f` takes them as a fifth argument, the threads of the code.
#[macro_export]
macro_rules! whole {
    (slots $n:literal, $($rest:tt)*) => {
        #[unsafe(link_section = "aqr_task_slots")]
        #[used]
        static AQR_TASK_SLOTS: [u8; 1] = $crate::task_slots($n);
        $crate::whole!(@slots $n, $($rest)*);
    };
    (@slots $n:literal, $(resources [$($r:ident: $ri:literal),* $(,)?])? $(inputs [$($i:ident: $ii:literal),* $(,)?])?, $f:expr) => {
        $crate::whole!($(resources [$($r: $ri),*])? $(inputs [$($i: $ii),*])?, |p, r, i, len| ($f)(p, r, i, len, $n));
    };
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
