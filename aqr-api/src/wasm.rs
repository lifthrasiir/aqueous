//! Imports from `aqr_api`, each of which `aqr-ld` replaces with an instruction or an `aqr` import.
//! Every import of a memory has the operands of its instruction in the same order.

#[link(wasm_import_module = "aqr_api")]
unsafe extern "C" {
    #[link_name = "request"]
    fn request_(k: u32, hi: u64);
    #[link_name = "release"]
    fn release_(k: u32, lo: u64);
    #[link_name = "commit"]
    fn commit_(j: u32, hi: u64);
    #[link_name = "heartbeat"]
    fn heartbeat_();
    #[link_name = "log"]
    fn log_(ptr: *const u8, len: usize);
    #[link_name = "spawn"]
    fn spawn_(rec: *const u8, n: u32) -> u32;
    #[link_name = "result"]
    fn result_(ptr: *const u8, len: usize);
    #[link_name = "join"]
    fn join_(h: u32, i: u32) -> u32;
    #[link_name = "take"]
    fn take_(h: u32, i: u32, dst: *mut u8, len: usize);
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
pub fn commit(j: u32, hi: u64) {
    unsafe { commit_(j, hi) }
}

#[inline]
pub fn heartbeat() {
    unsafe { heartbeat_() }
}

#[inline]
pub fn log(msg: &[u8]) {
    unsafe { log_(msg.as_ptr(), msg.len()) }
}

#[cfg(feature = "alloc")]
#[inline]
pub fn spawn(rec: &crate::task::Record, n: usize) -> u32 {
    let n = u32::try_from(n).unwrap_or_else(|_| trap());
    unsafe { spawn_(rec as *const crate::task::Record as *const u8, n) }
}

#[cfg(feature = "alloc")]
#[inline]
pub fn result(out: &[u8]) {
    unsafe { result_(out.as_ptr(), out.len()) }
}

#[cfg(feature = "alloc")]
#[inline]
pub fn join(h: u32, i: usize) -> usize {
    unsafe { join_(h, i as u32) as usize }
}

#[cfg(feature = "alloc")]
#[inline]
pub fn take(h: u32, i: usize, dst: &mut [u8]) {
    unsafe { take_(h, i as u32, dst.as_mut_ptr(), dst.len()) }
}

/// The body of `aqr_task`, which `aqr-ld` makes of it (see [`crate::task`]); a trap without
/// tasks (the feature `alloc`).
#[doc(hidden)]
#[inline]
pub unsafe fn task_body(rec: *const u8, i: u32) {
    #[cfg(feature = "alloc")]
    unsafe {
        crate::task::body(rec as *const crate::task::Record, i as usize)
    }
    #[cfg(not(feature = "alloc"))]
    {
        let _ = (rec, i);
        trap()
    }
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
        }
    };
}
pub(crate) use memory;

/// The writing of the `$j`-th output.
macro_rules! output {
    ($j:literal) => {
        impl $crate::Writable for $crate::Output<$j> {
            #[inline]
            fn write(dst: u64, src: &[u8]) {
                // `memory.copy output$j working`, trapping out of bounds.
                #[link(wasm_import_module = "aqr_api")]
                unsafe extern "C" {
                    #[link_name = concat!("write_output", $j)]
                    fn f(dst: u64, src: *const u8, len: usize);
                }
                unsafe { f(dst, src.as_ptr(), src.len()) }
            }
        }
    };
}
pub(crate) use output;

/// The copy into the `$j`-th output from the memory `$ty`.
macro_rules! copy {
    ($j:literal, $ty:ty, $name:literal, $id:expr) => {
        impl $crate::CopyTo<$j> for $ty {
            #[inline]
            fn copy_to(dst: u64, src: u64, len: u64) {
                // `memory.copy output$j $name`.
                #[link(wasm_import_module = "aqr_api")]
                unsafe extern "C" {
                    #[link_name = concat!("copy_output", $j, "_", $name)]
                    fn f(dst: u64, src: u64, len: u64);
                }
                unsafe { f(dst, src, len) }
            }
        }
    };
}
pub(crate) use copy;

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
