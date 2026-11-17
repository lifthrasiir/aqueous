//! The DPI of a window and the metrics at it, by the functions of Windows 10 1607 on where the
//! system has them (looked up as it runs, so that aqr-win starts on Windows 7 too), else the
//! system's DPI (Windows 7 and 8 scale every window by it, the manifest's per-monitor awareness
//! notwithstanding before 8.1).

use std::ffi::c_void;
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::Graphics::Gdi::{GetDC, GetDeviceCaps, LOGPIXELSY, ReleaseDC};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows_sys::Win32::UI::WindowsAndMessaging::{AdjustWindowRectEx, SystemParametersInfoW};
use windows_sys::w;

type GetDpiForWindow = unsafe extern "system" fn(HWND) -> u32;
type SystemParametersInfoForDpi = unsafe extern "system" fn(u32, u32, *mut c_void, u32, u32) -> i32;
type AdjustWindowRectExForDpi = unsafe extern "system" fn(*mut RECT, u32, i32, u32, u32) -> i32;

struct Functions {
    dpi_for_window: Option<GetDpiForWindow>,
    parameters_for_dpi: Option<SystemParametersInfoForDpi>,
    adjust_for_dpi: Option<AdjustWindowRectExForDpi>,
}

fn functions() -> &'static Functions {
    static F: OnceLock<Functions> = OnceLock::new();
    F.get_or_init(|| {
        // SAFETY: user32.dll, loaded with the program (it imports from it); each function taken as
        // the type of its documented signature
        unsafe {
            let user32 = GetModuleHandleW(w!("user32.dll"));
            let f = |name: &[u8]| if user32.is_null() { None } else { GetProcAddress(user32, name.as_ptr()) };
            Functions {
                dpi_for_window: f(b"GetDpiForWindow\0").map(|p| std::mem::transmute::<_, GetDpiForWindow>(p)),
                parameters_for_dpi: f(b"SystemParametersInfoForDpi\0")
                    .map(|p| std::mem::transmute::<_, SystemParametersInfoForDpi>(p)),
                adjust_for_dpi: f(b"AdjustWindowRectExForDpi\0")
                    .map(|p| std::mem::transmute::<_, AdjustWindowRectExForDpi>(p)),
            }
        }
    })
}

/// The DPI of window `h` (at least 96).
pub fn for_window(h: HWND) -> u32 {
    let dpi = match functions().dpi_for_window {
        // SAFETY: a window handle
        Some(f) => unsafe { f(h) },
        // SAFETY: the screen's DC, released
        None => unsafe {
            let dc = GetDC(std::ptr::null_mut());
            let d = GetDeviceCaps(dc, LOGPIXELSY as i32);
            ReleaseDC(std::ptr::null_mut(), dc);
            d.max(0) as u32
        },
    };
    dpi.max(96)
}

/// `SystemParametersInfoW` at `dpi` (where the system scales only by its own DPI, at that).
///
/// # Safety
/// As `SystemParametersInfoW`: `param` is what `action` wants, of `size` bytes.
pub unsafe fn parameters(action: u32, size: u32, param: *mut c_void, dpi: u32) -> i32 {
    match functions().parameters_for_dpi {
        Some(f) => unsafe { f(action, size, param, 0, dpi) },
        None => unsafe { SystemParametersInfoW(action, size, param, 0) },
    }
}

/// `AdjustWindowRectEx` at `dpi` (where the system scales only by its own DPI, at that).
pub fn adjust(r: &mut RECT, style: u32, ex_style: u32, dpi: u32) {
    // SAFETY: a rectangle of ours
    unsafe {
        match functions().adjust_for_dpi {
            Some(f) => f(r, style, 0, ex_style, dpi),
            None => AdjustWindowRectEx(r, style, 0, ex_style),
        };
    }
}
