#![cfg_attr(target_family = "wasm", no_std)]

extern crate alloc;

// (the allocator of its Wasm modules, built without std: their memory laid out as a stack, what
// each takes its plan, `module::plan` and the others there)
#[cfg(target_family = "wasm")]
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

#[cfg(target_family = "wasm")]
#[panic_handler]
unsafe fn panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

pub mod arch;
pub mod codesign;
pub mod format;
#[cfg(feature = "decode")]
pub mod module;
pub mod reloc;
pub mod symtab;
mod util;
