//! aqr-pdf as a module of entry formulas (see aqr-formula, FORMAT.md): the table, the
//! skeleton and the items' data as input slots, no parameters.
//!
//! The items that other recompressors code are not decoded by the module. Their results are
//! packed input slots, which the formula fills with the children's formulas: the DEFLATE
//! streams of `FULL` items (aqr-flate applied to a slice of a pool and a slice of the
//! metadata), the PNG files that wrap `PNG` items (aqr-png) and the `JPEG` items (aqr-jpeg).
//! The module only places their bytes, with the zlib header and Adler-32 around a DEFLATE
//! stream.

use crate::encode::{self, Options, Stats};
use aqr_formula::{Apply, Formula, Module};

pub use crate::module::{DUP, FULL, JPEG, MODES, NMODES, PNG, RAW, TOK, decode};

pub const MODULE: Module = Module { name: "pdf-recons", version: 2 };

/// The formula giving `input`, or `None` where the formula would not be smaller than `input`
/// (or no item is coded). `threads` as in [`Options::threads`].
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    encode_with(input, &Options { threads, ..Options::default() })
}

/// [`encode`] with options.
pub fn encode_with(input: &[u8], opts: &Options) -> Option<Formula> {
    encode_stats(input, opts, &mut Stats::default())
}

/// [`encode_with`], and what the encoder did (as of the last attempt, if a panic or a failed
/// verification made it code the file again).
pub fn encode_stats(input: &[u8], opts: &Options, st: &mut Stats) -> Option<Formula> {
    let attempt = |o: &Options| {
        std::panic::catch_unwind(|| {
            let mut s = Stats::default();
            let f = encode::try_formula(input, o, &mut s);
            (f, s)
        })
        .ok()
    };
    let (mut res, mut s) = attempt(opts)?;
    if opts.verify
        && let Some(f) = &res
        && f.eval(&|a: &Apply, i: &[&[u8]]| apply(a, i, opts.threads)).as_deref() != Ok(input)
    {
        let o = Options { tok: false, full: false, png: false, ..opts.clone() };
        (res, s) = attempt(&o)?;
    }
    *st = s;
    res
}

/// Runs the modules a formula of this module has in it (this one, aqr-flate, aqr-png and
/// aqr-jpeg).
pub fn apply(a: &Apply, inputs: &[&[u8]], threads: usize) -> aqr_formula::Result<Vec<u8>> {
    let err = |e: &dyn std::fmt::Debug| aqr_formula::Error::new(format!("{}: {e:?}", a.module));
    if a.module == MODULE {
        decode(&a.params, inputs, a.len, threads).map_err(|e| err(&e))
    } else if a.module == aqr_flate::formula::MODULE {
        let resources: Vec<&[u8]> = a.resources.iter().map(|r| &r[..]).collect();
        aqr_flate::formula::decode(&a.params, &resources, inputs, a.len, 1).map_err(|e| err(&e))
    } else if a.module == aqr_png::formula::MODULE {
        aqr_png::formula::decode(&a.params, inputs, a.len, 1).map_err(|e| err(&e))
    } else if a.module == aqr_jpeg::formula::MODULE {
        aqr_jpeg::formula::decode(&a.params, inputs, a.len, 1).map_err(|e| err(&e))
    } else {
        Err(aqr_formula::Error::new(format!("no module {}", a.module)))
    }
}
