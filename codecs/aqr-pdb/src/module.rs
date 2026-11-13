//! The code of the modules (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm modules.

use crate::{Result, corrupt};

/// The memory plan of `pdb-msf` ([`aqr_api::stack`]): what it takes beyond its initial memory at
/// most, of inputs of `inputs` bytes (the second the layout `layout`) and an output of `len`: what
/// `aqr_api::whole` copies, and what decoding takes ([`crate::msf::decode_plan`]).
pub fn msf_plan(inputs: &[u64], layout: &[u8], len: u64) -> Option<u64> {
    Some(aqr_api::stack::whole(0, &[], inputs) + crate::msf::decode_plan(layout, len)?)
}

/// `pdb-msf`: the file of `len` bytes from `params` (none) and `inputs` (those of
/// [`crate::msf::INPUT_NAMES`]).
pub fn msf(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    if !params.is_empty() {
        return Err(corrupt());
    }
    crate::msf::decode(inputs, len)
}

/// The memory plan of `pdb-symbols` ([`aqr_api::stack`]): what it takes beyond its initial
/// memory at most, of params `params`, inputs of `inputs` bytes (the first `head`, the fifth
/// `c13`) and an output of `len` (`None` where they are not so): what `aqr_api::whole` copies,
/// the streams' list, and the inputs' lengths and streams as each unpacks or what decoding takes
/// ([`crate::symbols::decode_plan`]).
pub fn symbols_plan(params: &[u8], inputs: &[u64], head: &[u8], c13: &[u8], len: u64) -> Option<u64> {
    use crate::symbols::{INPUTS, NUM_STREAMS};
    use aqr_api::stack::block;
    let kinds = crate::unpack(head, INPUTS[0].1.len()).ok()?[0];
    let ids = crate::unpack(c13, INPUTS[4].1.len()).ok()?.last()?.len();
    let unpacking =
        INPUTS.iter().map(|(_, w)| block(4 * w.len() as u64) + block(SLICE_BYTES * w.len() as u64)).max()?;
    let decoding = crate::symbols::decode_plan(params, kinds, ids, len)?;
    Some(
        aqr_api::stack::whole(params.len() as u64, &[], inputs)
            + block(SLICE_BYTES * NUM_STREAMS as u64)
            + unpacking.max(decoding),
    )
}

/// Bytes of a slice on WebAssembly.
const SLICE_BYTES: u64 = 8;
#[cfg(target_family = "wasm")]
const _: () = assert!(size_of::<&[u8]>() as u64 == SLICE_BYTES);

/// `pdb-symbols`: the modules' symbol substreams, one after another, from `params` and
/// `inputs` (those of [`crate::symbols::INPUTS`]), `len` bytes in all.
pub fn symbols(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    use crate::symbols::{INPUTS, NUM_STREAMS};
    if inputs.len() != INPUTS.len() {
        return Err(corrupt());
    }
    let mut streams: Vec<&[u8]> = vec![&[]; NUM_STREAMS];
    for (input, (_, which)) in inputs.iter().zip(INPUTS) {
        for (s, &k) in crate::unpack(input, which.len())?.into_iter().zip(which) {
            streams[k] = s;
        }
    }
    let out = crate::symbols::decode_sized(params, &streams, usize::try_from(len).map_err(|_| corrupt())?)?;
    if out.len() as u64 != len {
        return Err(corrupt());
    }
    Ok(out)
}

/// `pdb-types`: the TPI and IPI streams with their hash streams, the outputs of
/// [`crate::types::Config`] one after another, from `params` and `inputs` (the streams of
/// [`crate::types::STREAM_NAMES`]), `len` bytes in all.
pub fn types(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    use crate::types::{self, Config, NUM_INPUTS, NUM_OUTPUTS};
    use aqr_api::stream::Slice;
    let config = Config::get(params)?;
    if inputs.len() != NUM_INPUTS || config.sizes.iter().sum::<u64>() != len {
        return Err(corrupt());
    }
    let mut ins: Vec<Slice> = inputs.iter().map(|i| Slice(i)).collect();
    let mut outs: Vec<Vec<u8>> = config.sizes.iter().map(|&s| Vec::with_capacity(s.min(1 << 30) as usize)).collect();
    {
        let mut i = ins.iter_mut();
        let mut o = outs.iter_mut();
        let ins: [&mut dyn aqr_api::stream::Pull; NUM_INPUTS] =
            std::array::from_fn(|_| i.next().unwrap() as &mut dyn aqr_api::stream::Pull);
        let outs: [&mut dyn aqr_api::stream::Put; NUM_OUTPUTS] =
            std::array::from_fn(|_| o.next().unwrap() as &mut dyn aqr_api::stream::Put);
        types::run(&config, ins, outs)?;
    }
    Ok(outs.concat())
}

/// The pages of the windows of `pdb-types`'s inputs and outputs (as a part module,
/// [`types_part`]).
pub const TYPES_WINDOW: u32 = 1;

/// The `K`-th output of a part module put in order through its window, `size` bytes in all.
struct Sink<const K: u32> {
    at: u64,
    size: u64,
}

impl<const K: u32> aqr_api::stream::Put for Sink<K>
where
    aqr_api::Output<K>: aqr_api::Memory + aqr_api::Writable,
{
    fn put(&mut self, mut d: &[u8]) -> Option<()> {
        use aqr_api::{Memory, Output};
        if d.len() as u64 > self.size - self.at {
            return None;
        }
        let w = Output::<K>::size();
        while !d.is_empty() {
            let k = (d.len() as u64).min(w - self.at % w) as usize;
            Output::<K>::write(self.at % w, &d[..k]);
            self.at += k as u64;
            Output::<K>::commit(self.at);
            d = &d[k..];
        }
        Some(())
    }
}

/// The module `pdb-types` as a part module (its entry point, of `aqr_api::entry!`): what
/// [`types`] gives, its inputs taken and its outputs put in order through windows of
/// [`TYPES_WINDOW`] pages, in pieces: nothing is held, whatever the lengths.
pub fn types_part(params_len: u64, lens: [u64; crate::types::NUM_INPUTS], sizes: [u64; crate::types::NUM_OUTPUTS]) {
    aqr_api::finish(types_stream(params_len, lens, sizes));
}

fn types_stream(
    params_len: u64,
    lens: [u64; crate::types::NUM_INPUTS],
    sizes: [u64; crate::types::NUM_OUTPUTS],
) -> Result<()> {
    use crate::types::{self, Config};
    use aqr_api::{Memory, Params, stream::In};
    // (two bytes and six varints at most)
    let mut params = [0; 2 + 6 * 10];
    let params = params.get_mut(..params_len as usize).ok_or_else(corrupt)?;
    Params::read(0, params);
    let config = Config::get(params)?;
    if config.sizes != sizes {
        return Err(corrupt());
    }
    let mut ins = (
        In::<0>::new(lens[0]),
        In::<1>::new(lens[1]),
        In::<2>::new(lens[2]),
        In::<3>::new(lens[3]),
        In::<4>::new(lens[4]),
        In::<5>::new(lens[5]),
        In::<6>::new(lens[6]),
        In::<7>::new(lens[7]),
        In::<8>::new(lens[8]),
        In::<9>::new(lens[9]),
        In::<10>::new(lens[10]),
        In::<11>::new(lens[11]),
        In::<12>::new(lens[12]),
    );
    let mut outs = (
        Sink::<0> { at: 0, size: sizes[0] },
        Sink::<1> { at: 0, size: sizes[1] },
        Sink::<2> { at: 0, size: sizes[2] },
        Sink::<3> { at: 0, size: sizes[3] },
        Sink::<4> { at: 0, size: sizes[4] },
        Sink::<5> { at: 0, size: sizes[5] },
    );
    types::run(
        &config,
        [
            &mut ins.0,
            &mut ins.1,
            &mut ins.2,
            &mut ins.3,
            &mut ins.4,
            &mut ins.5,
            &mut ins.6,
            &mut ins.7,
            &mut ins.8,
            &mut ins.9,
            &mut ins.10,
            &mut ins.11,
            &mut ins.12,
        ],
        [&mut outs.0, &mut outs.1, &mut outs.2, &mut outs.3, &mut outs.4, &mut outs.5],
    )
}
