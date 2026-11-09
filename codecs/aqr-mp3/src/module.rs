//! The code of the module (see `formula`, and FORMAT.md), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use crate::codec::{self, N_BYTE_STREAMS};
use crate::par;

#[derive(Debug)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Slots: the eight byte streams of the coded form, then the rANS words (u32 LE) and the raw
/// bits.
pub(crate) const SLOTS: usize = N_BYTE_STREAMS + 2;

/// The module: the file of `len` bytes rebuilt from the ten `inputs` and the `resources` (the
/// tables), with `threads` threads (the same output whatever the number).
pub fn decode(
    params: &[u8],
    resources: &[&[u8]],
    inputs: &[&[u8]],
    len: u64,
    threads: usize,
) -> Result<Vec<u8>, Error> {
    let corrupt = |m: &str| Error(format!("corrupt: {m}"));
    if !params.is_empty() {
        return Err(corrupt("params"));
    }
    let [res] = resources else { return Err(corrupt("resources")) };
    // The built-in tables, built once for the process, unless the resource is another (or
    // none is built in)
    let own;
    let t = if cfg!(feature = "encode") && *res == &codec::builtin_raw()[..] {
        codec::tables()
    } else {
        own = codec::tables_from(res).ok_or_else(|| corrupt("tables"))?;
        // (the decoder's tables now, not at the first symbol: the plan has them here)
        own.prepare_dec(1);
        &own
    };
    let &[.., words, raw] = inputs else { return Err(corrupt("inputs")) };
    if inputs.len() != SLOTS || words.len() % 4 != 0 {
        return Err(corrupt("inputs"));
    }
    let total: usize = inputs.iter().map(|s| s.len()).sum();
    let streams: [&[u8]; N_BYTE_STREAMS] = std::array::from_fn(|i| inputs[i]);
    let out = if threads <= 1 || total < par::PAR_MIN {
        codec::decode_parts(t, len, &streams, words, raw).map_err(|e| corrupt(e.0))?
    } else {
        par::decode(t, len, &streams, words, raw, threads).map_err(|e| corrupt(e.0))?
    };
    if out.len() as u64 != len {
        return Err(corrupt("length"));
    }
    Ok(out)
}

/// Pages of the windows of the module as a part module ([`decode_part`]): of each input and of
/// the output.
pub const WINDOW: u32 = 16;

/// The module as a part module (its entry point, of `aqr_api::entry!`): what [`decode`] gives, its
/// inputs taken and its output put in order through windows ([`WINDOW`]), in pieces: it holds
/// neither, whatever their lengths.
pub fn decode_part(params_len: u64, res_len: u64, lens: [u64; SLOTS], size: u64) {
    aqr_api::finish(part(params_len, res_len, lens, size));
}

fn part(params_len: u64, res_len: u64, lens: [u64; SLOTS], size: u64) -> Result<(), Error> {
    use aqr_api::stream::{In, Pull, Written};
    use aqr_api::{Memory, Resource};
    let corrupt = |m: &str| Error(format!("corrupt: {m}"));
    if params_len != 0 {
        return Err(corrupt("params"));
    }
    if !lens[N_BYTE_STREAMS].is_multiple_of(4) {
        return Err(corrupt("inputs"));
    }
    // (the reservoir's positions are in bits)
    if usize::try_from(size.saturating_mul(8)).is_err() {
        return Err(corrupt("length"));
    }
    let mut raw = vec![0; usize::try_from(res_len).map_err(|_| corrupt("resources"))?];
    Resource::<0>::read(0, &mut raw);
    let t = codec::tables_from(&raw).ok_or_else(|| corrupt("tables"))?;
    // (the decoder's tables now, not at the first symbol: the plan has them here)
    t.prepare_dec(1);
    let (mut i0, mut i1, mut i2, mut i3, mut i4) = (
        In::<0>::new(lens[0]),
        In::<1>::new(lens[1]),
        In::<2>::new(lens[2]),
        In::<3>::new(lens[3]),
        In::<4>::new(lens[4]),
    );
    let (mut i5, mut i6, mut i7, mut i8, mut i9) = (
        In::<5>::new(lens[5]),
        In::<6>::new(lens[6]),
        In::<7>::new(lens[7]),
        In::<8>::new(lens[8]),
        In::<9>::new(lens[9]),
    );
    let inputs: [(&mut dyn Pull, u64); SLOTS] = [
        (&mut i0, lens[0]),
        (&mut i1, lens[1]),
        (&mut i2, lens[2]),
        (&mut i3, lens[3]),
        (&mut i4, lens[4]),
        (&mut i5, lens[5]),
        (&mut i6, lens[6]),
        (&mut i7, lens[7]),
        (&mut i8, lens[8]),
        (&mut i9, lens[9]),
    ];
    let mut out = Written::new(size);
    crate::part::run(&t, inputs, &mut out, size).map_err(|e| corrupt(e.0))
}

/// Bytes of the structures the module holds, on WebAssembly (32-bit pointers; checked there by
/// assertions): what its memory plan ([`crate::plan`]) is made of.
pub mod wasm {
    /// A frame of the queue of `crate::part` (header, side information, the payload's place and
    /// length, and the junk before it; checked there), and the models of the main pass and of
    /// the side information (boxed).
    pub const REC: u64 = crate::part::REC;
    pub const STATE: u64 = 6048;
    pub const SIDE_MODEL: u64 = 18806;
}

// (the sizes of the structures on WebAssembly, which the memory plan uses)
#[cfg(target_family = "wasm")]
const _: () = {
    assert!(size_of::<crate::model::State>() == wasm::STATE as usize);
    assert!(size_of::<crate::side::SideModel>() == wasm::SIDE_MODEL as usize);
};
