#![cfg_attr(target_family = "wasm", no_std)]

extern crate alloc;

#[cfg(target_family = "wasm")]
#[global_allocator]
static ALLOC: mini_alloc::MiniAlloc = mini_alloc::MiniAlloc::INIT;

#[cfg(target_family = "wasm")]
#[panic_handler]
unsafe fn panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

pub mod arch;
pub mod format;
mod util;
