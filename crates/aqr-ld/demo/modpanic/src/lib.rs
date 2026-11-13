//! A module of the PE-merge validation that unwinds (crates/aqr-ld/pemerge-test.sh): out[i] =
//! in[i], each byte given back by a panic caught (`catch_unwind`), and one more panic caught on a
//! thread of its own. Built for aqr-dll's no-TLS target (std's thread-locals by TlsAlloc), as the
//! modules' blobs are: their panics are C++ exceptions, thrown and caught through the program's
//! Visual C++ runtime (aqr_blob::VCRUNTIME) with the exception data the merge moved into the
//! stub's directory, or the tables `aqr_blob::load` registered.

use std::panic::{catch_unwind, panic_any};

/// # Safety
/// `inp`/`outp` point to `inlen`/`outlen` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn aqr_mod_apply(inp: *const u8, inlen: usize, outp: *mut u8, outlen: usize) -> u32 {
    if inlen != outlen {
        return 1;
    }
    std::panic::set_hook(Box::new(|_| {}));
    let (inp, out) = unsafe { (std::slice::from_raw_parts(inp, inlen), std::slice::from_raw_parts_mut(outp, outlen)) };
    for (o, &b) in out.iter_mut().zip(inp) {
        match catch_unwind(|| panic_any(b)) {
            Err(payload) => *o = payload.downcast_ref::<u8>().copied().unwrap_or(!b),
            Ok(()) => return 2,
        }
    }
    let thread = std::thread::spawn(|| catch_unwind(|| panic!("on a thread")).is_err());
    if !thread.join().unwrap_or(false) {
        return 3;
    }
    0
}
