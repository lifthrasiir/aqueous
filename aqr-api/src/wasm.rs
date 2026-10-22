//! Imports from `aqr_api`, each of which `aqr-ld` replaces with an instruction or an `aqr` import.
//! Every import of a memory has the operands of its instruction in the same order.

#[link(wasm_import_module = "aqr_api")]
unsafe extern "C" {
    #[link_name = "write_output"]
    fn write_output_(dst: u64, src: *const u8, len: usize);
    #[link_name = "request"]
    fn request_(k: u32, hi: u64);
    #[link_name = "release"]
    fn release_(k: u32, lo: u64);
    #[link_name = "commit"]
    fn commit_(hi: u64);
    #[link_name = "heartbeat"]
    fn heartbeat_();
    #[link_name = "log"]
    fn log_(ptr: *const u8, len: usize);
}

#[inline]
pub fn write_output(dst: u64, src: &[u8]) {
    // `memory.copy output working`, trapping out of bounds.
    unsafe { write_output_(dst, src.as_ptr(), src.len()) }
}

#[inline]
pub fn request(k: u32, hi: u64) {
    unsafe { request_(k, hi) }
}

#[inline]
pub fn release(k: u32, lo: u64) {
    unsafe { release_(k, lo) }
}

#[inline]
pub fn commit(hi: u64) {
    unsafe { commit_(hi) }
}

#[inline]
pub fn heartbeat() {
    unsafe { heartbeat_() }
}

#[inline]
pub fn log(msg: &[u8]) {
    unsafe { log_(msg.as_ptr(), msg.len()) }
}

#[inline]
pub fn trap() -> ! {
    core::arch::wasm32::unreachable()
}

macro_rules! memory {
    ($ty:ty, $name:literal, $id:expr) => {
        impl $crate::Memory for $ty {
            #[inline]
            fn size() -> u64 {
                // `memory.size`, in pages.
                #[link(wasm_import_module = "aqr_api")]
                unsafe extern "C" {
                    #[link_name = concat!("size_", $name)]
                    fn f() -> u64;
                }
                unsafe { f() << 16 }
            }

            #[inline]
            fn read(src: u64, dst: &mut [u8]) {
                // `memory.copy working $name`, trapping out of bounds.
                #[link(wasm_import_module = "aqr_api")]
                unsafe extern "C" {
                    #[link_name = concat!("read_", $name)]
                    fn f(dst: *mut u8, src: u64, len: usize);
                }
                unsafe { f(dst.as_mut_ptr(), src, dst.len()) }
            }

            #[inline]
            fn copy_to_output(dst: u64, src: u64, len: u64) {
                // `memory.copy output $name`.
                #[link(wasm_import_module = "aqr_api")]
                unsafe extern "C" {
                    #[link_name = concat!("copyout_", $name)]
                    fn f(dst: u64, src: u64, len: u64);
                }
                unsafe { f(dst, src, len) }
            }
        }
    };
}
pub(crate) use memory;

/// Logs `panicked at FILE:LINE:COLUMN` (with `panic-location`) and traps, without formatting
/// the message so that no formatting code is linked.
#[cfg(feature = "panic-handler")]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    #[cfg(feature = "panic-location")]
    log_location(_info);
    trap()
}

#[cfg(feature = "panic-location")]
fn log_location(info: &core::panic::PanicInfo) {
    let mut buf = [0; 256];
    let mut len = 0;
    let mut put = |bytes: &[u8]| {
        let n = bytes.len().min(buf.len() - len);
        buf[len..len + n].copy_from_slice(&bytes[..n]);
        len += n;
    };
    put(b"panicked");
    if let Some(loc) = info.location() {
        put(b" at ");
        put(loc.file().as_bytes());
        for n in [loc.line(), loc.column()] {
            let mut digits = [0; 10];
            let mut i = digits.len();
            let mut n = n;
            loop {
                i -= 1;
                digits[i] = b'0' + (n % 10) as u8;
                n /= 10;
                if n == 0 {
                    break;
                }
            }
            put(b":");
            put(&digits[i..]);
        }
    }
    log(&buf[..len]);
}
