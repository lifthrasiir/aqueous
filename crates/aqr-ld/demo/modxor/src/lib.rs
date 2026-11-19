//! A trivial no_std module for the PE-merge validation: out[i] = in[i] ^ 0x5a.
// (no_std where built as pemerge-test.sh does, with panic=abort; with std otherwise, as the
// workspace's checks and tests build it)
#![cfg_attr(panic = "abort", no_std)]

#[cfg(panic = "abort")]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

/// # Safety
/// `inp`/`outp` point to `inlen`/`outlen` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn aqr_mod_apply(inp: *const u8, inlen: usize, outp: *mut u8, outlen: usize) -> u32 {
    if inlen != outlen {
        return 1;
    }
    let mut i = 0;
    while i < inlen {
        unsafe { *outp.add(i) = *inp.add(i) ^ 0x5a };
        i += 1;
    }
    0
}
