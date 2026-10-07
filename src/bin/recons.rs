//! Recompression demo with the main representation (`flate_recons::deconstruct`): the
//! decisions are packed with the built-in priors (`model::pack`).

use std::process::ExitCode;

use flate_recons::model::{Packer, unpack};
use flate_recons::{Options, Priors};

#[path = "common/zst.rs"]
mod zst;

/// The extension of recompressed files.
const EXT: &str = ".zst+fr";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let join = |data: &[u8], meta: &[u8]| Ok(unpack(data, meta, Priors::builtin())?);
    let r = match args.first().map(String::as_str) {
        Some("compress") => zst::compress(
            &args[1..],
            EXT,
            |raw| {
                let mut p = Packer::new(Priors::builtin());
                let data = flate_recons::deconstruct_into(raw, &Options::default(), &mut p)?;
                Ok((data, p.finish()))
            },
            join,
        ),
        Some("decompress") => zst::decompress(&args[1..], EXT, join),
        _ => {
            eprint!(
                "usage:\n{}",
                zst::usage("flate-recons", EXT, "with the decision model")
            );
            return ExitCode::from(2);
        }
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("flate-recons: {e}");
            ExitCode::FAILURE
        }
    }
}
