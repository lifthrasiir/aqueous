//! The code of the modules (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm modules.

use aqr_api::stack::{block, whole};

use crate::Result;

/// The memory plan of `dwarf-apple` ([`aqr_api::stack`]): what it takes beyond its initial
/// memory at most, of `params`, the inputs `inputs` and `len` bytes of output (see
/// [`crate::apple::plan`]). `None` for params or inputs it does not take.
pub fn apple_plan(params: &[u8], inputs: &[&[u8]], len: u64) -> Option<u64> {
    let lens: Vec<u64> = inputs.iter().map(|i| i.len() as u64).collect();
    crate::apple::plan(params, &lens, inputs.get(1)?, inputs.get(2)?, len)
}

/// `dwarf-apple`: an Apple accelerator table of `len` bytes from `params` (its header) and
/// `inputs` (`.debug_str`, then the streams of [`crate::apple::STREAM_NAMES`]).
pub fn apple(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    crate::apple::decode(params, inputs, len)
}

/// The streams of `dwarf-info`'s `inputs`, after `.debug_abbrev` and `.debug_str`, unpacked:
/// all of them (borrowed) in order, as [`crate::info::decode`] takes them.
fn info_inputs<'a>(inputs: &[&'a [u8]]) -> Result<Vec<&'a [u8]>> {
    use crate::info::{INPUTS, NUM_STREAMS};
    let &[abbrev, strs, ref packed @ ..] = inputs else { return Err(crate::corrupt()) };
    if packed.len() != INPUTS.len() {
        return Err(crate::corrupt());
    }
    let mut all: Vec<&[u8]> = vec![&[]; 2 + NUM_STREAMS];
    (all[0], all[1]) = (abbrev, strs);
    for (input, (_, which)) in packed.iter().zip(INPUTS) {
        for (s, &k) in crate::unpack(input, which.len())?.into_iter().zip(which) {
            all[2 + k] = s;
        }
    }
    Ok(all)
}

/// The memory plan of `dwarf-info` ([`aqr_api::stack`]): what it takes beyond its initial
/// memory at most, of `params`, the inputs `inputs` and `len` bytes of output: what
/// `aqr_api::whole` copies, the streams unpacked (borrowed, a slice of each, and while an input
/// is unpacked, its lengths and slices), and the decoder's ([`crate::info::plan`]). `None` for
/// params or inputs it does not take.
pub fn info_plan(params: &[u8], inputs: &[&[u8]], len: u64) -> Option<u64> {
    use crate::info::{INPUTS, NUM_STREAMS};
    let lens: Vec<u64> = inputs.iter().map(|i| i.len() as u64).collect();
    let all = info_inputs(inputs).ok()?;
    let most = INPUTS.iter().map(|(_, w)| w.len() as u64).max()?;
    let unpacking = block(4 * (most - 1)) + block(8 * most);
    let decoding = crate::info::plan(params, &all, len)?;
    Some(whole(params.len() as u64, &[], &lens) + block(8 * (2 + NUM_STREAMS) as u64) + unpacking.max(decoding))
}

/// `dwarf-info`: `.debug_info` of `len` bytes from `params` ([`crate::info::Shape`]) and `inputs`
/// (`.debug_abbrev`, `.debug_str`, then the inputs of [`crate::info::INPUTS`]).
pub fn info(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    crate::info::decode(params, &info_inputs(inputs)?, len)
}
