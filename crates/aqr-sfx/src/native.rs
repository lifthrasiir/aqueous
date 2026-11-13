//! The native module blobs (aqr-dll, `aqr_api::dll`'s interface) merged into a self-extracting
//! program, and how an archive's shorthand module blocks reach them: [`Runner::registered`] gives
//! the decoder a stand-in Wasm module for a hash ([`stand_in`]), of no code but the module's task
//! slots (what the decoder reads of a module's bytes) and the hash, which [`Runner::load`] then
//! takes to the native blob.
//!
//! [`Runner::registered`]: aqr_formula::aqr::Runner::registered
//! [`Runner::load`]: aqr_formula::aqr::Runner::load

use aqr_api::dll::{ApplyFn, ApplyPartFn, DecodeFn, Slice, SliceMut, StreamedFn};
use aqr_formula::Resource;
use aqr_formula::aqr::{Decoded, Failure, Loaded, PartIn, PartOut};

/// The name in the dispatch table of the native blob of the module of SHA-512/256 `hash` and
/// task slots `slots` (the bytes of its custom section `aqr_task_slots`): both in hex,
/// `HASH/SLOTS`.
pub fn native_name(hash: &[u8; 32], slots: &[u8]) -> String {
    format!("{}/{}", hex(hash), hex(slots))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The hash and task slots of [`native_name`] `name`, if it is one.
#[cfg(any(windows, test))]
pub(crate) fn parse_native_name(name: &[u8]) -> Option<([u8; 32], Vec<u8>)> {
    let (hash, slots) = std::str::from_utf8(name).ok()?.split_once('/')?;
    let unhex = |h: &str| -> Option<Vec<u8>> {
        if !h.len().is_multiple_of(2) {
            return None;
        }
        (0..h.len()).step_by(2).map(|i| u8::from_str_radix(h.get(i..i + 2)?, 16).ok()).collect()
    };
    Some((unhex(hash)?.try_into().ok()?, unhex(slots)?))
}

/// The custom section that names the native blob of a stand-in module ([`stand_in`]).
const NATIVE_SECTION: &str = "aqr_sfx_native";

/// The stand-in Wasm module of the native blob of the module of `hash`: no code; its task slots
/// (where it has any) and the hash, in custom sections.
pub(crate) fn stand_in(hash: &[u8; 32], slots: &[u8]) -> Vec<u8> {
    fn custom(w: &mut Vec<u8>, name: &str, payload: &[u8]) {
        let mut s = vec![name.len() as u8];
        s.extend_from_slice(name.as_bytes());
        s.extend_from_slice(payload);
        w.push(0);
        let mut n = s.len();
        loop {
            let b = (n & 0x7f) as u8;
            n >>= 7;
            w.push(if n == 0 { b } else { b | 0x80 });
            if n == 0 {
                break;
            }
        }
        w.extend_from_slice(&s);
    }
    let mut w = b"\0asm\x01\0\0\0".to_vec();
    if !slots.is_empty() {
        custom(&mut w, "aqr_task_slots", slots);
    }
    custom(&mut w, NATIVE_SECTION, hash);
    w
}

/// The hash a stand-in module ([`stand_in`]) names, if `wasm` is one.
pub(crate) fn stand_in_hash(wasm: &[u8]) -> Option<[u8; 32]> {
    if !wasm.starts_with(b"\0asm") {
        return None;
    }
    let leb = |at: &mut usize| -> Option<usize> {
        let mut n = 0usize;
        for shift in (0..35).step_by(7) {
            let b = *wasm.get(*at)?;
            *at += 1;
            n |= usize::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Some(n);
            }
        }
        None
    };
    let mut at = 8;
    while at < wasm.len() {
        let id = wasm[at];
        at += 1;
        let len = leb(&mut at)?;
        let body = wasm.get(at..at.checked_add(len)?)?;
        if id != 0 {
            return None; // (a stand-in has custom sections alone)
        }
        let (&n, rest) = body.split_first()?;
        if rest.get(..usize::from(n))? == NATIVE_SECTION.as_bytes() {
            return rest[usize::from(n)..].try_into().ok();
        }
        at += len;
    }
    None
}

/// A native module blob merged: its module's hash and task slots, and its functions.
pub(crate) struct Native {
    pub hash: [u8; 32],
    pub slots: Vec<u8>,
    pub fns: Fns,
}

/// What a native module blob's `aqr_dll_info` tells of it ([`aqr_api::dll::Info`]).
#[derive(Clone, Copy)]
pub(crate) struct Fns {
    pub apply: ApplyFn,
    /// Its `aqr_dll_apply_part`, for a part module.
    pub apply_part: Option<ApplyPartFn>,
    /// How it runs natively (`Info::run`), whether it takes resources natively, its native
    /// decoding of whole inputs and, for `RUN_STREAMED`, when an application is streamed.
    pub run: u32,
    pub resources: bool,
    pub decode: Option<DecodeFn>,
    pub streamed: Option<StreamedFn>,
}

/// The most bytes of inputs and outputs of a part module's application gathered to be applied
/// whole by its native decoding (on threads) rather than streamed: the stub has no plans of the
/// modules' memory to judge by, as aqr-decompress has.
const GATHERED: u64 = 256 << 20;

/// A module of a native blob, run as aqr-decompress runs it natively: by its native decoding of
/// whole inputs (on `threads`) where it has one; a part module streamed where its inputs and
/// outputs are long (more than [`GATHERED`], or as `Info::streamed` says), its inputs pulled and
/// its outputs pushed as the module requests and commits them; by the emulation of its entry
/// point otherwise. The limits of its memory are not enforced (it is the module's own code,
/// run natively).
pub(crate) struct NativeLoaded {
    pub native: Fns,
    pub threads: usize,
}

impl NativeLoaded {
    /// Its native decoding of `inputs` into one output of `size` bytes.
    fn decode(
        &self,
        decode: DecodeFn,
        params: &[u8],
        resources: &[Resource],
        inputs: &[&[u8]],
        size: u64,
    ) -> Decoded<Vec<u8>> {
        // (a module that takes no resources natively is given none)
        let res: Vec<Slice> = match self.native.resources {
            true => resources.iter().map(|r| Slice::of(r)).collect(),
            false => Vec::new(),
        };
        let ins: Vec<Slice> = inputs.iter().map(|i| Slice::of(i)).collect();
        let size = usize::try_from(size).map_err(|_| Failure::Resource("an output too long".into()))?;
        let mut out = vec![0u8; size];
        let mut error = [0u8; 1024];
        let mut len = 0;
        // SAFETY: each pointer is to as many items as its count, the output apart
        let r = unsafe {
            decode(
                Slice::of(params),
                res.as_ptr(),
                res.len(),
                ins.as_ptr(),
                ins.len(),
                SliceMut::of(&mut out),
                self.threads,
                SliceMut::of(&mut error),
                &mut len,
            )
        };
        match r {
            0 => Ok(out),
            _ => Err(aqr_blob::interp::failure_of(&error[..len])),
        }
    }

    /// Whether an application of `inputs` and outputs of `sizes` is streamed rather than its
    /// inputs gathered and natively decoded.
    fn streams(&self, inputs: &[&mut dyn PartIn], sizes: &[u64]) -> bool {
        let lens: Vec<u64> = inputs.iter().map(|i| i.len()).collect();
        if self.native.decode.is_none() || sizes.len() != 1 {
            return true;
        }
        match (self.native.run, self.native.streamed) {
            // SAFETY: the lengths, as many as given
            (aqr_api::dll::RUN_STREAMED, Some(f)) => unsafe { f(lens.as_ptr(), lens.len(), sizes[0]) != 0 },
            _ => lens.iter().chain(sizes).fold(0u64, |a, &n| a.saturating_add(n)) > GATHERED,
        }
    }
}

impl Loaded for NativeLoaded {
    fn apply(
        &self,
        params: &[u8],
        resources: &[Resource],
        inputs: &[&[u8]],
        sizes: &[u64],
        _memory_limit: u64,
        _task_limits: &[u64],
    ) -> Decoded<Vec<Vec<u8>>> {
        if let (Some(decode), [size]) = (self.native.decode, sizes) {
            return Ok(vec![self.decode(decode, params, resources, inputs, *size)?]);
        }
        let res: Vec<Slice> = resources.iter().map(|r| Slice::of(r)).collect();
        let ins: Vec<Slice> = inputs.iter().map(|i| Slice::of(i)).collect();
        let mut outs: Vec<Vec<u8>> = sizes.iter().map(|&s| vec![0u8; s as usize]).collect();
        let os: Vec<SliceMut> = outs.iter_mut().map(|o| SliceMut::of(o)).collect();
        let mut error = [0u8; 1024];
        let mut len = 0;
        // SAFETY: each pointer is to as many items as its count, the outputs apart
        let r = unsafe {
            (self.native.apply)(
                Slice::of(params),
                res.as_ptr(),
                res.len(),
                ins.as_ptr(),
                ins.len(),
                os.as_ptr(),
                os.len(),
                SliceMut::of(&mut error),
                &mut len,
            )
        };
        match r {
            0 => Ok(outs),
            _ => Err(aqr_blob::interp::failure_of(&error[..len])),
        }
    }

    fn is_part(&self) -> bool {
        // (a threaded one, and a whole one, are applied whole, as aqr-decompress has them)
        self.native.apply_part.is_some()
            && matches!(self.native.run, aqr_api::dll::RUN_PART | aqr_api::dll::RUN_STREAMED)
    }

    fn apply_part(
        &self,
        params: &[u8],
        resources: &[Resource],
        inputs: &mut [&mut dyn PartIn],
        outputs: &mut [&mut dyn PartOut],
        sizes: &[u64],
        _memory_limit: u64,
        _task_limits: &[u64],
    ) -> Decoded<()> {
        if sizes.len() != outputs.len() {
            return Err(Failure::Invalid("outputs of other sizes than declared".into()));
        }
        if !self.streams(inputs, sizes) {
            // the inputs gathered, decoded natively, the output pushed
            let mut gathered = Vec::with_capacity(inputs.len());
            for i in inputs.iter_mut() {
                let len = i.len();
                let mut d = Vec::with_capacity(len as usize);
                i.pull(len, &mut |b: &[u8]| d.extend_from_slice(b))?;
                gathered.push(d);
            }
            let ins: Vec<&[u8]> = gathered.iter().map(Vec::as_slice).collect();
            let decode = self.native.decode.expect("streamed without one");
            let out = self.decode(decode, params, resources, &ins, sizes[0])?;
            return outputs[0].push(&out);
        }
        let apply_part = self.native.apply_part.ok_or_else(|| Failure::Invalid("not a part module".into()))?;
        let res: Vec<Slice> = resources.iter().map(|r| Slice::of(r)).collect();
        let mut failed = None;
        let (in_c, out_c, _ctx) = aqr_blob::interp::part_c(inputs, outputs, &mut failed);
        let mut error = [0u8; 1024];
        let mut len = 0;
        // SAFETY: the inputs and outputs live across the call, their callbacks driven from here
        let r = unsafe {
            apply_part(
                Slice::of(params),
                res.as_ptr(),
                res.len(),
                in_c.as_ptr(),
                in_c.len(),
                out_c.as_ptr(),
                out_c.len(),
                sizes.as_ptr(),
                SliceMut::of(&mut error),
                &mut len,
            )
        };
        match (r, failed) {
            (0, _) => Ok(()),
            // (an input's or an output's own failure, rather than the module's of it)
            (_, Some(f)) => Err(f),
            _ => Err(aqr_blob::interp::failure_of(&error[..len])),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_stand_ins() {
        let hash: [u8; 32] = std::array::from_fn(|i| i as u8 * 7);
        for slots in [&[][..], &[4, 0x82]] {
            assert_eq!(parse_native_name(native_name(&hash, slots).as_bytes()), Some((hash, slots.to_vec())));
            let w = stand_in(&hash, slots);
            assert_eq!(stand_in_hash(&w), Some(hash));
            assert_eq!(aqr_formula::aqr::class_slots(&w).len(), slots.len());
        }
        assert_eq!(parse_native_name(b"aqr-interp"), None);
        assert_eq!(stand_in_hash(b"\0asm\x01\0\0\0\x01\x01\x00"), None);
    }
}
