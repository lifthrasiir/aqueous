//! The code of the module (see `formula`, and FORMAT.md), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use crate::codec::{self, Coded, N_BYTE_STREAMS, Rd, Room};
use aqr_raw::audio::frame::{MAX_BLOCK, MAX_CHANNELS};
use aqr_raw::audio::residual;
use std::borrow::Cow;

#[derive(Debug)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// The task slots of the module (aqr-modules' `flac-recons`), which it is given as its threads.
pub const SLOTS: usize = 8;

/// The params of frames of `room`: its samples a channel, channels and bytes, varints.
pub fn put_params(room: &Room) -> Vec<u8> {
    let mut p = Vec::new();
    [room.block, room.channels, room.frame].iter().for_each(|&v| aqr_common::varint::put(&mut p, v as u64));
    p
}

/// The room that params `p` give, of a file of `len` bytes.
pub fn params(p: &[u8], len: u64) -> Result<Room> {
    let bad = || Error("corrupt: params".into());
    let mut r = Rd::new(p);
    let mut get = || r.uv().ok().and_then(|v| usize::try_from(v).ok()).ok_or_else(bad);
    let room = Room { block: get()?, channels: get()?, frame: get()? };
    drop(get);
    let ok = r.at_end() && room.block <= MAX_BLOCK && room.channels <= MAX_CHANNELS && room.frame as u64 <= len;
    ok.then_some(room).ok_or_else(bad)
}

/// The memory plan of the module ([`aqr_api::stack`]): the limits of its instance and its tasks,
/// its initial memory (`initial` bytes) with it, of params `params`, resources and inputs
/// of these lengths and a file of `len` bytes (`None` where they are not so): what
/// `aqr_api::whole` copies, the tables, the streams listed, and the pipeline
/// ([`codec::plan`]); and what its channels' messages (in the
/// instance's) may hold.
#[cfg(feature = "encode")]
pub fn plan(params: &[u8], resources: &[u64], inputs: &[u64], len: u64, initial: u64) -> Option<((u64, u64), u64)> {
    use aqr_api::stack::block;
    let room = self::params(params, len).ok()?;
    let &[.., words, raw0, raw1] = inputs else { return None };
    let held = aqr_api::stack::whole(params.len() as u64, resources, inputs);
    let top = initial + held + residual::tables_plan(block) + block(12 * N_BYTE_STREAMS as u64);
    Some(codec::plan(len, room, words, [raw0, raw1], top, SLOTS))
}

/// The module: the file of `len` bytes from `params` (what its frames take), `resources` (the
/// tables) and `inputs` (the streams).
pub fn decode(params: &[u8], resources: &[&[u8]], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let corrupt = |s: &str| Error(format!("corrupt: {s}"));
    let room = self::params(params, len)?;
    let [res] = resources else { return Err(corrupt("resources")) };
    let t = residual::tables_from(res).ok_or_else(|| corrupt("tables"))?;
    let [streams @ .., words, raw0, raw1] = inputs else { return Err(corrupt("inputs")) };
    if streams.len() != N_BYTE_STREAMS || words.len() % 4 != 0 {
        return Err(corrupt("inputs"));
    }
    // No output is larger than 64 times its coded form.
    let coded_len = inputs.iter().map(|i| i.len() as u64).sum::<u64>();
    if len > coded_len.saturating_mul(64) + (1 << 20) {
        return Err(Error("length".into()));
    }
    // (the inputs as they are: whatever is in memory when the tasks are spawned is copied)
    let c = Coded {
        streams: streams.iter().map(|&s| Cow::Borrowed(s)).collect(),
        words: Cow::Borrowed(words),
        raw: [Cow::Borrowed(raw0), Cow::Borrowed(raw1)],
    };
    let mut out = Vec::new();
    codec::decode_threads(&c, &t, len as usize, &mut out, room, threads).map_err(|e| corrupt(e.0))?;
    if out.len() as u64 != len {
        return Err(corrupt("length"));
    }
    Ok(out)
}
