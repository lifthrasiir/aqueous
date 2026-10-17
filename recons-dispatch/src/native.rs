//! The modules formulas apply, implemented natively by the recompressors (§5.7).

use std::panic::{AssertUnwindSafe, catch_unwind};

use exe_recons::arch::x86;
use recons_formula::{Apply, Error, Module, Modules, Result};

/// exe-recons' x86 disfilter on a code section. Parameters: the origin (u64, little endian),
/// the sentinel size (u8) and 1 for x86-64 or 0 for x86-32; inputs: the streams of
/// [`x86::STREAM_NAMES`].
pub const X86: Module = Module { name: "exe-recons/x86", version: 1 };

/// flate-recons reconstruction of a raw DEFLATE stream. Inputs: the data and the record.
pub const FLATE: Module = Module { name: "flate-recons", version: 1 };

/// The parameters of [`X86`].
pub(crate) fn x86_params(origin: u64, sentinel_size: u8, is64: bool) -> Vec<u8> {
    let mut p = origin.to_le_bytes().to_vec();
    p.extend_from_slice(&[sentinel_size, is64 as u8]);
    p
}

/// A recompressor taken whole: its container (magic, version or mode, internal zstd and
/// checksum included) is the module's single input. What the archive would take over from the
/// container stays in for now (appendix A).
pub struct Whole {
    pub name: &'static str,
    pub compress: fn(&[u8], usize) -> Vec<u8>,
    pub decompress: fn(&[u8], usize) -> std::result::Result<Vec<u8>, String>,
}

impl Whole {
    pub const fn module(&self) -> Module {
        Module { name: self.name, version: 1 }
    }
}

fn msg(e: impl std::fmt::Debug) -> String {
    format!("{e:?}")
}

pub const PNG: Whole = Whole {
    name: "png-recons",
    compress: png_recons::compress_threads,
    decompress: |c, t| png_recons::decompress_threads(c, t).map_err(msg),
};
pub const JPEG: Whole = Whole {
    name: "jpeg-recons",
    compress: |d, _| jpeg_recons::container::compress(d),
    decompress: |c, _| jpeg_recons::container::decompress(c).map_err(msg),
};
pub const MP3: Whole = Whole {
    name: "mp3-recons",
    compress: mp3_recons::compress_threads,
    decompress: |c, t| mp3_recons::decompress_threads(c, t).map_err(msg),
};
pub const FLAC: Whole = Whole {
    name: "flac-recons",
    compress: |d, threads| flac_recons::compress_opt(d, &flac_recons::Options { raw: None, threads }),
    decompress: |c, t| flac_recons::decompress_threads(c, t).map_err(msg),
};
pub const PSD: Whole = Whole {
    name: "psd-recons",
    compress: |d, threads| psd_recons::compress_with(d, &psd_recons::Options { threads }),
    decompress: |c, t| psd_recons::decompress_threads(c, t).map_err(msg),
};
pub const PDF: Whole = Whole {
    name: "pdf-recons",
    compress: |d, _| pdf_recons::compress(d),
    decompress: |c, t| pdf_recons::container::decompress_threads(c, t).map_err(msg),
};
pub const SQLITE: Whole = Whole {
    name: "sqlite-recons",
    compress: |d, _| sqlite_recons::compress(d),
    decompress: |c, t| sqlite_recons::container::decompress_threads(c, t).map_err(msg),
};

pub const WHOLE: [&Whole; 7] = [&PNG, &JPEG, &MP3, &FLAC, &PSD, &PDF, &SQLITE];

/// Numbers of the modules in the formula container ([`recons_formula::container`]). They are
/// stored, so a number is never reused for another module.
pub const IDS: [(u64, Module); 9] = [
    (0x80, X86),
    (0x81, FLATE),
    (0x82, PNG.module()),
    (0x83, JPEG.module()),
    (0x84, MP3.module()),
    (0x85, FLAC.module()),
    (0x86, PSD.module()),
    (0x87, PDF.module()),
    (0x88, SQLITE.module()),
];

/// Runs every module of this crate natively. The output does not depend on `threads`.
#[derive(Debug, Clone, Copy)]
pub struct Native {
    pub threads: usize,
}

impl Default for Native {
    fn default() -> Native {
        Native { threads: 1 }
    }
}

impl Native {
    /// The output of `m` on `inputs` (see [`Modules::apply`]).
    pub fn run(&self, m: Module, params: &[u8], inputs: &[&[u8]]) -> Result<Vec<u8>> {
        catch_unwind(AssertUnwindSafe(|| self.run_unchecked(m, params, inputs)))
            .unwrap_or_else(|_| Err("panicked".into()))
            .map_err(|e| Error::new(format!("{m}: {e}")))
    }

    fn run_unchecked(&self, m: Module, params: &[u8], inputs: &[&[u8]]) -> std::result::Result<Vec<u8>, String> {
        let threads = self.threads.max(1);
        if m == X86 {
            let (Some(p), Ok(streams)) = (params.first_chunk::<10>(), <[&[u8]; x86::NUM_STREAMS]>::try_from(inputs))
            else {
                return Err("bad parameters or inputs".into());
            };
            let origin = u64::from_le_bytes(*p.first_chunk().unwrap());
            x86::decode(&x86::DecodedStreams::new(origin, p[8], streams), p[9] != 0).ok_or_else(|| "corrupt".into())
        } else if m == FLATE {
            let &[data, record] = inputs else { return Err("bad inputs".into()) };
            flate_recons::reconstruct_with_threads(data, record, threads).map_err(msg)
        } else if let Some(w) = WHOLE.iter().find(|w| w.module() == m) {
            let &[c] = inputs else { return Err("bad inputs".into()) };
            (w.decompress)(c, threads)
        } else {
            Err("unknown module".into())
        }
    }
}

impl Modules for Native {
    fn apply(&self, a: &Apply, inputs: &[&[u8]]) -> Result<Vec<u8>> {
        self.run(a.module, &a.params, inputs)
    }
}
