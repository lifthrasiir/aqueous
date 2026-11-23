//! `aqr -Z interpret ARCHIVE` (a verb, though of `-Z`: not stable across versions): the archive
//! value an Aqueous archive is interpreted to (spec, "Archive model"), in EDN on standard output,
//! as the conformance suite writes it (`conformance/case.cddl`, [`aqr_archive::interpret`]).
//! The archive is located and decrypted as `-K` does, but nothing repaired from mirror blocks
//! nor salvaged: its failure is told by the exit code as `-K`'s is.

use std::io::Write;

use crate::{Args, exit};

pub fn interpret(a: &Args) -> Result<(), String> {
    let input = &a.inputs[0];
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", input.display());
    let c = crate::map_archive(input, &input.display())?;
    let sealed = aqr_archive::envelope::is_encrypted(&c) || aqr_archive::envelope::is_damaged_encrypted(&c);
    let decrypted = match sealed {
        true => match crate::decrypt(&c, input, a.opt.threads)? {
            (d, erased) if erased.is_empty() => Some(d),
            _ => {
                exit::note(exit::Code::Invalid);
                return Err(err(&"encrypted chunks not authenticating"));
            }
        },
        false => None,
    };
    let payload = match decrypted.as_deref() {
        Some(d) => d,
        None => crate::located(&c, input, a.quiet),
    };
    let decoder = crate::Decoder {
        native: aqr_decompress::Native { threads: a.opt.threads },
        wasm: crate::WASM.get(),
        sandbox: crate::SANDBOX.get().then(std::sync::OnceLock::new),
    };
    let value = aqr_archive::interpret::archive_value(payload, sealed, &decoder, a.opt.threads)
        .map_err(|e| err(&exit::seen(e)))?;
    let mut out = std::io::stdout().lock();
    writeln!(out, "{}", aqr_archive::interpret::edn(&value)).and_then(|_| out.flush()).map_err(|e| e.to_string())
}
