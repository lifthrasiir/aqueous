//! What the fuzz targets share: a runner of toy modules, known by their stand-ins
//! ([`aqr_archive::stand_in`]), so that formulas applying modules are evaluated without the
//! codecs (whose own decoding wants targets of its own).

pub mod formula;

use aqr_archive::{Decoded, Failure, Loaded, Natives, PartIn, PartOut, Runner, Wasm, natives};
use aqr_formula::{Apply, Error, Module, Result, Shared};

/// The concatenation of its inputs reversed.
pub const REV: Module = Module::new("rev", 1);
/// The concatenation of its inputs cut into the outputs' sizes.
pub const CUT: Module = Module::new("cut", 1);
/// The concatenation of its inputs, as a part module (streamed).
pub const COPY: Module = Module::new("copy", 1);

/// The most bytes a toy application makes: a declared size over it is the fuzzer's, not a
/// bug of the decoder's (which leaves sizes to the modules' memory limits).
pub const OUTPUT_MAX: u64 = 1 << 24;

/// The largest allocation the targets make (their global allocator, `aqr_testkit::Refusing`,
/// refusing those of it and above as an exhausted heap does): what tries a reservation goes on
/// without it, what does not aborts, which the fuzzer takes for a crash. libFuzzer's own
/// `-malloc_limit_mb` would take a reservation tried for one too.
pub const ALLOCATION_MAX: usize = 1 << 30;

/// The passphrase the seeds of `envelope` are sealed with, and the work factor (scrypt's least,
/// 4 MiB): the target unlocks only that.
pub const PASSPHRASE: &[u8] = b"aqueous";
pub const WORK_FACTOR: u8 = 12;

pub struct Toys;

fn bounded(n: u64) -> Result<()> {
    match n <= OUTPUT_MAX {
        true => Ok(()),
        false => Err(Error::new("output over the toys' bound")),
    }
}

impl aqr_archive::Modules for Toys {
    fn apply(&self, a: &Apply, inputs: &[&[u8]]) -> Result<Vec<u8>> {
        bounded(a.len)?;
        match a.module.name {
            "rev" => Ok(inputs.concat().into_iter().rev().collect()),
            "copy" => Ok(inputs.concat()),
            // (of one output, as the encoder writes it)
            "cut" => {
                let mut d = inputs.concat();
                d.truncate(a.len as usize);
                Ok(d)
            }
            _ => Err(Error::new("not a toy")),
        }
    }

    fn is_part(&self, a: &Apply) -> bool {
        a.module == COPY
    }

    fn apply_part(&self, a: &Apply, inputs: &mut [&mut dyn PartIn], output: &mut dyn PartOut) -> Result<()> {
        bounded(a.len)?;
        for i in inputs {
            let mut failed = None;
            let n = i.len();
            i.pull(n, &mut |d| {
                if failed.is_none()
                    && let Err(e) = output.push(d)
                {
                    failed = Some(e);
                }
            })
            .map_err(|e| Error::new(e.to_string()))?;
            if let Some(e) = failed {
                return Err(Error::new(e.to_string()));
            }
        }
        Ok(())
    }

    fn apply_shared(&self, s: &Shared, inputs: &[&[u8]]) -> Result<Vec<Vec<u8>>> {
        if s.apply().is_none_or(|a| a.module != CUT) {
            return Err(Error::new("not cut"));
        }
        bounded(s.sizes.iter().fold(0u64, |a, &n| a.saturating_add(n)))?;
        let (all, mut at) = (inputs.concat(), 0usize);
        Ok(s.sizes
            .iter()
            .map(|&n| {
                let a = at.min(all.len());
                at = at.saturating_add(n as usize);
                all[a..at.min(all.len())].to_vec()
            })
            .collect())
    }
}

pub fn toys() -> Natives<'static, Toys> {
    natives(&[REV, CUT, COPY], &Toys)
}

/// The modules of aqr-decompress run natively, and no other (a module not known fails rather
/// than running as WebAssembly: Wasmtime is fuzzed elsewhere, and slow here).
pub struct Known;

impl Runner for Known {
    fn load(&self, wasm: &[u8]) -> Decoded<Box<dyn Loaded + '_>> {
        match aqr_decompress::known(wasm) {
            Some(_) => NATIVE.load(wasm),
            None => Err(Failure::Resource("a module not known".into())),
        }
    }

    fn registered(&self, hash: &[u8; 32]) -> Decoded<Vec<u8>> {
        NATIVE.registered(hash)
    }
}

static NATIVE: aqr_decompress::Native = aqr_decompress::Native { threads: 1 };

/// How the encoder writes module `m`: its hash alone (a shorthand module block), so that the
/// archives fuzzed hold no module bytes to waste mutations on.
pub fn registered(m: Module) -> Option<Wasm> {
    let k = aqr_decompress::built(m)?;
    let wasm = aqr_archive::module_of_block_data(k.block)?;
    Some(Wasm::Registered { hash: k.sha512_256, classes: aqr_archive::task_classes(&wasm) })
}

/// The options files are compressed with: one thread, the default level, modules by their
/// hashes.
pub fn options() -> aqr_compress::Options {
    aqr_compress::Options { threads: 1, modules: registered, ..aqr_compress::Options::default() }
}

/// The targets' mutator of archives (libFuzzer's `LLVMFuzzerCustomMutator`): half of the time,
/// a block of `data` mutated as [`mutate_block`] does; else, and where that does not, `data`
/// mutated as it is.
#[cfg(fuzzing)]
pub fn mutate(data: &mut [u8], size: usize, max_size: usize, seed: u32) -> usize {
    let mut rng = Rng(seed | 1);
    if rng.next() % 2 == 0 && mutate_block(&mut data[..size], &mut rng) {
        return size;
    }
    libfuzzer_sys::fuzzer_mutate(data, size, max_size)
}

/// [`mutate`] of encrypted archives (`envelope`'s): half of the time, a block of the payload
/// mutated, the payload decrypted by the seeds' passphrase and put back
/// ([`aqr_archive::fuzzing::Sealed::put`]).
#[cfg(fuzzing)]
pub fn mutate_sealed(data: &mut [u8], size: usize, max_size: usize, seed: u32) -> usize {
    let mut rng = Rng(seed | 1);
    if rng.next() % 2 == 0
        && let Some(s) = aqr_archive::fuzzing::unsealed(&data[..size], PASSPHRASE, WORK_FACTOR)
    {
        let mut p = s.payload.clone();
        if mutate_block(&mut p, &mut rng) {
            s.put(&mut data[..size], &p);
            return size;
        }
    }
    libfuzzer_sys::fuzzer_mutate(data, size, max_size)
}

/// xorshift32, of the seed libFuzzer gives.
#[cfg(fuzzing)]
struct Rng(u32);

#[cfg(fuzzing)]
impl Rng {
    fn next(&mut self) -> u32 {
        let s = &mut self.0;
        *s ^= *s << 13;
        *s ^= *s >> 17;
        *s ^= *s << 5;
        *s
    }
}

/// Archive `c` mutated in place where a block's data decodes by itself
/// ([`aqr_archive::fuzzing::blocks`]): one such block picked, its data decoded, mutated by
/// libFuzzer's mutator and the block made again of it, a spacer block after it taking the bytes
/// it no longer does (so that every block after it, and what tells where they are, stays where
/// it was). Whether it was: not where no block decodes so, or the block made again is longer
/// than it was.
#[cfg(fuzzing)]
fn mutate_block(c: &mut [u8], rng: &mut Rng) -> bool {
    let blocks = aqr_archive::fuzzing::blocks(c);
    if blocks.is_empty() {
        return false;
    }
    let b = &blocks[rng.next() as usize % blocks.len()];
    let Some(mut d) = b.data(c) else { return false };
    // (a little longer at most: compressed again, it may still fit)
    let (n, max) = (d.len(), d.len() + d.len() / 8 + 16);
    d.resize(max, 0);
    let m = libfuzzer_sys::fuzzer_mutate(&mut d, n, max);
    d.truncate(m);
    let mut made = b.of(c, &d);
    let room = b.at.len();
    // (both of an even length: a spacer block, of 2 bytes at least, takes the rest)
    if made.len() > room {
        return false;
    }
    if made.len() < room {
        made.extend_from_slice(&aqr_archive::fuzzing::spacer(room - made.len()));
    }
    c[b.at.clone()].copy_from_slice(&made);
    true
}
