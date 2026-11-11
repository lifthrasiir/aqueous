//! A trivial no_std module for the PE-merge validation: out[i] = in[i] + 7.
#![no_std]
#![no_main]

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
        unsafe { *outp.add(i) = (*inp.add(i)).wrapping_add(7) };
        i += 1;
    }
    0
}
