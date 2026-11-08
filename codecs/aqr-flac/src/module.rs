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

/// The tasks of the module's one gang (aqr-modules' `flac-recons`): the LMS stage and the
/// workers.
pub const SLOTS: usize = 8;

/// Pages of the windows of the module as a part module ([`decode_part`]): of each input and of
/// the output.
pub const WINDOW: u32 = 16;

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

/// The memory plan of the module as a part module ([`aqr_api::stack`]): the limits of its
/// instance and its tasks, its initial memory (`initial` bytes) with it, of params `params`,
/// resources and inputs of these lengths and a file of `len` bytes (`None` where they are not
/// so): the tables, the pieces of the streams held ahead, and the pipeline ([`codec::plan`]); and
/// what its channels' messages (in the instance's) may hold. Whatever the lengths of the inputs
/// and the file.
#[cfg(feature = "encode")]
pub fn plan(params: &[u8], resources: &[u64], inputs: &[u64], len: u64, initial: u64) -> Option<((u64, u64), u64)> {
    let room = self::params(params, len).ok()?;
    let (&[res], true) = (resources, inputs.len() == N_BYTE_STREAMS + 3) else { return None };
    Some(codec::plan(room, res, initial, SLOTS - 1))
}

/// The module: the file of `len` bytes from `params` (what its frames take), `resources` (the
/// tables) and `inputs` (the streams), whole, on `threads` threads.
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

/// The module as a part module (its entry point, of `aqr_api::entry!`): what [`decode`] gives, its
/// inputs taken and its output put in order through windows ([`WINDOW`]), in pieces: it holds
/// neither, whatever their lengths.
pub fn decode_part(params_len: u64, res_len: u64, lens: [u64; N_BYTE_STREAMS + 3], size: u64) {
    aqr_api::finish(part(params_len, res_len, lens, size));
}

fn part(params_len: u64, res_len: u64, lens: [u64; N_BYTE_STREAMS + 3], size: u64) -> Result<()> {
    use aqr_api::stream::{In, Pull, Written};
    use aqr_api::{Memory, Params, Resource};
    let corrupt = |s: &str| Error(format!("corrupt: {s}"));
    // (three varints at most)
    let mut params = [0; 30];
    let params = params.get_mut(..params_len as usize).ok_or_else(|| corrupt("params"))?;
    Params::read(0, params);
    let room = self::params(params, size)?;
    if lens[N_BYTE_STREAMS] % 4 != 0 {
        return Err(corrupt("inputs"));
    }
    // No output is larger than 64 times its coded form.
    let coded_len = lens.iter().sum::<u64>();
    if size > coded_len.saturating_mul(64) + (1 << 20) {
        return Err(Error("length".into()));
    }
    let (mut i0, mut i1, mut i2) = (In::<0>::new(lens[0]), In::<1>::new(lens[1]), In::<2>::new(lens[2]));
    let (mut i3, mut i4, mut i5) = (In::<3>::new(lens[3]), In::<4>::new(lens[4]), In::<5>::new(lens[5]));
    let (mut i6, mut i7, mut i8) = (In::<6>::new(lens[6]), In::<7>::new(lens[7]), In::<8>::new(lens[8]));
    let mut out = Written::new(size);
    let tables = || {
        let mut raw = vec![0; usize::try_from(res_len).ok()?];
        Resource::<0>::read(0, &mut raw);
        residual::tables_from(&raw)
    };
    let inputs: [(&mut dyn Pull, u64); N_BYTE_STREAMS + 3] = [
        (&mut i0, lens[0]),
        (&mut i1, lens[1]),
        (&mut i2, lens[2]),
        (&mut i3, lens[3]),
        (&mut i4, lens[4]),
        (&mut i5, lens[5]),
        (&mut i6, lens[6]),
        (&mut i7, lens[7]),
        (&mut i8, lens[8]),
    ];
    codec::decode_part(room, size, tables, inputs, &mut out, SLOTS - 1).map_err(|e| corrupt(e.0))
}
